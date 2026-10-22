//! Durable image queue. The transaction lock serializes admission and deployment-wide claims.
use crate::error::SqlxResultExt;
use crate::DataLayerError;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};

#[derive(Clone)]
pub struct SqlxImageTaskRepository {
    pool: PgPool,
}
#[derive(Debug)]
pub enum ImageTaskAdmission {
    Created(Value),
    Existing(Value),
    Conflict,
    Full,
}
impl SqlxImageTaskRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    /// Commit related image policies together; the deferred DB invariant sees the final pair.
    pub async fn upsert_config_entries(
        &self,
        entries: &[(String, Value, Option<String>)],
    ) -> Result<(), DataLayerError> {
        let mut tx = self.pool.begin().await.map_postgres_err()?;
        sqlx::query("SELECT pg_advisory_xact_lock(1095062602)")
            .execute(&mut *tx)
            .await
            .map_postgres_err()?;
        for (key, value, description) in entries {
            sqlx::query("INSERT INTO system_configs(id,key,value,description,created_at,updated_at) VALUES($1,$2,$3,$4,NOW(),NOW()) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value,description=COALESCE(EXCLUDED.description,system_configs.description),updated_at=NOW()")
                .bind(uuid::Uuid::new_v4().to_string()).bind(key).bind(value).bind(description).execute(&mut *tx).await.map_postgres_err()?;
        }
        tx.commit().await.map_postgres_err()?;
        Ok(())
    }
    pub async fn get(&self, id: &str) -> Result<Option<Value>, DataLayerError> {
        sqlx::query_scalar("SELECT payload || jsonb_build_object('status',status,'started_at',started_at,'completed_at',completed_at) FROM image_tasks WHERE id=$1")
            .bind(id).fetch_optional(&self.pool).await.map_postgres_err()
    }
    pub async fn find_idempotent(
        &self,
        api_key_id: &str,
        key: &str,
    ) -> Result<Option<Value>, DataLayerError> {
        sqlx::query_scalar("SELECT payload || jsonb_build_object('status',status) FROM image_tasks WHERE api_key_id=$1 AND idempotency_key=$2")
            .bind(api_key_id).bind(key).fetch_optional(&self.pool).await.map_postgres_err()
    }
    pub async fn submit(
        &self,
        task: &Value,
        max_queued: i64,
    ) -> Result<ImageTaskAdmission, DataLayerError> {
        let mut tx = self.pool.begin().await.map_postgres_err()?;
        sqlx::query("SELECT pg_advisory_xact_lock(1095062601)")
            .execute(&mut *tx)
            .await
            .map_postgres_err()?;
        let key = task["idempotency_key"].as_str();
        if let Some(key) = key {
            if let Some(row)=sqlx::query("SELECT fingerprint,payload || jsonb_build_object('status',status) AS payload FROM image_tasks WHERE api_key_id=$1 AND idempotency_key=$2")
                .bind(task["api_key_id"].as_str()).bind(key).fetch_optional(&mut *tx).await.map_postgres_err()? {
                return Ok(if row.get::<String,_>("fingerprint")==task["fingerprint"].as_str().unwrap_or("") {
                    ImageTaskAdmission::Existing(row.get("payload"))
                } else { ImageTaskAdmission::Conflict });
            }
        }
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM image_tasks WHERE status='queued'")
                .fetch_one(&mut *tx)
                .await
                .map_postgres_err()?;
        if count >= max_queued {
            return Ok(ImageTaskAdmission::Full);
        }
        sqlx::query("INSERT INTO image_tasks(id,api_key_id,user_id,node_id,idempotency_key,fingerprint,created_at,payload) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(task["task_id"].as_str()).bind(task["api_key_id"].as_str()).bind(task["user_id"].as_str())
            .bind(task["node_id"].as_str()).bind(key).bind(task["fingerprint"].as_str()).bind(task["created_at"].as_i64()).bind(task)
            .execute(&mut *tx).await.map_postgres_err()?;
        tx.commit().await.map_postgres_err()?;
        Ok(ImageTaskAdmission::Created(task.clone()))
    }
    pub async fn claim(
        &self,
        node: &str,
        owner: &str,
        concurrency: i64,
        wait: i64,
    ) -> Result<Option<Value>, DataLayerError> {
        let mut tx = self.pool.begin().await.map_postgres_err()?;
        sqlx::query("SELECT pg_advisory_xact_lock(1095062601)")
            .execute(&mut *tx)
            .await
            .map_postgres_err()?;
        // A lost executor is never retried blindly: it may have submitted a paid upstream generation.
        sqlx::query("UPDATE image_tasks SET status='failed',completed_at=EXTRACT(EPOCH FROM NOW())::bigint,payload=payload || jsonb_build_object('error',jsonb_build_object('code','execution_interrupted','message','Executor interrupted; upstream result is unknown'),'billing_state','pending_release') WHERE status='running' AND payload->>'recovery' IS NULL AND lease_until < EXTRACT(EPOCH FROM NOW())::bigint")
            .execute(&mut *tx).await.map_postgres_err()?;
        sqlx::query("UPDATE image_tasks SET status='failed',settled=true,completed_at=EXTRACT(EPOCH FROM NOW())::bigint,payload=(payload - 'credential' - 'recovery' - 'terminal_event') || jsonb_build_object('error',jsonb_build_object('code','queue_timeout','message','Image queue wait timed out'),'billing_state','not_charged') WHERE status='queued' AND created_at < EXTRACT(EPOCH FROM NOW())::bigint-$1")
            .bind(wait).execute(&mut *tx).await.map_postgres_err()?;
        // Recover an interrupted conversation on its storage node, with a new fenced owner.
        let recovered:Option<Value>=sqlx::query_scalar("UPDATE image_tasks SET lease_owner=$2,lease_until=EXTRACT(EPOCH FROM NOW())::bigint+120 WHERE id=(SELECT id FROM image_tasks WHERE status='running' AND node_id=$1 AND payload->>'recovery' IS NOT NULL AND lease_until < EXTRACT(EPOCH FROM NOW())::bigint ORDER BY started_at,id FOR UPDATE SKIP LOCKED LIMIT 1) RETURNING payload || jsonb_build_object('status',status,'started_at',started_at)")
            .bind(node).bind(owner).fetch_optional(&mut *tx).await.map_postgres_err()?;
        if recovered.is_some() {
            tx.commit().await.map_postgres_err()?;
            return Ok(recovered);
        }
        let running: i64 =
            sqlx::query_scalar("SELECT count(*) FROM image_tasks WHERE status='running'")
                .fetch_one(&mut *tx)
                .await
                .map_postgres_err()?;
        if running >= concurrency {
            tx.commit().await.map_postgres_err()?;
            return Ok(None);
        }
        let task=sqlx::query_scalar("UPDATE image_tasks SET status='running',started_at=EXTRACT(EPOCH FROM NOW())::bigint,lease_owner=$2,lease_until=EXTRACT(EPOCH FROM NOW())::bigint+120,payload=payload || jsonb_build_object('stage','preparing','progress',10) WHERE id=(SELECT id FROM image_tasks WHERE status='queued' AND node_id=$1 ORDER BY created_at,id FOR UPDATE SKIP LOCKED LIMIT 1) RETURNING payload || jsonb_build_object('status',status,'started_at',started_at)")
            .bind(node).bind(owner).fetch_optional(&mut *tx).await.map_postgres_err()?;
        tx.commit().await.map_postgres_err()?;
        Ok(task)
    }
    pub async fn renew(&self, id: &str, owner: &str) -> Result<bool, DataLayerError> {
        Ok(sqlx::query("UPDATE image_tasks SET lease_until=EXTRACT(EPOCH FROM NOW())::bigint+120 WHERE id=$1 AND lease_owner=$2 AND status='running' AND lease_until >= EXTRACT(EPOCH FROM NOW())::bigint")
            .bind(id).bind(owner).execute(&self.pool).await.map_postgres_err()?.rows_affected()==1)
    }
    pub async fn progress(
        &self,
        id: &str,
        owner: &str,
        patch: &Value,
    ) -> Result<bool, DataLayerError> {
        Ok(sqlx::query("UPDATE image_tasks SET payload=(payload || $3) || jsonb_build_object('progress',GREATEST(COALESCE((payload->>'progress')::int,0),COALESCE(($3->>'progress')::int,0)),'stage',CASE WHEN COALESCE(($3->>'progress')::int,0) >= COALESCE((payload->>'progress')::int,0) THEN COALESCE($3->>'stage',payload->>'stage') ELSE payload->>'stage' END) WHERE id=$1 AND lease_owner=$2 AND status='running' AND lease_until >= EXTRACT(EPOCH FROM NOW())::bigint")
            .bind(id).bind(owner).bind(patch).execute(&self.pool).await.map_postgres_err()?.rows_affected()==1)
    }
    pub async fn finish(
        &self,
        id: &str,
        owner: &str,
        status: &str,
        patch: &Value,
        settled: bool,
    ) -> Result<bool, DataLayerError> {
        Ok(sqlx::query("UPDATE image_tasks SET status=$3,completed_at=EXTRACT(EPOCH FROM NOW())::bigint,settled=$5,payload=payload || $4,lease_until=NULL WHERE id=$1 AND lease_owner=$2 AND status='running' AND lease_until >= EXTRACT(EPOCH FROM NOW())::bigint")
            .bind(id).bind(owner).bind(status).bind(patch).bind(settled).execute(&self.pool).await.map_postgres_err()?.rows_affected()==1)
    }
    /// The encrypted outbox is retained until the usage row confirms settlement.
    pub async fn terminal_outbox(&self) -> Result<Vec<Value>, DataLayerError> {
        sqlx::query_scalar("SELECT payload FROM image_tasks WHERE NOT settled AND status IN ('succeeded','failed') AND payload->>'terminal_event' IS NOT NULL ORDER BY completed_at LIMIT 100")
            .fetch_all(&self.pool).await.map_postgres_err()
    }
    pub async fn reconcile_finished(&self) -> Result<(), DataLayerError> {
        let ids:Vec<String>=sqlx::query_scalar("SELECT id FROM image_tasks WHERE NOT settled AND status IN ('succeeded','failed') ORDER BY completed_at LIMIT 200")
            .fetch_all(&self.pool).await.map_postgres_err()?;
        for id in ids {
            self.reconcile_one(&id).await?;
        }
        Ok(())
    }
    pub async fn renew_reservations(&self, request_id: &str) -> Result<(), DataLayerError> {
        sqlx::query("UPDATE usage_cost_reservations SET reservation_expires_at=GREATEST(reservation_expires_at,NOW()+INTERVAL '24 hours'),retain_until=GREATEST(retain_until,NOW()+INTERVAL '48 hours'),updated_at=NOW() WHERE request_id=$1 AND state='reserved'")
            .bind(request_id).execute(&self.pool).await.map_postgres_err()?;
        Ok(())
    }
    pub async fn list(
        &self,
        status: Option<&str>,
        model: Option<&str>,
        task_id: Option<&str>,
        page: i64,
        size: i64,
    ) -> Result<Value, DataLayerError> {
        let total:i64=sqlx::query_scalar("SELECT count(*) FROM image_tasks WHERE ($1::text IS NULL OR status=$1) AND ($2::text IS NULL OR payload->>'model' ILIKE '%' || $2 || '%') AND ($3::text IS NULL OR strpos(lower(id),lower($3)) > 0)")
            .bind(status).bind(model).bind(task_id).fetch_one(&self.pool).await.map_postgres_err()?;
        let items:Vec<Value>=sqlx::query_scalar("SELECT (payload - 'request' - 'snapshot' - 'fingerprint' - 'idempotency_key' - 'credential' - 'result_refs' - 'result' - 'terminal_event' - 'recovery' - 'credential' - 'refusal') || jsonb_build_object('status',status,'started_at',started_at,'completed_at',completed_at) FROM image_tasks WHERE ($1::text IS NULL OR status=$1) AND ($2::text IS NULL OR payload->>'model' ILIKE '%' || $2 || '%') AND ($3::text IS NULL OR strpos(lower(id),lower($3)) > 0) ORDER BY created_at DESC,id LIMIT $4 OFFSET $5")
            .bind(status).bind(model).bind(task_id).bind(size).bind((page-1)*size).fetch_all(&self.pool).await.map_postgres_err()?;
        let stats:Vec<(String,i64)>=sqlx::query_as("SELECT status,count(*) FROM image_tasks WHERE ($1::text IS NULL OR payload->>'model' ILIKE '%' || $1 || '%') AND ($2::text IS NULL OR strpos(lower(id),lower($2)) > 0) GROUP BY status").bind(model).bind(task_id).fetch_all(&self.pool).await.map_postgres_err()?;
        Ok(
            json!({"items":items,"total":total,"page":page,"page_size":size,"stats":stats.into_iter().collect::<std::collections::BTreeMap<_,_>>()}),
        )
    }
    pub async fn active_input_ids(&self, node: &str) -> Result<Vec<String>, DataLayerError> {
        sqlx::query_scalar("SELECT id FROM image_tasks WHERE node_id=$1 AND (status IN ('queued','running') OR NOT settled)")
            .bind(node).fetch_all(&self.pool).await.map_postgres_err()
    }
    pub async fn cleanup(&self, days: i64) -> Result<u64, DataLayerError> {
        let deleted=sqlx::query("DELETE FROM image_tasks WHERE id IN (SELECT id FROM image_tasks WHERE status IN ('succeeded','failed') AND settled AND completed_at < EXTRACT(EPOCH FROM NOW())::bigint-$1*86400 LIMIT 1000)")
            .bind(days).execute(&self.pool).await.map_postgres_err()?.rows_affected();
        // Business video tasks use the same policy. System task runs have their own retention.
        sqlx::query("DELETE FROM video_tasks WHERE status IN ('completed','failed','cancelled','expired','deleted') AND completed_at < NOW()-make_interval(days => $1::int) AND NOT EXISTS (SELECT 1 FROM usage WHERE usage.request_id=video_tasks.request_id AND (usage.status IN ('pending','streaming') OR usage.billing_status NOT IN ('settled','void','refunded','insufficient_quota')))")
            .bind(days as i32).execute(&self.pool).await.map_postgres_err()?;
        Ok(deleted)
    }
}

