use super::{
    flow::{FlowState, ImageFlow},
    inputs, settings,
};
use crate::{AppState, GatewayError};
use axum::{
    body::{to_bytes, Body},
    extract::{ConnectInfo, State},
    http::Request,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) async fn node_id(state: &AppState) -> Result<String, GatewayError> {
    let path = state.image_storage.root.join(".image-task-node");
    if let Ok(id) = tokio::fs::read_to_string(&path).await {
        if uuid::Uuid::parse_str(id.trim()).is_ok() {
            return Ok(id.trim().to_owned());
        }
    }
    tokio::fs::create_dir_all(&state.image_storage.root)
        .await
        .map_err(|e| GatewayError::Internal(e.to_string()))?;
    let id = uuid::Uuid::new_v4().to_string();
    use tokio::io::AsyncWriteExt;
    match tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .await
    {
        Ok(mut f) => {
            f.write_all(id.as_bytes())
                .await
                .map_err(|e| GatewayError::Internal(e.to_string()))?;
            f.sync_all()
                .await
                .map_err(|e| GatewayError::Internal(e.to_string()))?;
            Ok(id)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            for _ in 0..20 {
                if let Ok(s) = tokio::fs::read_to_string(&path).await {
                    if uuid::Uuid::parse_str(s.trim()).is_ok() {
                        return Ok(s.trim().to_owned());
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(GatewayError::Internal(
                "Invalid image storage node identity".into(),
            ))
        }
        Err(e) => Err(GatewayError::Internal(e.to_string())),
    }
}
pub(crate) fn spawn_worker(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let owner = uuid::Uuid::new_v4().to_string();
        let mut running = tokio::task::JoinSet::new();
        let mut last_replay = std::time::Instant::now() - Duration::from_secs(10);
        loop {
            while running.try_join_next().is_some() {}
            if let Some(repo) = state.data.image_tasks() {
                let next = async {
                    let config = settings(&state).await?;
                    let node = node_id(&state).await?;
                    if last_replay.elapsed() >= Duration::from_secs(5) {
                        replay_outbox(&state).await;
                        last_replay = std::time::Instant::now();
                    }
                    repo.reconcile_finished()
                        .await
                        .map_err(|e| GatewayError::Internal(e.to_string()))?;
                    repo.claim(
                        &node,
                        &owner,
                        i64::from(config.concurrency),
                        i64::from(config.max_queue_seconds),
                    )
                    .await
                    .map_err(|e| GatewayError::Internal(e.to_string()))
                }
                .await;
                match next {
                    Ok(Some(task)) => {
                        let state = state.clone();
                        let owner = owner.clone();
                        running.spawn(async move {
                            execute(state, owner, task).await;
                        });
                        continue;
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!(error=?e,"image queue scheduling failed"),
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
}
async fn run(state: &AppState, task: &Value) -> Result<(bool, Value), String> {
    let id = task["task_id"].as_str().ok_or("Missing task ID")?;
    let snapshot =
        serde_json::from_value::<crate::image_storage::ImageRequest>(task["snapshot"].clone())
            .map_err(|e| e.to_string())?;
    let slot = crate::image_storage::ImageRequestSlot(Arc::new(Mutex::new(Some(snapshot))));
    let response = if let Some(encrypted) = task["recovery"].as_str() {
        let recovery = crate::handlers::shared::decrypt_system_config_secret(state, id, encrypted)
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .ok_or("Image recovery checkpoint cannot be decrypted")?;
        if let Some(flow) = super::flow::current() {
            let mut s = flow.0.lock().unwrap_or_else(|e| e.into_inner());
            s.resuming = true;
            s.recovery = Some(recovery.clone());
            s.attempts = task["attempt_count"].as_u64().unwrap_or(1);
            s.switches = task["switch_count"].as_u64().unwrap_or(0);
            s.request_id = task["request_id"].as_str().map(str::to_owned);
        }
        let e = &recovery["execution"];
        let plan = serde_json::from_value::<aether_contracts::ExecutionPlan>(e["plan"].clone())
            .map_err(|e| e.to_string())?;
        let path = e["path"].as_str().ok_or("Missing recovery path")?;
        let decision = crate::control::GatewayControlDecision::synthetic(
            path,
            Some("ai_public".into()),
            Some("image".into()),
            None,
            Some("openai:image".into()),
        );
        let response = crate::execution_runtime::sync::execute_execution_runtime_sync(
            state,
            path,
            plan,
            id,
            &decision,
            e["plan_kind"]
                .as_str()
                .ok_or("Missing recovery plan kind")?,
            e["report_kind"].as_str().map(str::to_owned),
            e.get("report_context").filter(|v| !v.is_null()).cloned(),
        )
        .await
        .map_err(|e| format!("{e:?}"))?
        .ok_or("Upstream image recovery failed; no new generation was submitted")?;
        crate::image_storage::transform_response(state.clone(), slot, response)
            .await
            .map_err(|e| format!("{e:?}"))?
    } else {
        let credential = crate::handlers::shared::decrypt_system_config_secret(
            state,
            id,
            task["credential"]
                .as_str()
                .ok_or("Missing worker credential")?,
        )
        .ok_or("Worker credential cannot be decrypted")?;
        let (body, ct) =
            inputs::restore(&state.image_storage.root, id, task["request"].clone()).await?;
        let mut request = Request::builder()
            .method("POST")
            .uri(task["path"].as_str().ok_or("Missing request path")?)
            .header("authorization", credential)
            .header("content-type", ct)
            .header(crate::constants::TRACE_ID_HEADER, id)
            .body(Body::from(body))
            .map_err(|e| e.to_string())?;
        request.extensions_mut().insert(slot);
        crate::handlers::proxy::proxy_request(
            State(state.clone()),
            ConnectInfo("127.0.0.1:0".parse().unwrap()),
            request,
        )
        .await
        .map_err(|e| format!("{e:?}"))?
    };
    let success = response.status().is_success();
    let bytes = to_bytes(response.into_body(), 256 * 1024 * 1024)
        .await
        .map_err(|e| e.to_string())?;
    let result =
        serde_json::from_slice::<Value>(&bytes).map_err(|_| "Image response was not JSON")?;
    Ok((
        success
            && (result.get("error").is_none_or(Value::is_null)
                || crate::image_storage::is_content_refusal(&result)),
        result,
    ))
}
async fn execute(state: AppState, owner: String, task: Value) {
    let Some(repo) = state.data.image_tasks() else {
        return;
    };
    let Some(id) = task["task_id"].as_str().map(str::to_owned) else {
        return;
    };
    let flow = ImageFlow(Arc::new(Mutex::new(FlowState {
        task: Some((repo.clone(), id.clone(), owner.clone())),
        request_id: Some(id.clone()),
        ..Default::default()
    })));
    let work = crate::request_diagnostics::scope_request_diagnostics(async {
        crate::request_diagnostics::record_request_accepted_at(std::time::Instant::now());
        super::flow::scope(
            Some(flow.clone()),
            tokio::time::timeout(Duration::from_secs(1800), run(&state, &task)),
        )
        .await
    });
    tokio::pin!(work);
    let mut renew = tokio::time::interval(Duration::from_secs(20));
    let mut last_lease = std::time::Instant::now();
    let result = loop {
        tokio::select! {
            result=&mut work=>break result,
            _=renew.tick()=>{
                match tokio::time::timeout(Duration::from_secs(10),repo.renew(&id,&owner)).await.unwrap_or_else(|_|Err(aether_data::DataLayerError::InvalidInput("Image lease renewal timed out".into()))) {
                    Ok(true)=>{last_lease=std::time::Instant::now();let request=flow.0.lock().unwrap_or_else(|e|e.into_inner()).request_id.clone();if let Some(request)=request{let _=tokio::time::timeout(Duration::from_secs(5),repo.renew_reservations(&request)).await;}},
                    Ok(false)=>{tracing::warn!(task_id=%id,"image task lease lost; stopping executor");return;},
                    Err(e)=>{tracing::warn!(task_id=%id,error=%e,"image task lease renewal unavailable");if last_lease.elapsed()>=Duration::from_secs(75){return;}}
                }
            }
        }
    };
    let (mut success, mut body) = match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => (
            false,
            json!({"error":{"code":"execution_failed","message":e}}),
        ),
        Err(_) => (
            false,
            json!({"error":{"code":"execution_timeout","message":"Image task execution exceeded 30 minutes"}}),
        ),
    };
    let (attempts, switches, last, request_id) = {
        let s = flow.0.lock().unwrap_or_else(|e| e.into_inner());
        (
            s.attempts,
            s.switches,
            s.attempt_started.map(|t| t.elapsed().as_millis() as u64),
            s.request_id.clone(),
        )
    };
    let refused = crate::image_storage::is_content_refusal(&body);
    if success
        && !refused
        && !body["data"].as_array().is_some_and(|items| {
            !items.is_empty() && items.iter().all(|i| i.get("$image_ref").is_some())
        })
    {
        success = false;
        body = json!({"error":{"code":"missing_image_result","message":"Upstream returned no durable image results"}});
    }
    let terminal = match flow.terminal_event(success) {
        Ok(Some(mut event)) => {
            if !success {
                if let Some(message) = body.pointer("/error/message").and_then(Value::as_str) {
                    event.data.error_message =
                        Some(crate::request_diagnostics::safe_failure_summary(message));
                }
            }
            event.data.request_body = None;
            event.data.provider_request_body = None;
            event.data.response_body = None;
            event.data.client_response_body = None;
            match serde_json::to_string(&event).ok().and_then(|raw| {
                crate::handlers::shared::encrypt_system_config_secret(&state, &id, &raw)
            }) {
                Some(encrypted) => Some(encrypted),
                None => {
                    tracing::error!(task_id=%id,"image terminal event encryption failed");
                    return;
                }
            }
        }
        Ok(None) => None,
        Err(e) => {
            tracing::error!(task_id=%id,error=%e,"image terminal construction failed");
            return;
        }
    };
    let mut patch = json!({"attempt_count":attempts,"switch_count":switches,"last_attempt_ms":last,"request_id":request_id,"error":if success{Value::Null}else{body.get("error").cloned().unwrap_or(json!({"code":"execution_failed","message":"Image execution failed"}))},"billing_state":if request_id.is_some(){"pending_settlement"}else{"not_charged"},"actual_cost":0});
    patch["terminal_event"] = json!(terminal);
    if success {
        patch["stage"] = json!("completed");
        patch["progress"] = json!(100);
        patch["outcome"] = json!(if refused {
            "content_refused"
        } else {
            "image_generated"
        });
        if refused {
            patch["result"] = body;
        } else {
            patch["result_refs"] = body;
        }
    }
    // Retain the completed result while transient DB failures are retried. Never
    // re-enter generation after an unknown commit outcome.
    loop {
        let finished = tokio::time::timeout(
            Duration::from_secs(10),
            repo.finish(
                &id,
                &owner,
                if success { "succeeded" } else { "failed" },
                &patch,
                request_id.is_none(),
            ),
        )
        .await;
        match finished {
            Ok(Ok(true)) => {
                replay_outbox(&state).await;
                let _ = tokio::fs::remove_dir_all(
                    state.image_storage.root.join("task-inputs").join(&id),
                )
                .await;
                break;
            }
            Ok(Ok(false)) => break, // A committed terminal state or a new fenced owner.
            _ => {
                tracing::warn!(task_id=%id,"retrying image completion persistence");
                match tokio::time::timeout(Duration::from_secs(10), repo.renew(&id, &owner)).await {
                    Ok(Ok(true)) => last_lease = std::time::Instant::now(),
                    Ok(Ok(false)) => break,
                    _ if last_lease.elapsed() >= Duration::from_secs(75) => break,
                    _ => {}
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

async fn replay_outbox(state: &AppState) {
    let Some(repo) = state.data.image_tasks() else {
        return;
    };
    let tasks = match repo.terminal_outbox().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error=%e,"image outbox read failed");
            return;
        }
    };
    for task in tasks {
        let (Some(id), Some(encrypted)) =
            (task["task_id"].as_str(), task["terminal_event"].as_str())
        else {
            continue;
        };
        let event = crate::handlers::shared::decrypt_system_config_secret(state, id, encrypted)
            .and_then(|raw| serde_json::from_str::<aether_usage_runtime::UsageEvent>(&raw).ok());
        if let Some(event) = event {
            state
                .usage_runtime
                .record_terminal_event_direct(state.usage_lifecycle_data_state().as_ref(), event)
                .await;
        } else {
            tracing::error!(task_id=%id,"image terminal outbox cannot be decrypted");
        }
    }
}
