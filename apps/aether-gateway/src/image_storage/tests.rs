use super::{config::*, response::*, storage::*};
use axum::{
    body::{to_bytes, Body, Bytes},
    http::{HeaderMap, Request, Response, StatusCode},
    Router,
};
use base64::Engine;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aD1sAAAAASUVORK5CYII=";
fn png() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(PNG)
        .unwrap()
}
struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("aether-images-test-{}", uuid::Uuid::new_v4())))
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn app(root: &PathBuf, config: &ImageConfig) -> crate::AppState {
    let mut state = crate::AppState::new().unwrap().with_data_state_for_tests(
        crate::data::GatewayDataState::disabled().with_system_config_values_for_tests([(
            CONFIG_KEY.into(),
            serde_json::to_value(config).unwrap(),
        )]),
    );
    state.image_storage = Arc::new(ImageStorage::new(root.clone()).unwrap());
    state
}
fn request_config(config: ImageConfig, use_url: bool) -> ImageRequest {
    ImageRequest {
        config,
        use_url,
        public_base: Some("https://images.example.test".into()),
    }
}

#[test]
fn format_override_is_opt_in_and_defaults_are_valid() {
    let mut c = ImageConfig::default();
    c.validate().unwrap();
    assert!(!c.honor_response_format);
    assert!(!c.use_url(Some("url")).unwrap());
    assert!(!c.use_url(Some("invalid")).unwrap());
    c.return_format = "url".into();
    assert!(c.use_url(Some("b64_json")).unwrap());
    c.honor_response_format = true;
    assert!(!c.use_url(Some("b64_json")).unwrap());
    assert!(c.use_url(None).unwrap());
    assert!(c.use_url(Some("invalid")).is_err());
    c.storage_mode = "local_webdav".into();
    c.webdav_url = "https://dav.example.test".into();
    c.webdav_public_url = "https://images.example.test".into();
    for invalid in ["../images", "/images", "a/%2f", "a\\b"] {
        c.webdav_root = invalid.into();
        assert!(c.validate().is_err());
    }
    assert!(validate_http_url("https://user:pass@example.test/").is_err());
    assert!(validate_http_url("https://example.test/?key=secret").is_err());
    assert!(valid_image_path(
        "2026/10/13/0123456789abcdef0123456789abcdef.png"
    ));
    assert!(!valid_image_path("../metadata/secret.json"));
}

