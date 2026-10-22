pub(crate) mod flow;
mod inputs;
pub(crate) mod worker;
use crate::control::GatewayControlDecision;
use crate::{AppState, GatewayError};
use aether_data::driver::postgres::ImageTaskAdmission;
use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
pub(crate) use inputs::{scheduling, Submission};
use serde_json::{json, Value};
pub(crate) use worker::spawn_worker;
pub(crate) const SETTINGS_KEY: &str = "image_async_tasks";
pub(crate) const RETENTION_KEY: &str = "async_task_retention_days";
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub concurrency: u32,
    pub max_queued: u32,
    pub max_queue_seconds: u32,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            concurrency: 200,
            max_queued: 10000,
            max_queue_seconds: 3600,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if self.concurrency == 0
            || self.concurrency > 10000
            || self.max_queued == 0
            || self.max_queued > 1000000
            || self.max_queue_seconds == 0
            || self.max_queue_seconds > 604800
        {
            return Err("Invalid image queue limits".into());
        }
        Ok(())
    }
}
pub(crate) async fn settings(state: &AppState) -> Result<Settings, GatewayError> {
    let settings = state
        .read_system_config_json_value(SETTINGS_KEY)
        .await?
        .map(serde_json::from_value::<Settings>)
        .transpose()
        .map_err(|e| GatewayError::Internal(e.to_string()))?
        .unwrap_or_default();
    settings.validate().map_err(GatewayError::Internal)?;
    Ok(settings)
}
fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"error":{"code":code,"message":message}})),
    )
        .into_response()
}
fn cleaned() -> Response {
    error(StatusCode::GONE, "image_cleaned", "图片已被清理")
}
/// Authenticated idempotent replays do not need fresh execution quota.
pub(crate) async fn repeated_submission(
    state: &AppState,
    headers: &HeaderMap,
    path: &str,
    original: &Bytes,
    decision: Option<&GatewayControlDecision>,
) -> Result<Option<Response>, GatewayError> {
    let Some(auth) = decision.and_then(|d| d.auth_context.as_ref()) else {
        return Ok(None);
    };
    let Some(key) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let Some(key) = key
        .to_str()
        .ok()
        .filter(|s| !s.is_empty() && s.len() <= 256 && s.bytes().all(|b| b.is_ascii_graphic()))
    else {
        return Ok(Some(error(
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
            "Idempotency-Key must contain 1-256 visible ASCII characters",
        )));
    };
    let ct = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json");
    let fingerprint = match inputs::fingerprint(original, ct, path) {
        Ok(v) => v,
        Err(e) => {
            return Ok(Some(error(
                StatusCode::BAD_REQUEST,
                "invalid_image_input",
                &e,
            )))
        }
    };
    let Some(repo) = state.data.image_tasks() else {
        return Ok(None);
    };
    let Some(task) = repo
        .find_idempotent(&auth.api_key_id, key)
        .await
        .map_err(|e| GatewayError::Internal(e.to_string()))?
    else {
        return Ok(None);
    };
    Ok(Some(
        if task["fingerprint"].as_str() == Some(fingerprint.as_str()) {
            accepted(&task)
        } else {
            error(
                StatusCode::CONFLICT,
                "idempotency_conflict",
                "Idempotency-Key was used with a different request",
            )
        },
    ))
}
fn public(task: &Value) -> Value {
    let mut task = task.clone();
    for key in [
        "request",
        "snapshot",
        "credential",
        "node_id",
        "fingerprint",
        "idempotency_key",
        "api_key_id",
        "user_id",
        "path",
        "content_type",
        "result_refs",
        "terminal_event",
        "recovery",
    ] {
        task.as_object_mut().map(|o| o.remove(key));
    }
    task
}
pub(crate) fn query<'a>(
    state: &'a AppState,
    id: &'a str,
    decision: Option<&'a GatewayControlDecision>,
) -> impl std::future::Future<Output = Result<Response, GatewayError>> + Send + 'a {
    Box::pin(query_inner(state, id, decision))
}
async fn query_inner(
    state: &AppState,
    id: &str,
    decision: Option<&GatewayControlDecision>,
) -> Result<Response, GatewayError> {
    if !valid_task_id(id) {
        return Ok(error(
            StatusCode::BAD_REQUEST,
            "invalid_task_id",
            "Invalid image task ID",
        ));
    }
    let Some(auth) = decision.and_then(|d| d.auth_context.as_ref()) else {
        return Ok(error(
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "API Key required",
        ));
    };
    let Some(repo) = state.data.image_tasks() else {
        return Ok(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "queue_unavailable",
            "Image task database unavailable",
        ));
    };
    let Some(task) = repo
        .get(id)
        .await
        .map_err(|e| GatewayError::Internal(e.to_string()))?
    else {
        return Ok(cleaned());
    };
    if task["api_key_id"].as_str() != Some(auth.api_key_id.as_str()) {
        return Ok(error(
            StatusCode::FORBIDDEN,
            "task_forbidden",
            "API Key does not own this task",
        ));
    }
    render(state, &task).await
}
async fn render(state: &AppState, task: &Value) -> Result<Response, GatewayError> {
    let mut result = public(task);
    if task["status"] == "succeeded" && task["outcome"] != "content_refused" {
        if task["node_id"].as_str() != Some(worker::node_id(state).await?.as_str())
            && task["result_refs"]["data"]
                .as_array()
                .is_some_and(|a| a.iter().any(|i| i["$image_ref"]["remote_url"].is_null()))
        {
            return Ok(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "result_node_unavailable",
                "Read this task from its storage node",
            ));
        }
        let _gate = state
            .image_storage
            .response_gate
            .acquire()
            .await
            .map_err(|_| GatewayError::Internal("Image reader stopped".into()))?;
        let local_available =
            task["node_id"].as_str() == Some(worker::node_id(state).await?.as_str());
        match crate::image_storage::read_task_result(state, &task["result_refs"], local_available)
            .await
        {
            Ok(value) => result["result"] = value,
            Err(crate::image_storage::ResultReadError::Cleaned) => return Ok(cleaned()),
            Err(crate::image_storage::ResultReadError::Unavailable(e)) => {
                return Ok(error(StatusCode::BAD_GATEWAY, "image_read_failed", &e))
            }
        }
    }
    Ok(Json(result).into_response())
}
// Construct large I/O futures outside the shared router's poll frame. Boxing
// only at the call site still reserves their construction temporaries there.
pub(crate) fn submit<'a>(
    state: &'a AppState,
    headers: &'a HeaderMap,
    path: &'a str,
    body: &'a Bytes,
    original: &'a Bytes,
    decision: Option<&'a GatewayControlDecision>,
    slot: &'a crate::image_storage::ImageRequestSlot,
) -> impl std::future::Future<Output = Result<Response, GatewayError>> + Send + 'a {
    Box::pin(submit_inner(
        state, headers, path, body, original, decision, slot,
    ))
}
async fn submit_inner(
    state: &AppState,
    headers: &HeaderMap,
    path: &str,
    body: &Bytes,
    original: &Bytes,
    decision: Option<&GatewayControlDecision>,
    slot: &crate::image_storage::ImageRequestSlot,
) -> Result<Response, GatewayError> {
    let Some(auth) = decision.and_then(|d| d.auth_context.as_ref()) else {
        return Ok(error(
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "API Key required",
        ));
    };
    let Some(repo) = state.data.image_tasks() else {
        return Ok(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "queue_unavailable",
            "Image task database unavailable",
        ));
    };
    let ct = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json");
    let key =
        match headers.get("idempotency-key") {
            Some(v) => match v.to_str().ok().filter(|s| {
                !s.is_empty() && s.len() <= 256 && s.bytes().all(|b| b.is_ascii_graphic())
            }) {
                Some(s) => Some(s),
                None => {
                    return Ok(error(
                        StatusCode::BAD_REQUEST,
                        "invalid_idempotency_key",
                        "Idempotency-Key must contain 1-256 visible ASCII characters",
                    ))
                }
            },
            None => None,
        };
    let fingerprint = match inputs::fingerprint(original, ct, path) {
        Ok(v) => v,
        Err(e) => return Ok(error(StatusCode::BAD_REQUEST, "invalid_image_input", &e)),
    };
    // Validate the same storage-normalized request the synchronous executor sees.
    // In particular, ignored client response_format values must not cause an
    // async-only rejection when honor_response_format is disabled.
    let normalized = match inputs::validate(body, ct, path) {
        Ok(v) => v,
        Err(e) => return Ok(error(StatusCode::BAD_REQUEST, "invalid_image_input", &e)),
    };
    let config = settings(state).await?;
    let node = worker::node_id(state).await?;
    let id = format!("imgtask_{}", uuid::Uuid::new_v4().simple());
    let request = match inputs::persist(&state.image_storage.root, &id, body, ct).await {
        Ok(v) => v,
        Err(e) => {
            return Ok(error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "input_persistence_failed",
                &e,
            ))
        }
    };
    let cleanup =
        || tokio::fs::remove_dir_all(state.image_storage.root.join("task-inputs").join(&id));
    let credential = headers
        .get("authorization")
        .or_else(|| headers.get("x-api-key"))
        .and_then(|v| v.to_str().ok());
    let Some(credential) = credential else {
        let _ = cleanup().await;
        return Ok(error(
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "API Key required",
        ));
    };
    let credential = if credential.starts_with("Bearer ") {
        credential.to_owned()
    } else {
        format!("Bearer {credential}")
    };
    let Some(credential) =
        crate::handlers::shared::encrypt_system_config_secret(state, &id, &credential)
    else {
        let _ = cleanup().await;
        return Ok(error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "credential_storage_failed",
            "Unable to encrypt worker credential",
        ));
    };
    let snapshot = slot.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let model = normalized
        .requested_model
        .unwrap_or_else(|| "gpt-image-1".into());
    let task = json!({"task_id":id,"request_id":id,"api_key_id":auth.api_key_id,"user_id":auth.user_id,"node_id":node,"idempotency_key":key,"fingerprint":fingerprint,"status":"queued","stage":"queued","progress":0,"created_at":chrono::Utc::now().timestamp(),"request":request,"snapshot":snapshot,"credential":credential,"path":path,"model":model,"attempt_count":0,"switch_count":0,"billing_state":"not_charged","actual_cost":0,"error":null,"result":null});
    let admission = repo.submit(&task, i64::from(config.max_queued)).await;
    let response = match admission {
        Ok(ImageTaskAdmission::Created(task)) => accepted(&task),
        Ok(ImageTaskAdmission::Existing(task)) => {
            let _ = cleanup().await;
            accepted(&task)
        }
        Ok(ImageTaskAdmission::Conflict) => {
            let _ = cleanup().await;
            error(
                StatusCode::CONFLICT,
                "idempotency_conflict",
                "Idempotency-Key was used with a different request",
            )
        }
        Ok(ImageTaskAdmission::Full) => {
            let _ = cleanup().await;
            error(
                StatusCode::TOO_MANY_REQUESTS,
                "queue_full",
                "Image task queue is full",
            )
        }
        Err(e) => {
            let _ = cleanup().await;
            return Err(GatewayError::Internal(e.to_string()));
        }
    };
    Ok(response)
}
fn accepted(task: &Value) -> Response {
    (StatusCode::ACCEPTED,Json(json!({"task_id":task["task_id"],"status":task["status"],"progress":task["progress"],"stage":task["stage"],"created_at":task["created_at"],"poll_after_seconds":3}))).into_response()
}
pub(crate) fn admin<'a>(
    state: &'a AppState,
    method: &'a http::Method,
    path: &'a str,
    query: Option<&'a str>,
) -> impl std::future::Future<Output = Result<Option<Response>, GatewayError>> + Send + 'a {
    Box::pin(admin_inner(state, method, path, query))
}
async fn admin_inner(
    state: &AppState,
    method: &http::Method,
    path: &str,
    query: Option<&str>,
) -> Result<Option<Response>, GatewayError> {
    if method != http::Method::GET || !path.starts_with("/api/admin/image-tasks") {
        return Ok(None);
    }
    let Some(repo) = state.data.image_tasks() else {
        return Ok(Some(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "queue_unavailable",
            "Image task database unavailable",
        )));
    };
    if path.trim_end_matches('/') == "/api/admin/image-tasks" {
        let params: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(query.unwrap_or("").as_bytes())
                .into_owned()
                .collect();
        let page = params
            .get("page")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(1)
            .max(1);
        let size = params
            .get("page_size")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(20)
            .clamp(1, 100);
        let value = repo
            .list(
                params
                    .get("status")
                    .map(String::as_str)
                    .filter(|s| !s.is_empty()),
                params
                    .get("model")
                    .map(String::as_str)
                    .filter(|s| !s.is_empty()),
                params
                    .get("task_id")
                    .map(String::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty()),
                page,
                size,
            )
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?;
        return Ok(Some(Json(value).into_response()));
    }
    let id = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let Some(task) = repo
        .get(id)
        .await
        .map_err(|e| GatewayError::Internal(e.to_string()))?
    else {
        return Ok(Some(cleaned()));
    };
    Ok(Some(render(state, &task).await?))
}

