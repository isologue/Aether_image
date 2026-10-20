//! Sub2API-compatible account ingestion. Credentials stay in the existing OAuth pipeline.
use crate::{admin_api::AdminAppState, AppState, GatewayError};
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::{header, HeaderMap, Method, StatusCode},
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
static IMPORT_GATE: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();

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
                .read_provider_catalog_providers_by_ids(std::slice::from_ref(&self.provider_id))
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
    valid_secret(token, secret)
}
fn valid_sub2api_key(headers: &HeaderMap, secret: &str) -> bool {
    let mut values = headers.get_all("x-api-key").iter();
    let Some(value) = values.next().and_then(|value| value.to_str().ok()) else {
        return false;
    };
    if values.next().is_some() || value.is_empty() || value.trim() != value {
        return false;
    }
    valid_secret(value, secret)
}
fn valid_secret(token: &str, secret: &str) -> bool {
    if secret.is_empty() {
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

fn sub2api_failure(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({"code":status.as_u16(),"message":message})),
    )
        .into_response()
}

#[derive(Debug)]
struct NormalizedPush {
    accounts: Vec<Value>,
    client_ids: Vec<PushClientIdDiagnostic>,
}

#[derive(Debug)]
struct PushClientIdDiagnostic {
    account_index: usize,
    fields: Vec<&'static str>,
    provided_client_id: Option<String>,
    duplicate: bool,
}

impl PushClientIdDiagnostic {
    fn from_credentials(account_index: usize, credentials: &Value, duplicate: bool) -> Self {
        let mut fields = Vec::new();
        for (container, field, path) in [
            (credentials, "client_id", "credentials.client_id"),
            (credentials, "clientId", "credentials.clientId"),
            (
                &credentials["auth_config"],
                "client_id",
                "credentials.auth_config.client_id",
            ),
            (
                &credentials["auth_config"],
                "clientId",
                "credentials.auth_config.clientId",
            ),
            (
                &credentials["authConfig"],
                "client_id",
                "credentials.authConfig.client_id",
            ),
            (
                &credentials["authConfig"],
                "clientId",
                "credentials.authConfig.clientId",
            ),
        ] {
            if container.get(field).is_some() {
                fields.push(path);
            }
        }
        Self {
            account_index,
            fields,
            provided_client_id: aether_oauth::provider::providers::explicit_oauth_client_id(
                credentials,
            ),
            duplicate,
        }
    }

    fn log(&self, push_id: &str, provider_id: &str, provider_type: &str) {
        let state = if self.provided_client_id.is_some() {
            "valid"
        } else if self.fields.is_empty() {
            "missing"
        } else {
            "invalid"
        };
        let default_client_id = aether_oauth::provider::providers::GENERIC_PROVIDER_OAUTH_TEMPLATES
            .iter()
            .find(|template| template.provider_type == provider_type)
            .map(|template| template.client_id);
        let (selected_client_id, source) = match provider_type {
            "chatgpt_web" => match self.provided_client_id.as_deref() {
                Some(value) => (Some(value), "account"),
                None => (default_client_id, "web_default"),
            },
            "codex" => (default_client_id, "codex_default"),
            _ => (None, "unsupported_provider"),
        };
        // Log only diagnostic fields, never the credentials object or invalid raw values.
        // This is the refresh policy selected at ingestion, not proof of an OAuth request.
        tracing::info!(
            push_id,
            provider_id,
            provider_type,
            account_index = self.account_index,
            duplicate = self.duplicate,
            client_id_present = !self.fields.is_empty(),
            client_id_state = state,
            client_id_fields = %self.fields.join(","),
            provided_client_id = self.provided_client_id.as_deref(),
            selected_refresh_client_id = selected_client_id,
            client_id_source = source,
            "account_push_client_id"
        );
    }
}

fn normalize_accounts(accounts: Vec<Value>) -> Result<NormalizedPush, &'static str> {
    let mut items = Vec::new();
    let mut client_ids = Vec::with_capacity(accounts.len());
    let mut seen = HashSet::new();
    for (account_index, item) in accounts.into_iter().enumerate() {
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
        let duplicate = !seen.insert(identity.to_owned());
        client_ids.push(PushClientIdDiagnostic::from_credentials(
            account_index,
            &item,
            duplicate,
        ));
        if duplicate {
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
            "chatgpt_user_id",
            "account_user_id",
            "accountUserId",
            "user_id",
            "userId",
            "workspace_id",
            "workspaceId",
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
        if let Some(client_id) = aether_oauth::provider::providers::explicit_oauth_client_id(&item)
        {
            clean.insert("client_id".into(), json!(client_id));
        }
        items.push(Value::Object(clean));
    }
    if items.is_empty() {
        return Err("没有可导入的账号，accounts 不能为空");
    }
    Ok(NormalizedPush {
        accounts: items,
        client_ids,
    })
}

