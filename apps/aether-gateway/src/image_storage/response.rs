use super::{
    config::{load_config, load_password, validate_http_url, ImageConfig},
    storage::{MAX_BODY_BYTES, MAX_IMAGE_BYTES},
};
use crate::{AppState, GatewayError};
use axum::{
    body::{to_bytes, Body, Bytes},
    http::{header, HeaderMap, Response, StatusCode},
};
use base64::Engine;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub(crate) struct ImageRequestSlot(pub Arc<Mutex<Option<ImageRequest>>>);
#[derive(Clone)]
pub(crate) struct ImageRequest {
    pub config: ImageConfig,
    pub use_url: bool,
    pub public_base: Option<String>,
}
pub(crate) fn is_image_path(path: &str) -> bool {
    matches!(
        path.trim_end_matches('/'),
        "/v1/images/generations" | "/v1/images/edits"
    )
}
impl ImageRequestSlot {
    pub async fn capture(
        &self,
        state: &AppState,
        headers: &HeaderMap,
        body: &Bytes,
        trusted_forwarded: bool,
    ) -> Result<Bytes, String> {
        let config = load_config(state)
            .await
            .map_err(|_| "读取图片存储配置失败".to_string())?;
        let content_type = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let mut body_json = serde_json::from_slice::<Value>(body)
            .ok()
            .filter(|v| v.is_object());
        let requested = if let Some(value) = &body_json {
            match value.get("response_format") {
                Some(Value::String(value)) => Some(value.clone()),
                Some(Value::Null) | None => None,
                Some(_) if config.honor_response_format => {
                    return Err("response_format 必须为字符串".into())
                }
                Some(_) => None,
            }
        } else {
            multipart_response_format(body, content_type).map(|(_, _, value)| value)
        };
        let use_url = config.use_url(requested.as_deref())?;
        let public_base = if !config.public_base_url.is_empty() {
            Some(config.public_base_url.clone())
        } else {
            std::env::var("AETHER_PUBLIC_BASE_URL")
                .ok()
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    let host = headers.get(header::HOST)?.to_str().ok()?;
                    let scheme = if trusted_forwarded {
                        headers
                            .get("x-forwarded-proto")
                            .and_then(|s| s.to_str().ok())
                            .filter(|s| *s == "https")
                            .unwrap_or("http")
                    } else {
                        "http"
                    };
                    let candidate = format!("{scheme}://{host}");
                    validate_http_url(&candidate).ok().map(|_| candidate)
                })
        };
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(ImageRequest {
            config,
            use_url,
            public_base,
        });
        // Only the gateway decides the caller's format. Ask providers for their normal Base64 output.
        if let Some(value) = &mut body_json {
            value
                .as_object_mut()
                .expect("object")
                .remove("response_format");
            return serde_json::to_vec(value)
                .map(Bytes::from)
                .map_err(|_| "图片请求序列化失败".into());
        }
        if let Some((start, end, _)) = multipart_response_format(body, content_type) {
            let mut normalized = Vec::with_capacity(body.len() + 8);
            normalized.extend_from_slice(&body[..start]);
            normalized.extend_from_slice(b"b64_json");
            normalized.extend_from_slice(&body[end..]);
            return Ok(Bytes::from(normalized));
        }
        Ok(body.clone())
    }
}
/// Read only the bounded text field; binary image parts are never decoded to text.
pub(crate) fn multipart_response_format(
    body: &[u8],
    content_type: &str,
) -> Option<(usize, usize, String)> {
    if !content_type
        .to_ascii_lowercase()
        .starts_with("multipart/form-data")
    {
        return None;
    }
    let boundary = content_type
        .split(';')
        .find_map(|s| s.trim().strip_prefix("boundary="))
        .map(|s| s.trim_matches('"'))?;
    if boundary.is_empty() || boundary.len() > 200 || boundary.contains(['\r', '\n']) {
        return None;
    }
    let marker = format!("--{boundary}");
    let marker = marker.as_bytes();
    let mut position = 0;
    while position < body.len() {
        let offset = body[position..]
            .windows(marker.len())
            .position(|s| s == marker)?
            + position
            + marker.len();
        if body.get(offset..offset + 2) != Some(b"\r\n") {
            return None;
        }
        let headers_start = offset + 2;
        let headers_end = body[headers_start..]
            .windows(4)
            .take(8192)
            .position(|s| s == b"\r\n\r\n")?
            + headers_start;
        let part_headers = std::str::from_utf8(&body[headers_start..headers_end]).ok()?;
        let data_start = headers_end + 4;
        let next_marker = format!("\r\n--{boundary}");
        let data_end = body[data_start..]
            .windows(next_marker.len())
            .position(|s| s == next_marker.as_bytes())?
            + data_start;
        if part_headers.lines().any(|line| {
            line.to_ascii_lowercase()
                .starts_with("content-disposition:")
                && line
                    .split(';')
                    .any(|part| part.trim() == "name=\"response_format\"")
        }) {
            if data_end - data_start > 64 {
                return Some((data_start, data_end, "invalid".into()));
            }
            return Some((
                data_start,
                data_end,
                std::str::from_utf8(&body[data_start..data_end])
                    .unwrap_or("invalid")
                    .trim()
                    .into(),
            ));
        }
        position = data_end + 2;
    }
    None
}
fn error_json(message: &str) -> Value {
    json!({"error":{"type":"image_storage_error", "code":"image_storage_error", "message":message}})
}
pub(crate) async fn transform_response(
    state: AppState,
    slot: ImageRequestSlot,
    response: Response<Body>,
) -> Result<Response<Body>, GatewayError> {
    if !response.status().is_success() {
        return Ok(response);
    }
    let request = slot.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(request) = request else {
        return Ok(response);
    };
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|s| s.to_str().ok())
        .unwrap_or("")
        .to_string();
    if response.headers().contains_key(header::CONTENT_ENCODING) {
        return Err(GatewayError::Internal(
            "图片响应仍带压缩编码，无法安全转换".into(),
        ));
    }
    if content_type.contains("text/event-stream") {
        return Ok(transform_sse(state, request, response));
    }
    if !content_type.contains("application/json") {
        return Ok(response);
    }
    let _permit = state
        .image_storage
        .response_gate
        .acquire()
        .await
        .map_err(|_| GatewayError::Internal("图片转换服务已停止".into()))?;
    let (mut parts, body) = response.into_parts();
    let bytes = to_bytes(body, MAX_BODY_BYTES)
        .await
        .map_err(|_| GatewayError::Internal("图片响应超过 256 MiB 或读取失败".into()))?;
    let mut value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| GatewayError::Internal("图片响应不是有效 JSON".into()))?;
    if let Err(error) = transform_json(&state, &request, &mut value).await {
        parts.status = StatusCode::BAD_GATEWAY;
        value = error_json(&error);
    }
    let bytes = serde_json::to_vec(&value)
        .map_err(|_| GatewayError::Internal("图片响应序列化失败".into()))?;
    strip_changed_headers(&mut parts.headers);
    Ok(Response::from_parts(parts, Body::from(bytes)))
}
fn strip_changed_headers(headers: &mut HeaderMap) {
    for name in ["content-length", "content-encoding", "etag", "content-md5"] {
        headers.remove(name);
    }
    headers.insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
}
pub(crate) async fn transform_json(
    state: &AppState,
    request: &ImageRequest,
    value: &mut Value,
) -> Result<(), String> {
    if let Some(items) = value.get_mut("data").and_then(Value::as_array_mut) {
        if items.len() > 64 {
            return Err("单次图片响应超过 64 张".into());
        }
        for item in items {
            transform_item(state, request, item).await?;
        }
    } else if matches!(
        value.get("type").and_then(Value::as_str),
        Some("image_generation.completed" | "image_edit.completed")
    ) {
        transform_item(state, request, value).await?;
    }
    Ok(())
}
async fn transform_item(
    state: &AppState,
    request: &ImageRequest,
    item: &mut Value,
) -> Result<(), String> {
    let Some(object) = item.as_object_mut() else {
        return Ok(());
    };
    let b64 = object
        .get("b64_json")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let provider_url = object
        .get("url")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    if b64.is_none() && provider_url.is_none() {
        return Ok(());
    }
    let bytes = if let Some(b64) = &b64 {
        let raw = if b64.starts_with("data:") {
            b64.split_once(',')
                .map(|(_, b)| b)
                .ok_or("图片 data URL 格式错误")?
        } else {
            b64.as_str()
        };
        if raw.len() > MAX_IMAGE_BYTES * 4 / 3 + 8 {
            return Err("图片超过 64 MiB".into());
        }
        base64::engine::general_purpose::STANDARD
            .decode(raw)
            .map_err(|_| "上游图片 Base64 解码失败".to_string())?
    } else {
        state
            .image_storage
            .download(provider_url.as_deref().ok_or("缺少图片 URL")?)
            .await?
    };
    let saved = if request.config.stores_local() {
        Some(state.image_storage.save(&bytes).await)
    } else {
        None
    };
    let mut remote_url = None;
    if request.config.stores_webdav() && request.use_url {
        let uploaded = match load_password(state).await {
            Ok(password) => {
                if let Some(Ok(record)) = &saved {
                    state
                        .image_storage
                        .upload(&request.config, &password, record)
                        .await
                } else {
                    state
                        .image_storage
                        .upload_bytes(&request.config, &password, bytes.clone())
                        .await
                }
            }
            Err(error) => Err(error),
        };
        match uploaded {
            Ok(url) => remote_url = Some(url),
            Err(error) if !request.config.stores_local() => return Err(error),
            Err(error) => {
                tracing::warn!(error = %error, "image WebDAV upload failed; falling back to local image")
            }
        }
    } else if request.config.stores_webdav() && !request.config.stores_local() {
        // Reserve capacity before spawning so slow WebDAV cannot retain unbounded image buffers.
        match load_password(state).await {
            Ok(password) => {
                let permit = state
                    .image_storage
                    .upload_gate
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| "图片上传服务已停止".to_string())?;
                let storage = state.image_storage.clone();
                let config = request.config.clone();
                let upload_bytes = bytes.clone();
                tokio::spawn(async move {
                    if let Err(error) = storage
                        .upload_bytes_with_permit(&config, &password, upload_bytes, permit)
                        .await
                    {
                        tracing::warn!(error = %error, "memory-only image WebDAV upload failed");
                    }
                });
            }
            Err(error) => {
                tracing::warn!(error = %error, "WebDAV password unavailable; returning provider Base64")
            }
        }
    }
    if request.use_url {
        let url = if let Some(remote) = remote_url {
            remote
        } else {
            let record = saved
                .as_ref()
                .ok_or("WebDAV 未返回图片地址")?
                .as_ref()
                .map_err(|e| e.clone())?;
            let base = request
                .public_base
                .as_deref()
                .ok_or("返回本地 URL 前请配置图片公开站点地址或 AETHER_PUBLIC_BASE_URL")?;
            super::webdav::joined_url(base, &format!("media/images/{}", record.path))?.to_string()
        };
        object.remove("b64_json");
        object.insert("url".into(), Value::String(url));
    } else {
        if let Some(Err(error)) = saved {
            tracing::warn!(error = %error, "image persistence failed; returning provider Base64");
        }
        object.remove("url");
        object.insert(
            "b64_json".into(),
            Value::String(base64::engine::general_purpose::STANDARD.encode(bytes)),
        );
    }
    Ok(())
}
fn transform_sse(
    state: AppState,
    request: ImageRequest,
    response: Response<Body>,
) -> Response<Body> {
    let (mut parts, body) = response.into_parts();
    strip_changed_headers(&mut parts.headers);
    let stream: std::pin::Pin<
        Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>,
    > = Box::pin(async_stream::try_stream! {
        let mut source = body.into_data_stream(); let mut buffer = Vec::new();
        while let Some(chunk) = source.next().await {
            let chunk = chunk.map_err(std::io::Error::other)?;
            if buffer.len() + chunk.len() > MAX_BODY_BYTES { Err(std::io::Error::other("Image SSE frame too large"))?; }
            buffer.extend_from_slice(&chunk);
            while let Some((position, delimiter)) = sse_delimiter(&buffer) {
                let frame: Vec<_> = buffer.drain(..position + delimiter).collect();
                let processing = transform_frame(&state, &request, frame);
                tokio::pin!(processing);
                let processed = loop {
                    tokio::select! {
                        result = &mut processing => break result,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => yield Bytes::from_static(b": image storage processing\n\n"),
                    }
                };
                match processed {
                    Ok(bytes) => yield Bytes::from(bytes),
                    Err(error) => {
                        let payload = json!({"type":"error", "error":error_json(&error)["error"]});
                        yield Bytes::from(format!("event: error\ndata: {payload}\n\n"));
                        return;
                    }
                }
            }
        }
        if !buffer.is_empty() { yield Bytes::from(transform_frame(&state, &request, buffer).await.map_err(std::io::Error::other)?); }
    });
    Response::from_parts(parts, Body::from_stream(stream))
}
pub(crate) fn sse_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    buffer
        .windows(2)
        .position(|s| s == b"\n\n")
        .map(|n| (n, 2))
        .into_iter()
        .chain(
            buffer
                .windows(4)
                .position(|s| s == b"\r\n\r\n")
                .map(|n| (n, 4)),
        )
        .min_by_key(|(n, _)| *n)
}
async fn transform_frame(
    state: &AppState,
    request: &ImageRequest,
    frame: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(&frame).map_err(|_| "图片 SSE 编码错误".to_string())?;
    let data = text
        .lines()
        .filter_map(|l| l.strip_prefix("data:").map(|s| s.trim_start()))
        .collect::<Vec<_>>()
        .join("\n");
    let Ok(mut value) = serde_json::from_str::<Value>(&data) else {
        return Ok(frame);
    };
    let event = text
        .lines()
        .find_map(|l| l.strip_prefix("event:").map(str::trim));
    if !matches!(
        value.get("type").and_then(Value::as_str).or(event),
        Some("image_generation.completed" | "image_edit.completed")
    ) && value.get("data").and_then(Value::as_array).is_none()
    {
        return Ok(frame);
    }
    let _permit = state
        .image_storage
        .response_gate
        .acquire()
        .await
        .map_err(|_| "图片转换服务已停止".to_string())?;
    if value.get("type").is_none() {
        if let Some(event) = event {
            value["type"] = Value::String(event.into());
        }
    }
    transform_json(state, request, &mut value).await?;
    let mut output = String::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with("data:") && !l.is_empty())
    {
        output.push_str(line);
        output.push('\n');
    }
    output.push_str(&format!(
        "data: {}\n\n",
        serde_json::to_string(&value).map_err(|_| "图片 SSE 序列化失败".to_string())?
    ));
    Ok(output.into_bytes())
}
