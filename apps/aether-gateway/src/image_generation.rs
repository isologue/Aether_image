use std::time::Duration;

use serde_json::Value;

use crate::AppState;

pub(crate) const NO_ASSET_GRACE_KEY: &str = "chatgpt_web_image_no_asset_grace_seconds";
pub(crate) const DEFAULT_NO_ASSET_GRACE_SECONDS: u64 = 20;
pub(crate) const MAX_NO_ASSET_GRACE_SECONDS: u64 = 120;
pub(crate) const MAX_IMAGE_COUNT_KEY: &str = "chatgpt_web_image_max_count";
pub(crate) const DEFAULT_MAX_IMAGE_COUNT: u64 = 1;
pub(crate) const MAX_IMAGE_COUNT: u64 = 10;
pub(crate) const MAX_PROMPT_CHARS_KEY: &str = "chatgpt_web_image_max_prompt_chars";
pub(crate) const DEFAULT_MAX_PROMPT_CHARS: u64 = 32_768;
pub(crate) const MAX_PROMPT_CHARS: u64 = 1_000_000;

pub(crate) fn parse_max_prompt_chars(value: &Value) -> Option<u64> {
    value.as_u64().filter(|chars| *chars <= MAX_PROMPT_CHARS)
}

pub(crate) async fn max_prompt_chars(state: &AppState) -> u64 {
    match state
        .read_system_config_json_value(MAX_PROMPT_CHARS_KEY)
        .await
    {
        Ok(value) => value
            .as_ref()
            .and_then(parse_max_prompt_chars)
            .unwrap_or(DEFAULT_MAX_PROMPT_CHARS),
        Err(error) => {
            tracing::warn!(
                ?error,
                "failed to read ChatGPT Web prompt length limit; using default"
            );
            DEFAULT_MAX_PROMPT_CHARS
        }
    }
}

pub(crate) fn parse_max_image_count(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .filter(|count| (1..=MAX_IMAGE_COUNT).contains(count))
}

pub(crate) async fn max_image_count(state: &AppState) -> u64 {
    match state
        .read_system_config_json_value(MAX_IMAGE_COUNT_KEY)
        .await
    {
        Ok(value) => value
            .as_ref()
            .and_then(parse_max_image_count)
            .unwrap_or(DEFAULT_MAX_IMAGE_COUNT),
        Err(error) => {
            tracing::warn!(
                ?error,
                "failed to read ChatGPT Web image count limit; using default"
            );
            DEFAULT_MAX_IMAGE_COUNT
        }
    }
}

pub(crate) fn parse_no_asset_grace_seconds(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .filter(|seconds| *seconds <= MAX_NO_ASSET_GRACE_SECONDS)
}

pub(crate) async fn no_asset_poll_grace(state: &AppState) -> Duration {
    let value = match state
        .read_system_config_json_value(NO_ASSET_GRACE_KEY)
        .await
    {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                ?error,
                "failed to read image generation grace period; using default"
            );
            None
        }
    };
    Duration::from_secs(
        value
            .as_ref()
            .and_then(parse_no_asset_grace_seconds)
            .unwrap_or(DEFAULT_NO_ASSET_GRACE_SECONDS),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prompt_length_limit_rejects_invalid_values_and_accepts_zero() {
        for value in [
            json!(-1),
            json!(1_000_001),
            json!(1.5),
            json!("32768"),
            Value::Null,
        ] {
            assert_eq!(parse_max_prompt_chars(&value), None);
        }
        for chars in [0, 1, 32_768, 1_000_000] {
            assert_eq!(parse_max_prompt_chars(&json!(chars)), Some(chars));
        }
    }

    #[tokio::test]
    async fn prompt_length_limit_updates_without_restarting_state() {
        let state = AppState::new()
            .expect("state should build")
            .with_data_state_for_tests(
                crate::data::GatewayDataState::disabled().with_system_config_values_for_tests([]),
            );
        assert_eq!(max_prompt_chars(&state).await, DEFAULT_MAX_PROMPT_CHARS);
        for chars in [0, 123, MAX_PROMPT_CHARS] {
            state
                .upsert_system_config_json_value(MAX_PROMPT_CHARS_KEY, &json!(chars), None)
                .await
                .unwrap();
            assert_eq!(max_prompt_chars(&state).await, chars);
        }
    }

    #[test]
    fn image_count_limit_rejects_invalid_values() {
        for value in [
            json!(0),
            json!(-1),
            json!(11),
            json!(1.5),
            json!("2"),
            Value::Null,
        ] {
            assert_eq!(parse_max_image_count(&value), None);
        }
        for count in [1, 2, 10] {
            assert_eq!(parse_max_image_count(&json!(count)), Some(count));
        }
    }

    #[test]
    fn rejects_invalid_grace_periods_and_accepts_zero() {
        for value in [json!(-1), json!(121), json!(1.5), json!("10"), Value::Null] {
            assert_eq!(parse_no_asset_grace_seconds(&value), None);
        }
        for seconds in [0, 1, 20, 120] {
            assert_eq!(parse_no_asset_grace_seconds(&json!(seconds)), Some(seconds));
        }
    }

    #[tokio::test]
    async fn grace_period_updates_without_restarting_state() {
        let state = AppState::new()
            .expect("state should build")
            .with_data_state_for_tests(
                crate::data::GatewayDataState::disabled().with_system_config_values_for_tests([]),
            );
        assert_eq!(no_asset_poll_grace(&state).await, Duration::from_secs(20));
        for seconds in [2, 0, 120] {
            state
                .upsert_system_config_json_value(NO_ASSET_GRACE_KEY, &json!(seconds), None)
                .await
                .unwrap();
            assert_eq!(
                no_asset_poll_grace(&state).await,
                Duration::from_secs(seconds)
            );
        }
    }
}