fn normalize_sub2api_payload(value: Value) -> Result<NormalizedPush, &'static str> {
    // Move the envelope instead of cloning a potentially 128 MiB account bundle.
    let data = match value {
        Value::Object(mut object) => match object.remove("data") {
            Some(data) => data,
            None => Value::Object(object),
        },
        value => value,
    };
    let object = data.as_object().ok_or("Sub2API 请求体必须是 JSON 对象")?;
    if object
        .get("type")
        .is_some_and(|kind| !matches!(kind.as_str(), Some("" | "sub2api-data" | "sub2api-bundle")))
    {
        return Err("不支持的 Sub2API 数据类型");
    }
    if object
        .get("version")
        .is_some_and(|version| !matches!(version.as_u64(), Some(0 | 1)))
    {
        return Err("不支持的 Sub2API 数据版本");
    }
    let accounts = object
        .get("accounts")
        .and_then(Value::as_array)
        .ok_or("Sub2API 数据缺少 accounts 数组")?;

    let mut normalized_accounts = Vec::with_capacity(accounts.len());
    for account in accounts {
        if contains_agent_identity(account) {
            return Err("Agent Identity 凭据必须使用专属导入接口");
        }
        let account = account
            .as_object()
            .ok_or("Sub2API accounts 中每项必须是账号对象")?;
        if !account
            .get("platform")
            .and_then(Value::as_str)
            .is_some_and(|platform| platform.trim().eq_ignore_ascii_case("openai"))
        {
            return Err("Sub2API 仅支持导入 OpenAI OAuth 账号");
        }
        if !account
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("oauth"))
        {
            return Err("Sub2API 仅支持导入 OpenAI OAuth 账号");
        }
        let credentials = account
            .get("credentials")
            .and_then(Value::as_object)
            .ok_or("Sub2API 账号缺少 credentials 对象")?;
        let mut merged = credentials.clone();
        if let Some(name) = account
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            merged.entry("account_name").or_insert_with(|| json!(name));
        }
        for source in
            std::iter::once(account).chain(account.get("extra").and_then(Value::as_object))
        {
            for field in [
                "account_id",
                "chatgpt_account_id",
                "chatgpt_user_id",
                "account_user_id",
                "user_id",
                "email",
                "plan_type",
                "workspace_id",
            ] {
                if let Some(value) = source.get(field).cloned() {
                    merged.entry(field).or_insert(value);
                }
            }
        }
        normalized_accounts.push(Value::Object(merged));
    }
    if normalized_accounts.is_empty() {
        return Err("Sub2API 数据中没有可导入的 OpenAI OAuth 账号");
    }
    normalize_accounts(normalized_accounts)
}

async fn authorize_sub2api(state: &AppState, headers: &HeaderMap) -> Result<PushConfig, Response> {
    let config: PushConfig = match state.read_system_config_json_value_strong(CONFIG_KEY).await {
        Ok(value) => serde_json::from_value(value.unwrap_or_else(|| json!({}))).map_err(|_| {
            sub2api_failure(StatusCode::SERVICE_UNAVAILABLE, "账号推送配置格式错误")
        })?,
        Err(_) => {
            return Err(sub2api_failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "账号推送暂不可用",
            ))
        }
    };
    if !config.enabled {
        return Err(sub2api_failure(StatusCode::NOT_FOUND, "账号推送未启用"));
    }
    let secret = load_secret(state)
        .await
        .map_err(|_| sub2api_failure(StatusCode::SERVICE_UNAVAILABLE, "账号推送密钥不可用"))?;
    if !valid_bearer(headers, &secret) && !valid_sub2api_key(headers, &secret) {
        return Err(sub2api_failure(StatusCode::UNAUTHORIZED, "推送密钥无效"));
    }
    Ok(config)
}