#[tokio::test]
async fn request_capture_ignores_field_unless_enabled_and_preserves_multipart_bytes() {
    let temp = TempDir::new();
    let c = ImageConfig::default();
    let state = app(&temp.0, &c);
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());
    headers.insert("host", "localhost:8084".parse().unwrap());
    let slot = ImageRequestSlot::default();
    let normalized = slot
        .capture(
            &state,
            &headers,
            &Bytes::from_static(br#"{"prompt":"test","response_format":123}"#),
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&normalized).unwrap(),
        json!({"prompt":"test"})
    );
    assert!(!slot.0.lock().unwrap().as_ref().unwrap().use_url);
    let mut enabled = c.clone();
    enabled.honor_response_format = true;
    let state = app(&temp.0, &enabled);
    assert!(slot
        .capture(
            &state,
            &headers,
            &Bytes::from_static(br#"{"response_format":123}"#),
            false
        )
        .await
        .is_err());
    slot.capture(
        &state,
        &headers,
        &Bytes::from_static(br#"{"response_format":"url"}"#),
        false,
    )
    .await
    .unwrap();
    assert!(slot.0.lock().unwrap().as_ref().unwrap().use_url);
    headers.insert(
        "content-type",
        "multipart/form-data; boundary=fixture".parse().unwrap(),
    );
    let mut body = b"--fixture\r\nContent-Disposition: form-data; name=\"image\"; filename=\"x.png\"\r\nContent-Type: image/png\r\n\r\n".to_vec();
    body.extend_from_slice(&[0, 255, 128, 2]);
    body.extend_from_slice(b"\r\n--fixture\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\nurl\r\n--fixture--\r\n");
    let normalized = slot
        .capture(&state, &headers, &Bytes::from(body.clone()), false)
        .await
        .unwrap();
    assert!(normalized.windows(4).any(|w| w == [0, 255, 128, 2]));
    assert_eq!(
        multipart_response_format(&normalized, "multipart/form-data; boundary=fixture")
            .unwrap()
            .2,
        "b64_json"
    );
    assert!(slot.0.lock().unwrap().as_ref().unwrap().use_url);
}

#[tokio::test]
async fn json_to_local_url_is_downloadable_with_head_range_and_restart() {
    let temp = TempDir::new();
    let c = ImageConfig::default();
    let state = app(&temp.0, &c);
    let mut value = json!({"created":123,"data":[{"b64_json":PNG,"revised_prompt":"preserved"}],"usage":{"total_tokens":7}});
    transform_json(&state, &request_config(c.clone(), true), &mut value)
        .await
        .unwrap();
    assert_eq!(value["usage"]["total_tokens"], 7);
    assert_eq!(value["data"][0]["revised_prompt"], "preserved");
    assert!(value["data"][0].get("b64_json").is_none());
    let url = url::Url::parse(value["data"][0]["url"].as_str().unwrap()).unwrap();
    assert_eq!(url.host_str(), Some("images.example.test"));
    let restarted = app(&temp.0, &c);
    assert_eq!(restarted.image_storage.stats(&c).await.unwrap()["count"], 1);
    let router = Router::new()
        .route(
            "/media/images/{*image_path}",
            axum::routing::get(super::serve_image),
        )
        .with_state(restarted);
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(url.path())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
        png()
    );
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri(url.path())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(to_bytes(response.into_body(), 1024)
        .await
        .unwrap()
        .is_empty());
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(url.path())
                .header("range", "bytes=0-7")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
        &png()[..8]
    );
    let response = router
        .oneshot(
            Request::builder()
                .uri("/media/images/../metadata/x.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn base64_response_survives_unwritable_storage() {
    let temp = TempDir::new();
    std::fs::write(&temp.0, b"not a directory").unwrap();
    let c = ImageConfig::default();
    let state = app(&temp.0, &c);
    let mut value = json!({"data":[{"b64_json":PNG}]});
    transform_json(&state, &request_config(c.clone(), false), &mut value)
        .await
        .unwrap();
    assert_eq!(value["data"][0]["b64_json"], PNG);
    assert!(transform_json(&state, &request_config(c, true), &mut value)
        .await
        .is_err());
    std::fs::remove_file(&temp.0).unwrap();
}

#[tokio::test]
async fn sse_handles_split_chunks_preserves_partial_and_converts_completed() {
    let temp = TempDir::new();
    let state = app(&temp.0, &ImageConfig::default());
    let slot = ImageRequestSlot::default();
    *slot.0.lock().unwrap() = Some(request_config(ImageConfig::default(), true));
    let wire = format!("event: image_generation.partial_image\r\ndata: {{\"type\":\"image_generation.partial_image\",\"b64_json\":\"partial\"}}\r\n\r\nevent: image_generation.completed\r\ndata: {{\"type\":\"image_generation.completed\",\"b64_json\":\"{PNG}\",\"usage\":{{\"total_tokens\":3}}}}\r\n\r\ndata: [DONE]\n\n");
    let chunks: Vec<Result<Bytes, std::io::Error>> = wire
        .as_bytes()
        .chunks(7)
        .map(|b| Ok(Bytes::copy_from_slice(b)))
        .collect();
    let response = Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(futures_util::stream::iter(chunks)))
        .unwrap();
    let response = transform_response(state, slot, response).await.unwrap();
    let wire =
        String::from_utf8(to_bytes(response.into_body(), 8192).await.unwrap().to_vec()).unwrap();
    assert!(wire.contains("\"b64_json\":\"partial\""));
    assert!(wire.contains("https://images.example.test/media/images/"));
    assert!(wire.contains("\"total_tokens\":3"));
    assert!(wire.contains("data: [DONE]"));
    let completed = wire
        .lines()
        .filter_map(|s| s.strip_prefix("data: "))
        .find_map(|s| {
            serde_json::from_str::<Value>(s)
                .ok()
                .filter(|v| v["type"] == "image_generation.completed")
        })
        .unwrap();
    assert!(completed.get("b64_json").is_none());
}

