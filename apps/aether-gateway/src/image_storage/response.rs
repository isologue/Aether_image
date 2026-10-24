use super::{
    config::{load_config, validate_http_url, ImageConfig},
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
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ImageRequest {
    pub config: ImageConfig,
    pub use_url: bool,
    pub public_base: Option<String>,
    pub target_size: Option<super::resize::ImageSize>,
}
pub(crate) fn is_image_path(path: &str) -> bool {
    matches!(
        path.trim_end_matches('/'),
        "/v1/images/generations" | "/v1/images/edits"
    )
}
impl ImageRequestSlot {
    pub(crate) fn resize_target(&self) -> Option<(u32, u32)> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()?
            .target_size
            .map(|size| (size.width, size.height))
    }

    pub async fn capture(
        &self,
        state: &AppState,
        headers: &HeaderMap,
        body: &Bytes,
        trusted_forwarded: bool,
    ) -> Result<Bytes, String> {
        if self.0.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
            return Ok(body.clone());
        }
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
        let target_size = if config.resize_enabled {
            let size = if let Some(value) = &body_json {
                match value.get("size") {
                    Some(Value::String(value)) => Some(value.clone()),
                    Some(Value::Null) | None => None,
                    Some(_) => return Err("启用图片缩放时 size 必须为宽x高或 auto".into()),
                }
            } else {
                multipart_text_field(body, content_type, "size").map(|(_, _, value)| value)
            };
            super::resize::ImageSize::parse(size.as_deref())?
        } else {
            None
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
            target_size,
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
    multipart_text_field(body, content_type, "response_format")
}
fn multipart_text_field(
    body: &[u8],
    content_type: &str,
    field: &str,
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
                    .any(|part| part.trim() == format!("name=\"{field}\""))
                && !line
                    .split(';')
                    .any(|part| part.trim().starts_with("filename="))
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
// Only a completed, explicitly refused result without images is non-billable.
// Keep this separate from HTTP errors so async workers retain a successful task.
pub(crate) fn is_content_refusal(value: &Value) -> bool {
    value["status"] == "completed"
        && value["outcome"] == "content_refused"
        && value["refusal"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty())
        && value["data"]
            .as_array()
            .is_some_and(|items| items.is_empty())
        && value.get("url").is_none()
        && value.get("b64_json").is_none()
        && (value.get("error").is_none_or(Value::is_null)
            || (value["error"]["type"] == "image_generation_user_error"
                && value["error"]["code"] == "content_policy_violation"))
}

fn error_json(message: &str) -> Value {
    json!({"error":{"type":"image_storage_error", "code":"image_storage_error", "message":message}})
}
pub(crate) fn transform_response(
    state: AppState,
    slot: ImageRequestSlot,
    response: Response<Body>,
) -> impl std::future::Future<Output = Result<Response<Body>, GatewayError>> + Send {
    Box::pin(transform_response_inner(state, slot, response))
}
async fn transform_response_inner(
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
    let refused = is_content_refusal(&value);
    if parts.status.is_success()
        && !refused
        && (value.get("error").is_some_and(|e| !e.is_null()) || !complete_images(&value))
    {
        parts.status = StatusCode::BAD_GATEWAY;
        if value.get("error").is_none_or(Value::is_null) {
            value = error_json("Upstream returned no completed image");
        }
    }
    if parts.status.is_success() && refused {
        let reason = value["refusal"].as_str().expect("validated refusal");
        crate::image_tasks::flow::content_refusal(reason);
        // Some OpenAI-compatible relays bill every successful image response,
        // even data=[] with usage=0. Their existing error parser refunds the
        // reservation and does not retry HTTP 200. Preserve Aether's completed
        // outcome and the original refusal while exposing that standard shape.
        value["error"] = json!({
            "type": "image_generation_user_error",
            "code": "content_policy_violation",
            "message": reason,
            "param": null,
        });
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
    crate::image_tasks::flow::stage("downloading", 70).await;
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
    crate::image_tasks::flow::stage("processing", 90).await;
    let bytes = if let Some(size) = request.target_size {
        let bytes = state.image_storage.resize(bytes, size).await?;
        for (field, dimension) in [("width", size.width), ("height", size.height)] {
            if object.contains_key(field) {
                object.insert(field.into(), json!(dimension));
            }
        }
        if object.contains_key("size") {
            object.insert(
                "size".into(),
                json!(format!("{}x{}", size.width, size.height)),
            );
        }
        bytes
    } else {
        bytes
    };
    if crate::image_tasks::flow::is_async() {
        let reference = super::task_results::persist(state, request, &bytes).await?;
        object.remove("b64_json");
        object.remove("url");
        object.insert("$image_ref".into(), reference);
        return Ok(());
    }
    let reference = super::task_results::persist(state, request, &bytes).await?;
    object.remove("url");
    object.remove("b64_json");
    if request.use_url {
        object.insert("url".into(), reference["url"].clone());
    } else {
        object.insert(
            "b64_json".into(),
            json!(base64::engine::general_purpose::STANDARD.encode(&bytes)),
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
    let flow = crate::image_tasks::flow::current();
    let mut delivery = crate::image_tasks::flow::DeliveryGuard::new(&state);
    let stream: std::pin::Pin<
        Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>,
    > = Box::pin(async_stream::try_stream! {
        let mut source = body.into_data_stream(); let mut buffer = Vec::new();
        let mut outcome = StreamOutcome::default();
        while let Some(chunk) = source.next().await {
            let chunk = chunk.map_err(std::io::Error::other)?;
            if buffer.len() + chunk.len() > MAX_BODY_BYTES { Err(std::io::Error::other("Image SSE frame too large"))?; }
            buffer.extend_from_slice(&chunk);
            while let Some((position, delimiter)) = sse_delimiter(&buffer) {
                let frame: Vec<_> = buffer.drain(..position + delimiter).collect();
                let processing = crate::image_tasks::flow::scope(flow.clone(),transform_frame(&state, &request, frame, &mut outcome));
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
                        delivery.complete(false).await;
                        let payload = json!({"type":"error", "error":error_json(&error)["error"]});
                        yield Bytes::from(format!("event: error\ndata: {payload}\n\n"));
                        return;
                    }
                }
            }
        }
        if !buffer.is_empty() { yield Bytes::from(crate::image_tasks::flow::scope(flow.clone(),transform_frame(&state, &request, buffer, &mut outcome)).await.map_err(std::io::Error::other)?); }
        let succeeded = !outcome.failed && (outcome.image_ready || outcome.refused);
        delivery.complete(succeeded).await;
        if !succeeded && !outcome.failed {
            let payload = error_json("Upstream image stream ended without a completed image");
            yield Bytes::from(format!("event: error\ndata: {payload}\n\n"));
        }
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
#[derive(Default)]
struct StreamOutcome {
    image_ready: bool,
    refused: bool,
    failed: bool,
}

fn complete_images(value: &Value) -> bool {
    let has_image = |v: &Value| {
        ["url", "b64_json"]
            .iter()
            .any(|k| v[*k].as_str().is_some_and(|s| !s.is_empty()))
            || v.get("$image_ref").is_some_and(Value::is_object)
    };
    has_image(value)
        || value["data"]
            .as_array()
            .is_some_and(|items| !items.is_empty() && items.iter().all(has_image))
}

async fn transform_frame(
    state: &AppState,
    request: &ImageRequest,
    frame: Vec<u8>,
    outcome: &mut StreamOutcome,
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
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .or(event)
        .unwrap_or("");
    if value.get("error").is_some_and(|v| !v.is_null())
        || kind == "error"
        || kind.ends_with(".failed")
    {
        outcome.failed = true;
    }
    if value["outcome"] == "content_refused"
        && value["refusal"].as_str().is_some_and(|s| !s.is_empty())
    {
        outcome.refused = true;
        crate::image_tasks::flow::content_refusal(value["refusal"].as_str().unwrap());
    }
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
    outcome.image_ready |= complete_images(&value);
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
