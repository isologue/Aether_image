use super::{config::load_password, response::ImageRequest};
use crate::AppState;
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::StreamExt;
use serde_json::{json, Value};

#[derive(Debug)]
pub(crate) enum ResultReadError {
    Cleaned,
    Unavailable(String),
}

pub(super) async fn persist(
    state: &AppState,
    request: &ImageRequest,
    bytes: &[u8],
) -> Result<Value, String> {
    let local = if request.config.stores_local() {
        Some(state.image_storage.save(bytes).await?)
    } else {
        None
    };
    let mut remote = None;
    if request.config.stores_webdav() {
        let uploaded = match load_password(state).await {
            Ok(password) => match &local {
                Some(record) => {
                    state
                        .image_storage
                        .upload(&request.config, &password, record)
                        .await
                }
                None => {
                    state
                        .image_storage
                        .upload_bytes(&request.config, &password, bytes.to_vec())
                        .await
                }
            },
            Err(e) => Err(e),
        };
        match uploaded {
            Ok(url) => remote = Some(url),
            Err(e) if local.is_none() => return Err(e),
            Err(e) => tracing::warn!(error=%e,"task image uses local fallback"),
        }
    }
    let local_url = if request.use_url && local.is_some() {
        let base = request
            .public_base
            .as_deref()
            .ok_or("Configure an image public base URL")?;
        Some(
            super::webdav::joined_url(
                base,
                &format!("media/images/{}", local.as_ref().unwrap().path),
            )?
            .to_string(),
        )
    } else {
        None
    };
    let url = if let Some(url) = &remote {
        Some(url.clone())
    } else if request.use_url {
        let base = request
            .public_base
            .as_deref()
            .ok_or("Configure an image public base URL")?;
        Some(
            super::webdav::joined_url(
                base,
                &format!(
                    "media/images/{}",
                    local.as_ref().ok_or("Image storage unavailable")?.path
                ),
            )?
            .to_string(),
        )
    } else {
        None
    };
    Ok(
        json!({"local":local,"local_url":local_url,"remote_url":remote,"url":url,"use_url":request.use_url}),
    )
}
async fn local_bytes(
    state: &AppState,
    reference: &Value,
    local_available: bool,
) -> Result<Vec<u8>, ResultReadError> {
    let Some(path) = reference["local"]["path"].as_str() else {
        return Err(ResultReadError::Cleaned);
    };
    if !local_available {
        return Err(ResultReadError::Unavailable(
            "Local image belongs to another storage node".into(),
        ));
    }
    if !super::storage::valid_image_path(path) {
        return Err(ResultReadError::Unavailable(
            "Invalid stored image path".into(),
        ));
    }
    let _reader = state
        .image_storage
        .reader(reference["local"]["id"].as_str().unwrap_or(""));
    use tokio::io::AsyncReadExt;
    let file = match tokio::fs::File::open(state.image_storage.root.join("objects").join(path))
        .await
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ResultReadError::Cleaned),
        Err(e) => return Err(ResultReadError::Unavailable(e.to_string())),
    };
    let mut b = Vec::new();
    file.take(super::storage::MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut b)
        .await
        .map_err(|e| ResultReadError::Unavailable(e.to_string()))?;
    if b.len() > super::storage::MAX_IMAGE_BYTES {
        return Err(ResultReadError::Unavailable(
            "Stored image exceeds 64 MiB".into(),
        ));
    }
    super::storage::image_type(&b).map_err(ResultReadError::Unavailable)?;
    Ok(b)
}
async fn remote_bytes(state: &AppState, reference: &Value) -> Result<Vec<u8>, ResultReadError> {
    let Some(url) = reference["remote_url"].as_str() else {
        return Err(ResultReadError::Cleaned);
    };
    let response = state
        .image_storage
        .public_client
        .get(url)
        .send()
        .await
        .map_err(|_| ResultReadError::Unavailable("WebDAV image temporarily unavailable".into()))?;
    if matches!(response.status().as_u16(), 404 | 410) {
        return Err(ResultReadError::Cleaned);
    }
    if !response.status().is_success() {
        return Err(ResultReadError::Unavailable(format!(
            "WebDAV image returned HTTP {}",
            response.status().as_u16()
        )));
    }
    let mut stream = response.bytes_stream();
    let mut b = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|_| ResultReadError::Unavailable("WebDAV image read failed".into()))?;
        if b.len() + chunk.len() > super::storage::MAX_IMAGE_BYTES {
            return Err(ResultReadError::Unavailable(
                "Stored image exceeds 64 MiB".into(),
            ));
        }
        b.extend_from_slice(&chunk);
    }
    super::storage::image_type(&b).map_err(ResultReadError::Unavailable)?;
    Ok(b)
}
pub(crate) async fn read_task_result(
    state: &AppState,
    stored: &Value,
    local_available: bool,
) -> Result<Value, ResultReadError> {
    let mut result = stored.clone();
    let items = result["data"]
        .as_array_mut()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| ResultReadError::Unavailable("Missing durable image results".into()))?;
    for item in items {
        let reference = item.get("$image_ref").cloned().ok_or_else(|| {
            ResultReadError::Unavailable("Missing durable image reference".into())
        })?;
        // For URL responses verify the URL actually returned; prefer DAV, fall back to local.
        let use_url = reference["use_url"] == true;
        let remote_first = use_url && reference["remote_url"].is_string();
        let first = if remote_first {
            remote_bytes(state, &reference).await
        } else {
            local_bytes(state, &reference, local_available).await
        };
        let (b, remote) = match first {
            Ok(b) => (b, remote_first),
            Err(first) => match if remote_first {
                local_bytes(state, &reference, local_available).await
            } else {
                remote_bytes(state, &reference).await
            } {
                Ok(b) => (b, !remote_first),
                Err(second) => {
                    return Err(match (first, second) {
                        (ResultReadError::Cleaned, ResultReadError::Cleaned) => {
                            ResultReadError::Cleaned
                        }
                        (ResultReadError::Unavailable(e), _)
                        | (_, ResultReadError::Unavailable(e)) => ResultReadError::Unavailable(e),
                    })
                }
            },
        };
        let object = item
            .as_object_mut()
            .ok_or_else(|| ResultReadError::Unavailable("Invalid image result".into()))?;
        object.remove("$image_ref");
        if use_url {
            let url = if remote {
                reference["remote_url"].as_str()
            } else {
                reference["local_url"].as_str().or_else(|| {
                    reference["url"]
                        .as_str()
                        .filter(|url| Some(*url) != reference["remote_url"].as_str())
                })
            }
            .ok_or_else(|| {
                ResultReadError::Unavailable("Available image has no configured public URL".into())
            })?;
            object.insert("url".into(), json!(url));
        } else {
            object.insert("b64_json".into(), json!(STANDARD.encode(b)));
        }
    }
    Ok(result)
}
