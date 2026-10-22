use super::*;
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;

fn task(id: &str, key: Option<&str>) -> Value {
    json!({"task_id":id,"api_key_id":"key-a","user_id":"user-a","node_id":"node-a","idempotency_key":key,"fingerprint":"same","created_at":chrono::Utc::now().timestamp(),"status":"queued","stage":"queued","progress":0})
}

#[tokio::test]
#[ignore = "requires AETHER_TEST_DATABASE_URL"]
async fn live_task_id_search_filters_image_items_total_and_stats() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("AETHER_TEST_DATABASE_URL").unwrap())
        .await.unwrap();
    // An isolated table exercises the real list SQL without touching persisted tasks.
    sqlx::query("CREATE TEMP TABLE image_tasks (id text PRIMARY KEY, status text, payload jsonb, created_at bigint, started_at bigint, completed_at bigint)")
        .execute(&pool).await.unwrap();
    for (id, status, model) in [
        ("img_Alpha-1", "running", "gpt-image-2"),
        ("img_Alpha-2", "succeeded", "gpt-image-2"),
        ("img_Beta-1", "failed", "other-model"),
    ] {
        sqlx::query("INSERT INTO image_tasks(id,status,payload,created_at) VALUES ($1,$2,$3,100)")
            .bind(id).bind(status).bind(json!({"task_id":id,"model":model}))
            .execute(&pool).await.unwrap();
    }
    let repo = SqlxImageTaskRepository::new(pool);
    let page = repo.list(None, None, Some("ALPHA"), 2, 1).await.unwrap();
    assert_eq!(page["total"], 2);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["task_id"], "img_Alpha-2");
    assert_eq!(page["stats"], json!({"running":1,"succeeded":1}));
    let page = repo.list(Some("running"), Some("gpt-image"), Some("alpha"), 1, 20).await.unwrap();
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["task_id"], "img_Alpha-1");
    for needle in ["missing", "%", "'"] {
        let page = repo.list(None, None, Some(needle), 1, 20).await.unwrap();
        assert_eq!(page["total"], 0);
        assert!(page["items"].as_array().unwrap().is_empty());
        assert_eq!(page["stats"], json!({}));
    }
}
#[tokio::test]
#[ignore = "requires local AETHER_TEST_DATABASE_URL with temporary database creation"]
async fn live_image_queue_policy_leases_and_refunds() {
    let options = std::env::var("AETHER_TEST_DATABASE_URL")
        .unwrap()
        .parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap();
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await
        .unwrap();
    let database = format!("image_tasks_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&admin)
        .await
        .unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect_with(options.database(&database))
        .await
        .unwrap();
    let outcome=AssertUnwindSafe(async {
        crate::POSTGRES_MIGRATOR.run(&pool).await.unwrap();
        let repo=SqlxImageTaskRepository::new(pool.clone());
        // Deferred invariant validates the final pair and rolls back both on failure.
        for (hours,days) in [(3,1),(24,1),(25,2),(168,7)] {
            repo.upsert_config_entries(&[("image_storage".into(),json!({"cleanup_enabled":true,"retention_hours":hours}),None),("async_task_retention_days".into(),json!(days),None)]).await.unwrap();
            if days>1 {
                assert!(matches!(repo.upsert_config_entries(&[("async_task_retention_days".into(),json!(days-1),None)]).await,Err(DataLayerError::InvalidInput(_))));
            }
        }
        for invalid in [json!(0),json!(-1),json!(1.5),json!(null),json!("7")] {
            assert!(repo.upsert_config_entries(&[("async_task_retention_days".into(),invalid,None)]).await.is_err());
        }
        for invalid in [json!({"concurrency":null}),json!({"concurrency":"2"}),json!({"max_queued":0}),json!([])] {
            assert!(matches!(repo.upsert_config_entries(&[("image_async_tasks".into(),invalid,None)]).await,Err(DataLayerError::InvalidInput(_))));
        }
        repo.upsert_config_entries(&[("image_async_tasks".into(),json!({"concurrency":2}),None)]).await.unwrap();
        // Concurrent admission gives one identity, independent of the local input directory.
        let a=task("a",Some("order-1"));let b=task("b",Some("order-1"));
        let(x,y)=tokio::join!(repo.submit(&a,10),repo.submit(&b,10));
        assert!(matches!((&x,&y),(Ok(ImageTaskAdmission::Created(_)),Ok(ImageTaskAdmission::Existing(_)))|(Ok(ImageTaskAdmission::Existing(_)),Ok(ImageTaskAdmission::Created(_)))));
        let mut conflict=a.clone();conflict["fingerprint"]=json!("other");
        assert!(matches!(repo.submit(&conflict,10).await.unwrap(),ImageTaskAdmission::Conflict));
        assert!(matches!(repo.submit(&task("full",None),1).await.unwrap(),ImageTaskAdmission::Full));
        repo.submit(&task("next",None),10).await.unwrap();
        let first=repo.claim("node-a","owner-a",1,3600).await.unwrap().unwrap();
        let id=first["task_id"].as_str().unwrap();
        assert!(repo.claim("node-b","owner-b",1,3600).await.unwrap().is_none());
        assert!(repo.claim("node-a","owner-b",1,3600).await.unwrap().is_none());
        assert!(!repo.progress(id,"wrong",&json!({"progress":100})).await.unwrap());
        repo.progress(id,"owner-a",&json!({"progress":90,"stage":"processing"})).await.unwrap();
        repo.progress(id,"owner-a",&json!({"progress":20,"stage":"generating"})).await.unwrap();
        let current=repo.get(id).await.unwrap().unwrap();assert_eq!(current["progress"],90);assert_eq!(current["stage"],"processing");
        // Expired lease with no upstream checkpoint fails; queued successor proceeds.
        sqlx::query("UPDATE image_tasks SET lease_until=0 WHERE id=$1").bind(id).execute(&pool).await.unwrap();
        let next=repo.claim("node-a","owner-b",1,3600).await.unwrap().unwrap();assert_eq!(next["task_id"],"next");
        assert_eq!(repo.get(id).await.unwrap().unwrap()["status"],"failed");
        assert!(!repo.finish(id,"owner-a","succeeded",&json!({}),true).await.unwrap());
        // Checkpoint recovery remains on its storage node and fences the previous owner.
        repo.progress("next","owner-b",&json!({"recovery":"encrypted-checkpoint"})).await.unwrap();
        sqlx::query("UPDATE image_tasks SET lease_until=0 WHERE id='next'").execute(&pool).await.unwrap();
        assert!(repo.claim("node-b","foreign",2,3600).await.unwrap().is_none());
        assert_eq!(repo.claim("node-a","owner-c",1,3600).await.unwrap().unwrap()["task_id"],"next");
        assert!(!repo.renew("next","owner-b").await.unwrap());
        repo.finish("next","owner-c","succeeded",&json!({"result_refs":{"data":[]}}),true).await.unwrap();
        repo.reconcile_finished().await.unwrap();
        let mut timeout=task("timeout",None);
        timeout["credential"]=json!("encrypted-api-key");
        repo.submit(&timeout,10).await.unwrap();
        sqlx::query("UPDATE image_tasks SET created_at=1 WHERE id='timeout'").execute(&pool).await.unwrap();
        assert!(repo.claim("node-a","owner-d",1,10).await.unwrap().is_none());
        let expired=repo.get("timeout").await.unwrap().unwrap();
        assert_eq!(expired["error"]["code"],"queue_timeout");
        assert!(expired.get("credential").is_none());
        sqlx::query("UPDATE image_tasks SET completed_at=1 WHERE status IN ('succeeded','failed')").execute(&pool).await.unwrap();
        assert_eq!(repo.cleanup(1).await.unwrap(),3);
        assert!(matches!(repo.submit(&a,10).await.unwrap(),ImageTaskAdmission::Created(_)));
        sqlx::query("DELETE FROM image_tasks").execute(&pool).await.unwrap();
        // A completed upstream call followed by failed delivery refunds the authoritative
        // original recharge/gift/overdraft allocation exactly once, and restores plan quota.
        sqlx::query("INSERT INTO users(id,username,email_verified,is_active,is_deleted) VALUES('user-a','user-a',false,true,false)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO wallets(id,user_id,balance,gift_balance,currency,status,limit_mode,total_consumed,total_refunded,created_at,updated_at) VALUES('wallet-a','user-a',10,20,'USD','active','finite',8,0,NOW(),NOW())").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO payment_orders(id,order_no,wallet_id,user_id,order_kind,payment_method,status,amount_usd,created_at) VALUES('order-a','order-a','wallet-a','user-a','plan_purchase','admin_grant','credited',1,NOW())").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO billing_plans(id,title,price_amount,price_currency,duration_unit,duration_value,entitlements_json,created_at,updated_at) VALUES('plan-a','Plan',1,'USD','day',30,'{}',NOW(),NOW())").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO user_plan_entitlements(id,user_id,plan_id,payment_order_id,starts_at,expires_at,entitlements_snapshot,created_at,updated_at) VALUES('ent-a','user-a','plan-a','order-a',NOW(),NOW()+INTERVAL '30 days','{}',NOW(),NOW())").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO usage(id,request_id,user_id,model,provider_name,status,billing_status,actual_total_cost_usd) VALUES('usage-a','imgtask-refund','user-a','image','provider','completed','settled',10)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO usage_settlement_snapshots(request_id,billing_status,wallet_id,allocation_status,wallet_debit_amount_usd,wallet_recharge_debit_usd,wallet_gift_debit_usd,wallet_overdraft_usd,wallet_consumed_amount_usd,quota_covered_amount_usd) VALUES('imgtask-refund','settled','wallet-a','complete',8,3,4,1,8,2)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO entitlement_usage_ledgers(id,user_entitlement_id,user_id,request_id,amount_usd,balance_before,balance_after,usage_date,created_at) VALUES('ledger-a','ent-a','user-a','imgtask-refund',2,20,18,'2026-10-22',NOW())").execute(&pool).await.unwrap();
        let mut refund=task("refund",None);refund["request_id"]=json!("imgtask-refund");
        repo.submit(&refund,10).await.unwrap();repo.claim("node-a","refund-owner",1,3600).await.unwrap();
        repo.finish("refund","refund-owner","failed",&json!({}),false).await.unwrap();
        repo.reconcile_finished().await.unwrap();repo.reconcile_finished().await.unwrap();
        let wallet=sqlx::query("SELECT balance::float8,gift_balance::float8,total_consumed::float8,total_refunded::float8 FROM wallets WHERE id='wallet-a'").fetch_one(&pool).await.unwrap();
        assert_eq!(wallet.get::<f64,_>("balance"),14.0);assert_eq!(wallet.get::<f64,_>("gift_balance"),24.0);
        assert_eq!(wallet.get::<f64,_>("total_consumed"),0.0);assert_eq!(wallet.get::<f64,_>("total_refunded"),8.0);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM wallet_transactions WHERE category='refund'").fetch_one(&pool).await.unwrap(),1);
        assert_eq!(sqlx::query_scalar::<_,f64>("SELECT amount_usd::float8 FROM entitlement_usage_ledgers WHERE id='ledger-a'").fetch_one(&pool).await.unwrap(),0.0);
        let metadata:Value=sqlx::query_scalar("SELECT request_metadata FROM usage WHERE id='usage-a'").fetch_one(&pool).await.unwrap();
        assert_eq!(metadata["refund"]["amount"].as_f64(),Some(10.0));assert!(metadata["refund"]["transaction_id"].is_string());
        assert_eq!(repo.get("refund").await.unwrap().unwrap()["billing_state"],"refunded");
    }).catch_unwind().await;
    pool.close().await;
    sqlx::query(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .execute(&admin)
        .await
        .unwrap();
    if let Err(e) = outcome {
        std::panic::resume_unwind(e)
    }
}
