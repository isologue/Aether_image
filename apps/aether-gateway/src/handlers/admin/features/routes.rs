use super::{background_tasks, gemini_files, video_tasks};
use crate::handlers::admin::request::{AdminRouteRequest, AdminRouteResult};

pub(crate) async fn maybe_build_local_admin_features_response(
    request: AdminRouteRequest<'_>,
) -> AdminRouteResult {
    // Result reads include storage I/O; do not embed that future in every
    // unrelated admin route's dispatcher state machine.
    if let Some(response) = Box::pin(crate::image_tasks::admin(
        request.state().app(),
        request.request_context().method(),
        request.request_context().path(),
        request.request_context().query_string(),
    ))
    .await?
    {
        return Ok(Some(response));
    }
    if let Some(response) = background_tasks::maybe_build_local_admin_background_tasks_response(
        &request.state(),
        &request.request_context(),
        request.request_body(),
    )
    .await?
    {
        return Ok(Some(response));
    }

    if let Some(response) = video_tasks::maybe_build_local_admin_video_tasks_response(
        &request.state(),
        &request.request_context(),
    )
    .await?
    {
        return Ok(Some(response));
    }

    if let Some(response) = gemini_files::maybe_build_local_admin_gemini_files_response(
        &request.state(),
        &request.request_context(),
        request.request_body(),
    )
    .await?
    {
        return Ok(Some(response));
    }

    Ok(None)
}
