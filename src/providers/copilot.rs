use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use miette::{Result, miette};

use crate::{
    config::{ModelConfig, redact_secret_text},
    core::{ModelProvider, ModelRequestOptions, TokenUsageInfo},
    reasoning::runtime::{AgentTurnRequest, AgentTurnStreamResult, PromptRequest},
};

use super::OpenAIClient;

// ---------------------------------------------------------------------------
// CopilotClient prefers a session token for the internal full-model API and
// falls back to the public API when needed.
// ---------------------------------------------------------------------------

const COPILOT_USER_AGENT: &str = "GitHubCopilotChat/0.26.7";
const COPILOT_EDITOR_VERSION: &str = "vscode/1.96.2";
const COPILOT_GITHUB_API_VERSION: &str = "2025-04-01";
const COPILOT_INTERNAL_BASE_URL: &str = "https://api.individual.githubcopilot.com";

struct CopilotSessionToken {
    expires_at_secs: u64,
}

pub struct CopilotClient {
    github_token: String,
    auth_client: reqwest::Client,
    cached: tokio::sync::Mutex<Option<CopilotSessionToken>>,
    inner: tokio::sync::Mutex<OpenAIClient>,
}

impl CopilotClient {
    pub fn new(github_token: &str, model_config: &ModelConfig) -> Self {
        let auth_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build copilot auth http client");
        let inner =
            OpenAIClient::from_parts("placeholder", COPILOT_INTERNAL_BASE_URL, model_config);
        Self {
            github_token: github_token.to_string(),
            auth_client,
            cached: tokio::sync::Mutex::new(None),
            inner: tokio::sync::Mutex::new(inner),
        }
    }

    async fn ensure_auth(&self) -> Result<()> {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let needs_exchange = {
            let cached = self.cached.lock().await;
            cached
                .as_ref()
                .is_none_or(|t| now_secs + 60 >= t.expires_at_secs)
        };

        if !needs_exchange {
            return Ok(());
        }

        let (token, base_url, expires_at_secs) = self.exchange_session_token().await?;
        tracing::info!(base_url = %base_url, "copilot: session token acquired");

        let mut hdrs = reqwest::header::HeaderMap::new();
        hdrs.insert("Editor-Version", COPILOT_EDITOR_VERSION.parse().unwrap());
        hdrs.insert("User-Agent", COPILOT_USER_AGENT.parse().unwrap());
        hdrs.insert(
            "X-Github-Api-Version",
            COPILOT_GITHUB_API_VERSION.parse().unwrap(),
        );

        let mut inner = self.inner.lock().await;
        inner.api_key.clone_from(&token);
        inner.base_url.clone_from(&base_url);
        inner.completions_path = "/chat/completions";
        inner.extra_headers = hdrs;
        drop(inner);

        *self.cached.lock().await = Some(CopilotSessionToken { expires_at_secs });
        Ok(())
    }

    async fn exchange_session_token(&self) -> Result<(String, String, u64)> {
        exchange_copilot_session_token_with_client(&self.auth_client, &self.github_token).await
    }
}

async fn exchange_copilot_session_token_with_client(
    auth_client: &reqwest::Client,
    github_token: &str,
) -> Result<(String, String, u64)> {
    tracing::debug!("copilot: exchanging github token for session token");
    let resp = auth_client
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Authorization", format!("Bearer {github_token}"))
        .header("Accept", "application/json")
        .header("User-Agent", COPILOT_USER_AGENT)
        .header("Editor-Version", COPILOT_EDITOR_VERSION)
        .header("X-Github-Api-Version", COPILOT_GITHUB_API_VERSION)
        .send()
        .await
        .map_err(|e| miette!("Copilot token exchange request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body = redact_secret_text(&body, github_token);
        tracing::debug!(http_status = %status, body = %body, "copilot session token exchange non-2xx");
        return Err(miette!("HTTP {status}"));
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| miette!("parse error: {e}"))?;

    let token = json["token"]
        .as_str()
        .ok_or_else(|| miette!("missing 'token' field"))?
        .to_string();
    let expires_at_secs = json["expires_at"].as_u64().unwrap_or(0);
    let base_url = derive_copilot_base_url(&token);

    Ok((token, base_url, expires_at_secs))
}

/// Session token is a semicolon-separated key=value string; derive API base URL from proxy-ep.
fn derive_copilot_base_url(session_token: &str) -> String {
    copilot_base_url_from_session_token(session_token)
        .unwrap_or_else(|| COPILOT_INTERNAL_BASE_URL.to_string())
}

/// Parse `proxy-ep` from a Copilot session token without positional offsets.
///
/// The token is semicolon-separated `key=value` pairs. `proxy-ep` is a host
/// (`proxy.example.com` is rewritten to `api.example.com`) or an absolute URL.
pub(crate) fn copilot_base_url_from_session_token(session_token: &str) -> Option<String> {
    let endpoint = semicolon_field(session_token, "proxy-ep")?;
    copilot_proxy_endpoint_to_base_url(&endpoint)
}

fn semicolon_field<'a>(token: &'a str, key: &str) -> Option<&'a str> {
    token.split(';').find_map(|part| {
        let (field, value) = part.trim().split_once('=')?;
        field.trim().eq_ignore_ascii_case(key).then_some(value.trim())
    })
}

