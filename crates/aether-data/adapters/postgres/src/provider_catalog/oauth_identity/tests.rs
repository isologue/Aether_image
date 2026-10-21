use super::*;
use aether_data_contracts::repository::provider_catalog::{
    provider_oauth_identity_hashes, ProviderCatalogKeyOAuthCredentialFence,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;

async fn fixture() -> (SqlxProviderCatalogReadRepository, String) {
    let url = std::env::var("AETHER_IDENTITY_TEST_DATABASE_URL")
        .expect("set AETHER_IDENTITY_TEST_DATABASE_URL to a disposable test database");
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect(&url)
        .await
        .unwrap();
    crate::run_migrations(&pool).await.unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let repository = SqlxProviderCatalogReadRepository::new(pool);
    let provider = StoredProviderCatalogProvider::new(
        id.clone(),
        format!("Identity test {id}"),
        None,
        "chatgpt_web".into(),
    )
    .unwrap();
    repository.create_provider(&provider, None).await.unwrap();
    (repository, id)
}
fn key(provider_id: &str, identity: &str) -> StoredProviderCatalogKey {
    let mut key = StoredProviderCatalogKey::new(
        uuid::Uuid::new_v4().to_string(),
        provider_id.into(),
        identity.into(),
        "oauth".into(),
        None,
        true,
    )
    .unwrap();
    key.encrypted_auth_config = Some(format!("test-credential-{identity}"));
    key.oauth_identity_hashes = Some(hashes(identity));
    key
}
fn hashes(identity: &str) -> Vec<String> {
    provider_oauth_identity_hashes(json!({"email":identity}).as_object().unwrap())
}

#[tokio::test]
#[ignore = "requires an isolated PostgreSQL database"]
async fn postgres_oauth_identity_backfill_cas_updates_deletion_and_lock_cancellation() {
    let (repository, provider_id) = fixture().await;
    let mut legacy = key(&provider_id, "old@example.test");
    legacy.oauth_identity_hashes = None;
    repository.create_key(&legacy).await.unwrap();
    let mut observed = repository
        .list_unindexed_oauth_keys(&provider_id, 100)
        .await
        .unwrap()
        .unwrap()
        .pop()
        .unwrap();
    observed.oauth_identity_hashes = Some(hashes("old@example.test"));
    sqlx::query("UPDATE provider_api_keys SET auth_config = 'changed' WHERE id = $1")
        .bind(&legacy.id)
        .execute(repository.pool())
        .await
        .unwrap();
    assert!(
        !repository.index_oauth_key(&observed).await.unwrap(),
        "stale backfill must fail its credential fence"
    );
    assert_eq!(
        repository
            .find_oauth_identity_key_ids(&provider_id, &hashes("new@example.test"))
            .await
            .unwrap()
            .unwrap(),
        vec![legacy.id.clone()],
        "pending credentials must remain candidates until reindexed"
    );
    let mut current = repository
        .list_keys_by_ids(&[legacy.id.clone()])
        .await
        .unwrap()
        .pop()
        .unwrap();
    current.oauth_identity_hashes = Some(hashes("new@example.test"));
    assert!(repository.index_oauth_key(&current).await.unwrap());
    assert!(repository
        .list_unindexed_oauth_keys(&provider_id, 100)
        .await
        .unwrap()
        .unwrap()
        .is_empty());
    assert!(repository
        .find_oauth_identity_key_ids(&provider_id, &hashes("old@example.test"))
        .await
        .unwrap()
        .unwrap()
        .is_empty());
    assert_eq!(
        repository
            .find_oauth_identity_key_ids(&provider_id, &hashes("new@example.test"))
            .await
            .unwrap()
            .unwrap(),
        vec![legacy.id.clone()]
    );
    current.name = "Renamed identity".into();
    repository.update_key(&current).await.unwrap();
    assert_eq!(
        repository
            .find_oauth_identity_key_ids(&provider_id, &hashes("new@example.test"))
            .await
            .unwrap()
            .unwrap(),
        vec![legacy.id.clone()]
    );
    assert!(repository
        .find_oauth_identity_key_ids(&provider_id, &hashes("old@example.test"))
        .await
        .unwrap()
        .unwrap()
        .is_empty());
    // Periodic pushes use the fenced admin writer; its projection must commit atomically too.
    let expected = current.clone();
    current.encrypted_auth_config = Some("pushed-credentials".into());
    current.oauth_identity_hashes = Some(hashes("pushed@example.test"));
    let update = ProviderCatalogKeyAdminCasUpdate {
        expected_encrypted_auth_config: expected.encrypted_auth_config.clone(),
        expected_credential: ProviderCatalogKeyOAuthCredentialFence {
            encrypted_api_key: expected.encrypted_api_key.clone(),
            auth_type: expected.auth_type.clone(),
            provider_id: provider_id.clone(),
            provider_type: "chatgpt_web".into(),
        },
        key: current.clone(),
        codex_rotation: None,
        reset_oauth_runtime: false,
    };
    assert!(repository
        .compare_and_update_key_admin_state(&update)
        .await
        .unwrap());
    assert!(
        !repository
            .compare_and_update_key_admin_state(&update)
            .await
            .unwrap(),
        "a stale push must not replace credentials or their identity projection"
    );
    assert_eq!(
        repository
            .find_oauth_identity_key_ids(&provider_id, &hashes("pushed@example.test"))
            .await
            .unwrap()
            .unwrap(),
        vec![legacy.id.clone()]
    );
    assert!(repository
        .find_oauth_identity_key_ids(&provider_id, &hashes("new@example.test"))
        .await
        .unwrap()
        .unwrap()
        .is_empty());
    // Waiters must release connections, leaving capacity for the holder to commit.
    let holder = repository
        .acquire_oauth_identity_lock(&provider_id)
        .await
        .unwrap();
    let mut waiters = Vec::new();
    for _ in 0..8 {
        let repository = repository.clone();
        let provider_id = provider_id.clone();
        waiters.push(tokio::spawn(async move {
            repository.acquire_oauth_identity_lock(&provider_id).await
        }));
    }
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(waiters.iter().all(|waiter| !waiter.is_finished()));
    let unrelated = key(&provider_id, "unrelated@example.test");
    tokio::time::timeout(Duration::from_secs(2), repository.create_key(&unrelated))
        .await
        .unwrap()
        .unwrap();
    for waiter in waiters {
        waiter.abort();
        let _ = waiter.await;
    }
    drop(holder);
    let recovered = tokio::time::timeout(
        Duration::from_secs(2),
        repository.acquire_oauth_identity_lock(&provider_id),
    )
    .await
    .unwrap()
    .unwrap();
    drop(recovered);
    repository.delete_key(&legacy.id).await.unwrap();
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM provider_oauth_identity_index WHERE key_id = $1")
            .bind(&legacy.id)
            .fetch_one(repository.pool())
            .await
            .unwrap();
    assert_eq!(remaining, 0);
    repository.delete_provider(&provider_id).await.unwrap();
}

#[tokio::test]
#[ignore = "requires an isolated PostgreSQL database; six batches of 1000 writes"]
async fn postgres_oauth_identity_lookup_scaling_six_batches() {
    let (repository, provider_id) = fixture().await;
    for batch in 0..6 {
        let started = std::time::Instant::now();
        for offset in 0..1000 {
            let identity = format!("{}@example.test", batch * 1000 + offset);
            let key = key(&provider_id, &identity);
            let ids = repository
                .find_oauth_identity_key_ids(
                    &provider_id,
                    key.oauth_identity_hashes.as_ref().unwrap(),
                )
                .await
                .unwrap()
                .unwrap();
            assert!(ids.is_empty());
            repository.create_key(&key).await.unwrap();
        }
        eprintln!(
            "identity index batch={} accounts=1000 elapsed_ms={}",
            batch + 1,
            started.elapsed().as_millis()
        );
    }
    assert!(repository
        .list_unindexed_oauth_keys(&provider_id, 100)
        .await
        .unwrap()
        .unwrap()
        .is_empty());
    let ids = repository
        .find_oauth_identity_key_ids(&provider_id, &hashes("5999@example.test"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ids.len(), 1);
    repository.delete_provider(&provider_id).await.unwrap();
}
