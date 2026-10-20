//! Image persistence is owned by the gateway, outside pure protocol converters and billing.
mod admin;
mod config;
mod response;
mod resize;
mod storage;
#[cfg(test)]
mod tests;
mod webdav;

pub(crate) use admin::admin_response;
pub(crate) use config::{parse_config, ImageConfig, CONFIG_KEY, PASSWORD_KEY};
pub(crate) use response::{is_image_path, transform_response, ImageRequestSlot};
pub(crate) use storage::ImageStorage;

pub(crate) fn spawn_worker(state: crate::AppState) -> tokio::task::JoinHandle<()> {
    // Per-node storage: a distributed singleton would leave other nodes' files unmaintained.
    tokio::spawn(async move {
        loop {
            if let Err(error) = maintenance_tick(&state).await {
                tracing::warn!(error = %error, "image storage maintenance failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    })
}
async fn maintenance_tick(state: &crate::AppState) -> Result<(), String> {
    let config = config::load_config(state)
        .await
        .map_err(|_| "读取图片维护配置失败".to_string())?;
    let storage = &state.image_storage;
    let index = storage.index().await?;
    if config.cleanup_enabled {
        let last = index
            .read()
            .await
            .last_cleanup
            .as_ref()
            .map(|r| r.started_at)
            .unwrap_or(storage.started_at);
        if storage::now() >= last + (config.cleanup_interval_hours * 3600) as i64 {
            storage.cleanup(&config, true).await?;
        }
    }
    if config.stores_webdav() {
        // Skip the automatic queue while a manual catch-up batch owns it.
        let Ok(_batch) = storage.sync_gate.clone().try_acquire_owned() else {
            return Ok(());
        };
        let password = config::load_password(state).await?;
        let destination = config.destination();
        let mut records: Vec<_> = index
            .read()
            .await
            .records
            .values()
            .filter(|r| {
                let same = r.destination.as_deref() == Some(destination.as_str());
                (!same || r.remote_url.is_none())
                    && (!same || (r.attempts < 3 && r.next_retry_at <= storage::now()))
            })
            .cloned()
            .collect();
        records.sort_by_key(|r| r.created_at);
        for record in records.into_iter().take(20) {
            if let Err(error) = storage.upload(&config, &password, &record).await {
                tracing::warn!(image_id = %record.id, error = %error, "image WebDAV background upload failed");
            }
        }
    }
    Ok(())
}

/// Public images use unguessable names; metadata and directory listings are never served.
pub(crate) async fn serve_image(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    axum::extract::Path(path): axum::extract::Path<String>,
    request: axum::extract::Request,
) -> axum::response::Response {
    use axum::{
        http::{HeaderValue, StatusCode},
        response::IntoResponse,
    };
    use futures_util::StreamExt;
    use tower::ServiceExt;
    if !storage::valid_image_path(&path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(id) = path.rsplit('/').next().and_then(|s| s.split('.').next()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let _active = state.image_storage.reader(id);
    let Ok(index) = state.image_storage.index().await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(record) = index
        .read()
        .await
        .records
        .get(id)
        .filter(|r| r.path == path)
        .cloned()
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let file = state.image_storage.root.join("objects").join(path);
    let Ok(canonical) = tokio::fs::canonicalize(&file).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(root) = tokio::fs::canonicalize(state.image_storage.root.join("objects")).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !canonical.starts_with(root) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let response = tower_http::services::ServeFile::new(canonical)
        .oneshot(request)
        .await
        .unwrap_or_else(|never| match never {});
    let mut response = response.into_response();
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_str(&record.content_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static("public, max-age=60"),
    );
    let (parts, body) = response.into_parts();
    let body = axum::body::Body::from_stream(body.into_data_stream().map(move |chunk| {
        let _ = &_active;
        chunk
    }));
    axum::response::Response::from_parts(parts, body)
}
