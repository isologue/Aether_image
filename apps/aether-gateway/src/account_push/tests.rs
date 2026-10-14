use super::*;
use crate::{data::GatewayDataState, router::build_router_with_state};
use aether_crypto::DEVELOPMENT_ENCRYPTION_KEY;
use aether_data::repository::{
    pool_scores::InMemoryPoolMemberScoreRepository,
    provider_catalog::InMemoryProviderCatalogReadRepository,
};
use aether_data_contracts::repository::provider_catalog::{
    ProviderCatalogReadRepository, StoredProviderCatalogEndpoint, StoredProviderCatalogProvider,
};
use axum::body::Body;
use tower::ServiceExt;

const TEST_SECRET: &str = "aether_push_test_secret_1234567890";
const PROVIDER_ID: &str = "push-test-provider";
static IMPORT_TEST_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn fixture(enabled: bool) -> (AppState, Arc<InMemoryProviderCatalogReadRepository>) {
    fixture_for_provider(enabled, "chatgpt_web")
}
fn fixture_for_provider(
    enabled: bool,
    provider_type: &str,
) -> (AppState, Arc<InMemoryProviderCatalogReadRepository>) {
    let mut provider = StoredProviderCatalogProvider::new(
        PROVIDER_ID.into(),
        "Push test".into(),
        None,
        provider_type.into(),
    )
    .unwrap();
    provider.config = Some(json!({"pool_advanced": {}}));
    let endpoint = StoredProviderCatalogEndpoint::new(
        "push-test-endpoint".into(),
        PROVIDER_ID.into(),
        "openai:image".into(),
        None,
        None,
        true,
    )
    .unwrap()
    .with_transport_fields(
        "http://127.0.0.1:1".into(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let repo = Arc::new(InMemoryProviderCatalogReadRepository::seed(
        vec![provider],
        vec![endpoint],
        vec![],
    ));
    let sealing_state = AppState::new().unwrap().with_data_state_for_tests(
        GatewayDataState::disabled().with_encryption_key_for_tests(DEVELOPMENT_ENCRYPTION_KEY),
    );
    let sealed = crate::handlers::shared::encrypt_system_config_secret(
        &sealing_state,
        SECRET_KEY,
        TEST_SECRET,
    )
    .unwrap();
    let state = AppState::new().unwrap().with_data_state_for_tests(
        GatewayDataState::with_provider_catalog_repository_for_tests(repo.clone())
            .with_pool_score_repository_for_tests(Arc::new(
                InMemoryPoolMemberScoreRepository::default(),
            ))
            .with_encryption_key_for_tests(DEVELOPMENT_ENCRYPTION_KEY)
            .with_system_config_values_for_tests([
                (
                    CONFIG_KEY.into(),
                    json!({"enabled":enabled,"provider_id":PROVIDER_ID}),
                ),
                (SECRET_KEY.into(), json!(sealed)),
            ]),
    );
    let state = state
        .with_provider_oauth_token_url_for_tests("chatgpt_web", "http://127.0.0.1:1/oauth/token");
    (state, repo)
}
async fn post(
    state: AppState,
    path: &str,
    secret: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(secret) = secret {
        request = request.header(header::AUTHORIZATION, format!("Bearer {secret}"));
    }
    let response = build_router_with_state(state)
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), BODY_LIMIT).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[test]
fn normalizes_sub2api_data_envelope_and_merges_account_metadata() {
    let normalized = normalize_sub2api_payload(json!({
        "data": {
            "type": "sub2api-data",
            "version": 1,
            "proxies": [],
            "accounts": [{
                "name": "pool-account",
                "platform": "openai",
                "type": "oauth",
                "account_id": "acct-sub2api",
                "credentials": {
                    "access_token": "sub2api-access",
                    "refresh_token": "sub2api-refresh"
                },
                "extra": {"email": "pool@example.test"}
            }]
        },
        "skip_default_group_bind": true
    }))
    .unwrap();

    assert_eq!(normalized.len(), 1);
    assert_eq!(normalized[0]["access_token"], "sub2api-access");
    assert_eq!(normalized[0]["refresh_token"], "sub2api-refresh");
    assert_eq!(normalized[0]["account_id"], "acct-sub2api");
    assert_eq!(normalized[0]["email"], "pool@example.test");
    assert_eq!(normalized[0]["account_name"], "pool-account");
}

#[test]
fn rejects_sub2api_data_without_oauth_accounts() {
    for value in [
        json!({"type":"sub2api-data", "proxies":[], "accounts":[]}),
        json!({"type":"sub2api-data", "proxies":[], "accounts":[{
            "name":"api-key-account", "platform":"openai", "type":"api_key",
            "credentials":{"api_key":"not-imported"}
        }]}),
    ] {
        assert!(normalize_sub2api_payload(value).is_err());
    }
}
#[test]
fn bearer_auth_rejects_duplicates_and_whitespace() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        format!("bEaReR {TEST_SECRET}").parse().unwrap(),
    );
    assert!(valid_bearer(&headers, TEST_SECRET));
    assert!(!valid_bearer(&headers, "another-secret"));
    assert!(!valid_bearer(&headers, ""));
    headers.append(
        header::AUTHORIZATION,
        format!("Bearer {TEST_SECRET}").parse().unwrap(),
    );
    assert!(!valid_bearer(&headers, TEST_SECRET));
    headers.remove(header::AUTHORIZATION);
    headers.insert(
        header::AUTHORIZATION,
        format!("Bearer {TEST_SECRET} ").parse().unwrap(),
    );
    assert!(!valid_bearer(&headers, TEST_SECRET));
}

