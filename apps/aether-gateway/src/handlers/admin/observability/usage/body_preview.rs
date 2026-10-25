use aether_data_contracts::repository::usage::{
    read_decompressed_usage_json, StoredUsageBodyPayload, MAX_DECOMPRESSED_USAGE_JSON_BYTES,
};
use flate2::read::GzDecoder;
use serde_json::Value;

const INLINE_BINARY_PREVIEW_LIMIT: usize = 4096;

pub(super) struct UsageBodyPreview {
    pub payload: StoredUsageBodyPayload,
    pub omitted_values: usize,
    pub original_bytes: usize,
}

// Only the admin preview is transformed; stored bytes and raw/cURL/replay stay intact.
// Run on a blocking worker because decompression and JSON parsing can be expensive.
pub(super) fn build_usage_body_preview(
    payload: StoredUsageBodyPayload,
) -> Result<UsageBodyPreview, &'static str> {
    let decoded = match &payload {
        StoredUsageBodyPayload::Gzip(bytes) => {
            if bytes.len() > MAX_DECOMPRESSED_USAGE_JSON_BYTES + 1024 * 1024 {
                return Err("too_large");
            }
            read_decompressed_usage_json(GzDecoder::new(bytes.as_slice())).map_err(|error| {
                if error
                    .to_string()
                    .contains("decompressed usage json exceeds ")
                {
                    "too_large"
                } else {
                    "decode_failed"
                }
            })?
        }
        StoredUsageBodyPayload::Json(bytes) => {
            if bytes.len() > MAX_DECOMPRESSED_USAGE_JSON_BYTES {
                return Err("too_large");
            }
            // Parse borrowed JSON bytes below without another copy.
            Vec::new()
        }
    };
    let bytes = match &payload {
        StoredUsageBodyPayload::Gzip(_) => decoded.as_slice(),
        StoredUsageBodyPayload::Json(bytes) => bytes.as_slice(),
    };
    let original_bytes = bytes.len();
    let mut value: Value = serde_json::from_slice(bytes).map_err(|_| "decode_failed")?;
    let mut omitted_values = 0;
    summarize_binary_values(&mut value, "", false, &mut omitted_values);
    let payload = if omitted_values == 0 {
        // Reuse the original compressed payload when nothing needs summarizing.
        payload
    } else {
        StoredUsageBodyPayload::Json(serde_json::to_vec(&value).map_err(|_| "decode_failed")?)
    };
    Ok(UsageBodyPreview {
        payload,
        omitted_values,
        original_bytes,
    })
}

fn summarize_binary_values(value: &mut Value, key: &str, encoded_data: bool, omitted: &mut usize) {
    match value {
        Value::Object(object) => {
            let encoded_data = object.get("type").and_then(Value::as_str) == Some("base64")
                || object.get("encoding").and_then(Value::as_str) == Some("base64");
            for (key, child) in object {
                summarize_binary_values(child, key, encoded_data && key == "data", omitted);
            }
        }
        Value::Array(items) => {
            for child in items {
                summarize_binary_values(child, key, encoded_data, omitted);
            }
        }
        Value::String(text) if text.len() > INLINE_BINARY_PREVIEW_LIMIT => {
            // Never abbreviate prompts or plain assistant text, even when they resemble Base64.
            if matches!(
                key,
                "prompt" | "negative_prompt" | "revised_prompt" | "text" | "content"
            ) {
                return;
            }
            let data_url = text
                .strip_prefix("data:")
                .and_then(|data| data.split_once(";base64,"));
            let binary_key = matches!(
                key,
                "b64_json"
                    | "base64"
                    | "image_base64"
                    | "mask_base64"
                    | "body_bytes_b64"
                    | "image"
                    | "images"
                    | "mask"
                    | "image_url"
                    | "image_urls"
                    | "input_image"
                    | "reference_image"
                    | "reference_images"
            );
            let (kind, length) = if let Some((mime, data)) = data_url {
                (mime, data.len())
            } else if (binary_key || encoded_data) && is_base64(text) {
                (
                    if key == "body_bytes_b64" {
                        "原始二进制正文"
                    } else {
                        "图片/二进制数据"
                    },
                    text.len(),
                )
            } else {
                return;
            };
            *value = Value::String(format!(
                "[已省略 {kind}，Base64 {length} 字符；加载完整正文查看]"
            ));
            *omitted += 1;
        }
        _ => {}
    }
}

fn is_base64(text: &str) -> bool {
    text.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'+' | b'/' | b'-' | b'_' | b'=' | b'\r' | b'\n')
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn multi_image_preview_preserves_prompts_parameters_urls_and_refusal_text() {
        let base64 = "a".repeat(350_000);
        let prompt = "提示词".repeat(12_000);
        let url = format!("https://example.test/image?token={}", "x".repeat(5000));
        let input = json!({
            "images": [format!("data:image/png;base64,{base64}"), base64],
            "mask": {"url": format!("data:image/webp;base64,{base64}")},
            "messages": [{"content": [{"type": "image", "source": {"type": "base64", "data": base64}}]}],
            "prompt": prompt, "model": "image-model", "n": 2, "size": "1024x1024",
            "image_url": url, "text": "I cannot generate this image.", "future_field": {"keep": true}
        });
        let bytes = serde_json::to_vec(&input).unwrap();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&bytes).unwrap();
        let preview =
            build_usage_body_preview(StoredUsageBodyPayload::Gzip(encoder.finish().unwrap()))
                .unwrap();
        assert_eq!(preview.omitted_values, 4);
        assert_eq!(preview.original_bytes, bytes.len());
        let StoredUsageBodyPayload::Json(body) = preview.payload else {
            panic!("expected JSON preview")
        };
        assert!(body.len() < bytes.len() / 5);
        let result: Value = serde_json::from_slice(&body).unwrap();
        for field in [
            "prompt",
            "model",
            "n",
            "size",
            "image_url",
            "text",
            "future_field",
        ] {
            assert_eq!(result[field], input[field], "{field}");
        }
        assert!(result["images"][0].as_str().unwrap().contains("image/png"));
        assert!(result["images"][1].as_str().unwrap().contains("350000"));
    }

    #[test]
    fn small_bodies_and_long_plain_text_reuse_original_payload() {
        let bytes = serde_json::to_vec(&json!({"prompt": "a".repeat(10000), "text": "data:image/png;base64,".to_owned() + &"a".repeat(10000), "images": ["data:image/png;base64,YQ=="]})).unwrap();
        let preview =
            build_usage_body_preview(StoredUsageBodyPayload::Json(bytes.clone())).unwrap();
        assert_eq!(preview.omitted_values, 0);
        let StoredUsageBodyPayload::Json(actual) = preview.payload else {
            panic!("expected JSON")
        };
        assert_eq!(actual, bytes);
    }

    #[test]
    fn invalid_or_oversized_bodies_have_explicit_errors() {
        assert!(matches!(
            build_usage_body_preview(StoredUsageBodyPayload::Gzip(vec![1, 2, 3])),
            Err("decode_failed")
        ));
        assert!(matches!(
            build_usage_body_preview(StoredUsageBodyPayload::Json(b"not JSON".to_vec())),
            Err("decode_failed")
        ));
        // Bound decompression as well as the encoded body size.
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        let chunk = vec![b' '; 1024 * 1024];
        for _ in 0..65 {
            encoder.write_all(&chunk).unwrap();
        }
        assert!(matches!(
            build_usage_body_preview(StoredUsageBodyPayload::Gzip(encoder.finish().unwrap())),
            Err("too_large")
        ));
    }
}
