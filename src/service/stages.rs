//! Owner operations are confined to this service's account/Actor store.
use super::{ApiError, ApiResult, ServiceState, manage, owner};
use crate::stage::{StageError, StageId, host::Settings, ledger::StageLedger, runtime::Runtime};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
pub(super) fn routes() -> Router<ServiceState> {
    Router::new()
        .route("/v1/stage", get(info))
        .route("/v1/stage/status", get(status))
        .route("/v1/stage/renew", post(renew))
        .route("/v1/stage/settings", get(settings).patch(save_settings))
        .route("/v1/stage/devices", get(devices))
        .route("/v1/stage/devices/{stage}", axum::routing::delete(revoke))
        .route("/v1/stages/{stage}/skills", get(skills))
}
async fn skills(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(stage): Path<StageId>,
    Query(query): Query<crate::super_skills_app::ListSkillsArgs>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    let runtime = runtime(&state)?;
    if stage != runtime.host.local {
        return Err(ApiError::unavailable());
    }
    let context = state
        .system
        .owner_app_context()
        .map_err(ApiError::from_report)?;
    let page = crate::super_skills_app::SuperSkillsApp::new(state.system.clone())
        .list(&query, &context)
        .map_err(ApiError::from_report)?;
    Ok(Json(
        serde_json::to_value(page).map_err(|_| ApiError::internal())?,
    ))
}
pub(super) fn runtime(state: &ServiceState) -> ApiResult<Arc<Runtime>> {
    state.stage.clone().ok_or_else(ApiError::unavailable)
}
async fn renew(State(state): State<ServiceState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    let runtime = runtime(&state)?;
    let _lifecycle = runtime.host.lifecycle.lock().await;
    let enrollment = runtime.host.renew_account_authority().map_err(failure)?;
    runtime.relay_changed();
    Ok(Json(json!({"enrollment":enrollment})))
}
pub(super) fn failure(error: StageError) -> ApiError {
    match error {
        StageError::Forbidden(_) | StageError::ApprovalDenied(_) => ApiError::forbidden(),
        StageError::Invalid(_) => ApiError::invalid(),
        StageError::Conflict(_) => ApiError {
            status: axum::http::StatusCode::CONFLICT,
            code: "conflict",
            message: "Device state conflicts with the request",
        },
        StageError::Capacity(_) => ApiError {
            status: axum::http::StatusCode::TOO_MANY_REQUESTS,
            code: "backlog_full",
            message: "Device resource budget reached",
        },
        StageError::Unavailable(_) => ApiError {
            status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
            code: "device_unreachable",
            message: "Device is unreachable",
        },
        _ => ApiError::internal(),
    }
}
async fn info(State(state): State<ServiceState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    let runtime = runtime(&state)?;
    let host = &runtime.host;
    let public_login = runtime.public_login_status().await;
    Ok(Json(
        json!({"stage_id":host.local,"account_id":host.account_id(),"name":host.accounts.current().ok().flatten().map(|record|record.name.clone()),"profile":host.profile(),"settings":host.settings(),"model_ready":state.system.config().validate().is_ok(),"public_login":public_login}),
    ))
}
// A bounded volatile status query; no configuration or conversation snapshots.
async fn status(State(state): State<ServiceState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    let runtime = runtime(&state)?;
    let public_login = runtime.public_login_status().await;
    Ok(Json(
        json!({"model_ready":state.system.config().validate().is_ok(),"public_login": public_login,
        "work_generation": crate::stage::reset::generation(&runtime.host).map_err(failure)?}),
    ))
}
async fn settings(
    State(state): State<ServiceState>,
    headers: HeaderMap,
) -> ApiResult<Json<Settings>> {
    owner(&state, &headers).await?;
    Ok(Json(runtime(&state)?.host.settings()))
}
async fn save_settings(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(input): Json<DevicePreferences>,
) -> ApiResult<Json<Settings>> {
    manage(&state, &headers).await?;
    let runtime = runtime(&state)?;
    runtime
        .patch_settings(input.label, input.relay_origin, input.mode)
        .await
        .map_err(failure)?;
    Ok(Json(runtime.host.settings()))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DevicePreferences {
    label: Option<String>,
    #[serde(default, deserialize_with = "nullable_origin")]
    relay_origin: Option<Option<String>>,
    mode: Option<crate::stage::host::Connectivity>,
}
fn nullable_origin<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}
#[derive(Deserialize)]
struct List {
    after: Option<StageId>,
    limit: Option<usize>,
}
async fn devices(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Query(query): Query<List>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    let runtime = runtime(&state)?;
    let limit = super::bounded(query.limit, 64)?;
    if runtime.host.account_id().is_none() {
        return Ok(Json(json!({"items":[],"next_cursor":null})));
    }
    let mut devices = StageLedger::attach(runtime.host.system.store(), runtime.host.local)
        .peers(chrono::Utc::now().timestamp_millis())
        .map_err(failure)?
        .into_iter()
        .filter(|peer| query.after.is_none_or(|after| peer.device.stage_id > after))
        .collect::<Vec<_>>();
    devices.sort_by_key(|peer| peer.device.stage_id);
    let next = (devices.len() > limit).then(|| devices[limit - 1].device.stage_id);
    devices.truncate(limit);
    Ok(Json(
        json!({"items":devices.into_iter().map(|peer|json!({"stage_id":peer.device.stage_id,"label":runtime.host.device_label(peer.device.stage_id).ok().flatten(),"connection":runtime.state(peer.device.stage_id),"error_code":runtime.connection_error_code(peer.device.stage_id)})).collect::<Vec<_>>(),"next_cursor":next}),
    ))
}
async fn revoke(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(stage): Path<StageId>,
) -> ApiResult<axum::http::StatusCode> {
    manage(&state, &headers).await?;
    let runtime = runtime(&state)?;
    let change = runtime.host.revoke_account_device(stage).map_err(failure)?;
    for peer in StageLedger::attach(runtime.host.system.store(), runtime.host.local)
        .peers(chrono::Utc::now().timestamp_millis())
        .map_err(failure)?
    {
        if let Ok(handle) = runtime.handle(peer.device.stage_id) {
            handle.refresh_members();
        }
    }
    if let Some(signal) = runtime.relay_tx.read().clone() {
        let _ = signal.try_send(crate::stage::relay::Command::Revoke(change));
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}
