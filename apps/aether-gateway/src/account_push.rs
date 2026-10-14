//! ChatGPT2API-compatible account ingestion. Credentials stay in the existing OAuth pipeline.
use crate::{admin_api::AdminAppState, AppState, GatewayError};
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::{
    collections::HashSet,
    sync::{Arc, OnceLock},
};

pub(crate) const CONFIG_KEY: &str = "account_push";
pub(crate) const SECRET_KEY: &str = "account_push_api_key";
pub(crate) const RESULT_KEY: &str = "account_push_last_result";
const BODY_LIMIT: usize = 128 * 1024 * 1024;
const IMPORT_BATCH_SIZE: usize = 100;

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PushConfig {
    pub enabled: bool,
    pub provider_id: String,
}
impl PushConfig {
    pub(crate) async fn validate(&self, state: &AppState) -> Result<(), String> {
        if self.provider_id.len() > 128 || self.provider_id.trim() != self.provider_id {
            return Err("目标提供商 ID 无效".into());
        }
        if !self.provider_id.is_empty() {
            let provider = state
                .read_provider_catalog_providers_by_ids(&[self.provider_id.clone()])
                .await
                .map_err(|_| "读取提供商失败".to_string())?
                .into_iter()
                .next()
                .ok_or_else(|| "目标提供商不存在".to_string())?;
            if !matches!(
                provider.provider_type.trim().to_ascii_lowercase().as_str(),
                "chatgpt_web" | "codex"
            ) {
                return Err("请选择 ChatGPT Web 或 Codex 提供商".into());
            }
        } else if self.enabled {
            return Err("启用前请选择目标提供商".into());
        }
        if self.enabled
            && load_secret(state)
                .await
                .map_err(|_| "读取推送密钥失败".to_string())?
                .is_empty()
        {
            return Err("启用前请配置推送密钥".into());
        }
        Ok(())
    }
}
async fn load_secret(state: &AppState) -> Result<String, GatewayError> {
    let stored = state
        .read_system_config_json_value_strong(SECRET_KEY)
        .await?;
    let Some(stored) = stored
        .and_then(|value| value.as_str().map(str::to_owned))
        .filter(|value| !value.is_empty())
    else {
        return Ok(String::new());
    };
    crate::handlers::shared::decrypt_system_config_secret(state, SECRET_KEY, &stored)
        .ok_or_else(|| GatewayError::Internal("推送密钥解密失败，请重新保存密钥".into()))
}
fn valid_bearer(headers: &HeaderMap, secret: &str) -> bool {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let Some(value) = values.next().and_then(|value| value.to_str().ok()) else {
        return false;
    };
    if values.next().is_some() || secret.is_empty() {
        return false;
    }
    let Some((scheme, token)) = value.split_once(' ') else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() || token.trim() != token {
        return false;
    }
    // HMAC verify uses a constant-time tag comparison without adding another dependency.
    let mut expected =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    expected.update(b"aether-account-push-auth");
    let tag = expected.finalize().into_bytes();
    let mut presented =
        Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("HMAC accepts any key length");
    presented.update(b"aether-account-push-auth");
    presented.verify_slice(&tag).is_ok()
}
fn failure(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({"success":false,"detail":{"error":message},"error":message})),
    )
        .into_response()
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct PushRequest {
    accounts: Vec<Value>,
    tokens: Vec<String>,
    // Accepted for compatibility. Refresh behavior is owned by Aether's OAuth importer;
    // account credentials are never echoed, even when return_items is true.
    refresh: Option<bool>,
    return_items: Option<bool>,
}
fn normalize_payload(value: Value) -> Result<String, &'static str> {
    let payload = match value {
        Value::Array(accounts) => PushRequest {
            accounts,
            ..Default::default()
        },
        Value::Object(ref object)
            if object.contains_key("access_token")
                || object.contains_key("accessToken")
                || object.contains_key("refresh_token")
                || object.contains_key("refreshToken") =>
        {
            PushRequest {
                accounts: vec![value],
                ..Default::default()
            }
        }
        value => serde_json::from_value::<PushRequest>(value)
            .map_err(|_| "请求体必须为包含 accounts 或 tokens 的 JSON 对象")?,
    };
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for item in payload.accounts {
        let Some(object) = item.as_object() else {
            return Err("accounts 中每项必须为账号对象");
        };
        if contains_agent_identity(&item) {
            return Err("Agent Identity 凭据必须使用专属导入接口");
        }
        let access = string_field(object, &["access_token", "accessToken"])?;
        let refresh = string_field(object, &["refresh_token", "refreshToken"])?;
        let identity = access
            .or(refresh)
            .ok_or("账号缺少 access_token 或 refresh_token")?;
        if !seen.insert(identity.to_owned()) {
            continue;
        }
        // Only account credential/identity fields can enter the importer. An external
        // sender cannot change proxy routing or inject request headers through this API.
        let mut clean = serde_json::Map::new();
        for field in [
            "access_token",
            "accessToken",
            "refresh_token",
            "refreshToken",
            "id_token",
            "account_id",
            "accountId",
            "chatgpt_account_id",
            "account_user_id",
            "accountUserId",
            "user_id",
            "userId",
            "email",
            "plan_type",
            "planType",
            "expires_at",
            "expiresAt",
            "expired",
            "name",
            "account_name",
        ] {
            if let Some(value) = object.get(field) {
                clean.insert(field.to_string(), value.clone());
            }
        }
        // Normalize aliases so an empty snake_case field cannot shadow a valid alias.
        clean.remove("accessToken");
        clean.remove("refreshToken");
        if let Some(access) = access {
            clean.insert("access_token".into(), json!(access));
        }
        if let Some(refresh) = refresh {
            clean.insert("refresh_token".into(), json!(refresh));
        }
        items.push(Value::Object(clean));
    }
    for token in payload.tokens {
        let token = token.trim();
        if !token.is_empty() && seen.insert(token.to_owned()) {
            items.push(json!({"access_token":token}));
        }
    }
    if items.is_empty() {
        return Err("没有可导入的账号，accounts 或 tokens 不能为空");
    }
    serde_json::to_string(&items).map_err(|_| "账号 JSON 编码失败")
}
fn string_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Result<Option<&'a str>, &'static str> {
    for name in names {
        if let Some(value) = object.get(*name) {
            let value = value.as_str().ok_or("账号 Token 字段必须为字符串")?.trim();
            if !value.is_empty() {
                return Ok(Some(value));
            }
        }
    }
    Ok(None)
}
fn contains_agent_identity(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            aether_provider_transport::is_codex_agent_identity_auth_config_value(value)
                || object.values().any(contains_agent_identity)
        }
        Value::Array(items) => items.iter().any(contains_agent_identity),
        _ => false,
    }
}

