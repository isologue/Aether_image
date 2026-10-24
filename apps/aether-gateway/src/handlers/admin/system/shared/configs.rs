use crate::handlers::admin::model::ADMIN_EXTERNAL_MODELS_PROXY_NODE_CONFIG_KEY;
use crate::handlers::admin::request::AdminAppState;
use crate::handlers::shared::{
    bark_device_key_binding, encrypt_bark_device_key, encrypt_smtp_password, smtp_password_binding,
    system_config_bool, system_config_string, unix_secs_to_rfc3339,
};
use crate::GatewayError;
use aether_admin::system::{
    admin_system_config_default_value as admin_system_config_default_value_pure,
    admin_system_config_delete_keys as admin_system_config_delete_keys_pure,
    build_admin_system_config_deleted_payload,
    build_admin_system_config_detail_payload as build_admin_system_config_detail_payload_pure,
    build_admin_system_config_updated_payload,
    build_admin_system_configs_payload as build_admin_system_configs_payload_pure,
    is_sensitive_admin_system_config_key as is_sensitive_admin_system_config_key_pure,
    normalize_admin_system_config_key as normalize_admin_system_config_key_pure,
    parse_admin_system_config_update,
};
use axum::body::Bytes;
use axum::http;
use serde_json::json;

const ADMIN_EXTERNAL_MODELS_CONFIG_ROUTE: &str = "/api/admin/models/external/config";

fn is_external_models_proxy_node_config_key(key: &str) -> bool {
    key.trim()
        .eq_ignore_ascii_case(ADMIN_EXTERNAL_MODELS_PROXY_NODE_CONFIG_KEY)
}

fn external_models_proxy_node_config_owner_error(
    key: &str,
) -> Option<(http::StatusCode, serde_json::Value)> {
    is_external_models_proxy_node_config_key(key).then(|| {
        (
            http::StatusCode::BAD_REQUEST,
            json!({
                "detail": format!(
                    "配置项 '{}' 由模型目录管理，请使用 {}",
                    ADMIN_EXTERNAL_MODELS_PROXY_NODE_CONFIG_KEY,
                    ADMIN_EXTERNAL_MODELS_CONFIG_ROUTE,
                )
            }),
        )
    })
}

fn normalize_admin_system_config_key(requested_key: &str) -> String {
    normalize_admin_system_config_key_pure(requested_key)
}

fn admin_system_config_delete_keys(requested_key: &str) -> Vec<String> {
    admin_system_config_delete_keys_pure(requested_key)
}

pub(crate) fn is_sensitive_admin_system_config_key(key: &str) -> bool {
    is_sensitive_admin_system_config_key_pure(key)
}

fn admin_system_config_default_value(key: &str) -> Option<serde_json::Value> {
    if key == crate::image_generation::MAX_IMAGE_COUNT_KEY {
        return Some(json!(crate::image_generation::DEFAULT_MAX_IMAGE_COUNT));
    }
    if key == crate::image_generation::NO_ASSET_GRACE_KEY {
        return Some(json!(
            crate::image_generation::DEFAULT_NO_ASSET_GRACE_SECONDS
        ));
    }
    if key == crate::image_generation::MAX_PROMPT_CHARS_KEY {
        return Some(json!(crate::image_generation::DEFAULT_MAX_PROMPT_CHARS));
    }
    admin_system_config_default_value_pure(key)
}

fn legacy_admin_system_config_fallback_key(normalized_key: &str) -> Option<&'static str> {
    match normalized_key {
        "module.server_chan_push.enabled" => {
            Some("module.important_notification.server_chan_enabled")
        }
        "module.server_chan_push.send_key" => {
            Some("module.important_notification.server_chan_send_key")
        }
        "module.server_chan_push.template" => {
            Some("module.important_notification.server_chan_template")
        }
        _ => None,
    }
}

pub(crate) fn build_admin_system_configs_payload(
    entries: &[aether_data::repository::system::StoredSystemConfigEntry],
) -> serde_json::Value {
    let visible_entries = entries
        .iter()
        .filter(|entry| !is_external_models_proxy_node_config_key(&entry.key))
        .cloned()
        .collect::<Vec<_>>();
    build_admin_system_configs_payload_pure(&visible_entries)
}

