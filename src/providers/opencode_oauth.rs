//! OpenCode Console OAuth (device-code flow) support.
//!
//! OpenCode's `/zen/v1` free tier rejects plain service-account API keys and
//! requires a personal Console login. This module implements the same OAuth
//! device-code flow the official OpenCode client uses, persists the resulting
//! tokens in a Daat-owned file, and exposes a provider that forwards requests
//! to the zen gateway with a freshly resolved access token.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use miette::{Result, miette};
use serde::{Deserialize, Serialize};

use super::OpenAIClient;
use super::responses_compat::ResponsesCompatibleClient;
use crate::config::{ModelConfig, redact_secret_text};
use crate::context_budget::RequestBudgetLimits;
use crate::core::{ModelProvider, ModelRequestOptions, TokenUsageInfo};
use crate::daat_locus_paths::daat_locus_paths_sync;
use crate::persistence::{PersistenceFileMode, write_bytes_atomic};
use crate::reasoning::runtime::{AgentTurnRequest, AgentTurnStreamResult, PromptRequest};

/// Default OpenCode Console origin used for the device-code OAuth flow.
pub const OPENCODE_OAUTH_DEFAULT_SERVER: &str = "https://opencode.ai/console";
/// Default OpenAI-compatible gateway base URL for OpenCode Console OAuth
/// models: the Console-issued inference endpoint from `GET /api/config`.
/// OAuth access tokens are only accepted here; the public `/zen/v1` gateway
/// only accepts `oc_sk_`/`sk-` service-account API keys.
pub const OPENCODE_ZEN_BASE_URL: &str = "https://opencode.ai/inference/openai/v1";

const OPENCODE_OAUTH_CLIENT_ID: &str = "opencode-cli";
const OPENCODE_DEVICE_CODE_PATH: &str = "/auth/device/code";
const OPENCODE_DEVICE_TOKEN_PATH: &str = "/auth/device/token";
const ACCESS_TOKEN_REFRESH_SKEW_MS: i64 = 60_000;

static REFRESH_LOCKS_BY_AUTH_FILE: LazyLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn expiry_from_secs(expires_in: Option<i64>) -> i64 {
    expires_in
        .filter(|secs| *secs > 0)
        .map_or(0, |secs| now_ms().saturating_add(secs.saturating_mul(1000)))
}

/// Persisted OpenCode Console OAuth tokens.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct OpenCodeOAuthTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    /// Unix epoch milliseconds at which the access token expires. `0` means
    /// the expiry is unknown and the token is used until a request rejects it.
    #[serde(default)]
    pub expires_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Console org picked at login (`GET /api/orgs`), sent back as the
    /// `x-opencode-org-id` header the Console-issued inference endpoint
    /// requires. OAuth tokens stay valid across orgs; this only selects
    /// which workspace the usage is billed to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
}

/// A resolved access token plus the Console org it was issued against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCodeOAuthAccess {
    pub access_token: String,
    pub expires_at_ms: Option<i64>,
    /// Console org id (`wrk_...`) picked at login, sent back as the
    /// `x-opencode-org-id` header the inference endpoint requires.
    pub org_id: Option<String>,
    /// Console origin the token and org were issued against.
    pub server: Option<String>,
}

impl OpenCodeOAuthAccess {
    fn needs_refresh(&self) -> bool {
        match self.expires_at_ms {
            Some(expires_at_ms) => {
                now_ms().saturating_add(ACCESS_TOKEN_REFRESH_SKEW_MS) >= expires_at_ms
            }
            None => false,
        }
    }
}

/// A started device-code login waiting for the user to authorize in a browser.
#[derive(Clone, Debug)]
pub struct OpenCodeDeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    pub expires_in_secs: u64,
    pub interval_secs: u64,
}