/// Reuse image maintenance cadence. A grace period protects files being admitted.
pub(crate) async fn maintain_inputs(state: &AppState) -> Result<(), String> {
    let Some(repo) = state.data.image_tasks() else {
        return Ok(());
    };
    let node = worker::node_id(state).await.map_err(|e| format!("{e:?}"))?;
    let active = repo
        .active_input_ids(&node)
        .await
        .map_err(|e| e.to_string())?;
    let mut dirs = match tokio::fs::read_dir(state.image_storage.root.join("task-inputs")).await {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    while let Some(dir) = dirs.next_entry().await.map_err(|e| e.to_string())? {
        let name = dir.file_name().to_string_lossy().to_string();
        if !valid_task_id(&name) || active.contains(&name) {
            continue;
        }
        let meta = tokio::fs::symlink_metadata(dir.path())
            .await
            .map_err(|e| e.to_string())?;
        if meta.is_dir()
            && meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|d| d.as_secs() > 600)
        {
            tokio::fs::remove_dir_all(dir.path())
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
pub(crate) async fn input_bytes(root: &std::path::Path) -> u64 {
    let mut stack = vec![root.join("task-inputs")];
    let mut total = 0u64;
    while let Some(path) = stack.pop() {
        if let Ok(mut dirs) = tokio::fs::read_dir(path).await {
            while let Ok(Some(entry)) = dirs.next_entry().await {
                if let Ok(meta) = tokio::fs::symlink_metadata(entry.path()).await {
                    if meta.is_dir() {
                        stack.push(entry.path())
                    } else if meta.is_file() {
                        total = total.saturating_add(meta.len())
                    }
                }
            }
        }
    }
    total
}

fn valid_task_id(id: &str) -> bool {
    id.starts_with("imgtask_") && id.len() == 40 && id[8..].bytes().all(|b| b.is_ascii_hexdigit())
}