#[tokio::test]
async fn cleanup_preserves_pending_busy_new_images_and_remote_files() {
    let temp = TempDir::new();
    let storage = ImageStorage::new(temp.0.clone()).unwrap();
    let mut config = ImageConfig::default();
    config.storage_mode = "local_webdav".into();
    config.retention_hours = 1;
    let pending = storage.save(&png()).await.unwrap();
    let synced = storage.save(&png()).await.unwrap();
    let busy = storage.save(&png()).await.unwrap();
    let fresh = storage.save(&png()).await.unwrap();
    let index = storage.index().await.unwrap();
    for id in [&pending.id, &synced.id, &busy.id] {
        let mut record = index.read().await.records[id].clone();
        record.created_at = now() - 7200;
        if id == &synced.id {
            record.remote_url = Some("https://example.test/remote.png".into());
            record.destination = Some(config.destination());
        }
        storage.persist(&record).await.unwrap();
        index.write().await.records.insert(id.clone(), record);
    }
    let guard = storage.reader(&busy.id);
    let run = storage.cleanup(&config, false).await.unwrap();
    assert_eq!(run.deleted, 1);
    assert_eq!(run.skipped_pending, 1);
    assert!(index.read().await.records.contains_key(&fresh.id));
    assert!(!index.read().await.records.contains_key(&synced.id));
    let run = storage.cleanup(&config, true).await.unwrap();
    assert_eq!(run.deleted, 1);
    drop(guard);
    assert_eq!(storage.cleanup(&config, true).await.unwrap().deleted, 1);
    assert_eq!(storage.stats(&config).await.unwrap()["count"], 1);
}

