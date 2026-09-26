//! Temporary session share: in-memory records, tokenless PIN exchange, and
//! the whitelist that decides what a share may reach.
//!
//! Nothing here is persisted. Share links only live as long as the Manager
//! process, so persistence buys no validity and only adds disk-write DoS
//! surface from the failure counter. State lives in [`ShareRegistry`], which is
//! cheap to clone because every field is an `Arc`.

use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, StatusCode, header::SET_COOKIE},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};

use crate::tunnel::{Tunnel, TunnelConfig, TunnelHandle};

use super::{
    ServerState,
    session::{SessionId, SessionInfo},
};

/// Default share lifetime.
pub(super) const DEFAULT_SHARE_TTL: Duration = Duration::from_secs(2 * 60 * 60);
/// Name of the share session cookie.
pub(super) const SHARE_COOKIE_NAME: &str = "daat_share";
/// Per-IP bucket capacity.
const IP_BUCKET_CAPACITY: f64 = 30.0;
/// Tokens refilled per second for the per-IP bucket.
const IP_BUCKET_REFILL_PER_SEC: f64 = 1.0 / 3.0;
/// Attempt thresholds for the per-share backoff ladder.
const BACKOFF_TIER_LOCK_UNTIL_EXPIRY: u32 = 20;
const BACKOFF_TIER_10_MIN: u32 = 15;
const BACKOFF_TIER_2_MIN: u32 = 10;
const BACKOFF_TIER_30_SEC: u32 = 5;

/// What a share exposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ShareScope {
    /// Frozen id snapshot taken when the share was created.
    Sessions(Vec<SessionId>),
    /// Every session, including ones created later.
    Unrestricted,
}

impl ShareScope {
    /// Stable hash of the scope, bound into the share cookie.
    fn hash(&self) -> String {
        let canonical = match self {
            Self::Unrestricted => "unrestricted".to_string(),
            Self::Sessions(ids) => {
                let mut ids: Vec<&str> = ids.iter().map(SessionId::as_str).collect();
                ids.sort_unstable();
                format!("sessions:{}", ids.join(","))
            }
        };
        hex::encode(sha256(canonical.as_bytes()))
    }

    fn session_ids(&self) -> Vec<String> {
        match self {
            Self::Unrestricted => Vec::new(),
            Self::Sessions(ids) => ids.iter().map(|id| id.as_str().to_string()).collect(),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Sessions(_) => "sessions",
            Self::Unrestricted => "unrestricted",
        }
    }
}

/// Lifecycle of a share while the tunnel is being prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ShareState {
    Preparing,
    Ready { url: String },
    Failed { code: String, message: String },
}

impl ShareState {
    fn label(&self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Ready { .. } => "ready",
            Self::Failed { .. } => "failed",
        }
    }
}

#[derive(Debug, Clone)]
struct ShareRecord {
    share_id: String,
    scope: ShareScope,
    pin: [u8; 2],
    pin_attempts: u32,
    locked_until_ms: i64,
    cookie_secret: [u8; 32],
    created_at_ms: i64,
    expires_at_ms: i64,
    last_used_at_ms: Option<i64>,
    state: ShareState,
}

impl ShareRecord {
    /// The 4-digit decimal PIN shown to the owner.
    fn pin_digits(&self) -> String {
        format_pin(self.pin)
    }

    fn cookie_value(&self, now_ms: i64) -> String {
        sign_cookie(
            &self.cookie_secret,
            &self.share_id,
            &self.scope.hash(),
            self.expires_at_ms,
        )
        .unwrap_or_else(|| {
            // Signing is infallible for well-formed records; the fallback keeps
            // this path panic-free.
            let _ = now_ms;
            String::new()
        })
    }

    fn is_expired(&self, now_ms: i64) -> bool {
        now_ms >= self.expires_at_ms
    }

    fn summary(&self, include_secrets: bool) -> ShareSummary {
        ShareSummary {
            share_id: self.share_id.clone(),
            state: self.state.label().to_string(),
            scope_kind: self.scope.kind().to_string(),
            session_ids: self.scope.session_ids(),
            created_at_ms: self.created_at_ms,
            expires_at_ms: self.expires_at_ms,
            last_used_at_ms: self.last_used_at_ms,
            url: match &self.state {
                ShareState::Ready { url } => Some(url.clone()),
                _ => None,
            },
            pin: include_secrets.then(|| self.pin_digits()),
            error: match &self.state {
                ShareState::Failed { code, message } => Some(ShareFailure {
                    code: code.clone(),
                    message: message.clone(),
                }),
                _ => None,
            },
        }
    }
}

/// Serializable failure detail surfaced to the UI.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct ShareFailure {
    pub code: String,
    pub message: String,
}