pub(crate) async fn build_admin_system_config_detail_payload(
    state: &AdminAppState<'_>,
    requested_key: &str,
) -> Result<Result<serde_json::Value, (http::StatusCode, serde_json::Value)>, GatewayError> {
    let requested_key = requested_key.trim();
    if let Some(error) = external_models_proxy_node_config_owner_error(requested_key) {
        return Ok(Err(error));
    }
    let normalized_key = normalize_admin_system_config_key(requested_key);
    let mut value = state.read_system_config_json_value(&normalized_key).await?;
    if value.is_none() {
        if let Some(legacy_key) = legacy_admin_system_config_fallback_key(&normalized_key) {
            value = state.read_system_config_json_value(legacy_key).await?;
        }
    }
    let value = value.or_else(|| admin_system_config_default_value(&normalized_key));
    if normalized_key == crate::account_push::SECRET_KEY {
        let stored = value
            .as_ref()
            .and_then(serde_json::Value::as_str)
            .filter(|stored| !stored.is_empty());
        let secret = match stored {
            Some(stored) => crate::handlers::shared::decrypt_system_config_secret(
                state.app(),
                crate::account_push::SECRET_KEY,
                stored,
            )
            .ok_or_else(|| GatewayError::Internal("推送密钥解密失败，请重新保存密钥".into()))?,
            None => String::new(),
        };
        return Ok(Ok(json!({
            "key": requested_key,
            "value": secret,
            "is_set": !secret.is_empty(),
        })));
    }
    Ok(build_admin_system_config_detail_payload_pure(
        requested_key,
        value,
    ))
}