#[derive(Clone, Default)]
struct DavFixture {
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    public_auth: Arc<Mutex<bool>>,
    delete_count: Arc<Mutex<u32>>,
    bad_public: bool,
    expected_auth: Arc<Mutex<String>>,
    public_gate: Arc<Mutex<Option<Arc<tokio::sync::Semaphore>>>>,
}
async fn dav_handler(
    axum::extract::State(state): axum::extract::State<DavFixture>,
    request: Request<Body>,
) -> Response<Body> {
    let path = request.uri().path().to_string();
    let method = request.method().as_str().to_string();
    if path.starts_with("/public/") {
        let gate = state.public_gate.lock().unwrap().clone();
        let _permit = if let Some(gate) = &gate {
            Some(gate.acquire().await.unwrap())
        } else {
            None
        };
        if request.headers().contains_key("authorization") {
            *state.public_auth.lock().unwrap() = true;
        }
        let key = path.replacen("/public/", "/dav/aether/images/", 1);
        let bytes = state.files.lock().unwrap().get(&key).cloned();
        return match bytes {
            Some(mut b) => {
                if state.bad_public {
                    b.push(0);
                }
                Response::builder()
                    .header("content-type", "image/png")
                    .body(Body::from(b))
                    .unwrap()
            }
            None => Response::builder().status(404).body(Body::empty()).unwrap(),
        };
    }
    if request
        .headers()
        .get("authorization")
        .and_then(|s| s.to_str().ok())
        != Some(state.expected_auth.lock().unwrap().as_str())
    {
        return Response::builder().status(401).body(Body::empty()).unwrap();
    }
    let status = match method.as_str() {
        "MKCOL" => 201,
        "PUT" => {
            let bytes = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
            state.files.lock().unwrap().insert(path, bytes.to_vec());
            201
        }
        "DELETE" => {
            state.files.lock().unwrap().remove(&path);
            *state.delete_count.lock().unwrap() += 1;
            204
        }
        _ => 405,
    };
    Response::builder()
        .status(status)
        .body(Body::empty())
        .unwrap()
}
async fn dav_server(bad_public: bool) -> (ImageConfig, DavFixture, tokio::task::JoinHandle<()>) {
    let fixture = DavFixture {
        bad_public,
        expected_auth: Arc::new(Mutex::new("Basic YWRtaW46cGFzcw==".into())),
        ..DavFixture::default()
    };
    let router = Router::new()
        .fallback(dav_handler)
        .with_state(fixture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let config = ImageConfig {
        storage_mode: "local_webdav".into(),
        webdav_url: format!("http://{address}/dav"),
        webdav_public_url: format!("http://{address}/public"),
        webdav_username: "admin".into(),
        ..ImageConfig::default()
    };
    (config, fixture, handle)
}

#[tokio::test]
async fn webdav_upload_verifies_anonymous_public_content_and_cleans_test_file() {
    let temp = TempDir::new();
    let storage = ImageStorage::new(temp.0.clone()).unwrap();
    let (config, fixture, server) = dav_server(false).await;
    let record = storage.save(&png()).await.unwrap();
    let public = storage.upload(&config, "pass", &record).await.unwrap();
    assert!(public.starts_with(&config.webdav_public_url));
    assert_eq!(storage.download(&public).await.unwrap(), png());
    assert!(!*fixture.public_auth.lock().unwrap());
    assert_eq!(storage.stats(&config).await.unwrap()["synced"], 1);
    // A stale queue snapshot must reuse persisted success instead of overwriting it.
    assert_eq!(
        storage.upload(&config, "pass", &record).await.unwrap(),
        public
    );
    assert_eq!(
        storage.index().await.unwrap().read().await.records[&record.id].attempts,
        1
    );
    let result = storage.test_webdav(&config, "pass").await;
    assert_eq!(result["success"], true);
    assert_eq!(*fixture.delete_count.lock().unwrap(), 1);
    assert_eq!(fixture.files.lock().unwrap().len(), 1);
    assert_eq!(
        storage.test_webdav(&config, "wrong").await["upload"]["success"],
        false
    );
    server.abort();
}

#[tokio::test]
async fn webdav_mapping_failure_is_recorded_and_url_falls_back_to_local() {
    let temp = TempDir::new();
    let (config, fixture, server) = dav_server(true).await;
    let state = app(&temp.0, &config);
    // Empty password deliberately exercises failed authentication and local fallback.
    let mut value = json!({"data":[{"b64_json":PNG}]});
    transform_json(&state, &request_config(config.clone(), true), &mut value)
        .await
        .unwrap();
    assert!(value["data"][0]["url"]
        .as_str()
        .unwrap()
        .starts_with("https://images.example.test/"));
    assert_eq!(
        state.image_storage.stats(&config).await.unwrap()["failed"],
        1
    );
    let record = state.image_storage.save(&png()).await.unwrap();
    let failure = state
        .image_storage
        .upload(&config, "pass", &record)
        .await
        .unwrap_err();
    assert!(failure.contains("同一图片"));
    assert!(
        state
            .image_storage
            .index()
            .await
            .unwrap()
            .read()
            .await
            .records[&record.id]
            .next_retry_at
            > now()
    );
    let mut changed = config.clone();
    changed.webdav_root = "another/destination".into();
    let stats = state.image_storage.stats(&changed).await.unwrap();
    assert_eq!(stats["failed"], 0);
    assert_eq!(stats["pending"], 2);
    assert_eq!(stats["failure_details"], json!([]));
    let result = state.image_storage.test_webdav(&config, "pass").await;
    assert_eq!(result["upload"]["success"], true);
    assert_eq!(result["public_access"]["success"], false);
    assert_eq!(result["delete"]["success"], true);
    assert_eq!(*fixture.delete_count.lock().unwrap(), 1);
    server.abort();
}

#[test]
fn webdav_secret_is_sensitive_and_never_returned_to_settings() {
    let payload = aether_admin::system::build_admin_system_config_detail_payload(
        PASSWORD_KEY,
        Some(json!("encrypted-secret")),
    )
    .unwrap();
    assert_eq!(payload["value"], Value::Null);
    assert_eq!(payload["is_set"], true);
    let empty =
        aether_admin::system::build_admin_system_config_detail_payload(PASSWORD_KEY, None).unwrap();
    assert_eq!(empty["is_set"], false);
    let pure = aether_admin::system::admin_system_config_default_value(CONFIG_KEY).unwrap();
    assert_eq!(pure, serde_json::to_value(ImageConfig::default()).unwrap());
}

#[tokio::test]
async fn json_http_response_updates_headers_and_preserves_usage() {
    let temp = TempDir::new();
    let config = ImageConfig::default();
    let state = app(&temp.0, &config);
    let slot = ImageRequestSlot::default();
    *slot.0.lock().unwrap() = Some(request_config(config, true));
    let response = Response::builder()
        .header("content-type", "application/json")
        .header("content-length", "1")
        .header("etag", "provider-etag")
        .body(Body::from(
            json!({"data":[{"b64_json":PNG}],"usage":{"total_tokens":12}}).to_string(),
        ))
        .unwrap();
    let response = transform_response(state.clone(), slot, response)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("content-length"));
    assert!(!response.headers().contains_key("etag"));
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert!(value["data"][0]["url"]
        .as_str()
        .unwrap()
        .contains("/media/images/"));
    assert_eq!(value["usage"]["total_tokens"], 12);
    assert_eq!(
        state
            .image_storage
            .stats(&ImageConfig::default())
            .await
            .unwrap()["count"],
        1
    );
}

