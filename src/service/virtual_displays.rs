use super::{ApiError, ApiResult, ServiceState, owner};
use crate::computer_use_app::preview;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};

pub(super) fn routes() -> Router<ServiceState> {
    Router::new()
        .route(
            "/v1/device/virtual-displays/capabilities",
            get(capabilities),
        )
        .route("/v1/device/virtual-displays", get(catalog))
        .route("/v1/device/virtual-displays/{id}/frame", get(frame))
}
async fn capabilities(
    State(state): State<ServiceState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    owner(&state, &headers).await?;
    Ok(Json(
        serde_json::json!({"supported":cfg!(any(windows, target_os="android", target_os="macos"))}),
    ))
}
async fn catalog(
    State(state): State<ServiceState>,
    headers: HeaderMap,
) -> ApiResult<Json<preview::Catalog>> {
    owner(&state, &headers).await?;
    let system = state.system.clone();
    let catalog = tokio::task::spawn_blocking(move || preview::catalog(&system))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::from_report)?;
    owner(&state, &headers).await?;
    Ok(Json(catalog))
}
async fn frame(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    owner(&state, &headers).await?;
    if id.len() > 128 {
        return Err(ApiError::not_found());
    }
    let system = state.system.clone();
    let frame = tokio::task::spawn_blocking(move || {
        static CAPTURE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
        let _capture = CAPTURE.try_lock().ok_or_else(ApiError::unavailable)?;
        let catalog = preview::catalog(&system).map_err(ApiError::from_report)?;
        if !catalog.supported {
            return Err(ApiError::not_found());
        }
        if catalog.state != preview::State::Ready {
            return Err(ApiError::unavailable());
        }
        if !catalog.displays.iter().any(|display| display.id == id) {
            return Err(ApiError::not_found());
        }
        preview::frame(&system, &id).map_err(|error| {
            tracing::debug!(%error, "Virtual display frame capture failed");
            ApiError::from_report(error)
        })
    })
    .await
    .map_err(|_| ApiError::internal())??;
    owner(&state, &headers).await?;
    let Some(bytes) = frame else {
        return Ok((StatusCode::NO_CONTENT, [("cache-control", "no-store")]).into_response());
    };
    if bytes.len() > 32 * 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(ApiError::internal());
    }
    Ok((
        [
            ("content-type", "image/png"),
            ("cache-control", "no-store"),
            ("x-content-type-options", "nosniff"),
        ],
        bytes,
    )
        .into_response())
}
