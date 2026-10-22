//! Shared image execution context: billing is committed only after the final image is ready.
use crate::AppState;
use aether_usage_runtime::{
    StreamTerminalUsagePayloadSeed, SyncTerminalUsagePayloadSeed, TerminalUsageContextSeed,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub(crate) struct ImageFlow(pub Arc<Mutex<FlowState>>);
#[derive(Default)]
pub(crate) struct FlowState {
    pub task: Option<(
        aether_data::driver::postgres::SqlxImageTaskRepository,
        String,
        String,
    )>,
    pub terminal: Option<DeferredTerminal>,
    pub completion: Option<bool>,
    pub emitted: bool,
    pub attempts: u64,
    pub switches: u64,
    pub last_key_id: Option<String>,
    pub attempt_started: Option<std::time::Instant>,
    pub refusal: Option<String>,
    pub request_id: Option<String>,
    pub execution: Option<Value>,
    pub recovery: Option<Value>,
    pub resuming: bool,
}
tokio::task_local! { static IMAGE_FLOW: ImageFlow; }
pub(crate) fn current() -> Option<ImageFlow> {
    IMAGE_FLOW.try_with(Clone::clone).ok()
}
pub(crate) fn scope<F: std::future::Future>(
    flow: Option<ImageFlow>,
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    // Routing futures contain large request/attempt state. Keep that state on the
    // heap before wrapping it in task-local and cancellation scopes.
    let future = Box::pin(future);
    async move {
        match flow {
            Some(flow) => IMAGE_FLOW.scope(flow, future).await,
            None => future.await,
        }
    }
}
pub(crate) fn defer(
    context: TerminalUsageContextSeed,
    payload: SyncTerminalUsagePayloadSeed,
) -> bool {
    let Some(flow) = current() else {
        return false;
    };
    let mut state = flow.0.lock().unwrap_or_else(|e| e.into_inner());
    state.request_id = Some(context.request_id.clone());
    if !state.emitted {
        state.terminal = Some(DeferredTerminal::Sync(context, payload));
    }
    true
}
pub(crate) async fn stage(stage: &str, progress: u8) {
    let Some(flow) = current() else {
        return;
    };
    let task = flow
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .task
        .clone();
    if let Some((repo, id, owner)) = task {
        if let Err(e) = repo
            .progress(&id, &owner, &json!({"stage":stage,"progress":progress}))
            .await
        {
            tracing::warn!(task_id=%id,error=%e,"failed to persist image stage");
        }
    }
}
pub(crate) async fn attempt(request_id: &str, key_id: &str) {
    let Some(flow) = current() else {
        return;
    };
    let (task, patch) = {
        let mut s = flow.0.lock().unwrap_or_else(|e| e.into_inner());
        let last = s.attempt_started.map(|t| t.elapsed().as_millis() as u64);
        if s.resuming {
            return;
        }
        s.recovery = None;
        if s.last_key_id.as_deref().is_some_and(|id| id != key_id) {
            s.switches += 1;
        }
        s.last_key_id = Some(key_id.to_owned());
        s.attempts += 1;
        s.attempt_started = Some(std::time::Instant::now());
        s.request_id = Some(request_id.to_owned());
        (
            s.task.clone(),
            json!({"stage":"generating","progress":20,"request_id":request_id,"attempt_count":s.attempts,"switch_count":s.switches,"last_attempt_ms":last,"recovery":null}),
        )
    };
    if let Some((repo, id, owner)) = task {
        let _ = repo.progress(&id, &owner, &patch).await;
    }
}
pub(crate) fn is_async() -> bool {
    current().is_some_and(|f| f.0.lock().unwrap_or_else(|e| e.into_inner()).task.is_some())
}
pub(crate) fn content_refusal(reason: &str) {
    if let Some(f) = current() {
        f.0.lock().unwrap_or_else(|e| e.into_inner()).refusal = Some(reason.to_owned());
    }
}
pub(crate) enum DeferredTerminal {
    Sync(TerminalUsageContextSeed, SyncTerminalUsagePayloadSeed),
    Stream(
        TerminalUsageContextSeed,
        StreamTerminalUsagePayloadSeed,
        bool,
    ),
}
/// Explicitly propagate the image context across transport producer tasks.
pub(crate) fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let flow = current();
    tokio::spawn(async move { scope(flow, future).await })
}
pub(crate) async fn defer_stream_for(
    flow: Option<ImageFlow>,
    state: &AppState,
    context: TerminalUsageContextSeed,
    payload: StreamTerminalUsagePayloadSeed,
    void: bool,
) -> bool {
    let Some(flow) = flow else {
        return false;
    };
    let completion = {
        let mut s = flow.0.lock().unwrap_or_else(|e| e.into_inner());
        if s.emitted {
            return true;
        }
        s.request_id = Some(context.request_id.clone());
        s.terminal = Some(DeferredTerminal::Stream(context, payload, void));
        s.completion
    };
    if let Some(success) = completion {
        flow.flush(state, success).await;
    }
    true
}
impl ImageFlow {
    pub(crate) fn terminal_event(
        &self,
        succeeded: bool,
    ) -> Result<Option<aether_usage_runtime::UsageEvent>, String> {
        let (terminal, task, refusal) = {
            let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if s.emitted {
                return Ok(None);
            }
            let terminal = s.terminal.take();
            if terminal.is_some() {
                s.emitted = true;
            }
            (terminal, s.task.clone(), s.refusal.clone())
        };
        let Some(mut terminal) = terminal else {
            return Ok(None);
        };
        let context = match &mut terminal {
            DeferredTerminal::Sync(c, _) | DeferredTerminal::Stream(c, _, _) => c,
        };
        if let Some((_, id, _)) = task {
            context.request_metadata.get_or_insert_with(|| json!({}))["image_task_id"] = json!(id);
        }
        if succeeded {
            if let Some(reason) = refusal {
                let metadata = context.request_metadata.get_or_insert_with(|| json!({}));
                metadata["response_outcome"] = json!("content_refused");
                metadata["content_refusal_reason"] = json!(reason);
                metadata["usage_available"] = json!(false);
                metadata["usage_pricing_available"] = json!(false);
            }
        }
        let seed = match terminal {
            DeferredTerminal::Sync(context, mut payload) => {
                if !succeeded {
                    payload.status_code = 502;
                    payload.report_kind = payload.report_kind.replace("success", "error");
                    payload.client_response = Some(
                        json!({"error":{"code":"image_result_failed","message":"Final image preparation failed"}}),
                    );
                }
                aether_usage_runtime::build_sync_terminal_usage_seed(context, payload)
            }
            DeferredTerminal::Stream(context, mut payload, void) => {
                if !succeeded {
                    payload.status_code = 502;
                    payload.terminal_error_message =
                        Some("Final image preparation failed or delivery was interrupted".into());
                    payload.report_kind = payload.report_kind.replace("success", "error");
                }
                aether_usage_runtime::build_stream_terminal_usage_seed(
                    context,
                    payload,
                    void || !succeeded,
                )
            }
        };
        aether_usage_runtime::build_terminal_usage_event_from_seed(seed)
            .map(Some)
            .map_err(|e| e.to_string())
    }
    pub(crate) fn flush<'a>(
        &'a self,
        state: &'a AppState,
        succeeded: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        // Settlement includes database and queue fallbacks. Keep its future out
        // of every routing and stream-attempt state machine, including non-image
        // routes that share the same executor.
        Box::pin(async move {
            {
                let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
                // Failure wins if body cancellation races with a late transport terminal.
                s.completion = Some(s.completion.unwrap_or(true) && succeeded);
            }
            let succeeded = self
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .completion
                .unwrap_or(false);
            match self.terminal_event(succeeded) {
                Ok(Some(event)) => {
                    state
                        .usage_runtime
                        .submit_terminal_event(state.usage_lifecycle_data_state().as_ref(), event)
                        .await
                }
                Err(e) => tracing::error!(error=%e,"image terminal usage construction failed"),
                _ => {}
            }
        })
    }
}
/// Dropping a streaming body before completion releases the original reservation.
pub(crate) struct DeliveryGuard {
    state: AppState,
    flow: Option<ImageFlow>,
}
impl DeliveryGuard {
    pub(crate) fn new(state: &AppState) -> Self {
        Self {
            state: state.clone(),
            flow: current(),
        }
    }
    pub(crate) async fn complete(&mut self, success: bool) {
        if let Some(flow) = self.flow.take() {
            flow.flush(&self.state, success).await;
        }
    }
}
impl Drop for DeliveryGuard {
    fn drop(&mut self) {
        if let Some(flow) = self.flow.take() {
            let state = self.state.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    flow.flush(&state, false).await;
                });
            }
        }
    }
}