/// Read-only view of a share returned by the management endpoints.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ShareSummary {
    pub share_id: String,
    pub state: String,
    pub scope_kind: String,
    pub session_ids: Vec<String>,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub last_used_at_ms: Option<i64>,
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ShareFailure>,
}

/// Request body for `POST /shares`.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ShareScopeRequest {
    /// Wall of explicitly chosen sessions.
    Sessions { session_ids: Vec<String> },
    /// "Select all" shortcut; frozen to the current sessions.
    AllSessions,
    /// No session restriction.
    Unrestricted,
}

/// The `POST /shares` body.
///
/// Two shapes are accepted so the Rust handler and the WebUI client cannot
/// silently drift apart: the wrapped form the WebUI sends
/// (`{ "scope": { "kind": ... } }`) and the bare scope object
/// (`{ "kind": ... }`) used by scripts and older clients.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum CreateShareRequest {
    /// `{ "scope": { "kind": ... } }`
    Wrapped { scope: ShareScopeRequest },
    /// `{ "kind": ... }`
    Bare(ShareScopeRequest),
}

impl CreateShareRequest {
    /// Normalize either accepted shape into the scope request.
    fn into_scope(self) -> ShareScopeRequest {
        match self {
            Self::Wrapped { scope } => scope,
            Self::Bare(scope) => scope,
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct ShareCreateResponse {
    pub share_id: String,
    pub state: String,
}

#[derive(Debug, Serialize)]
pub(super) struct ShareDeleteResponse {
    pub removed: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct ShareExchangeRequest {
    pub share_id: String,
    pub pin: String,
}

/// Outcome of a PIN exchange.
pub(super) enum ExchangeOutcome {
    Ok { cookie: String, max_age_secs: i64 },
    UnknownShare,
    Expired,
    Locked { retry_after_ms: i64 },
    RateLimited { retry_after_ms: i64 },
    WrongPin { retry_after_ms: i64 },
}

#[derive(Debug)]
struct TokenBucket {
    tokens: f64,
    last_refill_ms: i64,
}

impl Default for TokenBucket {
    fn default() -> Self {
        Self {
            tokens: IP_BUCKET_CAPACITY,
            last_refill_ms: 0,
        }
    }
}

impl TokenBucket {
    fn consume(&mut self, now_ms: i64) -> bool {
        let elapsed_ms = now_ms.saturating_sub(self.last_refill_ms).max(0);
        if elapsed_ms > 0 {
            let refill = elapsed_ms as f64 / 1000.0 * IP_BUCKET_REFILL_PER_SEC;
            self.tokens = (self.tokens + refill).min(IP_BUCKET_CAPACITY);
            self.last_refill_ms = now_ms;
        }
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn retry_after_ms(&self) -> i64 {
        let missing = (1.0 - self.tokens).max(0.0);
        (missing / IP_BUCKET_REFILL_PER_SEC * 1000.0).ceil() as i64
    }
}

/// Shared, cloneable share registry.
#[derive(Clone)]
pub(super) struct ShareRegistry {
    records: Arc<RwLock<HashMap<String, ShareRecord>>>,
    buckets: Arc<Mutex<HashMap<String, TokenBucket>>>,
    tunnel: Arc<Mutex<Option<TunnelHandle>>>,
    tunnel_error: Arc<std::sync::Mutex<Option<ShareFailure>>>,
}

impl Default for ShareRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ShareRegistry {
    pub(super) fn new() -> Self {
        Self {
            records: Arc::new(RwLock::new(HashMap::new())),
            buckets: Arc::new(Mutex::new(HashMap::new())),
            tunnel: Arc::new(Mutex::new(None)),
            tunnel_error: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Insert a new share and kick off tunnel preparation.
    pub(super) async fn create(&self, scope: ShareScope, target: SocketAddr) -> String {
        let now_ms = now_ms();
        let share_id = uuid::Uuid::new_v4().to_string();
        let record = ShareRecord {
            share_id: share_id.clone(),
            scope,
            pin: random_pin(),
            pin_attempts: 0,
            locked_until_ms: 0,
            cookie_secret: random_secret(),
            created_at_ms: now_ms,
            expires_at_ms: now_ms + DEFAULT_SHARE_TTL.as_millis() as i64,
            last_used_at_ms: None,
            state: ShareState::Preparing,
        };
        self.records.write().await.insert(share_id.clone(), record);

        let registry = self.clone();
        let share_id_task = share_id.clone();
        tokio::spawn(async move {
            registry.prepare_tunnel(&share_id_task, target).await;
        });

        share_id
    }

    async fn prepare_tunnel(&self, share_id: &str, target: SocketAddr) {
        let outcome = self.ensure_tunnel(target).await;
        let mut records = self.records.write().await;
        let Some(record) = records.get_mut(share_id) else {
            return;
        };
        record.state = match outcome {
            Ok(hostname) => ShareState::Ready {
                url: format!("https://{hostname}/#s={share_id}"),
            },
            Err(failure) => ShareState::Failed {
                code: failure.code,
                message: failure.message,
            },
        };
    }

    async fn ensure_tunnel(&self, target: SocketAddr) -> Result<String, ShareFailure> {
        // Fast path: a previous attempt already failed.
        if let Some(failure) = self.tunnel_error.lock().expect("poisoned").clone() {
            return Err(failure);
        }

        let mut slot = self.tunnel.lock().await;
        if let Some(handle) = slot.as_ref() {
            return Ok(handle.hostname().to_string());
        }

        match Tunnel::start(TunnelConfig::quick(target)).await {
            Ok(handle) => {
                let hostname = handle.hostname().to_string();
                *slot = Some(handle);
                Ok(hostname)
            }
            Err(error) => {
                let failure = ShareFailure {
                    code: error.code().as_str().to_string(),
                    message: error.message().to_string(),
                };
                *self.tunnel_error.lock().expect("poisoned") = Some(failure.clone());
                Err(failure)
            }
        }
    }

    /// Drop the shared tunnel once the last share is gone.
    async fn teardown_tunnel_if_idle(&self) {
        if !self.records.read().await.is_empty() {
            return;
        }
        let handle = self.tunnel.lock().await.take();
        if let Some(handle) = handle {
            handle.shutdown().await;
        }
        *self.tunnel_error.lock().expect("poisoned") = None;
    }

    /// Expunge expired shares and tear the tunnel down when none remain.
    async fn prune(&self, now_ms: i64) {
        let emptied = {
            let mut records = self.records.write().await;
            records.retain(|_, record| !record.is_expired(now_ms));
            records.is_empty()
        };
        if emptied {
            self.teardown_tunnel_if_idle().await;
        }
    }

    pub(super) async fn list(&self) -> Vec<ShareSummary> {
        self.prune(now_ms()).await;
        let records = self.records.read().await;
        let mut summaries: Vec<ShareSummary> = records
            .values()
            .map(|record| record.summary(false))
            .collect();
        summaries.sort_by_key(|summary| summary.created_at_ms);
        summaries
    }

    pub(super) async fn get(&self, share_id: &str) -> Option<ShareSummary> {
        let now = now_ms();
        self.prune(now).await;
        self.records
            .read()
            .await
            .get(share_id)
            .map(|record| record.summary(true))
    }

    pub(super) async fn delete(&self, share_id: &str) -> bool {
        let removed = self.records.write().await.remove(share_id).is_some();
        if removed {
            self.teardown_tunnel_if_idle().await;
        }
        removed
    }

    /// Validate a submitted PIN, applying per-share backoff and the per-IP
    /// token bucket before the comparison.
    pub(super) async fn exchange(
        &self,
        share_id: &str,
        pin: &str,
        ip: &str,
        now: i64,
    ) -> ExchangeOutcome {
        self.prune(now).await;

        {
            let records = self.records.read().await;
            let Some(record) = records.get(share_id) else {
                return ExchangeOutcome::UnknownShare;
            };
            if record.is_expired(now) {
                return ExchangeOutcome::Expired;
            }
            if record.locked_until_ms > now {
                return ExchangeOutcome::Locked {
                    retry_after_ms: record.locked_until_ms - now,
                };
            }
        }

        // Per-IP bucket: independent of any share, so restarting the Manager
        // (or creating a new share) does not reset an attacker's budget.
        {
            let mut buckets = self.buckets.lock().await;
            let bucket = buckets.entry(ip.to_string()).or_default();
            if !bucket.consume(now) {
                let retry_after_ms = bucket.retry_after_ms();
                return ExchangeOutcome::RateLimited { retry_after_ms };
            }
        }

        let mut records = self.records.write().await;
        let Some(record) = records.get_mut(share_id) else {
            return ExchangeOutcome::UnknownShare;
        };

        if constant_time_eq(&record.pin, &parse_pin(pin)) {
            record.pin_attempts = 0;
            record.locked_until_ms = 0;
            record.last_used_at_ms = Some(now);
            let cookie = record.cookie_value(now);
            let max_age_secs = ((record.expires_at_ms - now) / 1000).max(0);
            return ExchangeOutcome::Ok {
                cookie,
                max_age_secs,
            };
        }

        record.pin_attempts = record.pin_attempts.saturating_add(1);
        match backoff_for_attempts(record.pin_attempts) {
            Some(Backoff::UntilExpiry) => {
                record.locked_until_ms = record.expires_at_ms;
                ExchangeOutcome::Locked {
                    retry_after_ms: record.expires_at_ms - now,
                }
            }
            Some(Backoff::For(duration)) => {
                let until = now + duration.as_millis() as i64;
                record.locked_until_ms = until.min(record.expires_at_ms);
                ExchangeOutcome::WrongPin {
                    retry_after_ms: record.locked_until_ms - now,
                }
            }
            None => ExchangeOutcome::WrongPin { retry_after_ms: 0 },
        }
    }

    /// Validate the share cookie on an incoming request.
    pub(super) async fn access_from_headers(
        &self,
        headers: &HeaderMap,
        now: i64,
    ) -> Option<ShareAccess> {
        let cookie = share_cookie_value(headers)?;
        let records = self.records.read().await;
        for record in records.values() {
            if record.is_expired(now) {
                continue;
            }
            if verify_cookie(&cookie, record, now) {
                return Some(ShareAccess {
                    scope: record.scope.clone(),
                });
            }
        }
        None
    }
}

enum Backoff {
    For(Duration),
    UntilExpiry,
}

/// The per-share backoff ladder.
fn backoff_for_attempts(attempts: u32) -> Option<Backoff> {
    if attempts >= BACKOFF_TIER_LOCK_UNTIL_EXPIRY {
        Some(Backoff::UntilExpiry)
    } else if attempts >= BACKOFF_TIER_10_MIN {
        Some(Backoff::For(Duration::from_secs(600)))
    } else if attempts >= BACKOFF_TIER_2_MIN {
        Some(Backoff::For(Duration::from_secs(120)))
    } else if attempts >= BACKOFF_TIER_30_SEC {
        Some(Backoff::For(Duration::from_secs(30)))
    } else {
        None
    }
}

/// Authenticated identity for a share request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ShareAccess {
    scope: ShareScope,
}

/// Why a share may not touch a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShareDenied {
    /// Session is outside the frozen scope snapshot.
    OutOfScope,
}

impl ShareAccess {
    /// Whitelist: only explicitly listed `(method, path)` pairs are reachable.
    /// Anything not in the table is denied, so a newly added route defaults to
    /// invisible rather than leaking.
    pub(super) fn allows(&self, method: &str, path: &str) -> bool {
        let method = method.to_ascii_uppercase();
        match (method.as_str(), path) {
            ("POST", "/share/exchange") => true,
            ("GET", "/sessions") => true,
            ("GET", "/dashboard/snapshot") => true,
            ("GET", "/dashboard/stream") => true,
            ("GET", "/dashboard/activity-history") => true,
            ("GET", "/dashboard/activity-history/count") => true,
            ("GET", "/dashboard/workflow-worker-activity") => true,
            ("GET", "/dashboard/input-history") => true,
            ("POST", "/commands/run") => true,
            ("POST", "/dashboard/action") => true,
            ("GET", path) if path.starts_with("/dashboard/attachments/") => true,
            _ => false,
        }
    }

    /// Whether this share may address `session`.
    pub(super) fn session_allowed(&self, session: &SessionInfo) -> Result<(), ShareDenied> {
        match &self.scope {
            ShareScope::Unrestricted => Ok(()),
            ShareScope::Sessions(ids) => {
                if ids.contains(&session.session_id) {
                    Ok(())
                } else {
                    Err(ShareDenied::OutOfScope)
                }
            }
        }
    }
}

/// Result of authorizing a route for either a full token or a share cookie.
#[derive(Debug, Clone)]
pub(super) enum RouteAuth {
    Token,
    Share(ShareAccess),
}

impl RouteAuth {
    pub(super) fn share(&self) -> Option<&ShareAccess> {
        match self {
            Self::Token => None,
            Self::Share(access) => Some(access),
        }
    }
}

/// Authorize `(method, path)` for full tokens and shares alike. Returns 401 when
/// no credential is present, 403 when a share tries a non-whitelisted route.
// The error is a fully-built HTTP response, which is legitimately larger than
// clippy's `result_large_err` threshold; boxing it would only add allocation on
// the forbidden/unauthorized error paths.
#[allow(clippy::result_large_err)]
pub(super) async fn authorize_route(
    state: &ServerState,
    headers: &HeaderMap,
    method: &str,
    path: &str,
) -> Result<RouteAuth, Response> {
    if state.auth_registry.authorize_headers(headers).await {
        return Ok(RouteAuth::Token);
    }
    if let Some(access) = state.shares.access_from_headers(headers, now_ms()).await {
        if access.allows(method, path) {
            return Ok(RouteAuth::Share(access));
        }
        return Err(StatusCode::FORBIDDEN.into_response());
    }
    Err(StatusCode::UNAUTHORIZED.into_response())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(super) async fn create_share_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<CreateShareRequest>,
) -> Response {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let scope = match request.into_scope() {
        ShareScopeRequest::Sessions { session_ids } => {
            let mut ids = Vec::with_capacity(session_ids.len());
            for raw in session_ids {
                match SessionId::from_string(&raw) {
                    Ok(id) => ids.push(id),
                    Err(_) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(serde_json::json!({ "code": "invalid_session_id" })),
                        )
                            .into_response();
                    }
                }
            }
            ShareScope::Sessions(ids)
        }
        ShareScopeRequest::AllSessions => {
            let ids = state
                .sessions
                .list()
                .into_iter()
                .map(|info| info.session_id)
                .collect();
            ShareScope::Sessions(ids)
        }
        ShareScopeRequest::Unrestricted => ShareScope::Unrestricted,
    };

    let target = SocketAddr::from(([127, 0, 0, 1], state.port));
    let share_id = state.shares.create(scope, target).await;
    (
        StatusCode::ACCEPTED,
        Json(ShareCreateResponse {
            share_id,
            state: "preparing".to_string(),
        }),
    )
        .into_response()
}

pub(super) async fn list_shares_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Response {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(state.shares.list().await).into_response()
}

pub(super) async fn share_status_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    AxumPath(share_id): AxumPath<String>,
) -> Response {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.shares.get(&share_id).await {
        Some(summary) => Json(summary).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub(super) async fn delete_share_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    AxumPath(share_id): AxumPath<String>,
) -> Response {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let removed = state.shares.delete(&share_id).await;
    Json(ShareDeleteResponse { removed }).into_response()
}

/// Tokenless entry point: exchange a PIN for a share cookie.
pub(super) async fn share_exchange_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<ShareExchangeRequest>,
) -> Response {
    let ip = client_ip(&headers);
    let secure = is_secure_request(&headers);
    let outcome = state
        .shares
        .exchange(&request.share_id, &request.pin, &ip, now_ms())
        .await;

    match outcome {
        ExchangeOutcome::Ok {
            cookie,
            max_age_secs,
        } => {
            let mut value = format!(
                "{SHARE_COOKIE_NAME}={cookie}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age_secs}"
            );
            if secure {
                value.push_str("; Secure");
            }
            let mut response = Json(serde_json::json!({ "ok": true })).into_response();
            if let Ok(header) = value.parse() {
                response.headers_mut().insert(SET_COOKIE, header);
            }
            response
        }
        ExchangeOutcome::UnknownShare => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "code": "unknown_share" })),
        )
            .into_response(),
        ExchangeOutcome::Expired => (
            StatusCode::GONE,
            Json(serde_json::json!({ "code": "share_expired" })),
        )
            .into_response(),
        ExchangeOutcome::Locked { retry_after_ms } => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "code": "share_locked",
                "retry_after_ms": retry_after_ms
            })),
        )
            .into_response(),
        ExchangeOutcome::RateLimited { retry_after_ms } => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "code": "rate_limited",
                "retry_after_ms": retry_after_ms
            })),
        )
            .into_response(),
        ExchangeOutcome::WrongPin { retry_after_ms } => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "code": "wrong_pin",
                "retry_after_ms": retry_after_ms
            })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(super) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_slice());
    out
}