pub(crate) async fn receive(State(state): State<AppState>, request: Request) -> Response {
    match receive_inner(&state, request).await {
        Ok(response) => response,
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "账号推送暂不可用，请检查服务配置后重试",
        ),
    }
}
async fn receive_inner(state: &AppState, request: Request) -> Result<Response, GatewayError> {
    // Strong reads make disabling the endpoint or rotating its key effective immediately.
    let config: PushConfig = serde_json::from_value(
        state
            .read_system_config_json_value_strong(CONFIG_KEY)
            .await?
            .unwrap_or_else(|| json!({})),
    )
    .map_err(|_| GatewayError::Internal("账号推送配置格式错误".into()))?;
    if !config.enabled {
        return Ok(failure(StatusCode::NOT_FOUND, "账号推送未启用"));
    }
    if !valid_bearer(request.headers(), &load_secret(state).await?) {
        return Ok(failure(StatusCode::UNAUTHORIZED, "推送密钥无效"));
    }
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    if !content_type.eq_ignore_ascii_case("application/json") {
        return Ok(failure(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "请使用 application/json 推送账号",
        ));
    }
    let bytes = match to_bytes(request.into_body(), BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok(failure(
                StatusCode::PAYLOAD_TOO_LARGE,
                "推送请求不能超过 128 MiB",
            ))
        }
    };
    let value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => value,
        Err(_) => return Ok(failure(StatusCode::BAD_REQUEST, "请求体不是合法 JSON")),
    };
    let credentials = match normalize_payload(value) {
        Ok(credentials) => credentials,
        Err(message) => return Ok(failure(StatusCode::BAD_REQUEST, message)),
    };
    static IMPORT_GATE: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
    let Ok(guard) = IMPORT_GATE
        .get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
        .try_lock_owned()
    else {
        return Ok(failure(
            StatusCode::CONFLICT,
            "已有推送正在导入，请稍后重试",
        ));
    };
    // Keep importing and recording the result even if the sender disconnects.
    let state = state.clone();
    tokio::spawn(async move {
        let _guard = guard;
        complete_import(&state, &config.provider_id, credentials).await
    })
    .await
    .map_err(|_| GatewayError::Internal("账号导入任务异常".into()))?
}
async fn complete_import(
    state: &AppState,
    provider_id: &str,
    credentials: String,
) -> Result<Response, GatewayError> {
    let credentials: Vec<Value> = serde_json::from_str(&credentials)
        .map_err(|_| GatewayError::Internal("账号 JSON 格式错误".into()))?;
    let mut added = 0usize;
    let mut updated = 0usize;
    let mut failed = 0usize;
    let mut errors = Vec::new();

    for (batch_index, batch) in credentials.chunks(IMPORT_BATCH_SIZE).enumerate() {
        let response = crate::admin_api::import_pushed_accounts(
            &AdminAppState::new(state),
            provider_id,
            serde_json::to_string(batch)
                .map_err(|error| GatewayError::Internal(error.to_string()))?,
        )
        .await?;
        let body = to_bytes(response.into_body(), BODY_LIMIT)
            .await
            .map_err(|_| GatewayError::Internal("读取导入结果失败".into()))?;
        let result: Value = serde_json::from_slice(&body)
            .map_err(|_| GatewayError::Internal("导入结果格式错误".into()))?;
        let offset = batch_index * IMPORT_BATCH_SIZE;
        added += result["added"].as_u64().unwrap_or_default() as usize;
        updated += result["updated"].as_u64().unwrap_or_default() as usize;
        failed += result["failed"].as_u64().unwrap_or_default() as usize;
        if let Some(batch_errors) = result["errors"].as_array() {
            errors.extend(batch_errors.iter().map(|error| {
                let local_index = error["index"].as_u64().unwrap_or_default() as usize;
                json!({
                    "index": offset + local_index,
                    "error": error["error"],
                })
            }));
        }
    }

    let total = credentials.len();
    let status = if added + updated == 0 {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    };
    let result = json!({
        "success": failed == 0,
        "total": total,
        "added": added,
        "updated": updated,
        "skipped": 0,
        "failed": failed,
        "refreshed": 0,
        "errors": errors,
        "items": [],
    });
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let summary = json!({"received_at":timestamp,"provider_id":provider_id,"http_status":status.as_u16(),
        "total":total,"added":added,"updated":updated,"failed":failed,"success":failed == 0});
    if state
        .upsert_system_config_json_value(RESULT_KEY, &summary, Some("最近一次账号推送结果"))
        .await
        .is_err()
    {
        tracing::warn!("failed to persist account push result summary");
    }
    Ok((status, Json(result)).into_response())
}

#[cfg(test)]
mod tests;
