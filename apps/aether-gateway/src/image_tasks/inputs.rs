//! Persist binary inputs only. URL values are opaque references, never fetched here.
use axum::body::Bytes;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

const MAX_INPUT: usize = 64 * 1024 * 1024;
#[derive(Clone)]
pub(crate) struct Submission {
    pub original: Bytes,
}
struct Blob {
    name: String,
    bytes: Vec<u8>,
}
#[derive(Debug)]
struct Part {
    headers: String,
    name: String,
    filename: Option<String>,
    content_type: Option<String>,
    bytes: Vec<u8>,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn parts(body: &[u8], content_type: &str) -> Result<Vec<Part>, String> {
    let boundary = content_type
        .split(';')
        .find_map(|s| s.trim().strip_prefix("boundary="))
        .map(|s| s.trim_matches('"'))
        .filter(|s| !s.is_empty() && s.len() <= 200 && !s.contains(['\r', '\n']))
        .ok_or("Invalid image task input")?;
    let marker = format!("--{boundary}");
    let delim = format!("\r\n--{boundary}");
    if !body.starts_with(marker.as_bytes()) {
        return Err("Invalid image task input".into());
    }
    let mut at = marker.len();
    let mut output = Vec::new();
    while body.get(at..at + 2) != Some(b"--") {
        if body.get(at..at + 2) != Some(b"\r\n") {
            return Err("Invalid image task input".into());
        }
        at += 2;
        let end = body[at..]
            .windows(4)
            .take(65536)
            .position(|b| b == b"\r\n\r\n")
            .map(|n| n + at)
            .ok_or("Invalid image task input")?;
        let headers = std::str::from_utf8(&body[at..end])
            .map_err(|_| "Invalid image task input")?
            .to_string();
        let disposition = headers
            .lines()
            .find(|s| s.to_ascii_lowercase().starts_with("content-disposition:"))
            .ok_or("Invalid image task input")?;
        let field = |key: &str| {
            disposition
                .split(';')
                .find_map(|s| s.trim().strip_prefix(key))
                .map(|s| s.trim_matches('"').to_string())
        };
        let name = field("name=").ok_or("Invalid image task input")?;
        let filename = field("filename=");
        let content_type = headers.lines().find_map(|s| {
            s.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                .map(|(_, v)| v.trim().to_string())
        });
        let start = end + 4;
        let end = body[start..]
            .windows(delim.len())
            .position(|b| b == delim.as_bytes())
            .map(|n| n + start)
            .ok_or("Invalid image task input")?;
        output.push(Part {
            headers,
            name,
            filename,
            content_type,
            bytes: body[start..end].to_vec(),
        });
        if output.len() > 128 {
            return Err("Invalid image task input".into());
        }
        at = end + 2 + marker.len();
    }
    Ok(output)
}
fn multipart(parts: &[Part], boundary: &str) -> Bytes {
    let mut bytes = Vec::new();
    for part in parts {
        bytes.extend_from_slice(format!("--{boundary}\r\n{}\r\n\r\n", part.headers).as_bytes());
        bytes.extend_from_slice(&part.bytes);
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Bytes::from(bytes)
}
pub(crate) fn scheduling(body: &Bytes, ct: &str) -> Result<(bool, Bytes), String> {
    if ct.to_ascii_lowercase().starts_with("multipart/form-data") {
        let mut fields = parts(body, ct)?;
        let flags: Vec<_> = fields.iter().filter(|p| p.name == "async").collect();
        if flags.len() > 1 {
            return Err("Invalid image task input".into());
        }
        let enabled = match flags.first().map(|p| p.bytes.as_slice()) {
            None | Some(b"false") => false,
            Some(b"true") => true,
            _ => return Err("Invalid image task input".into()),
        };
        if enabled
            && fields.iter().any(|p| {
                p.name == "stream"
                    && std::str::from_utf8(&p.bytes).ok().is_some_and(|s| {
                        matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1")
                    })
            })
        {
            return Err("async=true cannot be combined with stream=true".into());
        }
        fields.retain(|p| p.name != "async");
        let boundary = ct
            .split(';')
            .find_map(|s| s.trim().strip_prefix("boundary="))
            .unwrap()
            .trim_matches('"');
        Ok((enabled, multipart(&fields, boundary)))
    } else {
        let Ok(mut value) = serde_json::from_slice::<Value>(body) else {
            return Ok((false, body.clone()));
        };
        let Some(object) = value.as_object_mut() else {
            return Ok((false, body.clone()));
        };
        let enabled = match object.remove("async") {
            None | Some(Value::Bool(false)) => false,
            Some(Value::Bool(true)) => true,
            _ => return Err("Invalid image task input".into()),
        };
        if enabled
            && object.get("stream").is_some_and(|v| {
                v == true
                    || v == 1
                    || v.as_str().is_some_and(|s| {
                        matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1")
                    })
            })
        {
            return Err("async=true cannot be combined with stream=true".into());
        }
        Ok((
            enabled,
            Bytes::from(serde_json::to_vec(&value).map_err(|_| "Invalid image task input")?),
        ))
    }
}
fn binary(value: &mut Value, blobs: &mut Vec<Blob>) -> Result<(), String> {
    match value {
        Value::String(s) => {
            if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("file-") {
                return Ok(());
            }
            let (raw, prefix) = if s.starts_with("data:") {
                let (a, b) = s.split_once(',').ok_or("Invalid image task input")?;
                if !a.ends_with(";base64") {
                    return Err("Invalid image task input".into());
                }
                (b, Some(format!("{a},")))
            } else {
                (s.as_str(), None)
            };
            if raw.len() > MAX_INPUT * 4 / 3 + 8 {
                return Err("Invalid image task input".into());
            }
            let bytes = STANDARD
                .decode(raw)
                .map_err(|_| "Invalid image task input")?;
            if bytes.is_empty() {
                return Err("Invalid image task input".into());
            }
            let name = format!("{}.bin", blobs.len());
            let digest = hash(&bytes);
            *value = json!({"$task_file":name,"sha256":digest,"prefix":prefix,"encoding":"base64"});
            blobs.push(Blob { name, bytes });
        }
        Value::Array(items) => {
            for item in items {
                binary(item, blobs)?;
            }
        }
        Value::Object(object) => {
            for (key, item) in object {
                if matches!(
                    key.as_str(),
                    "url" | "image_url" | "b64_json" | "data" | "image" | "mask"
                ) {
                    binary(item, blobs)?;
                }
            }
        }
        _ => {}
    };
    Ok(())
}
fn prepare(body: &Bytes, ct: &str) -> Result<(Value, Vec<Blob>), String> {
    let mut blobs = Vec::new();
    if ct.to_ascii_lowercase().starts_with("multipart/form-data") {
        let mut fields = Vec::new();
        for part in parts(body, ct)? {
            let value = if part.filename.is_some() {
                if part.bytes.is_empty() || part.bytes.len() > MAX_INPUT {
                    return Err("Invalid image task input".into());
                }
                let name = format!("{}.bin", blobs.len());
                let digest = hash(&part.bytes);
                blobs.push(Blob {
                    name: name.clone(),
                    bytes: part.bytes,
                });
                json!({"$task_file":name,"sha256":digest,"encoding":"bytes"})
            } else {
                let mut v = Value::String(
                    String::from_utf8(part.bytes).map_err(|_| "Invalid image task input")?,
                );
                if matches!(
                    part.name.as_str(),
                    "image" | "image[]" | "images" | "images[]" | "mask"
                ) {
                    binary(&mut v, &mut blobs)?;
                }
                v
            };
            fields.push(json!({"name":part.name,"filename":part.filename,"content_type":part.content_type,"value":value}));
        }
        Ok((json!({"multipart":fields}), blobs))
    } else {
        let mut value: Value =
            serde_json::from_slice(body).map_err(|_| "Invalid image task input")?;
        if let Some(obj) = value.as_object_mut() {
            for (key, v) in obj {
                if matches!(
                    key.as_str(),
                    "image" | "images" | "mask" | "input_image" | "input_image_mask"
                ) {
                    binary(v, &mut blobs)?;
                }
            }
        }
        Ok((json!({"json":value}), blobs))
    }
}
/// Use the existing synchronous normalizer for validation and semantic idempotency.
pub(crate) fn validate(
    body: &Bytes,
    ct: &str,
    path: &str,
) -> Result<aether_ai_formats::formats::openai::image::request::NormalizedOpenAiImageRequest, String>
{
    let request = http::Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", ct)
        .body(())
        .map_err(|e| e.to_string())?;
    let (parts, _) = request.into_parts();
    let multipart = ct.to_ascii_lowercase().starts_with("multipart/form-data");
    let encoded = multipart.then(|| STANDARD.encode(body));
    let json = if multipart {
        Value::Null
    } else {
        serde_json::from_slice(body).map_err(|_| "Image request must be valid JSON")?
    };
    aether_ai_formats::formats::openai::image::request::normalize_openai_image_request(
        &parts,
        &json,
        encoded.as_deref(),
    )
    .ok_or_else(|| "Invalid image parameters, image count, file type, or edit inputs".into())
}
pub(crate) fn fingerprint(body: &Bytes, ct: &str, path: &str) -> Result<String, String> {
    let normalized = validate(body, ct, path)?;
    let mut value=aether_ai_formats::formats::openai::image::request::build_openai_image_provider_request_body(&normalized);
    let mut blobs = Vec::new();
    // Only image fields are decoded; arbitrary prompt/user text must remain opaque.
    for content in value["input"].as_array_mut().into_iter().flatten() {
        if let Some(items) = content["content"].as_array_mut() {
            for item in items {
                if item["type"] == "input_image" {
                    binary(item, &mut blobs)?;
                }
            }
        }
    }
    if let Some(tools) = value["tools"].as_array_mut() {
        for tool in tools {
            if let Some(mask) = tool.get_mut("input_image_mask") {
                binary(mask, &mut blobs)?;
            }
        }
    }
    fn strip(v: &mut Value) {
        match v {
            Value::Object(o) => {
                o.remove("$task_file");
                o.remove("prefix");
                for v in o.values_mut() {
                    strip(v);
                }
            }
            Value::Array(a) => {
                for v in a {
                    strip(v);
                }
            }
            _ => {}
        }
    }
    strip(&mut value);
    // Retain extension fields (e.g. Web model/ratio) without multipart ordering/boundary.
    let mut extensions = serde_json::Map::new();
    const KNOWN: &[&str] = &[
        "async",
        "stream",
        "model",
        "prompt",
        "image",
        "image[]",
        "images",
        "images[]",
        "mask",
        "input_image",
        "input_image_mask",
        "n",
        "size",
        "quality",
        "background",
        "output_format",
        "output_compression",
        "moderation",
        "input_fidelity",
        "partial_images",
        "style",
        "user",
        "response_format",
    ];
    if ct.to_ascii_lowercase().starts_with("multipart/form-data") {
        for part in parts(body, ct)? {
            if !KNOWN.contains(&part.name.as_str()) {
                extensions.insert(
                    part.name,
                    Value::String(String::from_utf8(part.bytes).map_err(|_| "Invalid text field")?),
                );
            }
        }
    } else if let Some(obj) = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.as_object().cloned())
    {
        for (k, v) in obj {
            if !KNOWN.contains(&k.as_str()) {
                extensions.insert(k, v);
            }
        }
    }
    Ok(hash(&serde_json::to_vec(&json!({"path":path,"model":normalized.requested_model,"summary":normalized.summary_json,"request":value,"extensions":extensions})).map_err(|e|e.to_string())?))
}
pub(crate) async fn persist(
    root: &Path,
    id: &str,
    body: &Bytes,
    ct: &str,
) -> Result<Value, String> {
    let (value, blobs) = prepare(body, ct)?;
    if !blobs.is_empty() {
        let dir = root.join("task-inputs").join(id);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| format!("Cannot create image task input directory: {e}"))?;
        for blob in blobs {
            if let Err(e) =
                crate::image_storage::atomic_write(&dir.join(&blob.name), &blob.bytes).await
            {
                let _ = tokio::fs::remove_dir_all(&dir).await;
                return Err(format!("Cannot persist image task input: {e}"));
            }
        }
    }
    Ok(value)
}
async fn restore_values(root: &Path, id: &str, value: &mut Value) -> Result<(), String> {
    // Iterative JSON pointer collection avoids recursive async futures.
    fn collect(v: &Value, path: String, out: &mut Vec<String>) {
        if v.get("$task_file").is_some() {
            out.push(path);
            return;
        }
        match v {
            Value::Object(o) => {
                for (k, v) in o {
                    collect(
                        v,
                        format!("{path}/{}", k.replace('~', "~0").replace('/', "~1")),
                        out,
                    );
                }
            }
            Value::Array(a) => {
                for (i, v) in a.iter().enumerate() {
                    collect(v, format!("{path}/{i}"), out);
                }
            }
            _ => {}
        }
    }
    let mut paths = Vec::new();
    collect(value, String::new(), &mut paths);
    for path in paths {
        let v = value.pointer_mut(&path).ok_or("Invalid image task input")?;
        let name = v["$task_file"]
            .as_str()
            .filter(|s| {
                s.ends_with(".bin")
                    && s.trim_end_matches(".bin")
                        .bytes()
                        .all(|b| b.is_ascii_digit())
            })
            .ok_or("Invalid image task input")?;
        let bytes = tokio::fs::read(root.join("task-inputs").join(id).join(name))
            .await
            .map_err(|_| "Invalid image task input")?;
        if hash(&bytes) != v["sha256"].as_str().unwrap_or("") {
            return Err("Invalid image task input".into());
        }
        let prefix = v["prefix"].as_str().unwrap_or("");
        *v = Value::String(format!("{prefix}{}", STANDARD.encode(&bytes)));
    }
    Ok(())
}
pub(crate) async fn restore(
    root: &Path,
    id: &str,
    mut document: Value,
) -> Result<(Bytes, String), String> {
    if document.get("json").is_some() {
        restore_values(root, id, &mut document["json"]).await?;
        return Ok((
            Bytes::from(
                serde_json::to_vec(&document["json"]).map_err(|_| "Invalid image task input")?,
            ),
            "application/json".into(),
        ));
    }
    let boundary = format!("aether{}", uuid::Uuid::new_v4().simple());
    let mut output = Vec::new();
    for field in document["multipart"]
        .as_array()
        .ok_or("Invalid image task input")?
    {
        let mut value = field["value"].clone();
        let bytes = if value["encoding"] == "bytes" {
            let name = value["$task_file"]
                .as_str()
                .filter(|s| {
                    s.ends_with(".bin")
                        && s.trim_end_matches(".bin")
                            .bytes()
                            .all(|b| b.is_ascii_digit())
                })
                .ok_or("Invalid image task input")?;
            let b = tokio::fs::read(root.join("task-inputs").join(id).join(name))
                .await
                .map_err(|_| "Invalid image task input")?;
            if hash(&b) != value["sha256"].as_str().unwrap_or("") {
                return Err("Invalid image task input".into());
            }
            b
        } else {
            restore_values(root, id, &mut value).await?;
            value
                .as_str()
                .ok_or("Invalid image task input")?
                .as_bytes()
                .to_vec()
        };
        let name = field["name"]
            .as_str()
            .filter(|s| !s.contains(['\r', '\n', '"']))
            .ok_or("Invalid multipart field name")?;
        let mut headers = format!("Content-Disposition: form-data; name=\"{name}\"");
        if let Some(filename) = field["filename"].as_str() {
            headers += &format!(
                "; filename=\"{}\"",
                filename.replace(['\r', '\n', '"'], "_")
            );
        }
        if let Some(ct) = field["content_type"].as_str() {
            headers += &format!("\r\nContent-Type: {}", ct.replace(['\r', '\n'], ""));
        }
        output.push(Part {
            headers,
            name: name.into(),
            filename: None,
            content_type: None,
            bytes,
        });
    }
    Ok((
        multipart(&output, &boundary),
        format!("multipart/form-data; boundary={boundary}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    const PNG:&str="iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aD1sAAAAASUVORK5CYII=";
    fn json_bytes(v: Value) -> Bytes {
        serde_json::to_vec(&v).unwrap().into()
    }
    fn edit(boundary: &str, filename: &str) -> (Bytes, String) {
        let mut fields = vec![
            Part {
                headers: "Content-Disposition: form-data; name=\"model\"".into(),
                name: "model".into(),
                filename: None,
                content_type: None,
                bytes: b"gpt-image-1".to_vec(),
            },
            Part {
                headers: "Content-Disposition: form-data; name=\"prompt\"".into(),
                name: "prompt".into(),
                filename: None,
                content_type: None,
                bytes: b"make blue".to_vec(),
            },
        ];
        for name in ["image", "mask"] {
            fields.push(Part{headers:format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: image/png"),name:name.into(),filename:Some(filename.into()),content_type:Some("image/png".into()),bytes:STANDARD.decode(PNG).unwrap()});
        }
        (
            multipart(&fields, boundary),
            format!("multipart/form-data; boundary={boundary}"),
        )
    }
    #[test]
    fn scheduling_strips_only_flag_and_rejects_stream_variants() {
        for stream in [json!(true), json!("true"), json!(1)] {
            assert!(scheduling(
                &json_bytes(json!({"async":true,"stream":stream})),
                "application/json"
            )
            .is_err());
        }
        assert!(scheduling(&json_bytes(json!({"async":"true"})), "application/json").is_err());
        let (enabled, body) = scheduling(
            &json_bytes(
                json!({"async":true,"prompt":"hello","image":"https://example.test/i?sig=secret"}),
            ),
            "application/json",
        )
        .unwrap();
        assert!(enabled);
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(v.get("async").is_none());
        assert_eq!(v["image"], "https://example.test/i?sig=secret");
        for b in [b"".as_slice(), b"--x", b"--x\r\n", b"--x\r\nX:\r\n\r\n"] {
            assert!(parts(b, "multipart/form-data; boundary=x").is_err());
        }
    }
    #[test]
    fn multipart_digest_ignores_boundary_filename_but_preserves_mask_role() {
        let (a, act) = edit("first", "a.png");
        let (b, bct) = edit("second", "b.png");
        let ah = fingerprint(&a, &act, "/v1/images/edits").unwrap();
        assert_eq!(ah, fingerprint(&b, &bct, "/v1/images/edits").unwrap());
        let mut fields = parts(&b, &bct).unwrap();
        fields.retain(|p| p.name != "mask");
        assert_ne!(
            ah,
            fingerprint(&multipart(&fields, "second"), &bct, "/v1/images/edits").unwrap()
        );
    }
    #[test]
    fn signed_urls_are_opaque_and_binary_payload_has_only_references() {
        let v = json_bytes(
            json!({"model":"gpt-image-1","prompt":"edit","image":["https://example.invalid/i?sig=abc&expires=1",format!("data:image/png;base64,{PNG}")],"mask":format!("data:image/png;base64,{PNG}")}),
        );
        let (stored, blobs) = prepare(&v, "application/json").unwrap();
        assert_eq!(blobs.len(), 2);
        assert_eq!(
            stored["json"]["image"][0],
            "https://example.invalid/i?sig=abc&expires=1"
        );
        assert_eq!(stored["json"]["image"][1]["$task_file"], "0.bin");
        assert_eq!(stored["json"]["mask"]["$task_file"], "1.bin");
        assert!(!stored.to_string().contains(PNG));
    }
    #[tokio::test]
    async fn json_url_image_and_mask_keep_roles_without_input_files() {
        use aether_ai_formats::formats::openai::image::request::{
            build_openai_image_provider_request_body, OpenAiImageOperation,
        };

        let root = std::env::temp_dir().join(format!("aether-url-input-{}", uuid::Uuid::new_v4()));
        let id = "imgtask_00000000000000000000000000000000";
        let ct = "application/json";
        let path = "/v1/images/edits";
        // Unresolvable URLs must remain usable as opaque submission references.
        let image = "https://example.invalid/reference.png?sig=image%2B1&expires=1";
        let mask = "https://example.invalid/mask.png?sig=mask%2F1&expires=1";
        let request = json!({
            "n": 1,
            "mask": {"image_url": mask},
            "size": "1024x1024",
            "model": "gpt-image-2",
            "images": [{"image_url": image}],
            "prompt": "只修改蒙版透明区域，其他区域保持不变。移除选中区域文字",
            "output_format": "png",
            "response_format": "url",
            "async": true
        });
        let (enabled, body) = scheduling(&json_bytes(request.clone()), ct).unwrap();
        assert!(enabled);
        let mut expected = request;
        expected.as_object_mut().unwrap().remove("async");

        let stored = persist(&root, id, &body, ct).await.unwrap();
        assert_eq!(stored["json"], expected);
        assert!(!tokio::fs::try_exists(&root).await.unwrap());
        let (restored, restored_ct) = restore(&root, id, stored).await.unwrap();
        assert_eq!(restored_ct, ct);
        assert_eq!(
            serde_json::from_slice::<Value>(&restored).unwrap(),
            expected
        );
        let digest = fingerprint(&body, ct, path).unwrap();
        assert_eq!(digest, fingerprint(&restored, ct, path).unwrap());

        let normalized = validate(&restored, ct, path).unwrap();
        assert_eq!(normalized.operation, OpenAiImageOperation::Edit);
        assert_eq!(normalized.requested_model.as_deref(), Some("gpt-image-2"));
        assert_eq!(normalized.summary_json["response_format"], "url");
        let upstream = build_openai_image_provider_request_body(&normalized);
        let content = upstream["input"][0]["content"].as_array().unwrap();
        let references: Vec<_> = content
            .iter()
            .filter(|part| part["type"] == "input_image")
            .collect();
        assert_eq!(references.len(), 1);
        assert_eq!(references[0]["image_url"], image);
        assert_eq!(upstream["tools"][0]["input_image_mask"]["image_url"], mask);

        // The same URL in a different role, or a changed signed mask URL,
        // is a different request even though neither URL has been downloaded.
        let mut swapped = expected.clone();
        swapped["images"][0]["image_url"] = json!(mask);
        swapped["mask"]["image_url"] = json!(image);
        assert_ne!(digest, fingerprint(&json_bytes(swapped), ct, path).unwrap());
        expected["mask"]["image_url"] = json!(format!("{mask}&version=2"));
        assert_ne!(
            digest,
            fingerprint(&json_bytes(expected), ct, path).unwrap()
        );
        assert!(!tokio::fs::try_exists(&root).await.unwrap());
    }
    #[tokio::test]
    async fn persisted_inputs_round_trip_and_detect_corruption() {
        let root = std::env::temp_dir().join(format!("aether-input-test-{}", uuid::Uuid::new_v4()));
        let id = "imgtask_00000000000000000000000000000000";
        let (a, ct) = edit("first", "image.png");
        let stored = persist(&root, id, &a, &ct).await.unwrap();
        let (restored, rct) = restore(&root, id, stored.clone()).await.unwrap();
        assert_eq!(
            fingerprint(&a, &ct, "/v1/images/edits").unwrap(),
            fingerprint(&restored, &rct, "/v1/images/edits").unwrap()
        );
        tokio::fs::write(root.join("task-inputs").join(id).join("0.bin"), b"corrupt")
            .await
            .unwrap();
        assert!(restore(&root, id, stored).await.is_err());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