#[test]
fn sub2api_auth_accepts_x_api_key_and_bearer() {
    let mut headers = HeaderMap::new();
    headers.insert("x-api-key", TEST_SECRET.parse().unwrap());
    assert!(valid_sub2api_key(&headers, TEST_SECRET));
    assert!(!valid_sub2api_key(&headers, "another-secret"));
    headers.append("x-api-key", TEST_SECRET.parse().unwrap());
    assert!(!valid_sub2api_key(&headers, TEST_SECRET));

    headers.clear();
    headers.insert(
        header::AUTHORIZATION,
        format!("Bearer {TEST_SECRET}").parse().unwrap(),
    );
    assert!(valid_bearer(&headers, TEST_SECRET));
}

#[tokio::test]
async fn sub2api_get_connection_test_uses_authenticated_success_envelope() {
    let (state, _) = fixture(true);
    let response = build_router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/admin/accounts/data?include_proxies=false")
                .header("x-api-key", TEST_SECRET)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), BODY_LIMIT).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["code"], 0);
    assert_eq!(body["data"]["type"], "sub2api-data");
    assert_eq!(body["data"]["accounts"], json!([]));

    let response = build_router_with_state(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/admin/accounts/data")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
#[tokio::test]
async fn enforces_disabled_state_and_key_rotation_immediately() {
    let path = "/api/v1/admin/accounts/data";
    let (state, _) = fixture(false);
    assert_eq!(
        post(state, path, Some(TEST_SECRET), json!({})).await.0,
        StatusCode::NOT_FOUND
    );
    let (state, repo) = fixture(true);
    for secret in [None, Some("wrong")] {
        assert_eq!(
            post(state.clone(), path, secret, json!({})).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    state
        .upsert_system_config_json_value(SECRET_KEY, &json!(""), None)
        .await
        .unwrap();
    assert_eq!(
        post(state.clone(), path, Some(TEST_SECRET), json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post_sub2api(state, json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert!(repo
        .list_keys_by_provider_ids(&[PROVIDER_ID.into()])
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn removed_upload_routes_return_not_found_without_importing() {
    let (state, repo) = fixture(true);
    for path in ["/api/accounts", "/api/accounts/upload"] {
        let response = build_router_with_state(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .extension(axum::extract::ConnectInfo(
                        "127.0.0.1:12345".parse::<std::net::SocketAddr>().unwrap(),
                    ))
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_SECRET}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"tokens":["old-token"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    assert!(repo
        .list_keys_by_provider_ids(&[PROVIDER_ID.into()])
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn validates_content_type_json_and_body_size_before_importing() {
    let (state, repo) = fixture(true);
    for (content_type, body, expected) in [
        (
            "text/plain",
            "{}".to_string(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            "application/json",
            "not JSON".to_string(),
            StatusCode::BAD_REQUEST,
        ),
        (
            "application/json",
            " ".repeat(BODY_LIMIT + 1),
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
    ] {
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/admin/accounts/data")
            .header(header::AUTHORIZATION, format!("Bearer {TEST_SECRET}"))
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .unwrap();
        assert_eq!(
            build_router_with_state(state.clone())
                .oneshot(request)
                .await
                .unwrap()
                .status(),
            expected
        );
    }
    assert!(repo
        .list_keys_by_provider_ids(&[PROVIDER_ID.into()])
        .await
        .unwrap()
        .is_empty());
}
#[tokio::test]
async fn validates_push_configuration_and_masks_api_key() {
    let (state, _) = fixture(true);
    assert!(PushConfig {
        enabled: true,
        provider_id: PROVIDER_ID.into()
    }
    .validate(&state)
    .await
    .is_ok());
    assert!(PushConfig {
        enabled: true,
        provider_id: String::new()
    }
    .validate(&state)
    .await
    .is_err());
    assert!(PushConfig {
        enabled: true,
        provider_id: "missing".into()
    }
    .validate(&state)
    .await
    .is_err());
    assert!(aether_admin::system::is_sensitive_admin_system_config_key(
        SECRET_KEY
    ));
    let sealed = state
        .read_system_config_json_value_strong(SECRET_KEY)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(sealed.as_str().unwrap(), TEST_SECRET);
    let masked =
        aether_admin::system::build_admin_system_config_detail_payload(SECRET_KEY, Some(sealed))
            .unwrap();
    assert!(masked["value"].is_null());
    assert_eq!(masked["is_set"], true);
}
#[test]
fn imports_codex_refresh_tokens_without_echoing_credentials() {
    let _guard = IMPORT_TEST_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::tests::run_async_test_on_large_stack(
        "account_push_imports_codex",
        16 * 1024 * 1024,
        || async {
            let (state, repo) = fixture_for_provider(true, "codex");
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let token_server = axum::Router::new().route(
                "/oauth/token",
                axum::routing::post(|| async {
                    Json(json!({
                        "access_token":"codex-test-access",
                        "refresh_token":"codex-test-refreshed",
                        "token_type":"Bearer",
                        "expires_in":1800,
                        "email":"codex@example.test",
                        "account_id":"codex-test-account",
                        "account_user_id":"codex-test-user__codex-test-account",
                        "user_id":"codex-test-user",
                        "plan_type":"plus"
                    }))
                }),
            );
            let server = tokio::spawn(async move {
                axum::serve(listener, token_server).await.unwrap();
            });
            let state = state.with_provider_oauth_token_url_for_tests(
                "codex",
                format!("http://{address}/oauth/token"),
            );
            for (added, updated) in [(1, 0), (0, 1)] {
                let (status, result) = post_sub2api(
                    state.clone(),
                    json!({"accounts":[{"platform":"openai", "type":"oauth", "credentials":{"refresh_token":"codex-test-refresh"}}]}),
                )
                .await;
                assert_eq!(status, StatusCode::OK, "{result}");
                assert_eq!(result["data"]["account_created"], added);
                assert_eq!(result["data"]["account_updated"], updated);
                assert_eq!(result["code"], 0);
                assert!(!result.to_string().contains("codex-test-refresh"));
                let keys = repo
                    .list_keys_by_provider_ids(&[PROVIDER_ID.into()])
                    .await
                    .unwrap();
                assert_eq!(keys.len(), 1);
                assert_eq!(
                    state
                        .decrypt_provider_catalog_key_api_key(&keys[0])
                        .unwrap()
                        .unwrap(),
                    "codex-test-access"
                );
            }
            server.abort();
        },
    );
}

async fn post_sub2api(state: AppState, body: Value) -> (StatusCode, Value) {
    let response = build_router_with_state(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/admin/accounts/data")
                .header("x-api-key", TEST_SECRET)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), BODY_LIMIT).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[test]
fn sub2api_imports_multiple_batches_and_updates_without_echoing_credentials() {
    let _guard = IMPORT_TEST_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::tests::run_async_test_on_large_stack(
        "sub2api_import_batches",
        16 * 1024 * 1024,
        || async {
            let (state, repo) = fixture(true);
            let accounts: Vec<Value> = (0..101).map(|index| json!({
            "name": format!("sub2api-{index}"), "platform":"openai", "type":"oauth",
            "credentials": {"access_token":format!("sub2api-secret-{index}"), "expires_at":2100000000u64,
                "account_id":format!("acct-{index}"), "account_user_id":format!("user-{index}__acct-{index}"),
                "user_id":format!("user-{index}"), "plan_type":"plus"},
            "extra":{"email":format!("pool-{index}@example.test")}
        })).collect();
            let payload = json!({"data":{"type":"sub2api-data", "version":1, "proxies":[], "accounts":accounts}});
            for (created, updated) in [(101, 0), (0, 101)] {
                let (status, result) = post_sub2api(state.clone(), payload.clone()).await;
                assert_eq!(status, StatusCode::OK, "{result}");
                assert_eq!(result["code"], 0);
                assert_eq!(result["data"]["account_created"], created);
                assert_eq!(result["data"]["account_updated"], updated);
                assert_eq!(result["data"]["account_failed"], 0);
                assert_eq!(result["data"]["errors"], json!([]));
                assert!(!result.to_string().contains("sub2api-secret"));
                assert_eq!(
                    repo.list_keys_by_provider_ids(&[PROVIDER_ID.into()])
                        .await
                        .unwrap()
                        .len(),
                    101
                );
            }
        },
    );
}

#[test]
fn sub2api_import_reports_partial_failures_with_native_error_fields() {
    let _guard = IMPORT_TEST_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::tests::run_async_test_on_large_stack(
        "sub2api_partial_failure",
        16 * 1024 * 1024,
        || async {
            let (state, _) = fixture(true);
            let good = json!({"platform":"openai", "type":"oauth", "credentials":{
                "access_token":"partial-good-token", "expires_at":2100000000u64,
                "email":"partial@example.test", "account_id":"partial-account",
                "account_user_id":"partial-user__partial-account", "user_id":"partial-user", "plan_type":"plus"
            }});
            let bad = json!({"platform":"openai", "type":"oauth", "credentials":{"refresh_token":"partial-bad-token"}});
            let (status, result) =
                post_sub2api(state, json!({"data":{"accounts":[good,bad],"proxies":[]}})).await;
            assert_eq!(status, StatusCode::OK, "{result}");
            assert_eq!(result["code"], 0);
            assert_eq!(result["data"]["account_created"], 1);
            assert_eq!(result["data"]["account_failed"], 1);
            assert_eq!(result["data"]["errors"][0]["kind"], "account");
            assert_eq!(result["data"]["errors"][0]["index"], 1);
            assert!(result["data"]["errors"][0]["message"]
                .as_str()
                .is_some_and(|text| !text.is_empty()));
            assert!(!result.to_string().contains("partial-bad-token"));
        },
    );
}

#[tokio::test]
async fn sub2api_disabled_and_invalid_requests_return_native_errors() {
    let (state, _) = fixture(false);
    let (status, result) = post_sub2api(state, json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(result["code"], 404);
    for (secret, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some("wrong"), StatusCode::UNAUTHORIZED),
        (Some(TEST_SECRET), StatusCode::BAD_REQUEST),
    ] {
        let (state, _) = fixture(true);
        let (status, result) = post(
            state,
            "/api/v1/admin/accounts/data",
            secret,
            json!({"data":{"accounts":[]}}),
        )
        .await;
        assert_eq!(status, expected);
        assert_eq!(result["code"], expected.as_u16());
        assert!(result["message"].is_string());
    }
}

#[test]
fn sub2api_rejects_invalid_headers_and_agent_identity_before_import() {
    let account =
        json!({"platform":"openai", "type":"oauth", "credentials":{"access_token":"no-echo"}});
    for header in [
        json!({"type":"unrelated"}),
        json!({"version":2}),
        json!({"version":"1"}),
    ] {
        let mut payload = header.as_object().unwrap().clone();
        payload.insert("accounts".into(), json!([account.clone()]));
        assert!(normalize_sub2api_payload(Value::Object(payload)).is_err());
    }
    for marker in ["sub2api-data", "sub2api-bundle"] {
        assert!(normalize_sub2api_payload(
            json!({"type":marker,"version":1,"accounts":[account.clone()]})
        )
        .is_ok());
    }
    let mut marked = account;
    marked["extra"] = json!({"auth_mode":"agentIdentity", "agent_private_key":"no-echo", "agent_runtime_id":"runtime"});
    assert!(normalize_sub2api_payload(json!({"accounts":[marked]}))
        .unwrap_err()
        .contains("Agent Identity"));
    assert!(normalize_sub2api_payload(
        json!({"accounts":[{"type":"oauth","credentials":{"access_token":"no-echo"}}]})
    )
    .is_err());
}

#[test]
fn sub2api_rejects_concurrent_imports() {
    let _guard = IMPORT_TEST_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::tests::run_async_test_on_large_stack(
        "sub2api_import_gate",
        16 * 1024 * 1024,
        || async {
            let gate = IMPORT_GATE
                .get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone();
            let _import_guard = gate.lock_owned().await;
            let (state, _) = fixture(true);
            let (status, result) = post_sub2api(
                state.clone(),
                json!({"data":{"accounts":[{
                    "platform":"openai", "type":"oauth", "credentials":{"access_token":"busy-token"}
                }]}}),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(result["code"], 409);
        },
    );
}

#[test]
fn sub2api_reports_all_failed_accounts_without_claiming_success() {
    let _guard = IMPORT_TEST_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::tests::run_async_test_on_large_stack("sub2api_all_failed", 16 * 1024 * 1024, || async {
        let (state, _) = fixture(true);
        let (status, result) = post_sub2api(state, json!({"data":{"accounts":[{
            "platform":"openai", "type":"oauth", "credentials":{"refresh_token":"all-failed-token"}
        }]}})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");
        assert_ne!(result["code"], 0);
        assert_eq!(result["data"]["account_created"], 0);
        assert_eq!(result["data"]["account_failed"], 1);
        assert_eq!(result["data"]["errors"][0]["kind"], "account");
        assert!(!result.to_string().contains("all-failed-token"));
    });
}

#[test]
fn normalizes_aliases_deduplicates_and_strips_transport_overrides() {
    let normalized = normalize_accounts(vec![
        json!({"access_token":"", "accessToken":" test-token ", "accountId":"acct-test", "accountUserId":"user-test__acct-test", "userId":"user-test", "proxy":"http://invalid", "headers":{"Authorization":"secret"}}),
        json!({"access_token":"test-token"}),
        json!({"refreshToken":"refresh-test"}),
    ]).unwrap();
    assert_eq!(normalized.len(), 2);
    assert_eq!(normalized[0]["access_token"], "test-token");
    assert_eq!(normalized[0]["accountId"], "acct-test");
    assert_eq!(normalized[0]["accountUserId"], "user-test__acct-test");
    assert!(normalized[0].get("accessToken").is_none());
    assert!(normalized[0].get("proxy").is_none());
    assert!(normalized[0].get("headers").is_none());
    assert_eq!(normalized[1]["refresh_token"], "refresh-test");
    for account in [json!({}), json!({"access_token":42}), json!("secret-token")] {
        let error = normalize_accounts(vec![account]).unwrap_err();
        assert!(!error.contains("secret-token"));
    }
    assert!(normalize_accounts(vec![json!({"access_token":"test", "nested":{"auth_mode":"agentIdentity", "agent_private_key":"test", "agent_runtime_id":"test"}})]).is_err());
}
