use super::*;
use aether_data_contracts::repository::provider_catalog::ProviderOAuthIdentityLock;
use std::time::Duration;

struct PostgresIdentityLock {
    _transaction: sqlx::Transaction<'static, Postgres>,
}
impl ProviderOAuthIdentityLock for PostgresIdentityLock {}

impl SqlxProviderCatalogReadRepository {
    pub async fn acquire_oauth_identity_lock(
        &self,
        provider_id: &str,
    ) -> Result<Option<Box<dyn ProviderOAuthIdentityLock>>, DataLayerError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            let mut transaction = self.pool.begin().await.map_postgres_err()?;
            let locked: bool = sqlx::query_scalar(
                "SELECT pg_try_advisory_xact_lock(hashtextextended($1, 716931))",
            )
            .bind(provider_id)
            .fetch_one(&mut *transaction)
            .await
            .map_postgres_err()?;
            if locked {
                // Backfill may span several batches. Cancellation drops/rolls back this guard.
                sqlx::query("SET LOCAL idle_in_transaction_session_timeout = 0")
                    .execute(&mut *transaction)
                    .await
                    .map_postgres_err()?;
                return Ok(Some(Box::new(PostgresIdentityLock {
                    _transaction: transaction,
                })));
            }
            // Never let waiters exhaust the pool needed by the current lock holder.
            transaction.rollback().await.map_postgres_err()?;
            if tokio::time::Instant::now() >= deadline {
                return Err(DataLayerError::TimedOut(
                    "provider OAuth admission lock timed out".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub async fn list_unindexed_oauth_keys(
        &self,
        provider_id: &str,
        limit: usize,
    ) -> Result<Option<Vec<StoredProviderCatalogKey>>, DataLayerError> {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM provider_api_keys WHERE provider_id = $1 AND NOT oauth_identity_indexed ORDER BY id LIMIT $2"
        ).bind(provider_id).bind(limit.clamp(1, 500) as i64)
            .fetch_all(&self.pool).await.map_postgres_err()?;
        Ok(Some(self.list_keys_by_ids(&ids).await?))
    }

    pub async fn find_oauth_identity_key_ids(
        &self,
        provider_id: &str,
        hashes: &[String],
    ) -> Result<Option<Vec<String>>, DataLayerError> {
        // Include credentials invalidated by a concurrent refresh, even if backfill
        // just completed. The partial pending index keeps this lookup bounded.
        let ids = sqlx::query_scalar(
            "SELECT key_id FROM provider_oauth_identity_index WHERE provider_id = $1 AND identity_hashes && $2::text[] UNION SELECT id FROM provider_api_keys WHERE provider_id = $1 AND NOT oauth_identity_indexed"
        ).bind(provider_id).bind(hashes).fetch_all(&self.pool).await.map_postgres_err()?;
        Ok(Some(ids))
    }

    pub async fn index_oauth_key(
        &self,
        key: &StoredProviderCatalogKey,
    ) -> Result<bool, DataLayerError> {
        let mut transaction = self.pool.begin().await.map_postgres_err()?;
        let unchanged: Option<String> = sqlx::query_scalar(
            "SELECT id FROM provider_api_keys WHERE id = $1 AND provider_id = $2 AND auth_config IS NOT DISTINCT FROM $3 FOR UPDATE"
        ).bind(&key.id).bind(&key.provider_id).bind(&key.encrypted_auth_config)
            .fetch_optional(&mut *transaction).await.map_postgres_err()?;
        if unchanged.is_none() {
            return Ok(false);
        }
        sync_oauth_identity(&mut transaction, key).await?;
        transaction.commit().await.map_postgres_err()?;
        Ok(true)
    }
}

pub(super) async fn sync_oauth_identity(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    key: &StoredProviderCatalogKey,
) -> Result<(), DataLayerError> {
    let Some(hashes) = &key.oauth_identity_hashes else {
        return Ok(());
    };
    sqlx::query("INSERT INTO provider_oauth_identity_index (key_id, provider_id, identity_hashes) VALUES ($1, $2, $3) ON CONFLICT (key_id) DO UPDATE SET provider_id = EXCLUDED.provider_id, identity_hashes = EXCLUDED.identity_hashes")
        .bind(&key.id).bind(&key.provider_id).bind(hashes)
        .execute(&mut **transaction).await.map_postgres_err()?;
    sqlx::query("UPDATE provider_api_keys SET oauth_identity_indexed = TRUE WHERE id = $1")
        .bind(&key.id)
        .execute(&mut **transaction)
        .await
        .map_postgres_err()?;
    Ok(())
}

#[cfg(test)]
mod tests;
