use super::{ApiError, ApiResult, ServiceState, manage, owner};
use crate::config_setup::{
    self, SetupConfigRequest, SetupDiscoverModelsRequest, SetupProviderAuthCompleteRequest,
    SetupProviderAuthRunRequest, SetupProviderAuthStartRequest,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{get, post},
};
use serde_json::{Value, json};

pub(super) fn routes() -> Router<ServiceState> {
    Router::new()
        .route("/v1/access", get(access))
        .route("/v1/config/readiness", get(readiness))
        .route("/v1/config", get(config).put(save_config))
        .route("/v1/config/preview", post(preview_config))
        .route("/v1/config/models/discover", post(discover_models))
        .route("/v1/config/auth/run", post(auth_run))
        .route("/v1/config/auth/start", post(auth_start))
        .route("/v1/config/auth/complete", post(auth_complete))
        .route("/v1/logs", get(logs))
        .route("/v1/actors/{actor}/usage", get(usage))
}
async fn access(State(state): State<ServiceState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    Ok(Json(
        json!({"kind": "owner", "manage": true, "session_id":super::auth::client_session_id(&state, &headers).await,"account":super::auth::account_metadata(&state,&headers).await.map(|account|json!({"account_id":account.account_id,"name":account.name,"revision":account.revision}))}),
    ))
}
fn readiness_json(report: config_setup::ConfigReadinessReport) -> Value {
    json!({"kind": report.kind, "port": report.port, "recovered": report.recovery_note.is_some(), "ready": report.is_complete()})
}
async fn readiness(
    State(state): State<ServiceState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    Ok(Json(readiness_json(config_setup::readiness_for_config(
        &state.system.config(),
    ))))
}
async fn config(State(state): State<ServiceState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    let config = config_setup::setup_config_from_config(&state.system.config());
    let mut value = serde_json::to_value(config).map_err(|_| ApiError::internal())?;
    if let Some(providers) = value.get_mut("providers").and_then(Value::as_array_mut) {
        for provider in providers {
            let configured = provider
                .get("api_key")
                .and_then(Value::as_str)
                .is_some_and(|key| !key.is_empty());
            provider["api_key"] = Value::Null;
            provider["credential_configured"] = Value::Bool(configured);
        }
    }
    Ok(Json(value))
}
fn preserve_credentials(request: &mut SetupConfigRequest, config: &crate::config::Config) {
    if request
        .providers
        .iter()
        .all(|provider| provider.api_key.is_some())
    {
        return;
    }
    let existing = config_setup::setup_config_from_config(config);
    for provider in &mut request.providers {
        if provider.api_key.is_none()
            && let Some(old) = existing.providers.iter().find(|old| {
                old.name
                    == provider
                        .credential_name
                        .as_deref()
                        .unwrap_or(&provider.name)
                    && old.kind == provider.kind
            })
        {
            provider.api_key = old.api_key.clone();
        }
    }
}
async fn save_config(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(mut request): Json<SetupConfigRequest>,
) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    preserve_credentials(&mut request, &state.system.config());
    let candidate = config_setup::prepare_setup_config_with_base(&request, state.system.config())
        .map_err(|_| ApiError::invalid())?;
    state
        .system
        .validate_config_update(&candidate)
        .map_err(ApiError::from_report)?;
    let persistent = candidate.clone();
    let system = state.system.clone();
    let account_host = state.stage.as_ref().map(|runtime| runtime.host.clone());
    state
        .system
        .update_config_with(candidate, async move {
            crate::accounts::configuration::persist_device_config(&system, &persistent)?;
            if let Some(host) = account_host {
                host.configurations.publish(&host, &persistent)?;
            }
            Ok(())
        })
        .await
        .map_err(ApiError::from_report)?;
    Ok(Json(readiness_json(config_setup::readiness_for_config(
        &state.system.config(),
    ))))
}
async fn preview_config(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(mut request): Json<SetupConfigRequest>,
) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    preserve_credentials(&mut request, &state.system.config());
    Ok(Json(readiness_json(
        config_setup::preview_setup_config(request)
            .await
            .map_err(|_| ApiError::invalid())?,
    )))
}
async fn discover_models(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(mut request): Json<SetupDiscoverModelsRequest>,
) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    if request.provider.api_key.is_none() {
        let existing = config_setup::setup_config_from_config(&state.system.config());
        if let Some(old) = existing.providers.iter().find(|provider| {
            provider.name
                == request
                    .provider
                    .credential_name
                    .as_deref()
                    .unwrap_or(&request.provider.name)
                && provider.kind == request.provider.kind
                && provider
                    .base_url
                    .as_deref()
                    .map(crate::config::normalize_provider_base_url)
                    == request
                        .provider
                        .base_url
                        .as_deref()
                        .map(crate::config::normalize_provider_base_url)
        }) {
            request.provider.api_key = old.api_key.clone();
        }
    }
    Ok(Json(
        serde_json::to_value(
            config_setup::discover_setup_models(request)
                .await
                .map_err(|_| ApiError::unavailable())?,
        )
        .map_err(|_| ApiError::internal())?,
    ))
}
async fn auth_run(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(request): Json<SetupProviderAuthRunRequest>,
) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    Ok(Json(
        serde_json::to_value(
            config_setup::run_setup_provider_auth(request)
                .await
                .map_err(|_| ApiError::unavailable())?,
        )
        .map_err(|_| ApiError::internal())?,
    ))
}
async fn auth_start(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(request): Json<SetupProviderAuthStartRequest>,
) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    let (response, flow) = config_setup::start_setup_provider_auth(request)
        .await
        .map_err(|_| ApiError::unavailable())?;
    let mut flows = state.auth_flows.lock().await;
    flows.retain(|_, flow| !flow.is_expired());
    if flows.len() >= 16 {
        return Err(ApiError::invalid());
    }
    flows.insert(flow.flow_id().to_string(), flow);
    Ok(Json(
        serde_json::to_value(response).map_err(|_| ApiError::internal())?,
    ))
}
async fn auth_complete(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(request): Json<SetupProviderAuthCompleteRequest>,
) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    let flow = state
        .auth_flows
        .lock()
        .await
        .remove(&request.flow_id)
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(
        serde_json::to_value(
            config_setup::complete_setup_provider_auth(request, flow)
                .await
                .map_err(|_| ApiError::unavailable())?,
        )
        .map_err(|_| ApiError::internal())?,
    ))
}
async fn logs(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Query(query): Query<crate::daemon::logs::LogReadQuery>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    if query.limit.is_some_and(|limit| limit == 0 || limit > 200) {
        return Err(ApiError::invalid());
    }
    let paths = crate::daat_locus_paths::daat_locus_paths().await;
    let read = crate::daemon::logs::read_log_path(
        &paths.logs_file(crate::logging::DAEMON_LOG_FILE_NAME),
        &query,
    )
    .await
    .map_err(|_| ApiError::not_found())?;
    let mut value = serde_json::to_value(read).map_err(|_| ApiError::internal())?;
    super::redact(&mut value, &state.system.config());
    Ok(Json(value))
}

async fn usage(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(actor): Path<String>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    let actor = state.system.actor(&actor).map_err(ApiError::from_report)?;
    Ok(Json(
        serde_json::to_value(actor.context.usage).map_err(|_| ApiError::internal())?,
    ))
}
