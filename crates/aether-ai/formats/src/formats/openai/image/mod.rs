use url::Url;

pub mod request;
pub mod spec;
pub mod stream;

/// Explicit completed refusal produced by the ChatGPT-Web image adapter.
pub(crate) fn image_content_refusal(response: &serde_json::Value) -> Option<&str> {
    if response.get("status").and_then(serde_json::Value::as_str) != Some("completed")
        || response.get("outcome").and_then(serde_json::Value::as_str) != Some("content_refused")
    {
        return None;
    }
    // Real images take precedence over an inconsistent refusal marker.
    if response
        .get("data")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|data| !data.is_empty())
        || response
            .get("output")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("type").and_then(serde_json::Value::as_str)
                        == Some("image_generation_call")
                })
            })
    {
        return None;
    }
    response
        .get("refusal")
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.trim().is_empty() && text.len() <= 64 * 1024)
}

pub(crate) fn image_content_refusal_response(
    response: &serde_json::Value,
) -> Option<serde_json::Value> {
    let reason = image_content_refusal(response)?;
    Some(serde_json::json!({
        "created": response.get("created_at").or_else(|| response.get("created")),
        "data": [], "status": "completed", "outcome": "content_refused",
        "refusal": reason, "message": reason,
        "usage": {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0},
    }))
}

pub(crate) const MAX_OPENAI_IMAGE_DATA_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_OPENAI_IMAGE_EXTERNAL_URL_BYTES: usize = 64 * 1024;
pub(crate) const MAX_OPENAI_IMAGE_REVISED_PROMPT_BYTES: usize = 256 * 1024;

pub(crate) fn is_safe_openai_image_base64_payload(value: &str) -> bool {
    if value.is_empty()
        || value.len() > MAX_OPENAI_IMAGE_DATA_BYTES
        || value.len() % 4 == 1
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return false;
    }
    let bytes = value.as_bytes();
    let first_padding = bytes.iter().position(|byte| *byte == b'=');
    if let Some(index) = first_padding {
        let padding = bytes.len() - index;
        if padding > 2
            || bytes[index..].iter().any(|byte| *byte != b'=')
            || !bytes.len().is_multiple_of(4)
        {
            return false;
        }
    }
    bytes[..first_padding.unwrap_or(bytes.len())]
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'+' | b'/'))
}

/// Keep a successful refusal readable in each supported client protocol.
pub(crate) fn image_content_refusal_for_format(
    response: &serde_json::Value,
    client_api_format: &str,
    report_context: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    use serde_json::{json, Value};
    let reason = image_content_refusal(response)?;
    let model = response
        .get("model")
        .and_then(Value::as_str)
        .or_else(|| {
            report_context
                .and_then(|ctx| ctx.get("model"))
                .and_then(Value::as_str)
        })
        .unwrap_or("unknown");
    let id = response
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("resp-content-refused");
    let created = response
        .get("created_at")
        .or_else(|| response.get("created"))
        .cloned()
        .unwrap_or(json!(0));
    let usage = json!({"input_tokens":0,"output_tokens":0,"total_tokens":0});
    match client_api_format {
        "openai:image" => image_content_refusal_response(response),
        "openai:chat" => Some(json!({
            "id":id,"object":"chat.completion","created":created,"model":model,
            "outcome":"content_refused","refusal":reason,
            "choices":[{"index":0,"message":{"role":"assistant","content":reason,"refusal":reason},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}
        })),
        "openai:responses" | "openai:responses:compact" => Some(json!({
            "id":id,"object":"response","created_at":created,"model":model,
            "status":"completed","outcome":"content_refused","refusal":reason,
            "output":[{"id":format!("{id}-message"),"type":"message","role":"assistant","status":"completed",
                "content":[{"type":"refusal","refusal":reason}]}],"usage":usage
        })),
        "gemini:generate_content" => Some(json!({
            "modelVersion":model,"outcome":"content_refused","refusal":reason,
            "candidates":[{"index":0,"content":{"role":"model","parts":[{"text":reason}]},"finishReason":"STOP"}]
        })),
        _ => None,
    }
}

pub(crate) fn normalize_openai_image_output_format(value: &str) -> Option<&'static str> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("png") {
        Some("png")
    } else if value.eq_ignore_ascii_case("jpeg") || value.eq_ignore_ascii_case("jpg") {
        Some("jpeg")
    } else if value.eq_ignore_ascii_case("webp") {
        Some("webp")
    } else {
        None
    }
}

pub(crate) fn bounded_openai_image_revised_prompt(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= MAX_OPENAI_IMAGE_REVISED_PROMPT_BYTES).then_some(value)
}

pub(crate) fn parse_safe_openai_image_data_url(value: &str) -> Option<(&'static str, &str)> {
    let (metadata, payload) = value.trim().split_once(',')?;
    let mime_type = metadata.strip_prefix("data:")?.strip_suffix(";base64")?;
    let mime_type = safe_openai_image_mime_type(mime_type.trim())?;
    (!payload.is_empty() && is_safe_openai_image_base64_payload(payload))
        .then_some((mime_type, payload))
}

pub(crate) fn safe_openai_image_mime_type(value: &str) -> Option<&'static str> {
    if value.eq_ignore_ascii_case("image/png") {
        Some("image/png")
    } else if value.eq_ignore_ascii_case("image/jpeg") || value.eq_ignore_ascii_case("image/jpg") {
        Some("image/jpeg")
    } else if value.eq_ignore_ascii_case("image/webp") {
        Some("image/webp")
    } else {
        None
    }
}

pub(crate) fn sanitize_openai_image_source_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value
            .chars()
            .any(|character| character.is_ascii_control() || character.is_whitespace())
    {
        return None;
    }
    if let Some((mime_type, payload)) = parse_safe_openai_image_data_url(value) {
        return Some(format!("data:{mime_type};base64,{payload}"));
    }
    if value.len() > MAX_OPENAI_IMAGE_EXTERNAL_URL_BYTES {
        return None;
    }
    let parsed = Url::parse(value).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    Some(value.to_string())
}