#[tokio::test]
async fn maintenance_waits_for_interval_and_admin_actions_validate_requests() {
    let temp = TempDir::new();
    let config = ImageConfig {
        cleanup_enabled: true,
        retention_hours: 1,
        cleanup_interval_hours: 1,
        ..ImageConfig::default()
    };
    let state = app(&temp.0, &config);
    let mut record = state.image_storage.save(&png()).await.unwrap();
    record.created_at = now() - 7200;
    state.image_storage.persist(&record).await.unwrap();
    let index = state.image_storage.index().await.unwrap();
    index
        .write()
        .await
        .records
        .insert(record.id.clone(), record);
    super::maintenance_tick(&state).await.unwrap();
    assert_eq!(index.read().await.records.len(), 1);
    index.write().await.last_cleanup = Some(CleanupRun {
        started_at: now() - 7200,
        ..CleanupRun::default()
    });
    super::maintenance_tick(&state).await.unwrap();
    assert_eq!(index.read().await.records.len(), 0);
    let run_at = index.read().await.last_cleanup.as_ref().unwrap().started_at;
    super::maintenance_tick(&state).await.unwrap();
    assert_eq!(
        index.read().await.last_cleanup.as_ref().unwrap().started_at,
        run_at
    );
    let response = super::admin_response(
        &state,
        &axum::http::Method::GET,
        "/api/admin/system/images/stats",
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = super::admin_response(
        &state,
        &axum::http::Method::POST,
        "/api/admin/system/images/sync",
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = super::admin_response(
        &state,
        &axum::http::Method::POST,
        "/api/admin/system/images/cleanup",
        Some(&Bytes::from_static(b"invalid")),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn storage_modes_migrate_legacy_settings_and_validate_only_active_destinations() {
    let local = parse_config(json!({"webdav_enabled":false})).unwrap();
    assert_eq!(local.storage_mode, "local");
    assert!(local.stores_local());
    assert!(!local.stores_webdav());
    let both = parse_config(json!({"webdav_enabled":true})).unwrap();
    assert_eq!(both.storage_mode, "local_webdav");
    assert!(both.stores_local() && both.stores_webdav());
    assert!(serde_json::to_value(both)
        .unwrap()
        .get("webdav_enabled")
        .is_none());
    assert_eq!(
        parse_config(json!({"storage_mode":"webdav","webdav_enabled":true}))
            .unwrap()
            .storage_mode,
        "webdav"
    );
    assert!(parse_config(json!({"webdav_enabled":"yes"})).is_err());
    let mut config = ImageConfig::default();
    config.storage_mode = "unknown".into();
    assert!(config.validate().is_err());
    config.storage_mode = "local".into();
    config.webdav_url = "unfinished destination".into();
    config.webdav_root = "../unfinished".into();
    config.validate().unwrap();
    config.storage_mode = "webdav".into();
    assert!(config.validate().is_err());
}

#[tokio::test]
async fn webdav_only_url_waits_for_validation_and_never_creates_local_images() {
    let temp = TempDir::new();
    let (mut config, fixture, server) = dav_server(false).await;
    config.storage_mode = "webdav".into();
    *fixture.expected_auth.lock().unwrap() = "Basic YWRtaW46".into();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *fixture.public_gate.lock().unwrap() = Some(gate.clone());
    let state = app(&temp.0, &config);
    let task_state = state.clone();
    let task_config = config.clone();
    let request = tokio::spawn(async move {
        let mut value = json!({"data":[{"b64_json":PNG}]});
        transform_json(&task_state, &request_config(task_config, true), &mut value)
            .await
            .unwrap();
        value
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while fixture.files.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !request.is_finished(),
        "URL must wait for public download verification"
    );
    assert!(
        !temp.0.exists(),
        "WebDAV-only must not write image or metadata files"
    );
    gate.add_permits(1);
    let value = request.await.unwrap();
    let url = value["data"][0]["url"].as_str().unwrap();
    assert!(url.starts_with(&config.webdav_public_url));
    assert!(value["data"][0].get("b64_json").is_none());
    assert_eq!(state.image_storage.download(url).await.unwrap(), png());
    assert_eq!(fixture.files.lock().unwrap().len(), 1);
    assert!(!temp.0.exists());
    server.abort();
}

#[tokio::test]
async fn webdav_only_failure_returns_storage_error_without_local_fallback() {
    let temp = TempDir::new();
    let (mut config, fixture, server) = dav_server(true).await;
    config.storage_mode = "webdav".into();
    *fixture.expected_auth.lock().unwrap() = "Basic YWRtaW46".into();
    let state = app(&temp.0, &config);
    let mut value = json!({"data":[{"b64_json":PNG}]});
    let error = transform_json(&state, &request_config(config, true), &mut value)
        .await
        .unwrap_err();
    assert!(error.contains("同一图片"));
    assert!(value["data"][0].get("url").is_none());
    assert!(!temp.0.exists());
    server.abort();
}

#[tokio::test]
async fn webdav_only_base64_returns_before_upload_finishes_without_local_copy() {
    let temp = TempDir::new();
    let (mut config, fixture, server) = dav_server(false).await;
    config.storage_mode = "webdav".into();
    *fixture.expected_auth.lock().unwrap() = "Basic YWRtaW46".into();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *fixture.public_gate.lock().unwrap() = Some(gate.clone());
    let state = app(&temp.0, &config);
    let mut value = json!({"data":[{"b64_json":PNG}]});
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        transform_json(&state, &request_config(config.clone(), false), &mut value),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(value["data"][0]["b64_json"], PNG);
    assert!(value["data"][0].get("url").is_none());
    assert!(!temp.0.exists());
    gate.add_permits(1);
    // All upload slots become available only after the background upload and verification finish.
    let _idle = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        state
            .image_storage
            .upload_gate
            .clone()
            .acquire_many_owned(4),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(fixture.files.lock().unwrap().len(), 1);
    assert!(!temp.0.exists());
    server.abort();
}

#[tokio::test]
async fn automatic_cleanup_deletes_expired_pending_and_failed_but_preserves_active_and_fresh() {
    let temp = TempDir::new();
    let (mut config, fixture, server) = dav_server(false).await;
    config.cleanup_enabled = true;
    config.retention_hours = 1;
    config.cleanup_interval_hours = 1;
    *fixture.expected_auth.lock().unwrap() = "Basic YWRtaW46".into();
    let state = app(&temp.0, &config);
    let storage = &state.image_storage;
    let index = storage.index().await.unwrap();
    let mut pending = storage.save(&png()).await.unwrap();
    let mut failed = storage.save(&png()).await.unwrap();
    let mut busy = storage.save(&png()).await.unwrap();
    let fresh = storage.save(&png()).await.unwrap();
    pending.created_at = now() - 7200;
    failed.created_at = now() - 7200;
    failed.destination = Some(config.destination());
    failed.last_error = Some("upload failed".into());
    busy.created_at = now() - 7200;
    for record in [&pending, &failed, &busy] {
        storage.persist(record).await.unwrap();
        index
            .write()
            .await
            .records
            .insert(record.id.clone(), record.clone());
    }
    index.write().await.last_cleanup = Some(CleanupRun {
        started_at: now() - 7200,
        ..CleanupRun::default()
    });
    let active = storage.claim(&busy.id).unwrap();
    super::maintenance_tick(&state).await.unwrap();
    let guard = index.read().await;
    assert!(!guard.records.contains_key(&pending.id));
    assert!(!guard.records.contains_key(&failed.id));
    assert!(guard.records.contains_key(&busy.id));
    assert!(guard.records.contains_key(&fresh.id));
    assert_eq!(guard.last_cleanup.as_ref().unwrap().deleted, 2);
    assert_eq!(guard.last_cleanup.as_ref().unwrap().skipped_pending, 0);
    drop(guard);
    assert!(!storage.root.join("objects").join(&pending.path).exists());
    assert!(!storage.root.join("objects").join(&failed.path).exists());
    drop(active);
    assert_eq!(storage.cleanup(&config, true).await.unwrap().deleted, 1);
    assert_eq!(
        fixture.files.lock().unwrap().len(),
        1,
        "remote file remains after local cleanup"
    );
    server.abort();
}