/// Validate both submitted values against the final effective policy.
pub(crate) async fn validate_image_policy_entries(
    state: &AdminAppState<'_>,
    entries: &[(String, serde_json::Value, Option<String>)],
) -> Result<Result<(), (http::StatusCode, serde_json::Value)>, GatewayError> {
    let value_for = |key: &str| {
        entries
            .iter()
            .rev()
            .find(|(k, _, _)| k == key)
            .map(|(_, v, _)| v.clone())
    };
    let storage = match value_for("image_storage") {
        Some(v) => v,
        None => state
            .read_system_config_json_value("image_storage")
            .await?
            .unwrap_or_else(|| json!({})),
    };
    let config = match crate::image_storage::parse_config(storage) {
        Ok(c) => c,
        Err(_) => {
            return Ok(Err((
                http::StatusCode::BAD_REQUEST,
                json!({"detail":"图片存储配置字段或类型错误"}),
            )))
        }
    };
    if let Err(message) = config.validate() {
        return Ok(Err((
            http::StatusCode::BAD_REQUEST,
            json!({"detail":message}),
        )));
    }
    let days = match value_for("async_task_retention_days") {
        Some(v) => v,
        None => state
            .read_system_config_json_value("async_task_retention_days")
            .await?
            .unwrap_or(json!(30)),
    };
    let minimum = if config.cleanup_enabled {
        config.retention_hours.div_ceil(24).max(1)
    } else {
        1
    };
    if !days.as_u64().is_some_and(|n| n >= minimum && n <= 36500) {
        return Ok(Err((
            http::StatusCode::BAD_REQUEST,
            json!({"detail":format!("异步任务记录保留天数必须为 {minimum}–36500 的整数；不能低于图片保留时长折算天数")}),
        )));
    }
    Ok(Ok(()))
}
pub(crate) async fn apply_image_policy_entries(
    state: &AdminAppState<'_>,
    entries: &[(String, serde_json::Value, Option<String>)],
) -> Result<Result<(), (http::StatusCode, serde_json::Value)>, GatewayError> {
    if let Err(e) = validate_image_policy_entries(state, entries).await? {
        return Ok(Err(e));
    }
    match state.app().data.upsert_image_policy_entries(entries).await {
        Ok(()) => {}
        Err(aether_data::DataLayerError::InvalidInput(message)) => {
            return Ok(Err((
                http::StatusCode::BAD_REQUEST,
                json!({"detail":message}),
            )))
        }
        Err(error) => return Err(GatewayError::Internal(error.to_string())),
    }
    Ok(Ok(()))
}
pub(crate) async fn apply_admin_system_config_update(
    state: &AdminAppState<'_>,
    requested_key: &str,
    request_body: &Bytes,
) -> Result<Result<serde_json::Value, (http::StatusCode, serde_json::Value)>, GatewayError> {
    if let Some(error) = external_models_proxy_node_config_owner_error(requested_key) {
        return Ok(Err(error));
    }
    let update = match parse_admin_system_config_update(requested_key, request_body) {
        Ok(update) => update,
        Err(err) => return Ok(Err(err)),
    };
    let mut value = update.value;
    let normalized_key = update.normalized_key;
    let description = update.description;
    if normalized_key == "image_task_policy" {
        let Some(object) = value.as_object().filter(|o| {
            !o.is_empty()
                && o.keys()
                    .all(|k| matches!(k.as_str(), "image_storage" | "async_task_retention_days"))
        }) else {
            return Ok(Err((
                http::StatusCode::BAD_REQUEST,
                json!({"detail":"请提供图片存储和/或异步任务记录保留配置"}),
            )));
        };
        let entries = object
            .iter()
            .map(|(k, v)| (k.clone(), v.clone(), None))
            .collect::<Vec<_>>();
        if let Err(e) = apply_image_policy_entries(state, &entries).await? {
            return Ok(Err(e));
        }
        return Ok(Ok(
            json!({"message":"图片与任务保留策略已保存","value":value}),
        ));
    }
    if matches!(
        normalized_key.as_str(),
        "image_storage" | "async_task_retention_days"
    ) {
        let entries = [(normalized_key.clone(), value.clone(), description.clone())];
        if let Err(e) = validate_image_policy_entries(state, &entries).await? {
            return Ok(Err(e));
        }
    }

    if normalized_key == crate::image_generation::MAX_IMAGE_COUNT_KEY
        && crate::image_generation::parse_max_image_count(&value).is_none()
    {
        return Ok(Err((
            http::StatusCode::BAD_REQUEST,
            json!({"detail":"ChatGPT Web 单次请求图片数量上限必须为 1–10 的整数"}),
        )));
    }

    if normalized_key == crate::image_generation::NO_ASSET_GRACE_KEY
        && crate::image_generation::parse_no_asset_grace_seconds(&value).is_none()
    {
        return Ok(Err((
            http::StatusCode::BAD_REQUEST,
            json!({"detail":"无图片补查等待时间必须为 0–120 秒的整数"}),
        )));
    }

    if normalized_key == crate::image_generation::MAX_PROMPT_CHARS_KEY
        && crate::image_generation::parse_max_prompt_chars(&value).is_none()
    {
        return Ok(Err((
            http::StatusCode::BAD_REQUEST,
            json!({"detail":"ChatGPT Web 提示词本地长度限制必须为 0–1000000 的整数，0 表示不限制"}),
        )));
    }

    if normalized_key == crate::account_push::CONFIG_KEY {
        let config = match serde_json::from_value::<crate::account_push::PushConfig>(value.clone())
        {
            Ok(config) => config,
            Err(_) => {
                return Ok(Err((
                    http::StatusCode::BAD_REQUEST,
                    json!({"detail":"账号推送配置字段或类型错误"}),
                )))
            }
        };
        if let Err(message) = config.validate(state.app()).await {
            return Ok(Err((
                http::StatusCode::BAD_REQUEST,
                json!({"detail":message}),
            )));
        }
        value = serde_json::to_value(config)
            .map_err(|error| GatewayError::Internal(error.to_string()))?;
    }
    if normalized_key == crate::account_push::SECRET_KEY {
        if !value.as_str().is_some_and(|key| {
            key.is_empty()
                || (key.trim() == key
                    && (24..=256).contains(&key.len())
                    && key.bytes().all(|byte| byte.is_ascii_graphic()))
        }) {
            return Ok(Err((
                http::StatusCode::BAD_REQUEST,
                json!({"detail":"推送密钥必须为 24–256 个 ASCII 字符且不包含空白"}),
            )));
        }
    }
    if normalized_key == crate::account_push::RESULT_KEY {
        return Ok(Err((
            http::StatusCode::BAD_REQUEST,
            json!({"detail":"推送结果由接收接口记录，不能手动修改"}),
        )));
    }
    if normalized_key == crate::image_tasks::SETTINGS_KEY {
        let config = serde_json::from_value::<crate::image_tasks::Settings>(value.clone());
        let error = match config {
            Ok(config) => config.validate().err(),
            Err(e) => Some(e.to_string()),
        };
        if let Some(message) = error {
            return Ok(Err((
                http::StatusCode::BAD_REQUEST,
                json!({"detail":message}),
            )));
        }
    }
    if normalized_key == crate::image_tasks::RETENTION_KEY
        && !value.as_u64().is_some_and(|n| (1..=36500).contains(&n))
    {
        return Ok(Err((
            http::StatusCode::BAD_REQUEST,
            json!({"detail":"Async task retention must be 1-36500 whole days"}),
        )));
    }
    if normalized_key == crate::image_storage::CONFIG_KEY {
        let config = match crate::image_storage::parse_config(value.clone()) {
            Ok(config) => config,
            Err(_) => {
                return Ok(Err((
                    http::StatusCode::BAD_REQUEST,
                    json!({"detail":"图片存储配置字段或类型错误"}),
                )));
            }
        };
        if let Err(message) = config.validate() {
            return Ok(Err((
                http::StatusCode::BAD_REQUEST,
                json!({"detail":message}),
            )));
        }
        value = serde_json::to_value(config)
            .map_err(|error| GatewayError::Internal(error.to_string()))?;
    }
    if normalized_key == crate::image_storage::PASSWORD_KEY && value.as_str().is_none() {
        return Ok(Err((
            http::StatusCode::BAD_REQUEST,
            json!({"detail":"WebDAV 密码必须为字符串"}),
        )));
    }
    if is_sensitive_admin_system_config_key(&normalized_key)
        && value.as_str().is_some_and(|raw| !raw.is_empty())
    {
        let plaintext = value
            .as_str()
            .expect("sensitive config value was a non-empty string");
        let encrypted = if normalized_key.eq_ignore_ascii_case("smtp_password") {
            let host = state
                .read_system_config_json_value("smtp_host")
                .await?
                .and_then(|value| system_config_string(Some(&value)));
            let port = state
                .read_system_config_json_value("smtp_port")
                .await?
                .map(|value| crate::email_delivery::system_config_u16(Some(&value), 587))
                .unwrap_or(587);
            let user = state
                .read_system_config_json_value("smtp_user")
                .await?
                .and_then(|value| system_config_string(Some(&value)));
            let use_tls = state
                .read_system_config_json_value("smtp_use_tls")
                .await?
                .map(|value| system_config_bool(Some(&value), true))
                .unwrap_or(true);
            let use_ssl = state
                .read_system_config_json_value("smtp_use_ssl")
                .await?
                .map(|value| system_config_bool(Some(&value), false))
                .unwrap_or(false);
            let Some(binding) = host.as_deref().and_then(|host| {
                smtp_password_binding(host, port, user.as_deref(), use_tls, use_ssl)
            }) else {
                return Ok(Err((
                    http::StatusCode::BAD_REQUEST,
                    json!({
                        "detail": "保存 SMTP 密码前必须先配置有效的 smtp_host 和 smtp_user"
                    }),
                )));
            };
            encrypt_smtp_password(state.app(), &binding, plaintext)
        } else if normalized_key.eq_ignore_ascii_case("module.bark_push.device_key") {
            let server_url = state
                .read_system_config_json_value("module.bark_push.server_url")
                .await?
                .and_then(|value| system_config_string(Some(&value)))
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "https://api.day.app".to_string());
            let Some(binding) = bark_device_key_binding(&server_url) else {
                return Ok(Err((
                    http::StatusCode::BAD_REQUEST,
                    json!({
                        "detail": "保存 Bark Device Key 前必须先配置有效的 module.bark_push.server_url"
                    }),
                )));
            };
            encrypt_bark_device_key(state.app(), &binding, plaintext)
        } else {
            state.encrypt_system_config_secret(&normalized_key, plaintext)
        };
        let Some(encrypted) = encrypted else {
            return Ok(Err((
                http::StatusCode::SERVICE_UNAVAILABLE,
                json!({ "detail": "系统配置写入需要可用的加密密钥" }),
            )));
        };
        value = json!(encrypted);
    }

    let updated = state
        .upsert_system_config_entry(&normalized_key, &value, description.as_deref())
        .await?;
    let display_value = if is_sensitive_admin_system_config_key(&normalized_key) {
        json!("********")
    } else {
        updated.value.clone()
    };
    Ok(Ok(build_admin_system_config_updated_payload(
        updated.key,
        display_value,
        updated.description,
        updated.updated_at_unix_secs,
    )))
}

