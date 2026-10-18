use std::time::Duration;

use serde_json::Value;

use crate::AppState;

pub(crate) const NO_ASSET_GRACE_KEY: &str = "chatgpt_web_image_no_asset_grace_seconds";
pub(crate) const DEFAULT_NO_ASSET_GRACE_SECONDS: u64 = 20;
pub(crate) const MAX_NO_ASSET_GRACE_SECONDS: u64 = 120;

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