/// HMAC-SHA256, implemented directly so the share cookie needs no extra
/// dependency beyond `sha2`.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        key_block[..32].copy_from_slice(&sha256(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut inner = Sha256::new();
    let mut outer = Sha256::new();
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for index in 0..BLOCK {
        ipad[index] ^= key_block[index];
        opad[index] ^= key_block[index];
    }
    inner.update(ipad);
    inner.update(message);
    let inner = inner.finalize();
    outer.update(opad);
    outer.update(inner);
    let digest = outer.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_slice());
    out
}

/// `v1.<share_id>.<exp_ms>.<hex_hmac>`; the HMAC covers share id, scope hash
/// and expiry so a tampered cookie fails closed.
fn sign_cookie(
    secret: &[u8; 32],
    share_id: &str,
    scope_hash: &str,
    expires_at_ms: i64,
) -> Option<String> {
    if share_id.is_empty() || share_id.contains('.') {
        return None;
    }
    let message = format!("{share_id}|{scope_hash}|{expires_at_ms}");
    let mac = hmac_sha256(secret, message.as_bytes());
    Some(format!(
        "v1.{share_id}.{expires_at_ms}.{}",
        hex::encode(mac)
    ))
}

fn verify_cookie(cookie: &str, record: &ShareRecord, now_ms: i64) -> bool {
    let mut parts = cookie.split('.');
    let version = parts.next();
    let share_id = parts.next();
    let expires = parts.next();
    let mac = parts.next();
    if parts.next().is_some() {
        return false;
    }
    let (Some("v1"), Some(share_id), Some(expires), Some(mac)) = (version, share_id, expires, mac)
    else {
        return false;
    };
    if share_id != record.share_id {
        return false;
    }
    let Ok(expires_at_ms) = expires.parse::<i64>() else {
        return false;
    };
    if expires_at_ms != record.expires_at_ms || now_ms >= expires_at_ms {
        return false;
    }
    let Some(expected) = sign_cookie(
        &record.cookie_secret,
        &record.share_id,
        &record.scope.hash(),
        record.expires_at_ms,
    ) else {
        return false;
    };
    let expected_mac = expected.rsplit('.').next().unwrap_or("");
    constant_time_str_eq(expected_mac, mac)
}

