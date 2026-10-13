use super::{
    config::{load_config, load_password},
    storage::SyncProgress,
};
use crate::{AppState, GatewayError};
use axum::{
    body::{Body, Bytes},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

pub(crate) async fn admin_response(
    state: &AppState,
    method: &Method,
    path: &str,
    body: Option<&Bytes>,
) -> Result<Option<Response<Body>>, GatewayError> {
    if !path.starts_with("/api/admin/system/images/") {
        return Ok(None);
    }
    let config = load_config(state).await?;
    let response = match (method.as_str(), path) {
        ("GET", "/api/admin/system/images/stats") => {
            respond(state.image_storage.stats(&config).await)
        }
        ("POST", "/api/admin/system/images/cleanup") => {
            let include_failed = if let Some(body) = body.filter(|b| !b.is_empty()) {
                let Ok(value) = serde_json::from_slice::<Value>(body) else {
                    return Ok(Some(error(StatusCode::BAD_REQUEST, "清理请求必须为 JSON")));
                };
                value
                    .get("include_failed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            } else {
                false
            };
            respond(
                state
                    .image_storage
                    .cleanup(&config, include_failed)
                    .await
                    .and_then(|run| {
                        serde_json::to_value(run).map_err(|_| "清理记录序列化失败".into())
                    }),
            )
        }
        ("POST", "/api/admin/system/images/webdav-test") => {
            if !config.stores_webdav()
                || config.webdav_url.is_empty()
                || config.webdav_public_url.is_empty()
            {
                error(
                    StatusCode::BAD_REQUEST,
                    "请先选择包含 WebDAV 的存储方式并保存配置",
                )
            } else {
                match load_password(state).await {
                    Ok(password) => Json(state.image_storage.test_webdav(&config, &password).await)
                        .into_response(),
                    Err(message) => error(StatusCode::BAD_REQUEST, &message),
                }
            }
        }
        ("POST", "/api/admin/system/images/sync") => {
            if !config.stores_webdav() {
                return Ok(Some(error(
                    StatusCode::BAD_REQUEST,
                    "请先选择包含 WebDAV 的存储方式并保存配置",
                )));
            }
            let password = match load_password(state).await {
                Ok(password) => password,
                Err(message) => return Ok(Some(error(StatusCode::BAD_REQUEST, &message))),
            };
            let permit = match state.image_storage.sync_gate.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    return Ok(Some(error(
                        StatusCode::CONFLICT,
                        "图片同步队列正在运行，请稍后重试",
                    )))
                }
            };
            let index = state
                .image_storage
                .index()
                .await
                .map_err(GatewayError::Internal)?;
            let destination = config.destination();
            let mut records: Vec<_> = index
                .read()
                .await
                .records
                .values()
                .filter(|r| {
                    r.remote_url.is_none() || r.destination.as_deref() != Some(destination.as_str())
                })
                .cloned()
                .collect();
            records.sort_by_key(|r| r.created_at);
            index.write().await.sync = SyncProgress {
                running: true,
                total: records.len() as u64,
                started_at: Some(super::storage::now()),
                ..SyncProgress::default()
            };
            let app = state.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Ok(index) = app.image_storage.index().await {
                    for record in records {
                        let result = app.image_storage.upload(&config, &password, &record).await;
                        let mut guard = index.write().await;
                        if result.is_ok() {
                            guard.sync.completed += 1;
                        } else {
                            guard.sync.failed += 1;
                            guard.sync.error = result.err();
                        }
                    }
                    let mut guard = index.write().await;
                    guard.sync.running = false;
                    guard.sync.finished_at = Some(super::storage::now());
                }
            });
            (
                StatusCode::ACCEPTED,
                Json(json!({"message":"已有本地图片补传已开始"})),
            )
                .into_response()
        }
        _ => return Ok(None),
    };
    Ok(Some(response))
}
fn error(status: StatusCode, message: &str) -> Response<Body> {
    (status, Json(json!({"detail":message}))).into_response()
}
fn respond(result: Result<Value, String>) -> Response<Body> {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(message) => error(StatusCode::BAD_REQUEST, &message),
    }
}
