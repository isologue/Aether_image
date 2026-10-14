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
fn normalizes_compatibility_payload_and_strips_transport_overrides() {
    let normalized: Value = serde_json::from_str(&normalize_payload(json!({
        "accounts":[{"accessToken":"test-token", "accountId":"acct-test", "accountUserId":"user-test__acct-test", "userId":"user-test", "email":"pool@example.test", "proxy":"http://invalid", "headers":{"Authorization":"secret"}}],
        "tokens":["test-token", "another-token"], "refresh":false, "return_items":true,
    })).unwrap()).unwrap();
    assert_eq!(normalized.as_array().unwrap().len(), 2);
    assert_eq!(normalized[0]["accountUserId"], "user-test__acct-test");
    assert_eq!(normalized[0]["userId"], "user-test");
    assert_eq!(normalized[0]["access_token"], "test-token");
    assert!(normalized[0].get("proxy").is_none());
    assert!(normalized[0].get("headers").is_none());
    let aliases: Value = serde_json::from_str(
        &normalize_payload(json!({"access_token":"", "accessToken":" alias-token "})).unwrap(),
    )
    .unwrap();
    assert_eq!(aliases[0]["access_token"], "alias-token");
    assert_eq!(normalized[1]["access_token"], "another-token");
    assert!(normalize_payload(json!({"refreshToken":"refresh-test"}))
        .unwrap()
        .contains("refresh_token"));
}
#[test]
fn rejects_invalid_and_oversized_batches_without_echoing_tokens() {
    for value in [
        json!({}),
        json!({"accounts":[{}]}),
        json!({"accounts":[{"access_token":42}]}),
        json!({"accounts":["secret-token"]}),
        json!({"refresh":"false","tokens":["secret-token"]}),
    ] {
        let error = normalize_payload(value).unwrap_err();
        assert!(!error.contains("secret-token"));
    }
    let large_tokens: Vec<_> = (0..101).map(|index| format!("test-{index}")).collect();
    let large_batch = normalize_payload(json!({"tokens":large_tokens})).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&large_batch)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        101
    );
    assert!(normalize_payload(json!({"accounts":[{"access_token":"test", "nested":{"auth_mode":"agentIdentity", "agent_private_key":"test", "agent_runtime_id":"test"}}]})).is_err());
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
#[tokio::test]
async fn both_routes_enforce_disabled_state_and_authentication() {
    for path in ["/api/accounts", "/api/accounts/upload"] {
        let (state, _) = fixture(false);
        assert_eq!(
            post(state, path, Some(TEST_SECRET), json!({"tokens":["test"]}))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        let (state, repo) = fixture(true);
        assert_eq!(
            post(state.clone(), path, None, json!({"tokens":["test"]}))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post(
                state.clone(),
                path,
                Some("wrong"),
                json!({"tokens":["test"]})
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        state
            .upsert_system_config_json_value(SECRET_KEY, &json!(""), None)
            .await
            .unwrap();
        assert_eq!(
            post(state, path, Some(TEST_SECRET), json!({"tokens":["test"]}))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert!(repo
            .list_keys_by_provider_ids(&[PROVIDER_ID.into()])
            .await
            .unwrap()
            .is_empty());
    }
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
            .uri("/api/accounts/upload")
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
fn imports_and_updates_the_same_account_across_both_paths() {
    let _guard = IMPORT_TEST_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::tests::run_async_test_on_large_stack(
        "account_push_imports_and_updates",
        16 * 1024 * 1024,
        || async {
            let (state, repo) = fixture(true);
            let account = |token: &str| {
                json!({"accessToken":token,"expiresAt":2100000000u64,
            "email":"pool@example.test","accountId":"acct-test", "accountUserId":"user-test__acct-test", "userId":"user-test", "planType":"plus"})
            };
            let (status, result) = post(
                state.clone(),
                "/api/accounts/upload",
                Some(TEST_SECRET),
                json!({"accounts":[account("first-test-token")],"refresh":false}),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{result}");
            assert_eq!(result["added"], 1);
            assert_eq!(result["updated"], 0);
            assert_eq!(result["success"], true);
            assert_eq!(result["items"], json!([]));
            assert!(!result.to_string().contains("first-test-token"));
            let keys = repo
                .list_keys_by_provider_ids(&[PROVIDER_ID.into()])
                .await
                .unwrap();
            assert_eq!(keys.len(), 1);
            let original_id = keys[0].id.clone();
            assert_eq!(
                state
                    .decrypt_provider_catalog_key_api_key(&keys[0])
                    .unwrap()
                    .unwrap(),
                "first-test-token"
            );
            let auth: Value = serde_json::from_str(
                &state
                    .decrypt_provider_catalog_key_auth_config(&keys[0])
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(auth["account_user_id"], "user-test__acct-test");
            // Interactive imports still reject a healthy duplicate; only pushes upsert it.
            assert!(crate::handlers::admin::find_duplicate_provider_oauth_key(
                &AdminAppState::new(&state),
                PROVIDER_ID,
                auth.as_object().unwrap(),
                None,
            )
            .await
            .is_err());

            let (status, result) = post(
                state.clone(),
                "/api/accounts",
                Some(TEST_SECRET),
                json!({"accounts":[account("second-test-token")]}),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{result}");
            assert_eq!(result["added"], 0);
            assert_eq!(result["updated"], 1);
            let keys = repo
                .list_keys_by_provider_ids(&[PROVIDER_ID.into()])
                .await
                .unwrap();
            assert_eq!(keys.len(), 1);
            assert_eq!(keys[0].id, original_id);
            assert_eq!(
                state
                    .decrypt_provider_catalog_key_api_key(&keys[0])
                    .unwrap()
                    .unwrap(),
                "second-test-token"
            );
            let summary = state
                .read_system_config_json_value_strong(RESULT_KEY)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(summary["updated"], 1);
            assert_eq!(summary["success"], true);
            assert!(!summary.to_string().contains("second-test-token"));
            let (status, result) = post(state.clone(), "/api/accounts", Some(TEST_SECRET),
                json!({"accounts":[account("third-test-token"), {"refresh_token":"invalid-refresh-only"}]})).await;
            assert_eq!(status, StatusCode::OK, "{result}");
            assert_eq!(result["updated"], 1);
            assert_eq!(result["failed"], 1);
            assert_eq!(result["success"], false);
            assert_eq!(result["errors"].as_array().unwrap().len(), 1);
            assert!(!result.to_string().contains("invalid-refresh-only"));
            let (status, result) = post(
                state.clone(),
                "/api/accounts",
                Some(TEST_SECRET),
                json!({"accounts":[{"refresh_token":"invalid-refresh-only"}]}),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");
            assert_eq!(result["failed"], 1);
            assert_eq!(result["added"], 0);
            assert_eq!(result["success"], false);
            state
                .upsert_system_config_json_value(CONFIG_KEY, &json!({"enabled":false}), None)
                .await
                .unwrap();
            assert_eq!(
                post(
                    state,
                    "/api/accounts/upload",
                    Some(TEST_SECRET),
                    json!({"accounts":[account("third-test-token")]})
                )
                .await
                .0,
                StatusCode::NOT_FOUND
            );
        },
    );
}

#[test]
fn imports_more_than_one_internal_batch_and_merges_the_result() {
    let _guard = IMPORT_TEST_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::tests::run_async_test_on_large_stack(
        "account_push_imports_multiple_batches",
        16 * 1024 * 1024,
        || async {
            let (state, repo) = fixture(true);
            let accounts: Vec<_> = (0..101)
                .map(|index| {
                    json!({
                        "access_token": format!("batch-token-{index}"),
                        "expires_at": 2100000000u64,
                        "email": format!("batch-{index}@example.test"),
                        "account_id": format!("batch-account-{index}"),
                        "account_user_id": format!("batch-user-{index}"),
                        "user_id": format!("batch-user-{index}"),
                    })
                })
                .collect();
            let (status, result) = post(
                state,
                "/api/accounts",
                Some(TEST_SECRET),
                json!({"accounts": accounts, "refresh": false}),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{result}");
            assert_eq!(result["total"], 101);
            assert_eq!(result["added"], 101);
            assert_eq!(result["updated"], 0);
            assert_eq!(result["failed"], 0);
            assert_eq!(result["success"], true);
            assert_eq!(
                repo.list_keys_by_provider_ids(&[PROVIDER_ID.into()])
                    .await
                    .unwrap()
                    .len(),
                101
            );
        },
    );
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
                let (status, result) = post(
                    state.clone(),
                    "/api/accounts",
                    Some(TEST_SECRET),
                    json!({"accounts":[{"refresh_token":"codex-test-refresh"}]}),
                )
                .await;
                assert_eq!(status, StatusCode::OK, "{result}");
                assert_eq!(result["added"], added);
                assert_eq!(result["updated"], updated);
                assert_eq!(result["success"], true);
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
