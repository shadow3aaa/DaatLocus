//! OPAQUE account login. Browser passwords never reach this service.
use super::{ApiError, ApiResult, ServiceState, mutation_origin};
use crate::accounts::{CredentialBundle, Metadata, login_error::LoginError, normalize_name};
use axum::{
    Json, Router,
    extract::{ConnectInfo, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use daat_locus_auth::{PendingLogin, Setup};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, net::SocketAddr};

fn login_error(error: LoginError) -> ApiError {
    let status = match error {
        LoginError::InvalidCredentials | LoginError::Expired | LoginError::AccountChanged => {
            StatusCode::UNAUTHORIZED
        }
        LoginError::Revoked | LoginError::ImportRejected => StatusCode::FORBIDDEN,
        LoginError::Protocol => StatusCode::BAD_GATEWAY,
        LoginError::TimedOut => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    // Display contains only the fixed public reason, never a transport/provider diagnostic.
    let message = match error {
        LoginError::ConnectionRequired => "Configure a connection service or use the local network",
        LoginError::RelayUnavailable => "Cannot reach the public connection service",
        LoginError::NoOnlineDevice => "No online account device found",
        LoginError::DeviceUnreachable => "Cannot connect to the account device",
        LoginError::TimedOut => "Account login timed out",
        LoginError::InvalidCredentials => "Incorrect account name or password",
        LoginError::Expired => "Login attempt expired",
        LoginError::Revoked => "Device account access was revoked",
        LoginError::AccountChanged => "Account credentials changed",
        LoginError::Protocol => "Account login protocol could not be verified",
        LoginError::ImportRejected => "Account enrollment could not be verified",
        LoginError::Busy => "Account login is busy",
        LoginError::Unavailable => "Local account service unavailable",
    };
    ApiError {
        status,
        code: error.code(),
        message,
    }
}

const SESSION_SECONDS: i64 = 12 * 60 * 60;
const ATTEMPT_MS: i64 = 120_000;
const MAX_PENDING: usize = 128;
const MAX_SESSIONS: usize = 4096;
pub(super) struct BrowserSession {
    pub account: Metadata,
    expires_at_ms: i64,
    pub public_id: String,
}
struct LoginAttempt {
    opaque: PendingLogin,
    account: Metadata,
    origin: String,
    expires: i64,
}
struct RemoteAttempt {
    login: crate::accounts::network::RemoteLogin,
    name: String,
    origin: String,
    expires: i64,
}
struct ImportAttempt {
    candidate: crate::accounts::network::Candidate,
    device: crate::stage::identity::JoinRequest,
    name: String,
    binding: String,
    origin: String,
    expires: i64,
}
struct RegistrationAttempt {
    setup: Setup,
    account_id: String,
    name: String,
    previous: Option<CredentialBundle>,
    origin: String,
    expires: i64,
}
pub(super) struct Authentication {
    sessions: HashMap<String, BrowserSession>,
    logins: HashMap<String, LoginAttempt>,
    registrations: HashMap<String, RegistrationAttempt>,
    remote_logins: HashMap<String, RemoteAttempt>,
    imports: HashMap<String, ImportAttempt>,
    attempts: HashMap<std::net::IpAddr, (i64, u32)>,
}
impl Authentication {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            logins: HashMap::new(),
            registrations: HashMap::new(),
            remote_logins: HashMap::new(),
            imports: HashMap::new(),
            attempts: HashMap::new(),
        }
    }
    fn budget(&mut self, peer: SocketAddr) -> ApiResult<()> {
        let now = now();
        self.sessions
            .retain(|_, session| session.expires_at_ms > now);
        self.logins.retain(|_, state| state.expires > now);
        self.registrations.retain(|_, state| state.expires > now);
        self.remote_logins.retain(|_, state| state.expires > now);
        self.imports.retain(|_, state| state.expires > now);
        self.attempts.retain(|_, (until, _)| *until > now);
        if self.attempts.len() >= 4096 && !self.attempts.contains_key(&peer.ip()) {
            return Err(ApiError::unavailable());
        }
        let bucket = self.attempts.entry(peer.ip()).or_insert((now + 60_000, 0));
        bucket.1 += 1;
        if bucket.1 > 20
            || self.logins.len()
                + self.registrations.len()
                + self.remote_logins.len()
                + self.imports.len()
                >= MAX_PENDING
        {
            return Err(ApiError {
                status: StatusCode::TOO_MANY_REQUESTS,
                code: "login_limited",
                message: "Too many account login attempts",
            });
        }
        Ok(())
    }
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
fn origin(headers: &HeaderMap) -> ApiResult<String> {
    headers
        .get("origin")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(ApiError::forbidden)
}
fn decode(value: &str) -> ApiResult<Vec<u8>> {
    if value.len() > 4096 {
        return Err(ApiError::invalid());
    }
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ApiError::invalid())
}
fn cookie_name(state: &ServiceState) -> String {
    format!(
        "daat_account_{}",
        state
            .system
            .store()
            .public_stage_id()
            .map(|stage| stage.0.simple().to_string())
            .unwrap_or_default()
    )
}
fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut found = None;
    for value in headers.get_all("cookie") {
        for pair in value.to_str().ok()?.split(';') {
            if let Some((key, value)) = pair.trim().split_once('=')
                && key.trim() == name
            {
                if found.is_some()
                    || value.len() != 64
                    || !value.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return None;
                }
                found = Some(value.to_owned());
            }
        }
    }
    found
}
fn cookie_response(
    state: &ServiceState,
    headers: &HeaderMap,
    value: &str,
    max_age: i64,
) -> ApiResult<Response> {
    let secure = headers
        .get("origin")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("https://"));
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_str(&format!(
            "{}={value}; HttpOnly; SameSite=Strict; Path=/v1; Max-Age={max_age}{}",
            cookie_name(state),
            if secure { "; Secure" } else { "" }
        ))
        .map_err(|_| ApiError::internal())?,
    );
    Ok(response)
}
fn host(state: &ServiceState) -> ApiResult<&std::sync::Arc<crate::stage::host::Host>> {
    state
        .stage
        .as_ref()
        .map(|runtime| &runtime.host)
        .ok_or_else(ApiError::unavailable)
}
fn device_not_revoked(host: &crate::stage::host::Host) -> bool {
    crate::stage::revocation::removed(host.system.store())
        .is_ok_and(|removed| !removed.contains(&host.local))
}
fn local_browser_origin(headers: &HeaderMap) -> bool {
    origin(headers)
        .ok()
        .and_then(|value| url::Url::parse(&value).ok())
        .is_some_and(|value| matches!(value.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
}
fn can_create(state: &ServiceState, headers: &HeaderMap, peer: SocketAddr) -> bool {
    local_browser_origin(headers)
        && local_owner(state, peer)
        && host(state).ok().is_some_and(|host| {
            host.account_id().is_none()
                || host
                    .identity()
                    .is_ok_and(|identity| identity.has_group_authority())
        })
}
fn local_owner(state: &ServiceState, peer: SocketAddr) -> bool {
    state.bootstrap_owner || crate::accounts::local_owner_connection(peer, state.listen_address)
}

/// Only the OS-owner IPC dispatcher calls this. No browser session or existing-account action.
pub(crate) async fn native_bootstrap(
    mut state: ServiceState,
    service_origin: &str,
    input: daat_locus_client_protocol::BootstrapRequest,
) -> miette::Result<daat_locus_client_protocol::BootstrapReply> {
    use daat_locus_client_protocol::{BootstrapReply, BootstrapRequest as Operation};
    let url =
        url::Url::parse(service_origin).map_err(|_| miette::miette!("Invalid service origin"))?;
    let origin = url.origin().ascii_serialization();
    if !state.origins.contains(&origin)
        || !matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
            && !matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
    {
        return Err(miette::miette!(
            "Bootstrap requires the bound loopback origin"
        ));
    }
    state.bootstrap_owner = true;
    let registered = host(&state)
        .map_err(|_| miette::miette!("Account unavailable"))?
        .accounts
        .current()?
        .is_some();
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::ORIGIN,
        origin
            .parse()
            .map_err(|_| miette::miette!("Invalid origin"))?,
    );
    headers.insert("x-daat-request", axum::http::HeaderValue::from_static("1"));
    let peer = SocketAddr::from(([127, 0, 0, 1], 0));
    let result: ApiResult<Response> = if registered && !matches!(input, Operation::Info {}) {
        Err(ApiError::forbidden())
    } else {
        match input {
            Operation::Info {} => info(State(state), ConnectInfo(peer))
                .await
                .map(IntoResponse::into_response),
            Operation::RegisterStart { name, request } => registration_start(
                State(state),
                headers,
                ConnectInfo(peer),
                Json(Start {
                    name,
                    request,
                    exclude: vec![],
                }),
            )
            .await
            .map(IntoResponse::into_response),
            Operation::RegisterFinish { id, message } => {
                registration_finish(
                    State(state),
                    headers,
                    ConnectInfo(peer),
                    Json(Finish { id, message }),
                )
                .await
            }
            Operation::LoginStart {
                name,
                request,
                exclude,
            } => login_start(
                State(state),
                headers,
                ConnectInfo(peer),
                Json(Start {
                    name,
                    request,
                    exclude,
                }),
            )
            .await
            .map(IntoResponse::into_response),
            Operation::LoginImport { id, record } => {
                login_import(
                    State(state),
                    headers,
                    ConnectInfo(peer),
                    Json(Import { id, record }),
                )
                .await
            }
            Operation::Connection { signaling_origin } => {
                connection(
                    State(state),
                    headers,
                    ConnectInfo(peer),
                    Json(Connection { signaling_origin }),
                )
                .await
            }
        }
    };
    let response = result.unwrap_or_else(IntoResponse::into_response);
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .map_err(|_| miette::miette!("Bootstrap response too large"))?;
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).map_err(|_| miette::miette!("Invalid bootstrap response"))?
    };
    Ok(BootstrapReply { status, body })
}
pub(super) fn routes() -> Router<ServiceState> {
    Router::new()
        .route("/v1/account", get(info))
        .route("/v1/account/registration/start", post(registration_start))
        .route("/v1/account/registration/finish", post(registration_finish))
        .route("/v1/account/login/start", post(login_start))
        .route("/v1/account/login/finish", post(login_finish))
        .route("/v1/account/login/import", post(login_import))
        .route("/v1/account/login/cancel", post(login_cancel))
        .route("/v1/account/connection", axum::routing::put(connection))
        .route("/v1/account/session", axum::routing::delete(logout))
}
async fn info(
    State(state): State<ServiceState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let host = host(&state)?;
    let registered = host
        .accounts
        .current()
        .map_err(ApiError::from_report)?
        .is_some();
    let local = local_owner(&state, peer);
    let authority = host.account_id().is_none()
        || host
            .identity()
            .is_ok_and(|identity| identity.has_group_authority());
    Ok(Json(
        json!({"registered":registered,"account_id":host.account_id(),"name":host.accounts.current().ok().flatten().map(|record|record.name.clone()),"can_create":!registered&&local&&authority,"signaling_origin":host.settings().signaling_origin}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    name: String,
    request: String,
    #[serde(default)]
    exclude: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Finish {
    id: String,
    message: String,
}
async fn registration_start(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(input): Json<Start>,
) -> ApiResult<Json<Value>> {
    mutation_origin(&state, &headers, true).await?;
    state.authentication.lock().await.budget(peer)?;
    let name = normalize_name(&input.name).map_err(|_| ApiError::invalid())?;
    let request = decode(&input.request)?;
    let host = host(&state)?;
    let previous = host.accounts.current().map_err(ApiError::from_report)?;
    if previous.is_some() {
        super::owner(&state, &headers).await?;
    } else if !can_create(&state, &headers, peer) {
        return Err(ApiError::forbidden());
    }
    if previous.as_ref().is_some_and(|record| record.name != name) {
        return Err(ApiError::forbidden());
    }
    let _lifecycle = host.lifecycle.lock().await;
    if host.profile().is_none() || host.account_id().is_none() {
        host.initialize(true).map_err(super::stages::failure)?;
    }
    let account_id = host.account_id().ok_or_else(ApiError::unavailable)?;
    if previous
        .as_ref()
        .is_some_and(|record| record.account_id != account_id)
    {
        return Err(ApiError::forbidden());
    }
    let setup = daat_locus_auth::new_setup();
    let identifier = format!("DaatLocus.Account.v1/{account_id}/{name}");
    let response = daat_locus_auth::registration_response(&setup, identifier.as_bytes(), &request)
        .map_err(|_| ApiError::invalid())?;
    let id = uuid::Uuid::new_v4().to_string();
    let mut authentication = state.authentication.lock().await;
    if !authentication.registrations.is_empty() {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "registration_pending",
            message: "Account registration is already pending",
        });
    }
    authentication.registrations.insert(
        id.clone(),
        RegistrationAttempt {
            setup,
            account_id: account_id.clone(),
            name: name.clone(),
            previous,
            origin: origin(&headers)?,
            expires: now() + ATTEMPT_MS,
        },
    );
    Ok(Json(
        json!({"id":id,"response":URL_SAFE_NO_PAD.encode(response),"account_id":account_id,"name":name}),
    ))
}
async fn registration_finish(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(input): Json<Finish>,
) -> ApiResult<Response> {
    mutation_origin(&state, &headers, true).await?;
    let pending = state
        .authentication
        .lock()
        .await
        .registrations
        .remove(&input.id)
        .ok_or_else(ApiError::unauthorized)?;
    if pending.expires <= now() || pending.origin != origin(&headers)? {
        return Err(ApiError::unauthorized());
    }
    if pending.previous.is_some() {
        super::owner(&state, &headers).await?;
    } else if !can_create(&state, &headers, peer) {
        return Err(ApiError::forbidden());
    }
    let host = host(&state)?;
    let _lifecycle = host.lifecycle.lock().await;
    let current = host.accounts.current().map_err(ApiError::from_report)?;
    if current.as_ref().map(CredentialBundle::binding)
        != pending.previous.as_ref().map(CredentialBundle::binding)
    {
        return Err(ApiError::unauthorized());
    }
    let record = daat_locus_auth::password_file(&decode(&input.message)?)
        .map_err(|_| ApiError::invalid())?;
    let record = CredentialBundle::new(
        pending.account_id.clone(),
        pending.name,
        &pending.setup,
        &record,
        pending.previous.as_ref(),
    )
    .map_err(ApiError::from_report)?;
    if host.account_id().as_deref() != Some(&pending.account_id) {
        return Err(ApiError::forbidden());
    }
    host.accounts
        .install(record, &pending.account_id)
        .map_err(ApiError::from_report)?;
    state.authentication.lock().await.sessions.clear();
    if let Some(runtime) = state.stage.as_ref() {
        runtime.start().await.map_err(super::stages::failure)?;
    }
    state
        .system
        .activate_account_workers()
        .await
        .map_err(ApiError::from_report)?;
    state.system.store().changed().notify_waiters();
    Ok(StatusCode::NO_CONTENT.into_response())
}
async fn login_start(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(input): Json<Start>,
) -> ApiResult<Json<Value>> {
    mutation_origin(&state, &headers, true).await?;
    state.authentication.lock().await.budget(peer)?;
    let host = host(&state)?;
    if host
        .accounts
        .current()
        .map_err(ApiError::from_report)?
        .is_none()
    {
        if !local_browser_origin(&headers) || !local_owner(&state, peer) || input.exclude.len() > 8
        {
            return Err(ApiError::forbidden());
        }
        let runtime = state
            .stage
            .as_ref()
            .ok_or_else(|| login_error(LoginError::Unavailable))?;
        let name = normalize_name(&input.name).map_err(|_| ApiError::invalid())?;
        let network = runtime
            .account_network()
            .await
            .map_err(|_| login_error(LoginError::Unavailable))?;
        let origin = origin(&headers)?;
        let login = network
            .login(&name, &input.request, &origin, &input.exclude)
            .await
            .map_err(login_error)?;
        let id = uuid::Uuid::new_v4().to_string();
        let reply = json!({"id":id,"response":login.response,"context":URL_SAFE_NO_PAD.encode(&login.context),"remote":true,"account_id":login.candidate.account_id,"device_id":login.candidate.profile.stage_id});
        state.authentication.lock().await.remote_logins.insert(
            id,
            RemoteAttempt {
                login,
                name,
                origin,
                expires: now() + ATTEMPT_MS,
            },
        );
        return Ok(Json(reply));
    }
    let record = host
        .accounts
        .current()
        .map_err(ApiError::from_report)?
        .ok_or_else(|| login_error(LoginError::Expired))?;
    if !device_not_revoked(host) {
        return Err(login_error(LoginError::Revoked));
    }
    if host.account_id().as_deref() != Some(&record.account_id) {
        return Err(login_error(LoginError::AccountChanged));
    }
    let name =
        normalize_name(&input.name).map_err(|_| login_error(LoginError::InvalidCredentials))?;
    let request = decode(&input.request)?;
    let id = uuid::Uuid::new_v4().to_string();
    let origin = origin(&headers)?;
    let context = format!(
        "DaatLocus.BrowserLogin.v1/{}/{}/{id}/{origin}",
        record.account_id, host.local
    )
    .into_bytes();
    let opaque = PendingLogin::start(
        &record.setup().map_err(ApiError::from_report)?,
        if name == record.name {
            Some(record.password_file().map_err(ApiError::from_report)?)
        } else {
            None
        },
        &record.identifier(),
        &request,
        context.clone(),
    )
    .map_err(|_| login_error(LoginError::InvalidCredentials))?;
    let response = opaque.response();
    let mut authentication = state.authentication.lock().await;
    if authentication.logins.len() + authentication.registrations.len() >= MAX_PENDING {
        return Err(login_error(LoginError::Busy));
    }
    authentication.logins.insert(
        id.clone(),
        LoginAttempt {
            opaque,
            account: record.metadata(),
            origin,
            expires: now() + ATTEMPT_MS,
        },
    );
    Ok(Json(
        json!({"id":id,"response":URL_SAFE_NO_PAD.encode(response),"context":URL_SAFE_NO_PAD.encode(context)}),
    ))
}
async fn login_finish(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(input): Json<Finish>,
) -> ApiResult<Response> {
    mutation_origin(&state, &headers, true).await?;
    let remote = state
        .authentication
        .lock()
        .await
        .remote_logins
        .remove(&input.id);
    if let Some(pending) = remote {
        if pending.expires <= now()
            || pending.origin != origin(&headers)?
            || host(&state)?
                .accounts
                .current()
                .map_err(ApiError::from_report)?
                .is_some()
        {
            return Err(login_error(LoginError::Expired));
        }
        let context: crate::accounts::network::Context =
            serde_json::from_slice(&pending.login.context)
                .map_err(|_| login_error(LoginError::Protocol))?;
        let candidate = pending.login.candidate.clone();
        let device = pending.login.device.clone();
        let payload = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            pending.login.finish(input.message),
        )
        .await
        .map_err(|_| login_error(LoginError::TimedOut))?
        .map_err(|error| {
            login_error(match error {
                crate::stage::StageError::Forbidden(_) => LoginError::InvalidCredentials,
                _ => LoginError::connection(&error),
            })
        })?;
        let id = uuid::Uuid::new_v4().to_string();
        state.authentication.lock().await.imports.insert(
            id.clone(),
            ImportAttempt {
                candidate,
                device,
                name: pending.name,
                binding: context.credential_binding,
                origin: pending.origin,
                expires: now() + ATTEMPT_MS,
            },
        );
        return Ok(Json(json!({"import_id":id,"payload":payload})).into_response());
    }
    let pending = state
        .authentication
        .lock()
        .await
        .logins
        .remove(&input.id)
        .ok_or_else(|| login_error(LoginError::Expired))?;
    if pending.expires <= now() || pending.origin != origin(&headers)? {
        return Err(login_error(LoginError::Expired));
    }
    let host = host(&state)?;
    if !host
        .accounts
        .authorizes(&pending.account.account_id, &pending.account.binding)
        || host.account_id().as_deref() != Some(&pending.account.account_id)
        || !device_not_revoked(host)
    {
        return Err(login_error(if device_not_revoked(host) {
            LoginError::AccountChanged
        } else {
            LoginError::Revoked
        }));
    }
    pending
        .opaque
        .finish(&decode(&input.message)?)
        .map_err(|_| login_error(LoginError::InvalidCredentials))?;
    let secret = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let mut authentication = state.authentication.lock().await;
    authentication
        .sessions
        .retain(|_, session| session.expires_at_ms > now());
    if let Some(previous) = cookie(&headers, &cookie_name(&state)) {
        authentication.sessions.remove(&digest(&previous));
    }
    if authentication.sessions.len() >= MAX_SESSIONS {
        return Err(login_error(LoginError::Busy));
    }
    authentication.sessions.insert(
        digest(&secret),
        BrowserSession {
            account: pending.account,
            expires_at_ms: now() + SESSION_SECONDS * 1000,
            public_id: uuid::Uuid::new_v4().to_string(),
        },
    );
    cookie_response(&state, &headers, &secret, SESSION_SECONDS)
}
async fn login_cancel(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(input): Json<Finish>,
) -> ApiResult<Response> {
    mutation_origin(&state, &headers, true).await?;
    let mut authentication = state.authentication.lock().await;
    if authentication
        .remote_logins
        .get(&input.id)
        .is_some_and(|attempt| origin(&headers).is_ok_and(|origin| origin == attempt.origin))
    {
        authentication.remote_logins.remove(&input.id);
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Import {
    id: String,
    record: String,
}
async fn login_import(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(input): Json<Import>,
) -> ApiResult<Response> {
    mutation_origin(&state, &headers, true).await?;
    if !local_browser_origin(&headers) || !local_owner(&state, peer) {
        return Err(ApiError::forbidden());
    }
    let pending = state
        .authentication
        .lock()
        .await
        .imports
        .remove(&input.id)
        .ok_or_else(|| login_error(LoginError::Expired))?;
    if pending.expires <= now() || pending.origin != origin(&headers)? {
        return Err(login_error(LoginError::Expired));
    }
    if input.record.len() > 48 * 1024 {
        return Err(login_error(LoginError::ImportRejected));
    }
    let bytes = zeroize::Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(input.record)
            .map_err(|_| login_error(LoginError::ImportRejected))?,
    );
    let imported: crate::accounts::network::AccountImport =
        serde_json::from_slice(&bytes).map_err(|_| login_error(LoginError::ImportRejected))?;
    let runtime = state
        .stage
        .as_ref()
        .ok_or_else(|| login_error(LoginError::Unavailable))?;
    let _lifecycle = runtime.host.lifecycle.lock().await;
    if runtime
        .host
        .accounts
        .current()
        .map_err(ApiError::from_report)?
        .is_some()
        || runtime
            .host
            .account_id()
            .is_some_and(|account| account != pending.candidate.account_id)
    {
        return Err(login_error(LoginError::AccountChanged));
    }
    let member = crate::stage::identity::verify_enrollment(
        &imported.source.root,
        &imported.source.chain,
        now(),
        &Default::default(),
    )
    .map_err(|_| login_error(LoginError::ImportRejected))?;
    let child = crate::stage::identity::verify_enrollment(
        &imported.enrollment.root,
        &imported.enrollment.chain,
        now(),
        &Default::default(),
    )
    .map_err(|_| login_error(LoginError::ImportRejected))?;
    if imported.source.root.group_id != pending.candidate.account_id
        || imported.enrollment.root != imported.source.root
        || member.device != pending.candidate.profile
        || !member.can_enroll
        || child.device != pending.device.device
        || child.join_id != pending.device.nonce
        || !child.can_enroll
        || imported.credentials.account_id != pending.candidate.account_id
        || imported.credentials.name != pending.name
        || imported.credentials.binding() != pending.binding
        || imported.enrollment.chain.len() != imported.source.chain.len() + 1
        || imported.enrollment.chain[..imported.source.chain.len()] != imported.source.chain
        || imported.label.trim().is_empty()
        || imported.label.len() > 100
    {
        return Err(login_error(LoginError::ImportRejected));
    }
    runtime
        .host
        .join(imported.enrollment, &pending.candidate.account_id)
        .map_err(|_| login_error(LoginError::ImportRejected))?;
    runtime
        .host
        .accounts
        .install(imported.credentials, &pending.candidate.account_id)
        .map_err(|_| login_error(LoginError::Unavailable))?;
    runtime
        .host
        .record_device_label(member.device.stage_id, &imported.label)
        .map_err(|_| login_error(LoginError::ImportRejected))?;
    runtime
        .start()
        .await
        .map_err(|_| login_error(LoginError::Unavailable))?;
    runtime.want(member.device.stage_id);
    if let Some(port) = imported.stage_port.filter(|port| *port > 0) {
        for mut address in pending.candidate.addresses {
            address.set_port(port);
            runtime.account_candidate(member.device.stage_id, address);
        }
    }
    state
        .system
        .activate_account_workers()
        .await
        .map_err(|_| login_error(LoginError::Unavailable))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    signaling_origin: Option<String>,
}
async fn connection(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(input): Json<Connection>,
) -> ApiResult<Response> {
    mutation_origin(&state, &headers, true).await?;
    let runtime = state.stage.as_ref().ok_or_else(ApiError::unavailable)?;
    if runtime
        .host
        .accounts
        .current()
        .map_err(ApiError::from_report)?
        .is_some()
    {
        super::owner(&state, &headers).await?;
    } else if !local_browser_origin(&headers) || !local_owner(&state, peer) {
        return Err(ApiError::forbidden());
    }
    runtime
        .patch_settings(None, Some(input.signaling_origin), None)
        .await
        .map_err(super::stages::failure)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
async fn logout(State(state): State<ServiceState>, headers: HeaderMap) -> ApiResult<Response> {
    mutation_origin(&state, &headers, true).await?;
    if let Some(secret) = cookie(&headers, &cookie_name(&state)) {
        state
            .authentication
            .lock()
            .await
            .sessions
            .remove(&digest(&secret));
    }
    cookie_response(&state, &headers, "", 0)
}
pub(super) async fn authorize_session(state: &ServiceState, headers: &HeaderMap) -> bool {
    let Some(secret) = cookie(headers, &cookie_name(state)) else {
        return false;
    };
    let key = digest(&secret);
    let account = {
        let mut authentication = state.authentication.lock().await;
        authentication
            .sessions
            .retain(|_, session| session.expires_at_ms > now());
        authentication
            .sessions
            .get(&key)
            .map(|session| session.account.clone())
    };
    let Some(account) = account else {
        return false;
    };
    if host(state).is_ok_and(|host| {
        host.account_id().as_deref() == Some(&account.account_id)
            && device_not_revoked(host)
            && host
                .accounts
                .authorizes(&account.account_id, &account.binding)
    }) {
        true
    } else {
        state.authentication.lock().await.sessions.remove(&key);
        false
    }
}
pub(super) async fn client_session_id(state: &ServiceState, headers: &HeaderMap) -> Option<String> {
    let secret = cookie(headers, &cookie_name(state))?;
    state
        .authentication
        .lock()
        .await
        .sessions
        .get(&digest(&secret))
        .map(|session| session.public_id.clone())
}
pub(super) async fn account_metadata(
    state: &ServiceState,
    headers: &HeaderMap,
) -> Option<Metadata> {
    let secret = cookie(headers, &cookie_name(state))?;
    state
        .authentication
        .lock()
        .await
        .sessions
        .get(&digest(&secret))
        .map(|session| session.account.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn password_login_automatically_joins_an_unpaired_device_without_enrollment_controls() {
        use crate::{
            accounts::network::{Candidate, lookup},
            actors::ActorSystem,
            config::Config,
            daemon::control::{self, LaunchOptions, Server},
        };
        use daat_locus_client_protocol::BootstrapRequest;
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let a = ActorSystem::open_account_gated(
            a_dir.path().join("actors"),
            a_dir.path().join("workspace"),
            Config::default(),
        )
        .await
        .unwrap();
        let b = ActorSystem::open_account_gated(
            b_dir.path().join("actors"),
            b_dir.path().join("workspace"),
            Config::default(),
        )
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}");
        let source = ServiceState::new(a.clone(), "http://127.0.0.1:45825".into()).unwrap();
        let state = ServiceState::new(b.clone(), url.clone())
            .unwrap()
            .with_listen_address(address);
        let source_runtime = source.stage.clone().unwrap();
        let target_runtime = state.stage.clone().unwrap();
        source_runtime.host.initialize(true).unwrap();
        let password = b"automatic device password";
        let setup = daat_locus_auth::new_setup();
        let group = source_runtime.host.account_id().unwrap();
        let name = format!("fixture-{}", &group[..16]);
        let registration = daat_locus_auth::RegistrationClient::start(password.to_vec()).unwrap();
        let response = daat_locus_auth::registration_response(
            &setup,
            format!("DaatLocus.Account.v1/{group}/{name}").as_bytes(),
            &registration.request(),
        )
        .unwrap();
        let file =
            daat_locus_auth::password_file(&registration.finish(&response).unwrap()).unwrap();
        source_runtime
            .host
            .accounts
            .install(
                CredentialBundle::new(group.clone(), name.clone(), &setup, &file, None).unwrap(),
                &group,
            )
            .unwrap();
        source_runtime.start().await.unwrap();
        target_runtime.start().await.unwrap();
        let source_network = source_runtime.account_network().await.unwrap();
        let target_network = target_runtime.account_network().await.unwrap();
        // Inject only a public discovery hint, never membership or private records.
        target_network.add_test_candidate(Candidate {
            profile: source_runtime.host.profile().unwrap(),
            account_id: group.clone(),
            name_hash: lookup(&name),
            addresses: vec![SocketAddr::new(
                std::net::Ipv4Addr::LOCALHOST.into(),
                source_network.address.port(),
            )],
            label: "source".into(),
            relay: None,
            seen: std::time::Instant::now(),
        });
        // Initial native login/import uses OS-owner IPC on every desktop OS.
        // macOS deliberately has no browser TCP owner-proof implementation.
        let control_root = b_dir.path().canonicalize().unwrap().join("control");
        let control_server = Server::bind(
            control_root.clone(),
            control_root.clone(),
            LaunchOptions {
                profile: None,
                listen: address,
                workspace: b_dir.path().join("workspace"),
            },
            url.clone(),
            Some(state.clone()),
        )
        .await
        .unwrap();
        let bootstrap_start = async |request: Vec<u8>| {
            let reply = control::bootstrap(
                &control_root,
                BootstrapRequest::LoginStart {
                    name: name.clone(),
                    request: URL_SAFE_NO_PAD.encode(request),
                    exclude: vec![],
                },
            )
            .await
            .unwrap();
            assert_eq!(reply.status, 200, "native login start: {}", reply.body);
            reply.body
        };
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                super::super::router(state).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = crate::http_client::builder().build().expect("HTTP client");
        let start = |request: Vec<u8>| {
            client
                .post(format!("{url}/v1/account/login/start"))
                .header("origin", &url)
                .header("x-daat-request", "1")
                .json(&json!({"name":name,"request":URL_SAFE_NO_PAD.encode(request)}))
        };
        let wrong = daat_locus_auth::LoginClient::start(b"incorrect password".to_vec()).unwrap();
        let reply = bootstrap_start(wrong.request()).await;
        assert!(
            wrong
                .authenticate(
                    &URL_SAFE_NO_PAD
                        .decode(reply["response"].as_str().unwrap())
                        .unwrap(),
                    &URL_SAFE_NO_PAD
                        .decode(reply["context"].as_str().unwrap())
                        .unwrap()
                )
                .is_err()
        );
        assert!(target_runtime.host.account_id().is_none());
        assert!(target_runtime.host.accounts.current().unwrap().is_none());
        assert!(
            crate::stage::ledger::StageLedger::attach(a.store(), source_runtime.host.local)
                .peers(now())
                .unwrap()
                .is_empty()
        );
        let login = daat_locus_auth::LoginClient::start(password.to_vec()).unwrap();
        let reply = bootstrap_start(login.request()).await;
        let authenticated = login
            .authenticate(
                &URL_SAFE_NO_PAD
                    .decode(reply["response"].as_str().unwrap())
                    .unwrap(),
                &URL_SAFE_NO_PAD
                    .decode(reply["context"].as_str().unwrap())
                    .unwrap(),
            )
            .unwrap();
        let finished: Value = client
            .post(format!("{url}/v1/account/login/finish"))
            .header("origin", &url)
            .header("x-daat-request", "1")
            .json(
                &json!({"id":reply["id"],"message":URL_SAFE_NO_PAD.encode(authenticated.message)}),
            )
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let private = authenticated
            .channel
            .open(
                &URL_SAFE_NO_PAD
                    .decode(finished["payload"].as_str().unwrap())
                    .unwrap(),
            )
            .unwrap();
        // Recover an enrollment persisted before a credential-file write failed.
        let partially_persisted: crate::accounts::network::AccountImport =
            serde_json::from_slice(&private).unwrap();
        target_runtime
            .host
            .join(partially_persisted.enrollment.clone(), &group)
            .unwrap();
        assert!(target_runtime.host.accounts.current().unwrap().is_none());
        let response = control::bootstrap(
            &control_root,
            BootstrapRequest::LoginImport {
                id: finished["import_id"].as_str().unwrap().into(),
                record: URL_SAFE_NO_PAD.encode(&*private),
            },
        )
        .await
        .unwrap();
        assert_eq!(response.status, 204, "native import: {}", response.body);
        assert_eq!(
            target_runtime.host.account_id().as_deref(),
            Some(group.as_str())
        );
        assert_eq!(
            target_runtime
                .host
                .accounts
                .current()
                .unwrap()
                .unwrap()
                .name,
            name
        );
        let login = daat_locus_auth::LoginClient::start(password.to_vec()).unwrap();
        let reply: Value = start(login.request())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let message = login
            .finish(
                &URL_SAFE_NO_PAD
                    .decode(reply["response"].as_str().unwrap())
                    .unwrap(),
                &URL_SAFE_NO_PAD
                    .decode(reply["context"].as_str().unwrap())
                    .unwrap(),
            )
            .unwrap();
        let response = client
            .post(format!("{url}/v1/account/login/finish"))
            .header("origin", &url)
            .header("x-daat-request", "1")
            .json(&json!({"id":reply["id"],"message":URL_SAFE_NO_PAD.encode(message)}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let cookie = response
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        assert_eq!(
            client
                .get(format!("{url}/v1/actors"))
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let configuration = client
            .get(format!("{url}/v1/config"))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(configuration.status(), StatusCode::OK);
        assert!(configuration.json::<Value>().await.unwrap()["providers"].is_array());
        let patch = |body: Value| {
            client
                .patch(format!("{url}/v1/stage/settings"))
                .header("origin", &url)
                .header("x-daat-request", "1")
                .header("cookie", &cookie)
                .json(&body)
        };
        let (label, origin) = tokio::join!(
            patch(json!({"label":"automatic-hostname"})).send(),
            patch(json!({"signaling_origin":"wss://127.0.0.1:9"})).send()
        );
        assert_eq!(label.unwrap().status(), StatusCode::OK);
        assert_eq!(origin.unwrap().status(), StatusCode::OK);
        let settings = target_runtime.host.settings();
        assert_eq!(settings.label, "automatic-hostname");
        assert_eq!(
            settings.signaling_origin.as_deref(),
            Some("wss://127.0.0.1:9")
        );
        assert_eq!(
            patch(json!({"enabled":false}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        for retired in [
            "/v1/account/device/request",
            "/v1/account/device/join",
            "/v1/stage/authorize",
            "/v1/stage/join-request",
            "/v1/stage/join",
            "/v1/stage/initialize",
        ] {
            assert_eq!(
                client
                    .post(format!("{url}{retired}"))
                    .header("origin", &url)
                    .header("x-daat-request", "1")
                    .json(&json!({}))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NOT_FOUND
            );
        }
        drop(control_server);
        source_runtime.stop().await;
        target_runtime.stop().await;
        a.shutdown().await;
        b.shutdown().await;
        server.abort();
    }
    #[test]
    fn bootstrap_origin_must_locate_the_local_browser_even_with_a_trusted_proxy() {
        let mut headers = HeaderMap::new();
        assert!(!local_browser_origin(&headers));
        for value in [
            "http://localhost:45825",
            "http://127.0.0.1:45825",
            "http://[::1]:45825",
        ] {
            headers.insert("origin", HeaderValue::from_str(value).unwrap());
            assert!(local_browser_origin(&headers));
        }
        for value in [
            "null",
            "https://proxy.example",
            "http://localhost.attacker.invalid",
            "http://192.168.1.5:45825",
        ] {
            headers.insert("origin", HeaderValue::from_str(value).unwrap());
            assert!(!local_browser_origin(&headers));
        }
    }
    #[test]
    fn browser_cookie_remains_opaque_and_unambiguous() {
        let mut headers = HeaderMap::new();
        let value = "a".repeat(64);
        headers.insert(
            "cookie",
            HeaderValue::from_str(&format!("daat_account_test={value}")).unwrap(),
        );
        assert_eq!(
            cookie(&headers, "daat_account_test").as_deref(),
            Some(value.as_str())
        );
        headers.append(
            "cookie",
            HeaderValue::from_str(&format!("daat_account_test={value}")).unwrap(),
        );
        assert!(cookie(&headers, "daat_account_test").is_none());
        assert!(cookie(&headers, "daat_owner_test").is_none());
    }
}
