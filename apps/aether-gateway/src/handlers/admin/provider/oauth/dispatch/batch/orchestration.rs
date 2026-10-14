use super::execution::{
    estimate_admin_provider_oauth_batch_import_total,
    execute_admin_provider_oauth_batch_import_for_provider_type,
};
use super::parse::{
    admin_provider_oauth_batch_contains_agent_identity,
    build_admin_provider_oauth_batch_import_response,
    parse_admin_provider_oauth_batch_import_request, AdminProviderOAuthBatchImportRequest,
};
use crate::handlers::admin::provider::oauth::errors::build_internal_control_error_response;
use crate::handlers::admin::provider::oauth::state::{
    admin_provider_oauth_template, build_admin_provider_oauth_backend_unavailable_response,
    is_fixed_provider_type_for_provider_oauth,
};
use crate::handlers::admin::provider::shared::paths::admin_provider_oauth_batch_import_provider_id;
use crate::handlers::admin::request::{AdminAppState, AdminRequestContext};
use crate::GatewayError;
use axum::{
    body::Bytes,
    http,
    response::{IntoResponse, Response},
    Json,
};

pub(in super::super) async fn handle_admin_provider_oauth_batch_import(
    state: &AdminAppState<'_>,
    request_context: &AdminRequestContext<'_>,
    request_body: Option<&Bytes>,
) -> Result<Response, GatewayError> {
    if !state.has_provider_catalog_data_reader() {
        return Ok(build_admin_provider_oauth_backend_unavailable_response());
    }
    let Some(provider_id) = admin_provider_oauth_batch_import_provider_id(request_context.path())
    else {
        return Ok(build_internal_control_error_response(
            http::StatusCode::NOT_FOUND,
            "Provider 不存在",
        ));
    };
    let payload = match parse_admin_provider_oauth_batch_import_request(request_body) {
        Ok(payload) => payload,
        Err(response) => return Ok(response),
    };
    execute_import(state, provider_id, payload, false).await
}

pub(crate) async fn import_pushed_accounts(
    state: &AdminAppState<'_>,
    provider_id: &str,
    credentials: String,
) -> Result<Response, GatewayError> {
    execute_import(
        state,
        provider_id.to_string(),
        AdminProviderOAuthBatchImportRequest {
            credentials,
            proxy_node_id: None,
        },
        true,
    )
    .await
}

async fn execute_import(
    state: &AdminAppState<'_>,
    provider_id: String,
    payload: AdminProviderOAuthBatchImportRequest,
    push: bool,
) -> Result<Response, GatewayError> {
    if admin_provider_oauth_batch_contains_agent_identity(&payload.credentials) {
        return Ok(build_internal_control_error_response(
            http::StatusCode::BAD_REQUEST,
            "Agent Identity JSON 必须使用专属导入接口",
        ));
    }

    let Some(provider) = state
        .read_provider_catalog_providers_by_ids(std::slice::from_ref(&provider_id))
        .await?
        .into_iter()
        .next()
    else {
        return Ok(build_internal_control_error_response(
            http::StatusCode::NOT_FOUND,
            "Provider 不存在",
        ));
    };
    let provider_type = provider.provider_type.trim().to_ascii_lowercase();
    if push && !matches!(provider_type.as_str(), "chatgpt_web" | "codex") {
        return Ok(build_internal_control_error_response(
            http::StatusCode::BAD_REQUEST,
            "ChatGPT2API 账号推送仅支持 ChatGPT Web 或 Codex Provider",
        ));
    }
    if !is_fixed_provider_type_for_provider_oauth(&provider_type) {
        return Ok(build_internal_control_error_response(
            http::StatusCode::BAD_REQUEST,
            "该 Provider 不是固定类型，无法使用 provider-oauth",
        ));
    }
    if provider_type != "kiro"
        && provider_type != "windsurf"
        && admin_provider_oauth_template(&provider_type).is_none()
    {
        return Ok(build_admin_provider_oauth_backend_unavailable_response());
    }

    let total = estimate_admin_provider_oauth_batch_import_total(
        &provider_type,
        payload.credentials.as_str(),
    );
    if total == 0 {
        return Ok(build_internal_control_error_response(
            http::StatusCode::BAD_REQUEST,
            "未找到有效的 Token 数据",
        ));
    }

    let outcome = execute_admin_provider_oauth_batch_import_for_provider_type(
        state,
        &provider_id,
        &provider_type,
        payload.credentials.as_str(),
        payload.proxy_node_id.as_deref(),
        push,
        None,
    )
    .await?;
    if push {
        let updated = outcome
            .results
            .iter()
            .filter(|item| item["status"] == "success" && item["replaced"] == true)
            .count();
        let errors: Vec<_> = outcome.results.iter().filter(|item| item["status"] == "error")
            .map(|item| serde_json::json!({"index":item["index"], "error":"账号导入失败，请检查凭据有效期、账号身份或提供商代理配置"})).collect();
        let status = if outcome.success == 0 {
            http::StatusCode::BAD_REQUEST
        } else {
            http::StatusCode::OK
        };
        return Ok((
            status,
            Json(serde_json::json!({
                "success": outcome.failed == 0,
                "total": outcome.total,
                "added": outcome.success.saturating_sub(updated),
                "updated": updated,
                "skipped": 0,
                "failed": outcome.failed,
                "refreshed": 0,
                "errors": errors,
                "items": [],
            })),
        )
            .into_response());
    }
    Ok(build_admin_provider_oauth_batch_import_response(&outcome).into_response())
}