fn constant_time_eq(left: &[u8; 2], right: &Option<[u8; 2]>) -> bool {
    match right {
        Some(right) => {
            let mut diff = 0u8;
            for (a, b) in left.iter().zip(right.iter()) {
                diff |= a ^ b;
            }
            diff == 0
        }
        None => false,
    }
}

fn constant_time_str_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.bytes().zip(right.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Parse a 4-digit PIN; `None` for any malformed input.
fn parse_pin(raw: &str) -> Option<[u8; 2]> {
    let trimmed = raw.trim();
    if trimmed.len() != 4 || !trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: u16 = trimmed.parse().ok()?;
    Some(value.to_be_bytes())
}

fn format_pin(pin: [u8; 2]) -> String {
    let value = u16::from_be_bytes(pin);
    format!("{value:04}")
}

fn random_pin() -> [u8; 2] {
    let bytes = uuid::Uuid::new_v4();
    let raw = bytes.as_bytes();
    let value = u16::from_be_bytes([raw[0], raw[1]]) % 10_000;
    value.to_be_bytes()
}

fn random_secret() -> [u8; 32] {
    let mut secret = [0u8; 32];
    secret[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    secret[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    secret
}

fn share_cookie_value(headers: &HeaderMap) -> Option<String> {
    for header in headers.get_all(axum::http::header::COOKIE) {
        let Ok(raw) = header.to_str() else {
            continue;
        };
        for pair in raw.split(';') {
            let pair = pair.trim();
            if let Some(value) = pair.strip_prefix(&format!("{SHARE_COOKIE_NAME}=")) {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn client_ip(headers: &HeaderMap) -> String {
    for candidate in ["x-forwarded-for", "x-real-ip", "cf-connecting-ip"] {
        if let Some(value) = headers.get(candidate)
            && let Ok(value) = value.to_str()
        {
            let first = value.split(',').next().unwrap_or("").trim();
            if !first.is_empty() {
                return first.to_string();
            }
        }
    }
    "unknown".to_string()
}

fn is_secure_request(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("https"))
        .unwrap_or(false)
}

/// Helper for handlers that address a specific session.
pub(super) fn share_session_denied_response(denied: ShareDenied) -> Response {
    let code = match denied {
        ShareDenied::OutOfScope => "session_out_of_scope",
    };
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "code": code })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::session::{SessionScope, SessionStatus};

    fn session(id: &str, scope: SessionScope) -> SessionInfo {
        SessionInfo {
            session_id: SessionId::from_string(id).expect("session id"),
            scope,
            pid: None,
            status: SessionStatus::Dormant,
            ipc_name: None,
            ipc_token_hash: None,
            project_dir: None,
            title: None,
            started_at_ms: 0,
            last_seen_at_ms: None,
        }
    }

    fn access(scope: ShareScope) -> ShareAccess {
        ShareAccess { scope }
    }

    #[test]
    fn whitelist_allows_only_listed_routes() {
        let access = access(ShareScope::Unrestricted);
        assert!(access.allows("GET", "/dashboard/snapshot"));
        assert!(access.allows("GET", "/dashboard/attachments/abc"));
        assert!(access.allows("POST", "/dashboard/action"));
        assert!(access.allows("POST", "/commands/run"));
        assert!(access.allows("POST", "/share/exchange"));
        assert!(access.allows("GET", "/sessions"));
    }

    #[test]
    fn whitelist_denies_dangerous_and_unlisted_routes() {
        let access = access(ShareScope::Unrestricted);
        for (method, path) in [
            ("POST", "/filesystem/open"),
            ("GET", "/filesystem/dirs"),
            ("GET", "/settings/summary"),
            ("GET", "/config/readiness"),
            ("GET", "/logs/sources"),
            ("GET", "/logs/read"),
            ("GET", "/status/summary"),
            ("GET", "/status"),
            ("POST", "/sessions"),
            ("DELETE", "/sessions/abc"),
            ("POST", "/sessions/abc/title"),
            ("POST", "/study/ensure"),
            ("GET", "/study/graph"),
            ("POST", "/send"),
            ("POST", "/daemon/shutdown"),
            ("POST", "/daemon/restart"),
            ("GET", "/dashboard/unknown"),
        ] {
            assert!(
                !access.allows(method, path),
                "{method} {path} must be denied"
            );
        }
    }

    #[test]
    fn session_scope_hits_only_frozen_ids() {
        let scope = ShareScope::Sessions(vec![
            SessionId::from_string("a").expect("id"),
            SessionId::from_string("b").expect("id"),
        ]);
        let access = access(scope);
        assert!(
            access
                .session_allowed(&session("a", SessionScope::General))
                .is_ok()
        );
        assert_eq!(
            access
                .session_allowed(&session("c", SessionScope::General))
                .expect_err("out of scope"),
            ShareDenied::OutOfScope
        );
    }

    #[test]
    fn unrestricted_scope_includes_study() {
        let access = access(ShareScope::Unrestricted);
        assert!(
            access
                .session_allowed(&session("new", SessionScope::General))
                .is_ok()
        );
        assert!(
            access
                .session_allowed(&session("study", SessionScope::Study))
                .is_ok()
        );
    }

    #[test]
    fn study_is_shareable_when_listed() {
        let scope = ShareScope::Sessions(vec![SessionId::from_string("study").expect("id")]);
        assert!(
            access(scope)
                .session_allowed(&session("study", SessionScope::Study))
                .is_ok()
        );
    }

    #[test]
    fn pin_roundtrip_and_validation() {
        assert_eq!(format_pin([0x00, 0x2a]), "0042");
        assert_eq!(parse_pin("0042"), Some([0x00, 0x2a]));
        assert_eq!(parse_pin(" 0042 "), Some([0x00, 0x2a]));
        assert_eq!(parse_pin("42"), None);
        assert_eq!(parse_pin("abcd"), None);
        assert_eq!(parse_pin(""), None);
    }

    #[test]
    fn cookie_roundtrips_and_detects_tampering() {
        let secret = [7u8; 32];
        let scope = ShareScope::Sessions(Vec::new());
        let cookie = sign_cookie(&secret, "share-1", &scope.hash(), 1_000).expect("sign");
        let record = ShareRecord {
            share_id: "share-1".to_string(),
            scope: scope.clone(),
            pin: [0, 0],
            pin_attempts: 0,
            locked_until_ms: 0,
            cookie_secret: secret,
            created_at_ms: 0,
            expires_at_ms: 1_000,
            last_used_at_ms: None,
            state: ShareState::Preparing,
        };
        assert!(verify_cookie(&cookie, &record, 0));
        // Expired.
        assert!(!verify_cookie(&cookie, &record, 1_000));
        // Tampered mac.
        let tampered = format!("{}0", cookie);
        assert!(!verify_cookie(&tampered, &record, 0));
        // Tampered expiry.
        let tampered_expiry = cookie.replace(".1000.", ".9999.");
        assert!(!verify_cookie(&tampered_expiry, &record, 0));
        // Different secret.
        let mut other = record.clone();
        other.cookie_secret = [8u8; 32];
        assert!(!verify_cookie(&cookie, &other, 0));
    }

    #[test]
    fn backoff_ladder_matches_design() {
        assert!(backoff_for_attempts(1).is_none());
        assert!(backoff_for_attempts(4).is_none());
        assert!(matches!(
            backoff_for_attempts(5),
            Some(Backoff::For(duration)) if duration == Duration::from_secs(30)
        ));
        assert!(matches!(
            backoff_for_attempts(10),
            Some(Backoff::For(duration)) if duration == Duration::from_secs(120)
        ));
        assert!(matches!(
            backoff_for_attempts(15),
            Some(Backoff::For(duration)) if duration == Duration::from_secs(600)
        ));
        assert!(matches!(
            backoff_for_attempts(20),
            Some(Backoff::UntilExpiry)
        ));
    }

    #[test]
    fn token_bucket_refills_over_time() {
        let mut bucket = TokenBucket::default();
        for _ in 0..IP_BUCKET_CAPACITY as usize {
            assert!(bucket.consume(0), "initial burst should be allowed");
        }
        assert!(!bucket.consume(0), "bucket is empty");
        // After enough time, at least one token is back.
        assert!(bucket.consume(60_000));
    }

    #[tokio::test]
    async fn exchange_success_resets_attempts_and_sets_cookie() {
        let registry = ShareRegistry::new();
        let record = ShareRecord {
            share_id: "s".to_string(),
            scope: ShareScope::Unrestricted,
            pin: parse_pin("1234").expect("pin"),
            pin_attempts: 3,
            locked_until_ms: 0,
            cookie_secret: [1u8; 32],
            created_at_ms: 0,
            expires_at_ms: now_ms() + 60_000,
            last_used_at_ms: None,
            state: ShareState::Preparing,
        };
        registry
            .records
            .write()
            .await
            .insert("s".to_string(), record);

        let outcome = registry.exchange("s", "1234", "1.2.3.4", now_ms()).await;
        assert!(matches!(outcome, ExchangeOutcome::Ok { .. }));
        let records = registry.records.read().await;
        let record = records.get("s").expect("record");
        assert_eq!(record.pin_attempts, 0);
        assert!(record.last_used_at_ms.is_some());
    }

    #[tokio::test]
    async fn exchange_wrong_pin_locks_after_threshold() {
        let registry = ShareRegistry::new();
        let record = ShareRecord {
            share_id: "s".to_string(),
            scope: ShareScope::Unrestricted,
            pin: parse_pin("0000").expect("pin"),
            pin_attempts: 4,
            locked_until_ms: 0,
            cookie_secret: [1u8; 32],
            created_at_ms: 0,
            expires_at_ms: now_ms() + 600_000,
            last_used_at_ms: None,
            state: ShareState::Preparing,
        };
        registry
            .records
            .write()
            .await
            .insert("s".to_string(), record);

        // Fifth failure trips the 30s tier.
        let outcome = registry.exchange("s", "9999", "9.9.9.9", now_ms()).await;
        assert!(
            matches!(outcome, ExchangeOutcome::WrongPin { retry_after_ms } if retry_after_ms > 0)
        );
        let locked = registry.exchange("s", "0000", "9.9.9.9", now_ms()).await;
        assert!(matches!(locked, ExchangeOutcome::Locked { .. }));
    }

    #[tokio::test]
    async fn exchange_unknown_and_expired_shares() {
        let registry = ShareRegistry::new();
        assert!(matches!(
            registry.exchange("missing", "0000", "ip", now_ms()).await,
            ExchangeOutcome::UnknownShare
        ));

        let record = ShareRecord {
            share_id: "e".to_string(),
            scope: ShareScope::Unrestricted,
            pin: parse_pin("0000").expect("pin"),
            pin_attempts: 0,
            locked_until_ms: 0,
            cookie_secret: [1u8; 32],
            created_at_ms: 0,
            expires_at_ms: now_ms() - 1,
            last_used_at_ms: None,
            state: ShareState::Preparing,
        };
        registry
            .records
            .write()
            .await
            .insert("e".to_string(), record);
        assert!(matches!(
            registry.exchange("e", "0000", "ip", now_ms()).await,
            ExchangeOutcome::UnknownShare | ExchangeOutcome::Expired
        ));
    }

    #[test]
    fn create_share_request_accepts_the_wrapped_webui_body() {
        // Exactly what `createShare` in webui/src/lib/daemon-api.ts posts.
        let request: CreateShareRequest =
            serde_json::from_str(r#"{"scope":{"kind":"unrestricted"}}"#)
                .expect("wrapped body must deserialize");
        assert!(matches!(
            request.into_scope(),
            ShareScopeRequest::Unrestricted
        ));

        let request: CreateShareRequest =
            serde_json::from_str(r#"{"scope":{"kind":"sessions","session_ids":["a","b"]}}"#)
                .expect("wrapped sessions body must deserialize");
        match request.into_scope() {
            ShareScopeRequest::Sessions { session_ids } => {
                assert_eq!(session_ids, vec!["a".to_string(), "b".to_string()]);
            }
            other => panic!("unexpected scope: {other:?}"),
        }

        let request: CreateShareRequest =
            serde_json::from_str(r#"{"scope":{"kind":"all_sessions"}}"#)
                .expect("wrapped all_sessions body must deserialize");
        assert!(matches!(
            request.into_scope(),
            ShareScopeRequest::AllSessions
        ));
    }

    #[test]
    fn create_share_request_still_accepts_the_bare_scope_body() {
        let request: CreateShareRequest =
            serde_json::from_str(r#"{"kind":"unrestricted"}"#).expect("bare body must deserialize");
        assert!(matches!(
            request.into_scope(),
            ShareScopeRequest::Unrestricted
        ));
    }
}
