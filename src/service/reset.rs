//! Owner-only, account-bound reset; never exposed as an App operation.
use super::{ApiError, ApiResult, ServiceState, manage, owner, stages};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) fn routes() -> Router<ServiceState> {
    Router::new().route("/v1/account/reset", get(info).post(reset))
}
async fn info(State(state): State<ServiceState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    let runtime = stages::runtime(&state)?;
    let account = runtime
        .host
        .accounts
        .current()
        .map_err(ApiError::from_report)?
        .ok_or_else(ApiError::unavailable)?;
    Ok(Json(json!({
        "account_id": account.account_id,
        "name": account.name,
        "generation": crate::stage::reset::generation(&runtime.host).map_err(stages::failure)?,
        "resetting": crate::stage::reset::blocked(&runtime.host),
    })))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reset {
    account_id: String,
    expected_generation: Option<crate::stage::reset::Generation>,
}
async fn reset(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(input): Json<Reset>,
) -> ApiResult<StatusCode> {
    manage(&state, &headers).await?;
    let runtime = stages::runtime(&state)?;
    if runtime.host.account_id().as_deref() != Some(&input.account_id) {
        return Err(ApiError::invalid());
    }
    // Once accepted, finish the reset even if the browser disconnects midway.
    tokio::spawn(async move { runtime.reset_account_work(input.expected_generation).await })
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(stages::failure)?;
    Ok(StatusCode::NO_CONTENT)
}