fn copilot_proxy_endpoint_to_base_url(endpoint: &str) -> Option<String> {
    if endpoint.is_empty() {
        return None;
    }
    let parsed = parse_copilot_endpoint(endpoint)?;
    let host = copilot_host_string(parsed.host()?)?;
    let host = rewrite_copilot_proxy_host(&host);
    Some(format_copilot_base_url(&parsed, &host))
}

/// Parse an absolute HTTP(S) endpoint, or a host/port/path with an implied `https` scheme.
///
/// Userinfo is parsed so it does not become part of the host, but it is not copied
/// into the rebuilt base URL.
fn parse_copilot_endpoint(endpoint: &str) -> Option<url::Url> {
    if let Ok(parsed) = url::Url::parse(endpoint)
        && (parsed.scheme() == "http" || parsed.scheme() == "https")
        && parsed.host().is_some()
    {
        return Some(parsed);
    }
    let parsed = url::Url::parse(&format!("https://{endpoint}")).ok()?;
    parsed.host().is_some().then_some(parsed)
}

/// Rebuild `scheme://host[:port][path][?query]` after the proxy host rewrite.
///
/// A host-only URL has path `/` from the parser; that slash is omitted so
/// `proxy.example.com` stays `https://api.example.com`.
fn format_copilot_base_url(parsed: &url::Url, host: &str) -> String {
    let mut base = format!("{}://{host}", parsed.scheme());
    if let Some(port) = parsed.port() {
        base.push(':');
        base.push_str(&port.to_string());
    }
    let path = parsed.path();
    if path != "/" {
        base.push_str(path);
    }
    if let Some(query) = parsed.query() {
        base.push('?');
        base.push_str(query);
    }
    base
}

fn copilot_host_string(host: url::Host<&str>) -> Option<String> {
    match host {
        url::Host::Domain(domain) if !domain.is_empty() => Some(domain.to_string()),
        url::Host::Ipv4(addr) => Some(addr.to_string()),
        url::Host::Ipv6(addr) => Some(format!("[{addr}]")),
        url::Host::Domain(_) => None,
    }
}

fn rewrite_copilot_proxy_host(host: &str) -> String {
    let (name, rest) = host.split_once('.').unwrap_or((host, ""));
    if name.eq_ignore_ascii_case("proxy") && !rest.is_empty() {
        format!("api.{rest}")
    } else {
        host.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_ep_parses_without_magic_offset() {
        // "proxy-ep=" is 9 bytes; a shorter or longer key must not use that offset.
        let token = "tid=abc; Proxy-EP=proxy.individual.githubcopilot.com;exp=1";
        assert_eq!(
            derive_copilot_base_url(token),
            "https://api.individual.githubcopilot.com"
        );

        let short_key = "ep=proxy.individual.githubcopilot.com";
        assert_eq!(derive_copilot_base_url(short_key), COPILOT_INTERNAL_BASE_URL);

        let with_port_and_path = "proxy-ep=proxy.individual.githubcopilot.com:8443/v1";
        assert_eq!(
            derive_copilot_base_url(with_port_and_path),
            "https://api.individual.githubcopilot.com:8443/v1"
        );

        let absolute = "proxy-ep=https://user:secret@proxy.individual.githubcopilot.com:8443/v1?q=1";
        assert_eq!(
            derive_copilot_base_url(absolute),
            "https://api.individual.githubcopilot.com:8443/v1?q=1"
        );

        let ipv6 = "proxy-ep=[2001:db8::1]:8443/v1";
        assert_eq!(
            derive_copilot_base_url(ipv6),
            "https://[2001:db8::1]:8443/v1"
        );
    }
}

#[async_trait]
impl ModelProvider for CopilotClient {
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

    fn request_budget_limits(&self) -> crate::context_budget::RequestBudgetLimits {
        self.inner.try_lock().map_or(
            crate::context_budget::RequestBudgetLimits {
                context_window_tokens: crate::context_budget::DEFAULT_CONTEXT_WINDOW_TOKENS,
                auto_compact_threshold_tokens: crate::context_budget::DEFAULT_CONTEXT_WINDOW_TOKENS,
                reserved_output_tokens: crate::context_budget::DEFAULT_MAX_COMPLETION_TOKENS,
            },
            |inner| inner.request_budget_limits(),
        )
    }

    fn token_usage_info(&self) -> TokenUsageInfo {
        self.inner
            .try_lock()
            .map(|inner| inner.token_usage_info())
            .unwrap_or_default()
    }

    fn model_name(&self) -> String {
        self.inner
            .try_lock()
            .map(|inner| inner.model_name())
            .unwrap_or_default()
    }
}
