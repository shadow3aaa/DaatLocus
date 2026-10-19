use super::{ApiError, ApiResult, ServiceState, manage, owner};
use crate::computer_use_app::settings::{Patch, Status};
use axum::{Json, Router, extract::State, http::HeaderMap, routing::get};

pub(super) fn routes() -> Router<ServiceState> {
    Router::new()
        .route("/v1/device/computer-use", get(status).patch(configure))
        .route("/v1/device/computer-use/capabilities", get(capabilities))
}
async fn capabilities(
    State(state): State<ServiceState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    owner(&state, &headers).await?;
    Ok(Json(serde_json::json!({"supported": cfg!(windows)})))
}
async fn status(State(state): State<ServiceState>, headers: HeaderMap) -> ApiResult<Json<Status>> {
    owner(&state, &headers).await?;
    #[cfg(windows)]
    return tokio::task::spawn_blocking(move || {
        state
            .system
            .computer_workspace()
            .map(|workspace| Json(workspace.status()))
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(ApiError::from_report);
    #[cfg(not(windows))]
    Ok(Json(Status::unsupported()))
}
async fn configure(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(patch): Json<Patch>,
) -> ApiResult<Json<Status>> {
    manage(&state, &headers).await?;
    #[cfg(windows)]
    return tokio::task::spawn_blocking(move || {
        // Validation precedes all window/display effects.
        patch.validate().map_err(|_| ApiError::invalid())?;
        let workspace = state
            .system
            .computer_workspace()
            .map_err(ApiError::from_report)?;
        workspace
            .configure(patch)
            .map(Json)
            .map_err(ApiError::from_report)
    })
    .await
    .map_err(|_| ApiError::internal())?;
    #[cfg(not(windows))]
    {
        let _ = (state, patch);
        Err(ApiError::invalid())
    }
}