/// Capture routing/billing context in memory; binary inputs never enter the checkpoint.
pub(crate) fn capture_execution(
    plan: &aether_contracts::ExecutionPlan,
    path: &str,
    kind: &str,
    report_kind: &Option<String>,
    context: Option<&Value>,
) {
    let Some(flow) = current() else {
        return;
    };
    let mut plan = plan.clone();
    if let Some(body) = plan.body.json_body.as_mut() {
        if let Some(o) = body.as_object_mut() {
            // Recovery only polls an existing Web conversation. Retain its
            // output options, never input images or arbitrary nested payloads.
            o.retain(|key, value| {
                matches!(
                    key.as_str(),
                    "operation"
                        | "model"
                        | "web_model"
                        | "prompt"
                        | "size"
                        | "ratio"
                        | "output_format"
                        | "quality"
                        | "partial_images"
                ) && !value.is_array()
                    && !value.is_object()
            });
        }
    }
    plan.body.body_bytes_b64 = None;
    plan.body.body_ref = None;
    let mut context = context.cloned();
    fn prune(v: &mut Value) {
        match v {
            Value::Object(o) => {
                o.retain(|k, _| {
                    !matches!(
                        k.as_str(),
                        "request_body"
                            | "provider_request_body"
                            | "response_body"
                            | "client_response_body"
                            | "images"
                            | "image"
                            | "mask"
                            | "input_image_mask"
                            | "b64_json"
                            | "body_bytes_b64"
                    )
                });
                for v in o.values_mut() {
                    prune(v)
                }
            }
            Value::Array(a) => {
                for v in a {
                    prune(v)
                }
            }
            _ => {}
        }
    }
    if let Some(c) = context.as_mut() {
        prune(c)
    }
    flow.0.lock().unwrap_or_else(|e| e.into_inner()).execution = Some(
        json!({"plan":plan,"path":path,"plan_kind":kind,"report_kind":report_kind,"report_context":context}),
    );
}
pub(crate) fn recovery() -> Option<Value> {
    current().and_then(|f| {
        f.0.lock()
            .unwrap_or_else(|e| e.into_inner())
            .recovery
            .clone()
    })
}
pub(crate) async fn checkpoint(state: &AppState, upstream: Value) -> Result<(), String> {
    let Some(flow) = current() else {
        return Ok(());
    };
    let (task, execution) = {
        let s = flow.0.lock().unwrap_or_else(|e| e.into_inner());
        (s.task.clone(), s.execution.clone())
    };
    let Some((repo, id, owner)) = task else {
        return Ok(());
    };
    let execution = execution.ok_or("Missing image recovery execution context")?;
    let checkpoint = json!({"execution":execution,"upstream":upstream});
    let encrypted = crate::handlers::shared::encrypt_system_config_secret(
        state,
        &id,
        &serde_json::to_string(&checkpoint).map_err(|e| e.to_string())?,
    )
    .ok_or("Image checkpoint encryption unavailable")?;
    if !repo
        .progress(&id, &owner, &json!({"recovery":encrypted}))
        .await
        .map_err(|e| e.to_string())?
    {
        return Err("Image execution lease lost".into());
    }
    Ok(())
}