/// Result of one device-token poll.
pub enum OpenCodeDevicePoll {
    /// The user has not authorized yet; keep polling.
    Pending,
    /// The server asked us to poll more slowly.
    SlowDown,
    /// Authorization completed and tokens were issued.
    Ready(OpenCodeOAuthTokens),
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// The Daat-owned token file shared by every OpenCode Console OAuth provider.
pub fn opencode_auth_file_path() -> PathBuf {
    daat_locus_paths_sync().opencode_auth_file("oauth.json")
}

/// Resolve the Console billing org (`GET /api/orgs`) for an access token. The
/// Console-issued inference endpoint rejects requests without the matching
/// `x-opencode-org-id` header.
async fn opencode_fetch_org_id(
    client: &reqwest::Client,
    server: &str,
    access_token: &str,
) -> Result<Option<String>> {
    #[derive(Deserialize)]
    struct Org {
        id: String,
    }

    let endpoint = format!("{}/api/orgs", server.trim_end_matches('/'));
    let response = client
        .get(&endpoint)
        .header("Accept", "application/json")
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|err| miette!("OpenCode org lookup failed: {err}"))?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let body = response
        .text()
        .await
        .map_err(|err| miette!("OpenCode org lookup body read failed: {err}"))?;
    let orgs: Vec<Org> = serde_json::from_str(&body).unwrap_or_default();
    Ok(orgs.into_iter().next().map(|org| org.id))
}

fn auth_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| miette!("failed to build OpenCode auth http client: {err}"))
}

/// Build an absolute device verification URL from a possibly-relative value
/// returned by the Console (e.g. `/console/device?user_code=...`).
fn absolute_verification_url(server: &str, value: Option<&str>) -> String {
    let fallback = format!("{}/device", server.trim_end_matches('/'));
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return fallback;
    };
    if value.starts_with("http://") || value.starts_with("https://") {
        return value.to_string();
    }
    match url::Url::parse(server) {
        Ok(url) => match url.host_str() {
            Some(host) => format!("{}://{host}{value}", url.scheme()),
            None => fallback,
        },
        Err(_) => fallback,
    }
}

/// Start an OpenCode Console device-code login.
pub async fn opencode_start_device_code(server: &str) -> Result<OpenCodeDeviceCode> {
    let client = auth_http_client()?;
    let endpoint = format!(
        "{}{OPENCODE_DEVICE_CODE_PATH}",
        server.trim_end_matches('/')
    );
    let response = client
        .post(&endpoint)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "client_id": OPENCODE_OAUTH_CLIENT_ID }))
        .send()
        .await
        .map_err(|err| miette!("OpenCode device code request failed: {err}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| miette!("OpenCode device code body read failed: {err}"))?;
    if !status.is_success() {
        return Err(miette!(
            "OpenCode device code request returned HTTP {status}: {body}"
        ));
    }
    let device: DeviceCodeResponse = serde_json::from_str(&body)
        .map_err(|err| miette!("OpenCode device code response parse failed: {err}: {body}"))?;
    let verification_url = absolute_verification_url(
        server,
        device
            .verification_uri_complete
            .as_deref()
            .or(device.verification_uri.as_deref()),
    );
    Ok(OpenCodeDeviceCode {
        device_code: device.device_code,
        user_code: device.user_code,
        verification_url,
        expires_in_secs: device.expires_in.unwrap_or(600),
        interval_secs: device.interval.unwrap_or(5).max(5),
    })
}

/// Poll the device-token endpoint once.
pub async fn opencode_poll_device_token(
    server: &str,
    device_code: &str,
) -> Result<OpenCodeDevicePoll> {
    let client = auth_http_client()?;
    let endpoint = format!(
        "{}{OPENCODE_DEVICE_TOKEN_PATH}",
        server.trim_end_matches('/')
    );
    let response = client
        .post(&endpoint)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            "device_code": device_code,
            "client_id": OPENCODE_OAUTH_CLIENT_ID,
        }))
        .send()
        .await
        .map_err(|err| miette!("OpenCode device token request failed: {err}"))?;
    let body = response
        .text()
        .await
        .map_err(|err| miette!("OpenCode device token body read failed: {err}"))?;
    // The Console answers a still-pending poll with HTTP 400, so the body is
    // always parsed instead of branching on the status code.
    let token: TokenResponse = serde_json::from_str(&body)
        .map_err(|err| miette!("OpenCode device token response parse failed: {err}: {body}"))?;
    if let Some(access_token) = token.access_token {
        return Ok(OpenCodeDevicePoll::Ready(OpenCodeOAuthTokens {
            access_token,
            refresh_token: token.refresh_token.unwrap_or_default(),
            expires_at_ms: expiry_from_secs(token.expires_in),
            server: Some(server.to_string()),
            org_id: None,
        }));
    }
    match token.error.as_deref() {
        Some("authorization_pending") => Ok(OpenCodeDevicePoll::Pending),
        Some("slow_down") => Ok(OpenCodeDevicePoll::SlowDown),
        Some("expired_token") => Err(miette!("OpenCode device code expired")),
        Some("access_denied") => Err(miette!("OpenCode device authorization was denied")),
        Some(error) => Err(miette!(
            "OpenCode device authorization failed: {}",
            token
                .error_description
                .as_deref()
                .filter(|description| !description.trim().is_empty())
                .unwrap_or(error)
        )),
        None => Err(miette!(
            "OpenCode device token response did not include a token: {body}"
        )),
    }
}