pub(crate) async fn receive_sub2api(State(state): State<AppState>, request: Request) -> Response {
    let config = match authorize_sub2api(&state, request.headers()).await {
        Ok(config) => config,
        Err(response) => return response,
    };

    if request.method() == Method::GET {
        // Connection probe only; this endpoint never exports stored credentials.
        return Json(json!({
            "code": 0,
            "message": "success",
            "data": {
                "type": "sub2api-data",
                "version": 1,
                "exported_at": chrono::Utc::now().to_rfc3339(),
                "proxies": [],
                "accounts": [],
            }
        }))
        .into_response();
    }
    if request.method() != Method::POST {
        return sub2api_failure(StatusCode::METHOD_NOT_ALLOWED, "仅支持 GET 或 POST");
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
        return sub2api_failure(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "请使用 application/json 推送 Sub2API 数据",
        );
    }
    let bytes = match to_bytes(request.into_body(), BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return sub2api_failure(StatusCode::PAYLOAD_TOO_LARGE, "推送请求不能超过 128 MiB")
        }
    };
    let value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => value,
        Err(_) => return sub2api_failure(StatusCode::BAD_REQUEST, "请求体不是合法 JSON"),
    };
    let credentials = match normalize_sub2api_payload(value) {
        Ok(credentials) => credentials,
        Err(message) => return sub2api_failure(StatusCode::BAD_REQUEST, message),
    };
    let Ok(guard) = IMPORT_GATE
        .get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
        .try_lock_owned()
    else {
        return sub2api_failure(StatusCode::CONFLICT, "已有推送正在导入，请稍后重试");
    };
    let state = state.clone();
    let result = tokio::spawn(async move {
        let _guard = guard;
        complete_import(&state, &config.provider_id, credentials).await
    })
    .await;
    match result {
        Ok(Ok((status, result))) => (status, Json(result)).into_response(),
        Ok(Err(_)) | Err(_) => {
            sub2api_failure(StatusCode::SERVICE_UNAVAILABLE, "Sub2API 账号导入暂不可用")
        }
    }
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

async fn complete_import(
    state: &AppState,
    provider_id: &str,
    normalized: NormalizedPush,
) -> Result<(StatusCode, Value), GatewayError> {
    let push_id = uuid::Uuid::new_v4().to_string();
    let provider_type = state
        .read_provider_catalog_providers_by_ids(&[provider_id.to_owned()])
        .await?
        .into_iter()
        .next()
        .map(|provider| provider.provider_type.trim().to_ascii_lowercase())
        .unwrap_or_default();
    for diagnostic in &normalized.client_ids {
        diagnostic.log(&push_id, provider_id, &provider_type);
    }
    let credentials = normalized.accounts;
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
        let batch_status = response.status();
        let body = to_bytes(response.into_body(), BODY_LIMIT)
            .await
            .map_err(|_| GatewayError::Internal("读取导入结果失败".into()))?;
        let result: Value = serde_json::from_slice(&body)
            .map_err(|_| GatewayError::Internal("导入结果格式错误".into()))?;
        let offset = batch_index * IMPORT_BATCH_SIZE;
        let batch_added = result["added"].as_u64();
        let batch_updated = result["updated"].as_u64();
        let batch_failed = result["failed"].as_u64();
        if !matches!((batch_added, batch_updated, batch_failed), (Some(a), Some(u), Some(f)) if a + u + f == batch.len() as u64)
        {
            tracing::warn!(
                status = batch_status.as_u16(),
                "account push importer returned invalid counters"
            );
            failed += batch.len();
            errors.extend((0..batch.len()).map(|index| {
                json!({
                    "index": offset + index,
                    "kind": "account",
                    "message": "账号导入失败，请检查目标提供商配置",
                })
            }));
            continue;
        }
        added += result["added"].as_u64().unwrap_or_default() as usize;
        updated += result["updated"].as_u64().unwrap_or_default() as usize;
        failed += result["failed"].as_u64().unwrap_or_default() as usize;
        if let Some(batch_errors) = result["errors"].as_array() {
            errors.extend(batch_errors.iter().map(|error| {
                let local_index = error["index"].as_u64().unwrap_or_default() as usize;
                json!({
                    "index": offset + local_index,
                    "kind": "account",
                    "message": error["error"],
                })
            }));
        }
    }

    let total = credentials.len();
    tracing::info!(%push_id, provider_id, total, added, updated, failed, "account_push_completed");
    let status = if added + updated == 0 {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    };
    let result = json!({
        "code": if status.is_success() { 0 } else { status.as_u16() },
        "message": if status.is_success() { "success" } else { "import failed" },
        "data": {
            "proxy_created": 0,
            "proxy_reused": 0,
            "proxy_failed": 0,
            "account_created": added,
            "account_updated": updated,
            "account_failed": failed,
            "errors": errors,
        },
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
    Ok((status, result))
}

#[cfg(test)]
mod tests;