pub(crate) async fn delete_admin_system_config(
    state: &AdminAppState<'_>,
    requested_key: &str,
) -> Result<Result<serde_json::Value, (http::StatusCode, serde_json::Value)>, GatewayError> {
    if let Some(error) = external_models_proxy_node_config_owner_error(requested_key) {
        return Ok(Err(error));
    }
    let delete_keys = admin_system_config_delete_keys(requested_key);
    let mut deleted = false;
    for key in &delete_keys {
        deleted |= state.delete_system_config_value(key).await?;
    }
    if !deleted {
        return Ok(Err((
            http::StatusCode::NOT_FOUND,
            json!({ "detail": format!("配置项 '{requested_key}' 不存在") }),
        )));
    }
    Ok(Ok(build_admin_system_config_deleted_payload(requested_key)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_generation::{no_asset_poll_grace, NO_ASSET_GRACE_KEY};
    use std::time::Duration;

    #[tokio::test]
    async fn image_generation_admin_config_hot_updates_image_count_limit() {
        use crate::image_generation::{max_image_count, MAX_IMAGE_COUNT_KEY};
        let app = crate::AppState::new().unwrap().with_data_state_for_tests(
            crate::data::GatewayDataState::disabled().with_system_config_values_for_tests([]),
        );
        let state = AdminAppState::new(&app);
        let detail = build_admin_system_config_detail_payload(&state, MAX_IMAGE_COUNT_KEY)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail["value"], 1);
        assert_eq!(max_image_count(&app).await, 1);
        for value in [
            json!(0),
            json!(-1),
            json!(11),
            json!(1.5),
            json!("2"),
            serde_json::Value::Null,
        ] {
            let body = Bytes::from(serde_json::to_vec(&json!({"value": value})).unwrap());
            let error = apply_admin_system_config_update(&state, MAX_IMAGE_COUNT_KEY, &body)
                .await
                .unwrap()
                .unwrap_err();
            assert_eq!(error.0, http::StatusCode::BAD_REQUEST);
            assert_eq!(max_image_count(&app).await, 1);
        }
        for count in [2, 10, 1] {
            let body = Bytes::from(serde_json::to_vec(&json!({"value": count})).unwrap());
            apply_admin_system_config_update(&state, MAX_IMAGE_COUNT_KEY, &body)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(max_image_count(&app).await, count);
            let detail = build_admin_system_config_detail_payload(&state, MAX_IMAGE_COUNT_KEY)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(detail["value"], count);
        }
    }

    #[tokio::test]
    async fn image_generation_admin_config_defaults_validates_and_hot_updates_prompt_limit() {
        use crate::image_generation::{max_prompt_chars, MAX_PROMPT_CHARS_KEY};
        let app = crate::AppState::new().unwrap().with_data_state_for_tests(
            crate::data::GatewayDataState::disabled().with_system_config_values_for_tests([]),
        );
        let state = AdminAppState::new(&app);
        let detail = build_admin_system_config_detail_payload(&state, MAX_PROMPT_CHARS_KEY)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail["value"], 32_768);
        assert_eq!(max_prompt_chars(&app).await, 32_768);
        for value in [
            json!(-1),
            json!(1_000_001),
            json!(1.5),
            json!("32768"),
            serde_json::Value::Null,
        ] {
            let body = Bytes::from(serde_json::to_vec(&json!({"value": value})).unwrap());
            let error = apply_admin_system_config_update(&state, MAX_PROMPT_CHARS_KEY, &body)
                .await
                .unwrap()
                .unwrap_err();
            assert_eq!(error.0, http::StatusCode::BAD_REQUEST);
            assert_eq!(max_prompt_chars(&app).await, 32_768);
        }
        for chars in [0, 123, 1_000_000] {
            let body = Bytes::from(serde_json::to_vec(&json!({"value": chars})).unwrap());
            apply_admin_system_config_update(&state, MAX_PROMPT_CHARS_KEY, &body)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(max_prompt_chars(&app).await, chars);
        }
    }

    #[tokio::test]
    async fn image_generation_admin_config_defaults_validates_and_hot_updates_grace() {
        let app = crate::AppState::new().unwrap().with_data_state_for_tests(
            crate::data::GatewayDataState::disabled().with_system_config_values_for_tests([]),
        );
        let state = AdminAppState::new(&app);
        let detail = build_admin_system_config_detail_payload(&state, NO_ASSET_GRACE_KEY)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail["value"], 20);
        // Prime the runtime cache before saving through the admin path.
        assert_eq!(no_asset_poll_grace(&app).await, Duration::from_secs(20));
        for value in [
            json!(-1),
            json!(121),
            json!(1.5),
            json!("10"),
            serde_json::Value::Null,
        ] {
            let body = Bytes::from(serde_json::to_vec(&json!({"value": value})).unwrap());
            let error = apply_admin_system_config_update(&state, NO_ASSET_GRACE_KEY, &body)
                .await
                .unwrap()
                .unwrap_err();
            assert_eq!(error.0, http::StatusCode::BAD_REQUEST);
            assert_eq!(no_asset_poll_grace(&app).await, Duration::from_secs(20));
        }
        for seconds in [1, 0, 120] {
            let body = Bytes::from(serde_json::to_vec(&json!({"value": seconds})).unwrap());
            apply_admin_system_config_update(&state, NO_ASSET_GRACE_KEY, &body)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                no_asset_poll_grace(&app).await,
                Duration::from_secs(seconds)
            );
        }
    }
}