/// Persist OpenCode OAuth tokens to a Daat-owned, private file.
pub async fn write_opencode_oauth_tokens(
    auth_file: &Path,
    tokens: &OpenCodeOAuthTokens,
) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(tokens)
        .map_err(|err| miette!("serialize OpenCode tokens failed: {err}"))?;
    write_bytes_atomic(auth_file.to_path_buf(), bytes, PersistenceFileMode::Private)
        .await
        .map_err(|err| {
            miette!(
                "write OpenCode tokens {} failed: {err}",
                auth_file.display()
            )
        })
}

async fn read_opencode_oauth_tokens(auth_file: &Path) -> Result<OpenCodeOAuthTokens> {
    let bytes = tokio::fs::read(auth_file)
        .await
        .map_err(|err| miette!("read {} failed: {err}", auth_file.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|err| miette!("parse {} failed: {err}", auth_file.display()))
}

fn access_from_tokens(tokens: &OpenCodeOAuthTokens) -> OpenCodeOAuthAccess {
    OpenCodeOAuthAccess {
        access_token: tokens.access_token.clone(),
        expires_at_ms: (tokens.expires_at_ms > 0).then_some(tokens.expires_at_ms),
        org_id: tokens.org_id.clone(),
        server: tokens.server.clone(),
    }
}

/// Resolve the access token, Console origin, and billing org needed to read the
/// Console model catalog (`GET {server}/api/config`). Older token files predate
/// the org field, so resolve it on demand, mirroring `ensure_auth`.
pub async fn opencode_console_access(auth_file: &Path) -> Result<OpenCodeOAuthAccess> {
    let client = auth_http_client()?;
    let mut access = opencode_oauth_access_from_file_with_client(auth_file, &client).await?;
    if access.org_id.is_none() {
        let server = access
            .server
            .clone()
            .unwrap_or_else(|| OPENCODE_OAUTH_DEFAULT_SERVER.to_string());
        access.org_id = opencode_fetch_org_id(&client, &server, &access.access_token)
            .await
            .ok()
            .flatten();
    }
    Ok(access)
}

async fn opencode_oauth_access_from_file_with_client(
    auth_file: &Path,
    client: &reqwest::Client,
) -> Result<OpenCodeOAuthAccess> {
    let mut tokens = read_opencode_oauth_tokens(auth_file).await?;
    let mut access = access_from_tokens(&tokens);
    if !access.needs_refresh() {
        return Ok(access);
    }

    let refresh_lock = refresh_lock_for_auth_file(auth_file);
    let _guard = refresh_lock.lock().await;
    tokens = read_opencode_oauth_tokens(auth_file).await?;
    access = access_from_tokens(&tokens);
    if !access.needs_refresh() {
        return Ok(access);
    }
    if tokens.refresh_token.trim().is_empty() {
        return Err(miette!(
            "OpenCode access token expired and {} has no refresh token; log in again",
            auth_file.display()
        ));
    }

    let server = tokens
        .server
        .clone()
        .unwrap_or_else(|| OPENCODE_OAUTH_DEFAULT_SERVER.to_string());
    let refreshed = refresh_opencode_oauth_tokens(client, &server, &tokens).await?;
    write_opencode_oauth_tokens(auth_file, &refreshed).await?;
    Ok(access_from_tokens(&refreshed))
}

fn refresh_lock_for_auth_file(auth_file: &Path) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = REFRESH_LOCKS_BY_AUTH_FILE
        .lock()
        .expect("refresh lock map poisoned");
    locks
        .entry(auth_file.to_path_buf())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

async fn refresh_opencode_oauth_tokens(
    client: &reqwest::Client,
    server: &str,
    tokens: &OpenCodeOAuthTokens,
) -> Result<OpenCodeOAuthTokens> {
    let endpoint = format!(
        "{}{OPENCODE_DEVICE_TOKEN_PATH}",
        server.trim_end_matches('/')
    );
    let response = client
        .post(&endpoint)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": tokens.refresh_token,
            "client_id": OPENCODE_OAUTH_CLIENT_ID,
        }))
        .send()
        .await
        .map_err(|err| miette!("OpenCode token refresh request failed: {err}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| miette!("OpenCode token refresh body read failed: {err}"))?;
    if !status.is_success() {
        return Err(miette!(
            "OpenCode token refresh returned HTTP {status}: {}",
            redact_secret_text(&body, &tokens.refresh_token)
        ));
    }
    let refreshed: TokenResponse = serde_json::from_str(&body)
        .map_err(|err| miette!("OpenCode token refresh JSON parse failed: {err}"))?;
    let access_token = refreshed.access_token.ok_or_else(|| {
        miette!("OpenCode token refresh response did not include an access token")
    })?;
    Ok(OpenCodeOAuthTokens {
        access_token,
        refresh_token: refreshed
            .refresh_token
            .filter(|token| !token.trim().is_empty())
            .unwrap_or_else(|| tokens.refresh_token.clone()),
        expires_at_ms: expiry_from_secs(refreshed.expires_in),
        server: Some(server.to_string()),
        org_id: tokens.org_id.clone(),
    })
}

enum OpenCodeInner {
    Chat(OpenAIClient),
    Responses(ResponsesCompatibleClient),
}

impl OpenCodeInner {
    fn set_api_key(&mut self, api_key: String) {
        match self {
            Self::Chat(client) => client.set_api_key(api_key),
            Self::Responses(client) => client.set_api_key(api_key),
        }
    }

    /// Attach the Console billing org header the inference endpoint requires.
    fn set_org_header(&mut self, org_id: &str) {
        match self {
            Self::Chat(client) => client.set_extra_header("x-opencode-org-id", org_id.to_string()),
            Self::Responses(client) => {
                client.set_extra_header("x-opencode-org-id", org_id.to_string());
            }
        }
    }

    async fn complete_json(
        &self,
        request: PromptRequest,
        options: ModelRequestOptions,
    ) -> Result<serde_json::Value> {
        match self {
            Self::Chat(client) => client.complete_json(request, options).await,
            Self::Responses(client) => client.complete_json(request, options).await,
        }
    }

    async fn complete_agent_turn(
        &self,
        request: AgentTurnRequest,
        options: ModelRequestOptions,
    ) -> Result<AgentTurnStreamResult> {
        match self {
            Self::Chat(client) => client.complete_agent_turn(request, options).await,
            Self::Responses(client) => client.complete_agent_turn(request, options).await,
        }
    }

    fn token_usage_info(&self) -> TokenUsageInfo {
        match self {
            Self::Chat(client) => client.token_usage_info(),
            Self::Responses(client) => client.token_usage_info(),
        }
    }
}

/// OpenAI-compatible provider for OpenCode Console OAuth connections.
///
/// The access token is resolved (and refreshed) from a Daat-owned auth file on
/// each request, then installed on an inner chat-completions or responses
/// client chosen by the model's `api_style`.
pub struct OpenCodeOAuthClient {
    auth_file: PathBuf,
    model: String,
    auth_client: reqwest::Client,
    cached: tokio::sync::Mutex<Option<OpenCodeOAuthAccess>>,
    inner: tokio::sync::Mutex<OpenCodeInner>,
    request_budget_limits: RequestBudgetLimits,
}

impl OpenCodeOAuthClient {
    pub fn new(auth_file: PathBuf, base_url: Option<&str>, model_config: &ModelConfig) -> Self {
        let base_url =
            crate::config::normalize_provider_base_url(base_url.unwrap_or(OPENCODE_ZEN_BASE_URL));
        let inner = if model_config.api_style.as_deref() == Some("responses") {
            OpenCodeInner::Responses(ResponsesCompatibleClient::new("", &base_url, model_config))
        } else {
            OpenCodeInner::Chat(OpenAIClient::from_parts("", &base_url, model_config))
        };
        let request_budget_limits = RequestBudgetLimits {
            context_window_tokens: model_config.effective_context_window_tokens(),
            auto_compact_threshold_tokens: model_config.auto_compact_token_limit(),
            reserved_output_tokens: model_config.reserved_output_tokens(),
        };
        Self {
            auth_file,
            model: model_config.model_id.clone(),
            auth_client: auth_http_client().unwrap_or_default(),
            cached: tokio::sync::Mutex::new(None),
            inner: tokio::sync::Mutex::new(inner),
            request_budget_limits,
        }
    }

    async fn ensure_auth(&self) -> Result<()> {
        {
            let cached = self.cached.lock().await;
            if let Some(access) = cached.as_ref()
                && !access.needs_refresh()
            {
                return Ok(());
            }
        }
        let mut access =
            opencode_oauth_access_from_file_with_client(&self.auth_file, &self.auth_client)
                .await
                .map_err(|err| {
                    miette!(
                        "OpenCode auth at {} is unavailable: {err}",
                        self.auth_file.display()
                    )
                })?;
        // The Console-issued inference endpoint needs the billing org header.
        // Older token files predate that field, so resolve it on demand.
        if access.org_id.is_none() {
            let server = access
                .server
                .clone()
                .unwrap_or_else(|| OPENCODE_OAUTH_DEFAULT_SERVER.to_string());
            access.org_id = opencode_fetch_org_id(&self.auth_client, &server, &access.access_token)
                .await
                .ok()
                .flatten();
        }
        let mut inner = self.inner.lock().await;
        inner.set_api_key(access.access_token.clone());
        if let Some(org_id) = access.org_id.as_deref() {
            inner.set_org_header(org_id);
        }
        drop(inner);
        *self.cached.lock().await = Some(access);
        Ok(())
    }
}

#[async_trait]
impl ModelProvider for OpenCodeOAuthClient {
    async fn complete_json(
        &self,
        request: PromptRequest,
        options: ModelRequestOptions,
    ) -> Result<serde_json::Value> {
        self.ensure_auth().await?;
        self.inner
            .lock()
            .await
            .complete_json(request, options)
            .await
    }

    async fn complete_agent_turn(
        &self,
        request: AgentTurnRequest,
        options: ModelRequestOptions,
    ) -> Result<AgentTurnStreamResult> {
        self.ensure_auth().await?;
        self.inner
            .lock()
            .await
            .complete_agent_turn(request, options)
            .await
    }

    fn request_budget_limits(&self) -> RequestBudgetLimits {
        self.request_budget_limits
    }

    fn token_usage_info(&self) -> TokenUsageInfo {
        self.inner
            .try_lock()
            .map(|inner| inner.token_usage_info())
            .unwrap_or_default()
    }

    fn model_name(&self) -> String {
        self.model.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_verification_url_prefixes_relative_paths() {
        assert_eq!(
            absolute_verification_url(
                "https://opencode.ai/console",
                Some("/console/device?user_code=ABCD")
            ),
            "https://opencode.ai/console/device?user_code=ABCD"
        );
        assert_eq!(
            absolute_verification_url(
                "https://opencode.ai/console",
                Some("https://example.com/verify")
            ),
            "https://example.com/verify"
        );
        assert_eq!(
            absolute_verification_url("https://opencode.ai/console", None),
            "https://opencode.ai/console/device"
        );
    }

    #[test]
    fn expiry_from_secs_marks_unknown_expiry_as_zero() {
        assert_eq!(expiry_from_secs(None), 0);
        assert_eq!(expiry_from_secs(Some(0)), 0);
        assert!(expiry_from_secs(Some(600)) > now_ms());
    }
}