impl SqlxImageTaskRepository {
    async fn reconcile_one(&self, id: &str) -> Result<(), DataLayerError> {
        let mut tx = self.pool.begin().await.map_postgres_err()?;
        let Some(task) = sqlx::query(
            "SELECT status,payload FROM image_tasks WHERE id=$1 AND NOT settled FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_postgres_err()?
        else {
            return Ok(());
        };
        let status: String = task.get("status");
        let payload: Value = task.get("payload");
        let Some(request) = payload["request_id"].as_str() else {
            sqlx::query("UPDATE image_tasks SET settled=true,payload=payload || jsonb_build_object('billing_state','not_charged','actual_cost',0) WHERE id=$1")
                .bind(id).execute(&mut *tx).await.map_postgres_err()?;
            tx.commit().await.map_postgres_err()?;
            return Ok(());
        };
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::BIGINT)")
            .bind(request)
            .execute(&mut *tx)
            .await
            .map_postgres_err()?;
        let usage=sqlx::query("SELECT id,status,billing_status,actual_total_cost_usd::double precision AS cost FROM usage WHERE request_id=$1 FOR UPDATE")
            .bind(request).fetch_optional(&mut *tx).await.map_postgres_err()?;
        // An interrupted executor has no terminal outbox. Fence its pending usage and release
        // only this task's reservations. A late pending event cannot reopen a terminal row.
        if status == "failed" {
            sqlx::query("UPDATE usage SET status='failed',billing_status=CASE WHEN billing_status IN ('settled','refunded') THEN billing_status ELSE 'void' END,finalized_at=COALESCE(finalized_at,NOW()) WHERE request_id=$1")
                .bind(request).execute(&mut *tx).await.map_postgres_err()?;
            sqlx::query("UPDATE usage_cost_reservations SET state='released',actual_cost_units=0,finalized_at=NOW(),updated_at=NOW() WHERE request_id=$1 AND state='reserved'")
                .bind(request).execute(&mut *tx).await.map_postgres_err()?;
            sqlx::query("UPDATE usage_request_admissions SET state='released',released_at=NOW() WHERE request_id=$1 AND state='active'")
                .bind(request).execute(&mut *tx).await.map_postgres_err()?;
        }
        let mut billing = usage
            .as_ref()
            .map(|u| u.get::<String, _>("billing_status"))
            .unwrap_or_default();
        if status == "failed" && billing != "settled" && billing != "refunded" {
            billing = "void".into();
        }
        let mut cost = usage
            .as_ref()
            .and_then(|u| u.try_get::<f64, _>("cost").ok())
            .unwrap_or(0.0);
        if status == "failed" && billing == "settled" {
            // The original allocation snapshot is authoritative. Never infer refund sources
            // from current wallet balances or refund a legacy allocation by guessing.
            let snapshot=sqlx::query("SELECT wallet_id,allocation_status FROM usage_settlement_snapshots WHERE request_id=$1 FOR UPDATE")
                .bind(request).fetch_optional(&mut *tx).await.map_postgres_err()?;
            let Some(snapshot) = snapshot else {
                return Ok(());
            };
            if snapshot.get::<String, _>("allocation_status") != "complete" {
                return Ok(());
            }
            let wallet: Option<String> = snapshot.get("wallet_id");
            if let Some(wallet) = wallet {
                sqlx::query("SELECT id FROM wallets WHERE id=$1 FOR UPDATE")
                    .bind(&wallet)
                    .fetch_one(&mut *tx)
                    .await
                    .map_postgres_err()?;
                sqlx::query(r#"INSERT INTO wallet_transactions(id,wallet_id,category,reason_code,amount,balance_before,balance_after,recharge_balance_before,recharge_balance_after,gift_balance_before,gift_balance_after,link_type,link_id,description,created_at)
                    SELECT $2,w.id,'refund','image_task_failed',s.wallet_debit_amount_usd,w.balance+w.gift_balance,w.balance+w.gift_balance+s.wallet_debit_amount_usd,w.balance,w.balance+s.wallet_recharge_debit_usd+s.wallet_overdraft_usd,w.gift_balance,w.gift_balance+s.wallet_gift_debit_usd,'usage',u.id,'Image task failed after settlement; restored original sources',NOW()
                    FROM usage_settlement_snapshots s JOIN wallets w ON w.id=s.wallet_id JOIN usage u ON u.request_id=s.request_id WHERE s.request_id=$1"#)
                    .bind(request).bind(uuid::Uuid::new_v4().to_string()).execute(&mut *tx).await.map_postgres_err()?;
                sqlx::query("UPDATE wallets w SET balance=w.balance+s.wallet_recharge_debit_usd+s.wallet_overdraft_usd,gift_balance=w.gift_balance+s.wallet_gift_debit_usd,total_consumed=GREATEST(0,w.total_consumed-s.wallet_consumed_amount_usd),total_refunded=w.total_refunded+s.wallet_debit_amount_usd,updated_at=NOW() FROM usage_settlement_snapshots s WHERE s.request_id=$1 AND w.id=s.wallet_id")
                    .bind(request).execute(&mut *tx).await.map_postgres_err()?;
            }
            sqlx::query("SELECT id FROM user_plan_entitlements WHERE id IN (SELECT user_entitlement_id FROM entitlement_usage_ledgers WHERE request_id=$1) ORDER BY id FOR UPDATE")
                .bind(request).fetch_all(&mut *tx).await.map_postgres_err()?;
            sqlx::query("UPDATE usage SET request_metadata=COALESCE(request_metadata::jsonb,'{}'::jsonb) || jsonb_build_object('refund',jsonb_build_object('reason','image_task_failed','task_id',$2::text,'amount',actual_total_cost_usd,'refunded_at',EXTRACT(EPOCH FROM NOW()),'transaction_id',(SELECT id FROM wallet_transactions WHERE category='refund' AND reason_code='image_task_failed' AND link_id=usage.id ORDER BY created_at DESC LIMIT 1),'quota_sources',COALESCE((SELECT jsonb_agg(to_jsonb(l)) FROM entitlement_usage_ledgers l WHERE l.request_id=$1),'[]'::jsonb))) WHERE request_id=$1")
                .bind(request).bind(id).execute(&mut *tx).await.map_postgres_err()?;
            sqlx::query("UPDATE entitlement_usage_ledgers SET amount_usd=0,balance_after=balance_before WHERE request_id=$1")
                .bind(request).execute(&mut *tx).await.map_postgres_err()?;
            sqlx::query("UPDATE usage_settlement_snapshots SET billing_status='refunded',updated_at=NOW() WHERE request_id=$1")
                .bind(request).execute(&mut *tx).await.map_postgres_err()?;
            sqlx::query("UPDATE usage SET billing_status='refunded' WHERE request_id=$1")
                .bind(request)
                .execute(&mut *tx)
                .await
                .map_postgres_err()?;
            sqlx::query("UPDATE usage_cost_reservations SET state='released',actual_cost_units=0,updated_at=NOW() WHERE request_id=$1")
                .bind(request).execute(&mut *tx).await.map_postgres_err()?;
            billing = "refunded".into();
        }
        if matches!(
            billing.as_str(),
            "settled" | "void" | "refunded" | "insufficient_quota"
        ) {
            let state = match billing.as_str() {
                "settled" => "charged",
                "refunded" => "refunded",
                _ if payload["outcome"] == "content_refused" => "not_charged",
                _ => "reservation_released",
            };
            if billing != "settled" {
                cost = 0.0;
            }
            let patch = json!({"billing_state":state,"actual_cost":cost,"usage_id":usage.as_ref().map(|u|u.get::<String,_>("id"))});
            sqlx::query("UPDATE image_tasks SET settled=true,payload=(payload - 'terminal_event' - 'credential' - 'recovery') || $2 WHERE id=$1")
                .bind(id).bind(patch).execute(&mut *tx).await.map_postgres_err()?;
        }
        tx.commit().await.map_postgres_err()?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "image_tasks_tests.rs"]
mod tests;
