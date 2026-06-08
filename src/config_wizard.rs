//! Interactive configuration wizard for first-run setup and `config` subcommands.

use std::{collections::HashMap, sync::Arc, time::Duration};

use base64::Engine;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use miette::{Result, miette};
use ratatui::{
    DefaultTerminal, Frame, TerminalOptions, Viewport,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Wrap,
    },
};

use crate::{
    config::{
        Config, JudgeConfig, ModelConfig, ProviderConfig, TelegramConfig, ThinkingBudget,
        normalize_provider_base_url, redact_secret_text, resolve_env_reference, write_config,
    },
    i18n::Locale,
    model_catalog::{
        ModelCapacity, catalog_model_capacity, conservative_model_capacity,
        fetch_models_dev_capacity,
    },
    providers::{
        CodexOAuthTokens, codex_oauth_access_from_file, codex_oauth_auth_file,
        codex_oauth_client_version, codex_oauth_default_base_url, write_codex_oauth_tokens,
    },
};
use sha2::Digest;
use tokio::{net::TcpListener, sync::oneshot};

// ---------------------------------------------------------------------------
// GitHub OAuth device code flow
// ---------------------------------------------------------------------------

// Public Client ID used by the official GitHub Copilot app.
// Tokens from this flow can be exchanged through copilot_internal/v2/token
// for the session token that exposes the full Copilot model set.
const GITHUB_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const GITHUB_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

// Public client id and OAuth endpoints used by OpenAI Codex CLI for ChatGPT login.
const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_OAUTH_ISSUER: &str = "https://auth.openai.com";
const CODEX_OAUTH_DEFAULT_BROWSER_PORT: u16 = 1455;
const CODEX_DEVICE_USER_CODE_PATH: &str = "/api/accounts/deviceauth/usercode";
const CODEX_DEVICE_TOKEN_PATH: &str = "/api/accounts/deviceauth/token";
const CODEX_OAUTH_TOKEN_PATH: &str = "/oauth/token";
const CODEX_OAUTH_ORIGINATOR: &str = "codex_cli_rs";
const CODEX_OAUTH_SCOPES: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";

/// Run the GitHub OAuth device code flow and return an access token.
async fn run_github_device_flow<F>(locale: Locale, mut status: F) -> Result<String>
where
    F: FnMut(String, Vec<String>) -> Result<()>,
{
    let client_id = GITHUB_CLIENT_ID;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| {
            miette!(
                "{}",
                crate::tr!(locale, "github.http_client_failed", error = e)
            )
        })?;

    status(
        crate::tr!(locale, "github.authorization"),
        vec![crate::tr!(locale, "github.request_device_code")],
    )?;
    let resp = http
        .post(GITHUB_DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("client_id={}&scope=read%3Auser", urlenc(client_id)))
        .send()
        .await
        .map_err(|e| {
            miette!(
                "{}",
                crate::tr!(locale, "github.request_device_code_failed", error = e)
            )
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(miette!(
            "{}",
            crate::tr!(locale, "github.http_error", status = status, body = body)
        ));
    }

    let device: serde_json::Value = resp.json().await.map_err(|e| {
        miette!(
            "{}",
            crate::tr!(locale, "github.parse_device_code_failed", error = e)
        )
    })?;

    let device_code = device["device_code"]
        .as_str()
        .ok_or_else(|| miette!("{}", crate::tr!(locale, "github.missing_device_code")))?
        .to_string();
    let user_code = device["user_code"]
        .as_str()
        .ok_or_else(|| miette!("{}", crate::tr!(locale, "github.missing_user_code")))?
        .to_string();
    let verification_uri = device["verification_uri"]
        .as_str()
        .unwrap_or("https://github.com/login/device")
        .to_string();
    let expires_in = device["expires_in"].as_u64().unwrap_or(900);
    let interval_secs = device["interval"].as_u64().unwrap_or(5).max(5);

    let auth_title = crate::tr!(locale, "github.authorization");
    let auth_lines = vec![
        crate::tr!(locale, "github.open_url", url = verification_uri.clone()),
        crate::tr!(locale, "github.enter_code", code = user_code.clone()),
    ];
    status(auth_title.clone(), auth_lines.clone())?;

    let _ = open_browser(&verification_uri);

    let expires_at = std::time::Instant::now() + Duration::from_secs(expires_in);
    let poll_interval = Duration::from_secs(interval_secs);
    let mut dots = 0usize;

    loop {
        if std::time::Instant::now() >= expires_at {
            return Err(miette!("{}", crate::tr!(locale, "github.expired")));
        }

        tokio::time::sleep(poll_interval).await;

        dots = (dots + 1) % 4;
        let mut lines = auth_lines.clone();
        lines.push(crate::tr!(
            locale,
            "github.waiting",
            dots = ".".repeat(dots + 1)
        ));
        status(auth_title.clone(), lines)?;

        let poll_resp = http
            .post(GITHUB_ACCESS_TOKEN_URL)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!(
                "client_id={}&device_code={}&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
                urlenc(client_id),
                urlenc(&device_code),
            ))
            .send()
            .await
            .map_err(|e| miette!("{}", crate::tr!(locale, "github.poll_failed", error = e)))?;

        let body: serde_json::Value = poll_resp.json().await.map_err(|e| {
            miette!(
                "{}",
                crate::tr!(locale, "github.parse_token_failed", error = e)
            )
        })?;

        if let Some(token) = body["access_token"].as_str() {
            status(
                auth_title.clone(),
                vec![crate::tr!(locale, "github.success")],
            )?;
            return Ok(token.to_string());
        }

        match body["error"].as_str() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                // GitHub asks clients to slow down by adding an extra delay.
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            Some("expired_token") => {
                return Err(miette!("{}", crate::tr!(locale, "github.expired")));
            }
            Some("access_denied") => {
                return Err(miette!("{}", crate::tr!(locale, "github.access_denied")));
            }
            Some(other) => {
                return Err(miette!(
                    "{}",
                    crate::tr!(locale, "github.auth_error", error = other)
                ));
            }
            None => {
                return Err(miette!(
                    "{}",
                    crate::tr!(locale, "github.unknown_response", body = body)
                ));
            }
        }
    }
}

#[derive(serde::Deserialize)]
struct CodexDeviceUserCodeResponse {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    #[serde(default)]
    interval: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct CodexDeviceTokenResponse {
    authorization_code: String,
    code_verifier: String,
}

#[derive(serde::Deserialize)]
struct CodexOAuthTokenResponse {
    id_token: String,
    access_token: String,
    refresh_token: String,
}

#[derive(Debug, Clone)]
struct CodexPkceCodes {
    code_verifier: String,
    code_challenge: String,
}

#[derive(Clone)]
struct CodexOAuthCallbackState {
    expected_state: String,
    callback_tx: Arc<tokio::sync::Mutex<Option<oneshot::Sender<CodexOAuthCallbackResult>>>>,
}

#[derive(Debug)]
enum CodexOAuthCallbackResult {
    Code(String),
    Error(String),
}

#[derive(serde::Deserialize)]
struct CodexOAuthCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

struct CodexDevicePollContext<'a> {
    auth_title: String,
    auth_lines: Vec<String>,
    device_auth_id: &'a str,
    user_code: &'a str,
    interval: Duration,
}

async fn run_codex_oauth_browser_flow<F>(locale: Locale, mut status: F) -> Result<CodexOAuthTokens>
where
    F: FnMut(String, Vec<String>) -> Result<()>,
{
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| miette!("Codex OAuth HTTP client failed: {e}"))?;

    let pkce = generate_codex_pkce();
    let state = generate_codex_oauth_state();
    let listener = bind_codex_oauth_callback_listener().await?;
    let actual_port = listener
        .local_addr()
        .map_err(|e| miette!("Codex OAuth callback listener address failed: {e}"))?
        .port();
    let redirect_uri = format!("http://localhost:{actual_port}/auth/callback");
    let auth_url = build_codex_authorize_url(&redirect_uri, &pkce, &state);
    let (callback_tx, callback_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let app = axum::Router::new()
        .route(
            "/auth/callback",
            axum::routing::get(handle_codex_oauth_callback),
        )
        .with_state(CodexOAuthCallbackState {
            expected_state: state,
            callback_tx: Arc::new(tokio::sync::Mutex::new(Some(callback_tx))),
        });

    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
    });

    let auth_title = crate::tr!(locale, "codex_oauth.authorization");
    status(
        auth_title.clone(),
        vec![
            crate::tr!(locale, "codex_oauth.request_browser_login"),
            crate::tr!(locale, "codex_oauth.open_url", url = auth_url.clone()),
            crate::tr!(
                locale,
                "codex_oauth.callback_waiting",
                url = redirect_uri.clone()
            ),
        ],
    )?;

    let _ = open_browser(&auth_url);

    let callback = tokio::time::timeout(Duration::from_secs(15 * 60), callback_rx)
        .await
        .map_err(|_| miette!("Codex OAuth browser authorization timed out"))?
        .map_err(|_| miette!("Codex OAuth callback server stopped before authorization"))?;
    let _ = shutdown_tx.send(());
    match tokio::time::timeout(Duration::from_secs(2), server_handle).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(err))) => tracing::debug!("Codex OAuth callback server stopped: {err}"),
        Ok(Err(err)) => tracing::debug!("Codex OAuth callback server task failed: {err}"),
        Err(_) => tracing::debug!("Codex OAuth callback server did not stop within timeout"),
    }

    let authorization_code = match callback {
        CodexOAuthCallbackResult::Code(code) => {
            status(
                auth_title.clone(),
                vec![crate::tr!(locale, "codex_oauth.success")],
            )?;
            code
        }
        CodexOAuthCallbackResult::Error(error) => {
            return Err(miette!(
                "{}",
                crate::tr!(locale, "codex_oauth.browser_failed", error = error)
            ));
        }
    };

    exchange_codex_authorization_code_with_pkce(
        &http,
        &authorization_code,
        &pkce.code_verifier,
        &redirect_uri,
    )
    .await
}

async fn bind_codex_oauth_callback_listener() -> Result<TcpListener> {
    let default_addr =
        std::net::SocketAddr::from(([127, 0, 0, 1], CODEX_OAUTH_DEFAULT_BROWSER_PORT));
    match TcpListener::bind(default_addr).await {
        Ok(listener) => Ok(listener),
        Err(default_err) => {
            let fallback_addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
            TcpListener::bind(fallback_addr).await.map_err(|fallback_err| {
                miette!(
                    "Codex OAuth callback listener failed on {default_addr} ({default_err}) and on an ephemeral port ({fallback_err})"
                )
            })
        }
    }
}

async fn handle_codex_oauth_callback(
    axum::extract::State(state): axum::extract::State<CodexOAuthCallbackState>,
    axum::extract::Query(query): axum::extract::Query<CodexOAuthCallbackQuery>,
) -> impl axum::response::IntoResponse {
    let result = match (query.error, query.code, query.state) {
        (Some(error), _, _) => {
            let detail = query
                .error_description
                .filter(|description| !description.trim().is_empty())
                .map(|description| format!("{error}: {description}"))
                .unwrap_or(error);
            CodexOAuthCallbackResult::Error(detail)
        }
        (_, Some(code), Some(callback_state)) if callback_state == state.expected_state => {
            CodexOAuthCallbackResult::Code(code)
        }
        (_, Some(_), _) => {
            CodexOAuthCallbackResult::Error("OAuth callback state did not match".to_string())
        }
        _ => CodexOAuthCallbackResult::Error(
            "OAuth callback did not include an authorization code".to_string(),
        ),
    };

    let is_success = matches!(result, CodexOAuthCallbackResult::Code(_));
    if let Some(tx) = state.callback_tx.lock().await.take() {
        let _ = tx.send(result);
    }

    let status = if is_success {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::BAD_REQUEST
    };
    let title = if is_success {
        "OpenAI Codex OAuth complete"
    } else {
        "OpenAI Codex OAuth failed"
    };
    let body = if is_success {
        "Authorization is complete. You can close this tab and return to Daat Locus."
    } else {
        "Authorization failed. Return to Daat Locus for details."
    };
    (
        status,
        axum::response::Html(codex_oauth_callback_html(title, body)),
    )
}

fn codex_oauth_callback_html(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{title}</title>
  <style>
    body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; margin: 3rem; line-height: 1.5; }}
    main {{ max-width: 42rem; }}
  </style>
</head>
<body>
  <main>
    <h1>{title}</h1>
    <p>{body}</p>
  </main>
</body>
</html>"#
    )
}

fn build_codex_authorize_url(redirect_uri: &str, pkce: &CodexPkceCodes, state: &str) -> String {
    let query = [
        ("response_type", "code"),
        ("client_id", CODEX_OAUTH_CLIENT_ID),
        ("redirect_uri", redirect_uri),
        ("scope", CODEX_OAUTH_SCOPES),
        ("code_challenge", pkce.code_challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", state),
        ("originator", CODEX_OAUTH_ORIGINATOR),
    ];
    let qs = query
        .into_iter()
        .map(|(key, value)| format!("{key}={}", urlenc(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{CODEX_OAUTH_ISSUER}/oauth/authorize?{qs}")
}

fn generate_codex_pkce() -> CodexPkceCodes {
    let mut bytes = [0u8; 64];
    fill_uuid_random_bytes(&mut bytes);
    let code_verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let digest = sha2::Sha256::digest(code_verifier.as_bytes());
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    CodexPkceCodes {
        code_verifier,
        code_challenge,
    }
}

fn generate_codex_oauth_state() -> String {
    let mut bytes = [0u8; 32];
    fill_uuid_random_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn fill_uuid_random_bytes(bytes: &mut [u8]) {
    for chunk in bytes.chunks_mut(16) {
        let uuid = uuid::Uuid::new_v4();
        chunk.copy_from_slice(&uuid.as_bytes()[..chunk.len()]);
    }
}

async fn run_codex_oauth_device_flow<F>(locale: Locale, mut status: F) -> Result<CodexOAuthTokens>
where
    F: FnMut(String, Vec<String>) -> Result<()>,
{
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| miette!("Codex OAuth HTTP client failed: {e}"))?;

    let auth_title = crate::tr!(locale, "codex_oauth.authorization");
    status(
        auth_title.clone(),
        vec![crate::tr!(locale, "codex_oauth.request_device_code")],
    )?;
    let user_code_url = format!("{CODEX_OAUTH_ISSUER}{CODEX_DEVICE_USER_CODE_PATH}");
    let resp = http
        .post(user_code_url)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "client_id": CODEX_OAUTH_CLIENT_ID }))
        .send()
        .await
        .map_err(|e| miette!("Codex OAuth device code request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(miette!(
            "Codex OAuth device code request returned HTTP {status}: {body}"
        ));
    }

    let device: CodexDeviceUserCodeResponse = resp
        .json()
        .await
        .map_err(|e| miette!("Codex OAuth device code response parse failed: {e}"))?;
    let interval_secs = parse_codex_device_interval(&device.interval).max(5);
    let verification_url = format!("{CODEX_OAUTH_ISSUER}/codex/device");

    let auth_lines = vec![
        crate::tr!(
            locale,
            "codex_oauth.open_url",
            url = verification_url.clone()
        ),
        crate::tr!(
            locale,
            "codex_oauth.enter_code",
            code = device.user_code.clone()
        ),
        crate::tr!(locale, "codex_oauth.code_warning"),
    ];
    status(auth_title.clone(), auth_lines.clone())?;

    let _ = open_browser(&verification_url);

    let token_response = poll_codex_device_authorization(
        &http,
        locale,
        status,
        CodexDevicePollContext {
            auth_title,
            auth_lines,
            device_auth_id: &device.device_auth_id,
            user_code: &device.user_code,
            interval: Duration::from_secs(interval_secs),
        },
    )
    .await?;
    let redirect_uri = format!("{CODEX_OAUTH_ISSUER}/deviceauth/callback");
    let tokens = exchange_codex_authorization_code_with_pkce(
        &http,
        &token_response.authorization_code,
        &token_response.code_verifier,
        &redirect_uri,
    )
    .await?;
    Ok(tokens)
}

fn parse_codex_device_interval(value: &serde_json::Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
        .unwrap_or(5)
}

async fn poll_codex_device_authorization<F>(
    http: &reqwest::Client,
    locale: Locale,
    mut show_status: F,
    context: CodexDevicePollContext<'_>,
) -> Result<CodexDeviceTokenResponse>
where
    F: FnMut(String, Vec<String>) -> Result<()>,
{
    let token_url = format!("{CODEX_OAUTH_ISSUER}{CODEX_DEVICE_TOKEN_PATH}");
    let expires_at = std::time::Instant::now() + Duration::from_secs(15 * 60);
    let mut dots = 0usize;

    loop {
        if std::time::Instant::now() >= expires_at {
            return Err(miette!("Codex OAuth device authorization expired"));
        }

        tokio::time::sleep(context.interval).await;
        dots = (dots + 1) % 4;
        let mut lines = context.auth_lines.clone();
        lines.push(crate::tr!(
            locale,
            "codex_oauth.waiting",
            dots = ".".repeat(dots + 1)
        ));
        show_status(context.auth_title.clone(), lines)?;

        let resp = http
            .post(&token_url)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "device_auth_id": context.device_auth_id,
                "user_code": context.user_code,
            }))
            .send()
            .await
            .map_err(|e| miette!("Codex OAuth device polling failed: {e}"))?;

        let status = resp.status();
        if status.is_success() {
            show_status(
                context.auth_title.clone(),
                vec![crate::tr!(locale, "codex_oauth.success")],
            )?;
            return resp
                .json()
                .await
                .map_err(|e| miette!("Codex OAuth device token response parse failed: {e}"));
        }

        if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND {
            continue;
        }

        let body = resp.text().await.unwrap_or_default();
        return Err(miette!(
            "Codex OAuth device polling returned HTTP {status}: {body}"
        ));
    }
}

async fn exchange_codex_authorization_code_with_pkce(
    http: &reqwest::Client,
    authorization_code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<CodexOAuthTokens> {
    let token_url = format!("{CODEX_OAUTH_ISSUER}{CODEX_OAUTH_TOKEN_PATH}");
    let resp = http
        .post(token_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
            urlenc(authorization_code),
            urlenc(redirect_uri),
            urlenc(CODEX_OAUTH_CLIENT_ID),
            urlenc(code_verifier),
        ))
        .send()
        .await
        .map_err(|e| miette!("Codex OAuth token exchange failed: {e}"))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| miette!("Codex OAuth token exchange body read failed: {e}"))?;
    if !status.is_success() {
        return Err(miette!(
            "Codex OAuth token exchange returned HTTP {status}: {body}"
        ));
    }

    let tokens: CodexOAuthTokenResponse = serde_json::from_str(&body)
        .map_err(|e| miette!("Codex OAuth token exchange response parse failed: {e}"))?;
    Ok(CodexOAuthTokens {
        id_token: tokens.id_token,
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        account_id: None,
        last_refresh_at_ms: chrono::Utc::now().timestamp_millis(),
    })
}

/// Minimal percent-encoding for OAuth form fields and query values.
fn urlenc(s: &str) -> String {
    let mut encoded = String::new();
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    std::process::Command::new("open")
        .arg(url)
        .spawn()?
        .wait()?;
    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()?
        .wait()?;
    #[cfg(target_os = "windows")]
    std::process::Command::new("cmd")
        .args(["/c", "start", url])
        .spawn()?
        .wait()?;
    Ok(())
}

fn prompt_cancelled(locale: Locale) -> miette::Report {
    miette!("{}", crate::tr!(locale, "common.cancelled"))
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("config prompt requested parent navigation")]
#[diagnostic(code(config_wizard::navigate_parent))]
struct PromptNavigateParent;

fn prompt_navigate_parent() -> miette::Report {
    PromptNavigateParent.into()
}

fn is_prompt_navigate_parent(err: &miette::Report) -> bool {
    err.downcast_ref::<PromptNavigateParent>().is_some()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfigMenuPromptNavigation {
    ExitMenu,
    ReturnToMenu,
}

fn config_menu_navigation_from_prompt_error(
    err: &miette::Report,
    at_root: bool,
) -> Option<ConfigMenuPromptNavigation> {
    if !is_prompt_navigate_parent(err) {
        return None;
    }

    Some(if at_root {
        ConfigMenuPromptNavigation::ExitMenu
    } else {
        ConfigMenuPromptNavigation::ReturnToMenu
    })
}

const PROMPT_VIEWPORT_HEIGHT: u16 = 14;

struct PromptUi {
    terminal: Option<DefaultTerminal>,
    locale: Locale,
}

impl PromptUi {
    fn new(locale: Locale) -> Result<Self> {
        let mut ui = Self {
            terminal: None,
            locale,
        };
        ui.resume()?;
        Ok(ui)
    }

    fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
    }

    fn locale(&self) -> Locale {
        self.locale
    }

    fn resume(&mut self) -> Result<()> {
        if self.terminal.is_none() {
            self.terminal = Some(
                ratatui::try_init_with_options(TerminalOptions {
                    viewport: Viewport::Inline(PROMPT_VIEWPORT_HEIGHT),
                })
                .map_err(|e| {
                    miette!(
                        "{}",
                        crate::tr!(self.locale, "prompt_ui.init_failed", error = e)
                    )
                })?,
            );
        }
        Ok(())
    }

    fn suspend(&mut self) {
        if self.terminal.take().is_some() {
            let _ = ratatui::try_restore();
        }
    }

    fn terminal_mut(&mut self) -> Result<&mut DefaultTerminal> {
        self.resume()?;
        Ok(self.terminal.as_mut().expect("prompt terminal initialized"))
    }

    fn select<T: AsRef<str>>(
        &mut self,
        prompt: &str,
        items: &[T],
        default: usize,
    ) -> Result<usize> {
        self.select_inner(prompt, items, default, false)
    }

    fn select_compact<T: AsRef<str>>(&mut self, items: &[T], default: usize) -> Result<usize> {
        self.select_inner("", items, default, true)
    }

    fn select_inner<T: AsRef<str>>(
        &mut self,
        prompt: &str,
        items: &[T],
        default: usize,
        compact: bool,
    ) -> Result<usize> {
        if items.is_empty() {
            return Err(miette!(
                "{}",
                crate::tr!(self.locale, "prompt_ui.internal_empty_options")
            ));
        }

        let mut state = ListState::default().with_selected(Some(default.min(items.len() - 1)));

        loop {
            let locale = self.locale;
            self.terminal_mut()?
                .draw(|frame| {
                    render_select_prompt(frame, locale, prompt, items, &mut state, compact)
                })
                .map_err(|e| {
                    miette!(
                        "{}",
                        crate::tr!(locale, "prompt_ui.render_failed", error = e)
                    )
                })?;

            let key = read_prompt_key()?;
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    let current = state.selected().unwrap_or(0);
                    let next = if current == 0 {
                        items.len() - 1
                    } else {
                        current - 1
                    };
                    state.select(Some(next));
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let current = state.selected().unwrap_or(0);
                    let next = if current + 1 >= items.len() {
                        0
                    } else {
                        current + 1
                    };
                    state.select(Some(next));
                }
                KeyCode::Enter => return Ok(state.selected().unwrap_or(0)),
                KeyCode::Esc => return Err(prompt_navigate_parent()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(prompt_cancelled(self.locale));
                }
                _ => {}
            }
        }
    }

    fn text(&mut self, prompt: &str, default: Option<&str>) -> Result<String> {
        self.text_inner(prompt, default.unwrap_or_default().to_string(), false, None)
    }

    fn password(&mut self, prompt: &str) -> Result<String> {
        self.text_inner(prompt, String::new(), true, None)
    }

    fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool> {
        let items = [
            crate::tr!(self.locale, "common.yes"),
            crate::tr!(self.locale, "common.no"),
        ];
        Ok(self.select(prompt, &items, if default { 0 } else { 1 })? == 0)
    }

    fn usize(&mut self, prompt: &str, default: usize) -> Result<usize> {
        let mut current = default.to_string();
        let mut error: Option<String> = None;
        loop {
            let raw = self.text_inner(prompt, current, false, error.as_deref())?;
            match raw.trim().parse::<usize>() {
                Ok(value) => return Ok(value),
                Err(_) => {
                    current = raw;
                    error = Some(crate::tr!(self.locale, "prompt_ui.non_negative_integer"));
                }
            }
        }
    }

    fn text_inner(
        &mut self,
        prompt: &str,
        initial: String,
        secret: bool,
        error: Option<&str>,
    ) -> Result<String> {
        let mut value = initial;
        let mut cursor = value.len();
        let field_title = text_prompt_field_title(self.locale, prompt, secret, error.is_some());

        loop {
            let locale = self.locale;
            self.terminal_mut()?
                .draw(|frame| {
                    let render_state = TextPromptRenderState {
                        locale,
                        prompt,
                        field_title: &field_title,
                        value: &value,
                        cursor,
                        secret,
                        error,
                    };
                    render_text_prompt(frame, &render_state);
                })
                .map_err(|e| {
                    miette!(
                        "{}",
                        crate::tr!(locale, "prompt_ui.render_failed", error = e)
                    )
                })?;

            let key = read_prompt_key()?;
            match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(prompt_cancelled(self.locale));
                }
                KeyCode::Esc => return Err(prompt_navigate_parent()),
                KeyCode::Enter => return Ok(value),
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    value.insert(cursor, ch);
                    cursor += ch.len_utf8();
                }
                KeyCode::Backspace if cursor > 0 => {
                    let prev = previous_char_boundary(&value, cursor);
                    value.drain(prev..cursor);
                    cursor = prev;
                }
                KeyCode::Delete if cursor < value.len() => {
                    let next = next_char_boundary(&value, cursor);
                    value.drain(cursor..next);
                }
                KeyCode::Left => {
                    cursor = previous_char_boundary(&value, cursor);
                }
                KeyCode::Right => {
                    cursor = next_char_boundary(&value, cursor);
                }
                KeyCode::Home => cursor = 0,
                KeyCode::End => cursor = value.len(),
                _ => {}
            }
        }
    }

    fn loading(&mut self, prompt: &str, note: &str) -> Result<()> {
        let locale = self.locale;
        self.terminal_mut()?
            .draw(|frame| render_loading_prompt(frame, locale, prompt, note))
            .map(|_| ())
            .map_err(|e| {
                miette!(
                    "{}",
                    crate::tr!(locale, "prompt_ui.render_failed", error = e)
                )
            })
    }

    fn status(&mut self, prompt: &str, lines: &[String]) -> Result<()> {
        let locale = self.locale;
        self.terminal_mut()?
            .draw(|frame| render_status_prompt(frame, locale, prompt, lines))
            .map(|_| ())
            .map_err(|e| {
                miette!(
                    "{}",
                    crate::tr!(locale, "prompt_ui.render_failed", error = e)
                )
            })
    }

    fn detail(&mut self, prompt: &str, lines: &[String]) -> Result<()> {
        let mut scroll: u16 = 0;
        loop {
            let locale = self.locale;
            self.terminal_mut()?
                .draw(|frame| render_detail_prompt(frame, locale, prompt, lines, scroll))
                .map_err(|e| {
                    miette!(
                        "{}",
                        crate::tr!(locale, "prompt_ui.render_failed", error = e)
                    )
                })?;

            let key = read_prompt_key()?;
            match key.code {
                KeyCode::Esc | KeyCode::Enter => return Ok(()),
                KeyCode::Up
                | KeyCode::Char('k')
                | KeyCode::Down
                | KeyCode::Char('j')
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End => {
                    scroll = detail_scroll_offset(scroll, key.code, lines.len(), detail_body_rows())
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(prompt_cancelled(self.locale));
                }
                _ => {}
            }
        }
    }
}

fn detail_body_rows() -> u16 {
    // Inline viewport minus panel borders, header, range line, and help line.
    PROMPT_VIEWPORT_HEIGHT.saturating_sub(5).max(1)
}

fn detail_scroll_offset(current: u16, key: KeyCode, line_count: usize, visible_rows: u16) -> u16 {
    let max_scroll = line_count.saturating_sub(visible_rows.max(1) as usize) as u16;
    let page_step = visible_rows.max(1);
    match key {
        KeyCode::Up | KeyCode::Char('k') => current.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => current.saturating_add(1).min(max_scroll),
        KeyCode::PageUp => current.saturating_sub(page_step),
        KeyCode::PageDown => current.saturating_add(page_step).min(max_scroll),
        KeyCode::Home => 0,
        KeyCode::End => max_scroll,
        _ => current.min(max_scroll),
    }
}

impl Drop for PromptUi {
    fn drop(&mut self) {
        self.suspend();
    }
}

fn read_prompt_key() -> Result<crossterm::event::KeyEvent> {
    loop {
        let event = event::read().map_err(|e| miette!("failed to read terminal input: {e}"))?;
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            return Ok(key);
        }
    }
}

fn prompt_panel_block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Line::from(vec![Span::styled(
            "Config",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]))
}

fn render_select_prompt<T: AsRef<str>>(
    frame: &mut Frame,
    locale: Locale,
    prompt: &str,
    items: &[T],
    state: &mut ListState,
    compact: bool,
) {
    let block = prompt_panel_block();
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let (list_area, help_area) = if compact {
        let layout = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]);
        let [list_area, help_area] = inner.layout(&layout);
        (list_area, help_area)
    } else {
        let layout = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ]);
        let [prompt_area, list_area, help_area] = inner.layout(&layout);

        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                prompt.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )])),
            prompt_area,
        );

        (list_area, help_area)
    };

    let list_items: Vec<ListItem> = items
        .iter()
        .map(|item| {
            ListItem::new(Line::from(vec![
                Span::styled("· ", Style::default().fg(Color::DarkGray)),
                Span::styled(item.as_ref().to_string(), Style::default().fg(Color::Gray)),
            ]))
        })
        .collect();
    let list = List::new(list_items)
        .highlight_symbol("› ")
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, list_area, state);
    frame.render_widget(
        Paragraph::new(crate::tr!(locale, "prompt_ui.help_select"))
            .style(Style::default().fg(Color::DarkGray)),
        help_area,
    );
}

#[cfg(test)]
fn render_select_prompt_to_text<T: AsRef<str>>(
    locale: Locale,
    prompt: &str,
    items: &[T],
    compact: bool,
) -> String {
    use ratatui::{Terminal, backend::TestBackend};

    let backend = TestBackend::new(80, PROMPT_VIEWPORT_HEIGHT);
    let mut terminal = Terminal::new(backend).expect("test terminal initializes");
    let mut state = ListState::default().with_selected(Some(0));
    terminal
        .draw(|frame| render_select_prompt(frame, locale, prompt, items, &mut state, compact))
        .expect("test select prompt renders");

    buffer_to_text(terminal.backend().buffer())
}
#[cfg(test)]
fn render_text_prompt_to_text(
    locale: Locale,
    prompt: &str,
    value: &str,
    secret: bool,
    error: Option<&str>,
) -> String {
    use ratatui::{Terminal, backend::TestBackend};

    let backend = TestBackend::new(80, PROMPT_VIEWPORT_HEIGHT);
    let mut terminal = Terminal::new(backend).expect("test terminal initializes");
    let field_title = text_prompt_field_title(locale, prompt, secret, error.is_some());
    let render_state = TextPromptRenderState {
        locale,
        prompt,
        field_title: &field_title,
        value,
        cursor: value.len(),
        secret,
        error,
    };
    terminal
        .draw(|frame| render_text_prompt(frame, &render_state))
        .expect("test text prompt renders");

    buffer_to_text(terminal.backend().buffer())
}

#[cfg(test)]
fn buffer_to_text(buffer: &ratatui::buffer::Buffer) -> String {
    let width = buffer.area.width as usize;
    buffer
        .content()
        .chunks(width)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn text_prompt_field_title(locale: Locale, prompt: &str, secret: bool, numeric: bool) -> String {
    let prompt = prompt.to_lowercase();

    if prompt.contains("environment variable name") || prompt.contains("变量名") {
        return match locale {
            Locale::ZhCn => "名称".to_string(),
            Locale::EnUs => "Name".to_string(),
        };
    }
    if prompt.contains("host") || prompt.contains("主机") {
        return match locale {
            Locale::ZhCn => "主机".to_string(),
            Locale::EnUs => "Host".to_string(),
        };
    }
    if prompt.contains("base url") || prompt.contains("url") || prompt.contains("地址") {
        return "URL".to_string();
    }
    if numeric || prompt.contains("seconds") || prompt.contains("tokens") || prompt.contains("秒")
    {
        return match locale {
            Locale::ZhCn => "数值".to_string(),
            Locale::EnUs => "Number".to_string(),
        };
    }
    if prompt.contains("名称") || prompt.contains("name") {
        return match locale {
            Locale::ZhCn => "名称".to_string(),
            Locale::EnUs => "Name".to_string(),
        };
    }
    if prompt.contains("token")
        || prompt.contains("api key")
        || prompt.contains("key")
        || prompt.contains("密钥")
    {
        return match locale {
            Locale::ZhCn => "密钥".to_string(),
            Locale::EnUs => "Key".to_string(),
        };
    }
    if prompt.contains("model id") || prompt.ends_with(" id") {
        return "ID".to_string();
    }
    if prompt.contains("路径")
        || prompt.contains("path")
        || prompt.contains("文件")
        || prompt.contains("file")
    {
        return match locale {
            Locale::ZhCn => "路径".to_string(),
            Locale::EnUs => "Path".to_string(),
        };
    }
    if prompt.contains("duration") || prompt.contains("时长") {
        return match locale {
            Locale::ZhCn => "时长".to_string(),
            Locale::EnUs => "Duration".to_string(),
        };
    }
    if secret {
        return match locale {
            Locale::ZhCn => "密文".to_string(),
            Locale::EnUs => "Secret".to_string(),
        };
    }

    match locale {
        Locale::ZhCn => "输入".to_string(),
        Locale::EnUs => "Input".to_string(),
    }
}

struct TextPromptRenderState<'a> {
    locale: Locale,
    prompt: &'a str,
    field_title: &'a str,
    value: &'a str,
    cursor: usize,
    secret: bool,
    error: Option<&'a str>,
}

fn render_text_prompt(frame: &mut Frame, state: &TextPromptRenderState<'_>) {
    let block = prompt_panel_block();
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ]);
    let [prompt_area, input_area, help_area, note_area] = inner.layout(&layout);

    let display = if state.secret {
        "*".repeat(state.value.chars().count())
    } else {
        state.value.to_string()
    };
    let input = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Cyan)),
        Span::raw(display),
    ]);
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            state.prompt.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )])),
        prompt_area,
    );

    let field_block = Block::default()
        .borders(Borders::ALL)
        .border_style(if state.error.is_some() {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Cyan)
        })
        .title(Line::from(Span::styled(
            state.field_title.to_string(),
            Style::default().fg(Color::DarkGray),
        )));
    let field_inner = field_block.inner(input_area);
    frame.render_widget(
        Paragraph::new(input)
            .block(field_block)
            .wrap(Wrap { trim: false }),
        input_area,
    );
    frame.set_cursor_position((
        field_inner.x + 2 + state.value[..state.cursor].chars().count() as u16,
        field_inner.y,
    ));

    frame.render_widget(
        Paragraph::new(crate::tr!(state.locale, "prompt_ui.help_text"))
            .style(Style::default().fg(Color::DarkGray)),
        help_area,
    );
    frame.render_widget(
        Paragraph::new(match state.error {
            Some(error) => Line::from(Span::styled(
                error.to_string(),
                Style::default().fg(Color::Red),
            )),
            None => Line::raw(""),
        }),
        note_area,
    );
}

fn render_loading_prompt(frame: &mut Frame, locale: Locale, prompt: &str, note: &str) {
    let block = prompt_panel_block();
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ]);
    let [prompt_area, body_area, help_area] = inner.layout(&layout);

    let mut prompt_spans = vec![Span::styled(
        prompt.to_string(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )];
    if !note.trim().is_empty() {
        prompt_spans.push(Span::raw("  ·  "));
        prompt_spans.push(Span::styled(
            note.to_string(),
            Style::default().fg(Color::Gray),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(prompt_spans)), prompt_area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("•", Style::default().fg(Color::Cyan)),
            Span::raw(" "),
            Span::styled(
                crate::tr!(locale, "prompt_ui.loading_body"),
                Style::default().fg(Color::White),
            ),
        ])),
        body_area,
    );
    frame.render_widget(
        Paragraph::new(crate::tr!(locale, "prompt_ui.loading_help"))
            .style(Style::default().fg(Color::DarkGray)),
        help_area,
    );
}

fn render_detail_prompt(
    frame: &mut Frame,
    locale: Locale,
    prompt: &str,
    lines: &[String],
    scroll: u16,
) {
    let block = prompt_panel_block();
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ]);
    let [prompt_area, body_area, help_area] = inner.layout(&layout);

    let visible_rows = body_area.height.max(1) as usize;
    let total_rows = lines.len().max(1);
    let visible_end = (scroll as usize)
        .saturating_add(visible_rows)
        .min(total_rows);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                prompt.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  ·  "),
            Span::styled(
                format!(
                    "{}  ·  {}-{} / {}",
                    crate::tr!(locale, "prompt_ui.line_count", count = lines.len()),
                    (scroll as usize).min(total_rows).saturating_add(1),
                    visible_end,
                    total_rows
                ),
                Style::default().fg(Color::Gray),
            ),
        ])),
        prompt_area,
    );
    frame.render_widget(
        Paragraph::new(
            lines
                .iter()
                .map(|line| {
                    Line::from(Span::styled(line.clone(), Style::default().fg(Color::Gray)))
                })
                .collect::<Vec<_>>(),
        )
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false }),
        body_area,
    );

    if lines.len() > visible_rows {
        let mut scrollbar = ScrollbarState::new(lines.len()).position(scroll as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            body_area,
            &mut scrollbar,
        );
    }

    frame.render_widget(
        Paragraph::new(crate::tr!(locale, "prompt_ui.help_detail"))
            .style(Style::default().fg(Color::DarkGray)),
        help_area,
    );
}

fn render_status_prompt(frame: &mut Frame, locale: Locale, prompt: &str, lines: &[String]) {
    let block = prompt_panel_block();
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ]);
    let [prompt_area, body_area, help_area] = inner.layout(&layout);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                prompt.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  ·  "),
            Span::styled(
                crate::tr!(locale, "prompt_ui.line_count", count = lines.len()),
                Style::default().fg(Color::Gray),
            ),
        ])),
        prompt_area,
    );
    frame.render_widget(
        Paragraph::new(
            lines
                .iter()
                .map(|line| {
                    Line::from(Span::styled(line.clone(), Style::default().fg(Color::Gray)))
                })
                .collect::<Vec<_>>(),
        )
        .wrap(Wrap { trim: false }),
        body_area,
    );
    frame.render_widget(
        Paragraph::new(crate::tr!(locale, "prompt_ui.loading_help"))
            .style(Style::default().fg(Color::DarkGray)),
        help_area,
    );
}

fn previous_char_boundary(s: &str, index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    s[..index]
        .char_indices()
        .next_back()
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn next_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    s[index..]
        .char_indices()
        .nth(1)
        .map(|(offset, _)| index + offset)
        .unwrap_or(s.len())
}

// ---------------------------------------------------------------------------
// Provider wizard
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum ProviderKind {
    OpenAI,
    OpenAICodexOauth,
    GithubCopilot,
    OpenAICompatible,
    Ollama,
    OllamaCloud,
}

impl ProviderKind {
    fn labels(locale: Locale) -> Vec<String> {
        vec![
            "OpenAI".to_string(),
            "OpenAI Codex OAuth".to_string(),
            "GitHub Copilot".to_string(),
            crate::tr!(locale, "config.provider_openai_compatible"),
            crate::tr!(locale, "config.provider_ollama_local"),
            crate::tr!(locale, "config.provider_ollama_cloud"),
        ]
    }

    fn from_index(i: usize) -> Self {
        match i {
            0 => Self::OpenAI,
            1 => Self::OpenAICodexOauth,
            2 => Self::GithubCopilot,
            3 => Self::OpenAICompatible,
            4 => Self::Ollama,
            _ => Self::OllamaCloud,
        }
    }
}

/// Prompt for one provider definition and return its name and config.
async fn prompt_provider(
    ui: &mut PromptUi,
    existing_names: &[String],
) -> Result<(String, ProviderConfig)> {
    let locale = ui.locale();
    let labels = ProviderKind::labels(locale);
    let kind_idx = ui.select(&crate::tr!(locale, "config.provider_kind"), &labels, 0)?;
    let kind = ProviderKind::from_index(kind_idx);

    let default_name = match kind {
        ProviderKind::OpenAI => "openai",
        ProviderKind::OpenAICodexOauth => "codex-oauth",
        ProviderKind::GithubCopilot => "copilot",
        ProviderKind::OpenAICompatible => "openai",
        ProviderKind::Ollama => "ollama",
        ProviderKind::OllamaCloud => "ollama-cloud",
    };
    // Suffix duplicate defaults to avoid a collision.
    let default_name = if existing_names.contains(&default_name.to_string()) {
        format!("{}-2", default_name)
    } else {
        default_name.to_string()
    };

    let name = ui.text(
        &crate::tr!(locale, "config.provider_name"),
        Some(&default_name),
    )?;

    let provider = match kind {
        ProviderKind::OpenAI => {
            let api_key = ui.password(&crate::tr!(locale, "config.openai_api_key"))?;
            let use_custom_url =
                ui.confirm(&crate::tr!(locale, "config.custom_base_url"), false)?;
            let base_url = if use_custom_url {
                let url = ui.text(&crate::tr!(locale, "config.base_url_openai"), None)?;
                Some(normalize_provider_base_url(&url))
            } else {
                None
            };
            ProviderConfig::Openai { api_key, base_url }
        }
        ProviderKind::OpenAICodexOauth => {
            let auth_method = ui.select(
                &crate::tr!(locale, "config.codex_oauth_auth_method"),
                &[
                    crate::tr!(locale, "config.codex_oauth_browser_login"),
                    crate::tr!(locale, "config.codex_oauth_device_login"),
                    crate::tr!(locale, "config.codex_oauth_auth_file"),
                ],
                0,
            )?;
            let default_auth_file = codex_oauth_auth_file(&name, None);
            let auth_file = if auth_method == 0 {
                let result = run_codex_oauth_browser_flow(locale, |prompt, lines| {
                    ui.status(&prompt, &lines)
                })
                .await;
                let tokens = result?;
                write_codex_oauth_tokens(&default_auth_file, &tokens).await?;
                Some(default_auth_file.to_string_lossy().to_string())
            } else if auth_method == 1 {
                let result =
                    run_codex_oauth_device_flow(locale, |prompt, lines| ui.status(&prompt, &lines))
                        .await;
                let tokens = result?;
                write_codex_oauth_tokens(&default_auth_file, &tokens).await?;
                Some(default_auth_file.to_string_lossy().to_string())
            } else {
                let default_auth_file_display = default_auth_file.to_string_lossy().to_string();
                let path = ui.text(
                    &crate::tr!(locale, "config.codex_oauth_auth_file_path"),
                    Some(&default_auth_file_display),
                )?;
                Some(path)
            };
            let use_custom_url =
                ui.confirm(&crate::tr!(locale, "config.custom_base_url"), false)?;
            let base_url = if use_custom_url {
                let url = ui.text(&crate::tr!(locale, "config.base_url_openai"), None)?;
                Some(normalize_provider_base_url(&url))
            } else {
                None
            };
            ProviderConfig::OpenaiCodexOauth {
                auth_file,
                base_url,
            }
        }
        ProviderKind::GithubCopilot => {
            let auth_method = ui.select(
                &crate::tr!(locale, "config.github_auth_method"),
                &[
                    crate::tr!(locale, "config.github_device_login"),
                    crate::tr!(locale, "config.github_manual_token"),
                    crate::tr!(locale, "config.github_env_token"),
                ],
                0,
            )?;

            let github_token = match auth_method {
                0 => {
                    let result =
                        run_github_device_flow(locale, |prompt, lines| ui.status(&prompt, &lines))
                            .await;
                    result?
                }
                1 => ui.password(&crate::tr!(locale, "config.github_token"))?,
                _ => "${GITHUB_TOKEN}".to_string(),
            };
            ProviderConfig::GithubCopilot { github_token }
        }
        ProviderKind::OpenAICompatible => {
            let base_url = ui.text(
                &crate::tr!(locale, "config.base_url_local"),
                Some("https://api.openai.com/v1"),
            )?;
            let api_key = ui.text(&crate::tr!(locale, "config.local_api_key"), None)?;
            ProviderConfig::OpenaiCompatible {
                base_url: normalize_provider_base_url(&base_url),
                api_key,
                api_style: None,
            }
        }
        ProviderKind::Ollama => {
            let default_host = "http://127.0.0.1:11434".to_string();
            let host = ui.text(
                &crate::tr!(locale, "config.ollama_host"),
                Some(&default_host),
            )?;
            let host = if host.trim().is_empty() {
                default_host
            } else {
                host
            };
            let use_keep_alive = ui.confirm(
                &crate::tr!(locale, "config.ollama_keep_alive_enable"),
                false,
            )?;
            let keep_alive = if use_keep_alive {
                let v = ui.text(&crate::tr!(locale, "config.ollama_keep_alive"), Some("5m"))?;
                if v.trim().is_empty() { None } else { Some(v) }
            } else {
                None
            };
            ProviderConfig::Ollama {
                host: Some(host),
                api_key: None,
                keep_alive,
            }
        }
        ProviderKind::OllamaCloud => {
            let host = "https://ollama.com".to_string();
            let api_key = ui.password(&crate::tr!(locale, "config.ollama_cloud_api_key"))?;
            let use_keep_alive = ui.confirm(
                &crate::tr!(locale, "config.ollama_keep_alive_enable"),
                false,
            )?;
            let keep_alive = if use_keep_alive {
                let v = ui.text(&crate::tr!(locale, "config.ollama_keep_alive"), Some("5m"))?;
                if v.trim().is_empty() { None } else { Some(v) }
            } else {
                None
            };
            ProviderConfig::Ollama {
                host: Some(host),
                api_key: Some(api_key),
                keep_alive,
            }
        }
    };

    Ok((name, provider))
}

// ---------------------------------------------------------------------------
// Model discovery
// ---------------------------------------------------------------------------

/// Static fallback list of known GitHub Copilot models.
const COPILOT_DEFAULT_MODELS: &[&str] = &[
    "claude-sonnet-4.6",
    "claude-sonnet-4.5",
    "claude-opus-4.5",
    "gpt-4o",
    "gpt-4.1",
    "gpt-4.1-mini",
    "gpt-4.1-nano",
    "o3-mini",
    "o1",
    "o1-mini",
];

/// Static fallback for Codex OAuth. The ChatGPT Codex backend may return an
/// empty `/models` list while still accepting current Codex model slugs.
const CODEX_OAUTH_DEFAULT_MODELS: &[&str] = &["gpt-5.4", "gpt-5.4-mini"];

fn codex_oauth_fallback_models() -> Vec<DiscoveredModel> {
    CODEX_OAUTH_DEFAULT_MODELS
        .iter()
        .map(|id| {
            let capacity = catalog_model_capacity(id);
            DiscoveredModel {
                id: (*id).to_string(),
                context_window: capacity.map(|capacity| capacity.context_window_tokens),
                max_output_tokens: capacity.map(|capacity| capacity.max_completion_tokens),
                supports_vision: capacity.map(|c| c.supports_vision),
            }
        })
        .collect()
}

fn resolve_model_capacity(
    model_id: &str,
    detected_context_window: Option<usize>,
    detected_max_output: Option<usize>,
    detected_supports_vision: Option<bool>,
) -> ModelCapacity {
    let catalog = catalog_model_capacity(model_id);
    let fallback = conservative_model_capacity();

    let models_dev = || fetch_models_dev_capacity(model_id);
    let ctx = || models_dev().map(|c| c.context_window_tokens);
    let out = || models_dev().map(|c| c.max_completion_tokens);

    ModelCapacity {
        context_window_tokens: detected_context_window
            .or_else(|| catalog.map(|capacity| capacity.context_window_tokens))
            .or_else(ctx)
            .unwrap_or(fallback.context_window_tokens),
        max_completion_tokens: detected_max_output
            .or_else(|| catalog.map(|capacity| capacity.max_completion_tokens))
            .or_else(out)
            .unwrap_or(fallback.max_completion_tokens),
        supports_vision: detected_supports_vision.unwrap_or_else(|| {
            catalog
                .map(|c| c.supports_vision)
                .unwrap_or(fallback.supports_vision)
        }),
        supports_tool_call: catalog
            .map(|c| c.supports_tool_call)
            .unwrap_or(fallback.supports_tool_call),
    }
}

/// Discover Copilot models via the internal session-token API, falling back to a static list.
async fn fetch_copilot_models(github_token: &str) -> Vec<DiscoveredModel> {
    let fallback = || {
        COPILOT_DEFAULT_MODELS
            .iter()
            .map(|s| DiscoveredModel {
                id: s.to_string(),
                context_window: None,
                max_output_tokens: None,
                supports_vision: None,
            })
            .collect::<Vec<_>>()
    };

    let token = resolve_env_reference(github_token);
    if token.is_empty() {
        tracing::warn!("copilot model discovery: github token empty, using static list");
        return fallback();
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("copilot model discovery: http client error: {e}");
            return fallback();
        }
    };

    match try_fetch_via_session_token(&client, &token).await {
        Some(models) => {
            tracing::info!(
                "copilot model discovery: {} models via internal API",
                models.len()
            );
            models
        }
        None => {
            tracing::warn!(
                "copilot model discovery: session token exchange failed, using static list"
            );
            fallback()
        }
    }
}

async fn try_fetch_via_session_token(
    client: &reqwest::Client,
    github_token: &str,
) -> Option<Vec<DiscoveredModel>> {
    let resp = client
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Authorization", format!("Bearer {github_token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("Editor-Version", "vscode/1.96.2")
        .header("X-Github-Api-Version", "2025-04-01")
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        tracing::debug!(http_status = %resp.status(), "copilot model discovery: session token exchange failed");
        return None;
    }

    let json: serde_json::Value = resp.json().await.ok()?;
    let session_token = json["token"].as_str()?.to_string();

    let base_url = session_token
        .split(';')
        .find_map(|part| {
            let trimmed = part.trim();
            let host = trimmed.strip_prefix("proxy-ep=").or_else(|| {
                if trimmed.to_lowercase().starts_with("proxy-ep=") {
                    Some(&trimmed[9..])
                } else {
                    None
                }
            })?;
            if host.is_empty() {
                return None;
            }
            let host = if host.to_lowercase().starts_with("proxy.") {
                format!("api.{}", &host[6..])
            } else {
                host.to_string()
            };
            Some(format!("https://{host}"))
        })
        .unwrap_or_else(|| "https://api.individual.githubcopilot.com".to_string());

    let models =
        fetch_copilot_internal_models(client, &format!("{base_url}/models"), &session_token).await;
    if models.is_empty() {
        None
    } else {
        Some(models)
    }
}

/// Model metadata returned by the provider API.
#[derive(Debug, Clone)]
struct DiscoveredModel {
    id: String,
    context_window: Option<usize>,
    max_output_tokens: Option<usize>,
    supports_vision: Option<bool>,
}

/// Fetch provider model IDs. Failures return an empty list.
async fn fetch_model_ids(provider_name: &str, provider: &ProviderConfig) -> Vec<DiscoveredModel> {
    match provider {
        ProviderConfig::GithubCopilot { github_token } => fetch_copilot_models(github_token).await,
        ProviderConfig::Openai { api_key, base_url } => {
            let base = base_url.as_deref().unwrap_or("https://api.openai.com/v1");
            let api_key = resolve_env_reference(api_key);
            fetch_openai_models(base, &api_key).await
        }
        ProviderConfig::OpenaiCodexOauth {
            auth_file,
            base_url,
        } => {
            let base = base_url
                .as_deref()
                .unwrap_or(codex_oauth_default_base_url());
            fetch_codex_oauth_models(provider_name, auth_file.as_deref(), base).await
        }
        ProviderConfig::OpenaiCompatible {
            base_url, api_key, ..
        } => {
            let api_key = resolve_env_reference(api_key);
            fetch_openai_models(base_url, &api_key).await
        }
        ProviderConfig::Ollama { host, .. } => {
            let host = host
                .as_deref()
                .map(|h| h.to_string())
                .unwrap_or_else(|| "http://127.0.0.1:11434".to_string());
            fetch_ollama_models(&host).await
        }
    }
}

async fn fetch_openai_models(base_url: &str, api_key: &str) -> Vec<DiscoveredModel> {
    let url = format!("{}/models", normalize_provider_base_url(base_url));
    fetch_openai_models_path(&url, api_key).await
}

async fn fetch_openai_models_path(url: &str, api_key: &str) -> Vec<DiscoveredModel> {
    let url = url.to_string();
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("fetch_openai_models: failed to build http client: {e}");
            return vec![];
        }
    };
    let resp = match client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(url = %url, "fetch_openai_models: request failed: {e}");
            return vec![];
        }
    };
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body = redact_secret_text(&body, api_key);
        tracing::warn!(url = %url, http_status = %status, body = %body, "fetch_openai_models: non-2xx response");
        return vec![];
    }
    parse_models_response(resp.json().await.ok())
}

async fn fetch_ollama_models(host: &str) -> Vec<DiscoveredModel> {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("fetch_ollama_models: failed to build http client: {e}");
            return vec![];
        }
    };

    let tags_url = format!("{host}/api/tags");
    let resp = match client.get(&tags_url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(url = %tags_url, "fetch_ollama_models: request failed: {e}");
            return vec![];
        }
    };
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(url = %tags_url, http_status = %status, "fetch_ollama_models: non-2xx");
        return vec![];
    }
    let tags_json: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("fetch_ollama_models: failed to parse JSON: {e}");
            return vec![];
        }
    };
    let Some(model_list) = tags_json.get("models").and_then(|m| m.as_array()) else {
        return vec![];
    };

    let model_ids: Vec<String> = model_list
        .iter()
        .filter_map(|m| {
            m.get("model")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    let show_client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            return model_ids
                .into_iter()
                .map(|id| DiscoveredModel {
                    id,
                    context_window: None,
                    max_output_tokens: None,
                    supports_vision: None,
                })
                .collect();
        }
    };

    let mut handles = Vec::new();
    for model_id in model_ids {
        let client = show_client.clone();
        let url = format!("{host}/api/show");
        let handle = tokio::spawn(async move {
            let resp = client
                .post(&url)
                .json(&serde_json::json!({"model": model_id, "verbose": true}))
                .send()
                .await?;
            if !resp.status().is_success() {
                return Ok::<_, reqwest::Error>((model_id, None, None));
            }
            let json: serde_json::Value = resp.json().await?;
            let ctx = extract_context_from_model_info(&json);
            let vision = extract_vision_from_capabilities(&json);
            Ok((model_id, ctx, vision))
        });
        handles.push(handle);
    }

    let mut discovered = Vec::new();
    for handle in handles {
        if let Ok(Ok((id, ctx, vision))) = handle.await {
            discovered.push(DiscoveredModel {
                id,
                context_window: ctx,
                max_output_tokens: None,
                supports_vision: vision,
            });
        }
    }
    discovered.sort_by(|a, b| a.id.cmp(&b.id));
    discovered
}

fn extract_context_from_model_info(response: &serde_json::Value) -> Option<usize> {
    let info = response.get("model_info")?;
    if let Some(obj) = info.as_object() {
        for (key, val) in obj {
            if let Some(ctx) = extract_context_value(key, val) {
                return Some(ctx);
            }
        }
    }
    None
}

fn extract_vision_from_capabilities(response: &serde_json::Value) -> Option<bool> {
    let caps = response.get("capabilities")?.as_array()?;
    for cap in caps {
        if let Some(s) = cap.as_str()
            && s == "vision"
        {
            return Some(true);
        }
    }
    Some(false)
}

fn extract_context_value(key: &str, val: &serde_json::Value) -> Option<usize> {
    if key.ends_with("context_length") {
        if let Some(n) = val.as_u64() {
            return Some(n as usize);
        }
        if let Some(n) = val.as_i64()
            && n > 0
        {
            return Some(n as usize);
        }
    }
    if let Some(inner) = val.as_object() {
        for (sub_key, sub_val) in inner {
            if let Some(ctx) = extract_context_value(sub_key, sub_val) {
                return Some(ctx);
            }
        }
    }
    None
}

async fn fetch_codex_oauth_models(
    provider_name: &str,
    auth_file: Option<&str>,
    base_url: &str,
) -> Vec<DiscoveredModel> {
    let auth_file = codex_oauth_auth_file(provider_name, auth_file);
    let access = match codex_oauth_access_from_file(&auth_file).await {
        Ok(access) => access,
        Err(err) => {
            tracing::warn!(
                auth_file = %auth_file.display(),
                "Codex OAuth model discovery: auth unavailable: {err}"
            );
            return vec![];
        }
    };
    let url = format!(
        "{}/models?client_version={}",
        normalize_provider_base_url(base_url),
        codex_oauth_client_version()
    );
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Codex OAuth model discovery: failed to build http client: {e}");
            return vec![];
        }
    };
    let mut request = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", access.access_token))
        .header("version", codex_oauth_client_version())
        .header("originator", "codex_cli_rs");
    if let Some(account_id) = access.account_id.as_deref() {
        request = request.header("ChatGPT-Account-ID", account_id);
    }
    if access.is_fedramp_account {
        request = request.header("X-OpenAI-Fedramp", "true");
    }
    let resp = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(url = %url, "Codex OAuth model discovery request failed: {e}");
            return vec![];
        }
    };
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body = redact_secret_text(&body, &access.access_token);
        tracing::warn!(url = %url, http_status = %status, body = %body, "Codex OAuth model discovery non-2xx");
        return vec![];
    }
    let models = parse_models_response(resp.json().await.ok());
    if models.is_empty() {
        codex_oauth_fallback_models()
    } else {
        models
    }
}

async fn fetch_copilot_internal_models(
    client: &reqwest::Client,
    url: &str,
    session_token: &str,
) -> Vec<DiscoveredModel> {
    let resp = match client
        .get(url)
        .header("Authorization", format!("Bearer {session_token}"))
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("Editor-Version", "vscode/1.96.2")
        .header("X-Github-Api-Version", "2025-04-01")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(url, "copilot internal models request failed: {e}");
            return vec![];
        }
    };
    if !resp.status().is_success() {
        let s = resp.status();
        let b = resp.text().await.unwrap_or_default();
        let b = redact_secret_text(&b, session_token);
        tracing::warn!(url, http_status = %s, body = %b, "copilot internal models non-2xx");
        return vec![];
    }
    parse_models_response(resp.json().await.ok())
}

fn parse_models_response(json: Option<serde_json::Value>) -> Vec<DiscoveredModel> {
    let json = match json {
        Some(j) => j,
        None => return vec![],
    };
    let items = json
        .get("data")
        .or_else(|| json.get("models"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut models: Vec<DiscoveredModel> = items
        .iter()
        .filter_map(|m| {
            if m["supported_in_api"].as_bool() == Some(false)
                || m["visibility"].as_str() == Some("hide")
            {
                return None;
            }
            let id = m["id"].as_str().or_else(|| m["slug"].as_str())?.to_string();
            let limits = &m["capabilities"]["limits"];
            let context_window = limits["max_context_window_tokens"]
                .as_u64()
                .or_else(|| m["context_window"].as_u64())
                .or_else(|| m["max_context_window"].as_u64())
                .map(|v| v as usize);
            let max_output_tokens = limits["max_output_tokens"]
                .as_u64()
                .or_else(|| m["max_output_tokens"].as_u64())
                .map(|v| v as usize);
            Some(DiscoveredModel {
                id,
                context_window,
                max_output_tokens,
                supports_vision: None,
            })
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

// ---------------------------------------------------------------------------
// Model wizard
// ---------------------------------------------------------------------------

/// Prompt for one model definition and return its name and config.
async fn prompt_model(
    ui: &mut PromptUi,
    provider_name: &str,
    provider: &ProviderConfig,
) -> Result<(String, ModelConfig)> {
    let locale = ui.locale();
    ui.loading(
        &crate::tr!(locale, "config.discover_models"),
        &format!("provider: {provider_name}"),
    )?;
    let discovered = fetch_model_ids(provider_name, provider).await;

    let (model_id, api_ctx, api_out, api_vision) = if discovered.is_empty() {
        let id = ui.text("Model ID", None)?;
        (id, None, None, None)
    } else {
        let manual = crate::tr!(locale, "config.manual_model");
        let labels: Vec<String> = discovered
            .iter()
            .map(|m| m.id.clone())
            .chain(std::iter::once(manual.clone()))
            .collect();

        let idx = ui.select(&crate::tr!(locale, "config.select_model"), &labels, 0)?;

        if labels[idx] == manual {
            let id = ui.text("Model ID", None)?;
            (id, None, None, None)
        } else {
            let m = &discovered[idx];
            (
                m.id.clone(),
                m.context_window,
                m.max_output_tokens,
                m.supports_vision,
            )
        }
    };

    let capacity = resolve_model_capacity(&model_id, api_ctx, api_out, api_vision);

    let default_name = model_id
        .split(['/', ':'])
        .next_back()
        .unwrap_or(&model_id)
        .to_string();
    let name = ui.text(
        &crate::tr!(locale, "config.model_name"),
        Some(&default_name),
    )?;

    let context_window = ui.usize("Context window tokens", capacity.context_window_tokens)?;

    let max_completion = ui.usize("Max completion tokens", capacity.max_completion_tokens)?;

    if !capacity.supports_tool_call {
        return Err(miette!(
            "model {model_id} does not support tool/function calling; Daat Locus requires tool_call support for agent operation. Choose a model with tool_call: true in the models.dev catalog."
        ));
    }

    let thinking_budget = prompt_reasoning_config(
        ui,
        &model_id,
        &crate::tr!(locale, "config.reasoning_config"),
    )?;

    Ok((
        name,
        ModelConfig {
            provider: provider_name.to_string(),
            model_id,
            context_window_tokens: context_window,
            max_completion_tokens: max_completion,
            supports_vision: api_vision,
            thinking_budget,
            ..ModelConfig::default()
        },
    ))
}

fn prompt_reasoning_config(
    ui: &mut PromptUi,
    model_id: &str,
    title: &str,
) -> Result<Option<ThinkingBudget>> {
    use crate::model_catalog::catalog_model_reasoning_options;
    let options = catalog_model_reasoning_options(model_id);
    if options.is_empty() {
        return Ok(None);
    }

    let skip_label = crate::tr!(ui.locale(), "config.reasoning_skip");
    let mut labels: Vec<String> = options
        .iter()
        .flat_map(|opt| match opt {
            crate::model_catalog::ReasoningOption::Toggle => {
                vec!["high (recommended)".to_string()]
            }
            crate::model_catalog::ReasoningOption::Effort { values } => values.clone(),
            crate::model_catalog::ReasoningOption::BudgetTokens { .. } => {
                vec!["custom (budget tokens)".to_string()]
            }
        })
        .collect();
    let skip_idx = labels.len();
    labels.push(skip_label);

    let idx = ui.select(title, &labels, skip_idx)?;
    if idx == skip_idx {
        return Ok(None);
    }

    let mut flat_idx = idx;
    for opt in &options {
        match opt {
            crate::model_catalog::ReasoningOption::Toggle => {
                if flat_idx == 0 {
                    return Ok(Some(ThinkingBudget::new("high")));
                }
                flat_idx -= 1;
            }
            crate::model_catalog::ReasoningOption::Effort { values } => {
                if flat_idx < values.len() {
                    return Ok(Some(ThinkingBudget::new(&values[flat_idx])));
                }
                flat_idx -= values.len();
            }
            crate::model_catalog::ReasoningOption::BudgetTokens { min, max } => {
                if flat_idx == 0 {
                    let default = (*min).max(1024);
                    let val = ui.usize("Reasoning budget tokens", default)?;
                    let clamped = val.clamp(*min, max.unwrap_or(usize::MAX));
                    return Ok(Some(ThinkingBudget::new(clamped.to_string())));
                }
                flat_idx -= 1;
            }
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run the first-time setup wizard, write config.toml, and return the generated Config.
pub async fn run_first_time_setup() -> Result<Config> {
    let mut ui = PromptUi::new(Locale::default())?;
    let language_items = [
        crate::tr!(Locale::EnUs, "setup.language_english"),
        crate::tr!(Locale::ZhCn, "setup.language_chinese"),
    ];
    let language_idx = ui.select(
        &crate::tr!(Locale::EnUs, "setup.language_prompt"),
        &language_items,
        0,
    )?;
    let locale = Locale::from_language_setup_index(language_idx);
    ui.set_locale(locale);

    let skip = ui.select(
        &crate::tr!(locale, "setup.init_mode"),
        &[
            crate::tr!(locale, "setup.interactive"),
            crate::tr!(locale, "setup.skip_default"),
        ],
        0,
    )?;

    if skip == 1 {
        let config = Config {
            locale,
            ..Config::default()
        };
        write_config(&config).await?;
        ui.detail(
            &crate::tr!(locale, "setup.written"),
            &[crate::tr!(locale, "setup.default_created")],
        )?;
        return Ok(config);
    }

    let (provider_name, provider_config) = prompt_provider(&mut ui, &[]).await?;

    let mut providers = HashMap::new();
    providers.insert(provider_name.clone(), provider_config.clone());

    let (model_name, model_config) =
        prompt_model(&mut ui, &provider_name, &provider_config).await?;

    let mut models = HashMap::new();
    models.insert(model_name.clone(), model_config);

    let telegram = prompt_telegram_config(&mut ui, None)?;

    let config = Config {
        locale,
        providers,
        models,
        main_model: model_name.clone(),
        judge: JudgeConfig::default(),
        telegram,
        ..Config::default()
    };

    write_config(&config).await?;

    ui.detail(
        &crate::tr!(locale, "setup.written"),
        &[format!(
            "main_model = \"{model_name}\" (provider: {provider_name})"
        )],
    )?;

    Ok(config)
}

/// `config add-provider` subcommand.
pub async fn run_add_provider() -> Result<()> {
    let mut config = crate::config::load_config().await.map_err(|e| {
        miette!(
            "{}",
            crate::tr!(Locale::default(), "common.config_load_failed", error = e)
        )
    })?;
    let locale = config.locale;

    let existing: Vec<String> = config.providers.keys().cloned().collect();
    let mut ui = PromptUi::new(locale)?;
    let (name, provider) = prompt_provider(&mut ui, &existing).await?;

    if config.providers.contains_key(&name) {
        let overwrite = ui.confirm(
            &crate::tr!(locale, "common.overwrite_provider", name = name.clone()),
            false,
        )?;
        if !overwrite {
            ui.detail(
                &crate::tr!(locale, "config.add_provider"),
                &[crate::tr!(locale, "common.cancelled_action")],
            )?;
            return Ok(());
        }
    }

    config.providers.insert(name.clone(), provider);
    write_config(&config).await?;
    ui.detail(
        &crate::tr!(locale, "config.add_provider"),
        &[crate::tr!(locale, "config.provider_saved", name = name)],
    )?;
    Ok(())
}

/// `config add-model` subcommand.
pub async fn run_add_model() -> Result<()> {
    let mut config = crate::config::load_config().await.map_err(|e| {
        miette!(
            "{}",
            crate::tr!(Locale::default(), "common.config_load_failed", error = e)
        )
    })?;
    let locale = config.locale;

    let mut ui = PromptUi::new(locale)?;
    let provider_names: Vec<String> = config.providers.keys().cloned().collect();
    if provider_names.is_empty() {
        return Err(miette!("{}", crate::tr!(locale, "common.no_providers")));
    }
    let provider_idx = if provider_names.len() == 1 {
        0
    } else {
        ui.select(
            &crate::tr!(locale, "config.bind_provider"),
            &provider_names,
            0,
        )?
    };
    let provider_name = &provider_names[provider_idx];
    let provider_config = config.providers.get(provider_name).unwrap();
    let (name, model) = prompt_model(&mut ui, provider_name, provider_config).await?;

    if config.models.contains_key(&name) {
        let overwrite = ui.confirm(
            &crate::tr!(locale, "common.overwrite_model", name = name.clone()),
            false,
        )?;
        if !overwrite {
            ui.detail(
                &crate::tr!(locale, "config.add_model"),
                &[crate::tr!(locale, "common.cancelled_action")],
            )?;
            return Ok(());
        }
    }

    config.models.insert(name.clone(), model);

    let set_main = ui.confirm(
        &crate::tr!(locale, "config.set_as_main", name = name.clone()),
        false,
    )?;
    if set_main {
        config.main_model = name.clone();
    }

    write_config(&config).await?;
    ui.detail(
        &crate::tr!(locale, "config.add_model"),
        &[crate::tr!(locale, "config.model_saved", name = name)],
    )?;
    Ok(())
}

/// `config set-main-model` subcommand.
pub async fn run_set_main_model() -> Result<()> {
    let mut config = crate::config::load_config().await.map_err(|e| {
        miette!(
            "{}",
            crate::tr!(Locale::default(), "common.config_load_failed", error = e)
        )
    })?;
    let locale = config.locale;
    let mut ui = PromptUi::new(locale)?;

    let model_names: Vec<String> = config.models.keys().cloned().collect();
    if model_names.is_empty() {
        return Err(miette!("{}", crate::tr!(locale, "common.no_models")));
    }

    let current_idx = model_names
        .iter()
        .position(|n| n == &config.main_model)
        .unwrap_or(0);

    let idx = ui.select(
        &crate::tr!(locale, "config.select_main_model"),
        &model_names,
        current_idx,
    )?;

    config.main_model = model_names[idx].clone();
    write_config(&config).await?;
    ui.detail(
        &crate::tr!(locale, "config.select_main_model"),
        &[crate::tr!(
            locale,
            "config.main_model_set",
            name = config.main_model.clone()
        )],
    )?;
    Ok(())
}

/// `config show` subcommand. Prints the current config summary with secrets masked.
pub async fn show_config() -> Result<()> {
    let config = crate::config::load_config().await.map_err(|e| {
        miette!(
            "{}",
            crate::tr!(Locale::default(), "common.config_load_failed", error = e)
        )
    })?;

    for line in render_config_summary_lines(&config, config.locale) {
        println!("{line}");
    }
    println!();
    Ok(())
}

fn push_config_section(lines: &mut Vec<String>, title: impl Into<String>) {
    if !lines.is_empty() {
        lines.push(String::new());
    }
    let title = title.into();
    lines.push(format!("▸ {title}"));
    lines.push("  ─────────────────────────────".to_string());
}

fn push_config_field(lines: &mut Vec<String>, label: &str, value: impl Into<String>) {
    lines.push(format!("  {label:<24} {}", value.into()));
}

fn push_config_subfield(lines: &mut Vec<String>, label: &str, value: impl Into<String>) {
    lines.push(format!("      {label:<20} {}", value.into()));
}

fn render_config_summary_lines(config: &Config, locale: Locale) -> Vec<String> {
    let mut lines = Vec::new();

    push_config_section(&mut lines, crate::tr!(locale, "config.locale_heading"));
    push_config_field(
        &mut lines,
        "locale",
        format!(
            "{} ({})",
            config.locale.as_str(),
            config.locale.display_name()
        ),
    );
    push_config_field(&mut lines, "daemon.port", config.daemon.port.to_string());
    push_config_field(
        &mut lines,
        "sandbox.enabled",
        config.sandbox.enabled.to_string(),
    );
    push_config_field(
        &mut lines,
        "sandbox.filesystem",
        format!("{:?}", config.sandbox.strong_filesystem).to_lowercase(),
    );

    push_config_section(&mut lines, crate::tr!(locale, "config.providers_heading"));
    push_config_field(
        &mut lines,
        "provider_count",
        config.providers.len().to_string(),
    );
    for (name, provider) in &config.providers {
        let (kind, fields): (&str, Vec<(&str, String)>) = match provider {
            ProviderConfig::Openai { api_key, base_url } => {
                let masked = mask_secret(api_key);
                let url = base_url.as_deref().unwrap_or("https://api.openai.com/v1");
                (
                    "openai",
                    vec![("base_url", url.to_string()), ("api_key", masked)],
                )
            }
            ProviderConfig::GithubCopilot { github_token } => {
                let masked = mask_secret(github_token);
                ("github-copilot", vec![("github_token", masked)])
            }
            ProviderConfig::OpenaiCodexOauth {
                auth_file,
                base_url,
            } => {
                let url = base_url
                    .as_deref()
                    .unwrap_or(codex_oauth_default_base_url());
                let auth_file = auth_file.as_deref().unwrap_or("<default>");
                (
                    "openai-codex-oauth",
                    vec![
                        ("base_url", url.to_string()),
                        ("auth_file", auth_file.to_string()),
                    ],
                )
            }
            ProviderConfig::OpenaiCompatible {
                base_url, api_key, ..
            } => {
                let masked = mask_secret(api_key);
                (
                    "openai-compatible",
                    vec![("base_url", base_url.clone()), ("api_key", masked)],
                )
            }
            ProviderConfig::Ollama {
                host,
                api_key,
                keep_alive,
            } => {
                let host = host.as_deref().unwrap_or("http://127.0.0.1:11434");
                let mut fields = vec![("host", host.to_string())];
                if let Some(api_key) = api_key {
                    fields.push(("api_key", mask_secret(api_key)));
                }
                if let Some(keep_alive) = keep_alive {
                    fields.push(("keep_alive", keep_alive.clone()));
                }
                ("ollama", fields)
            }
        };

        lines.push(format!("  • {name}"));
        push_config_subfield(&mut lines, "type", kind);
        for (label, value) in fields {
            push_config_subfield(&mut lines, label, value);
        }
    }

    push_config_section(&mut lines, crate::tr!(locale, "config.models_heading"));
    push_config_field(&mut lines, "model_count", config.models.len().to_string());
    push_config_field(&mut lines, "main_model", config.main_model.clone());
    push_config_field(
        &mut lines,
        "efficient_model",
        config.efficient_model.clone(),
    );
    for (name, model) in &config.models {
        let mut roles = Vec::new();
        if name == &config.main_model {
            roles.push("main");
        }
        if name == &config.efficient_model {
            roles.push("efficient");
        }
        let role_suffix = if roles.is_empty() {
            String::new()
        } else {
            format!(" ({})", roles.join(", "))
        };
        lines.push(format!("  • {name}{role_suffix}"));
        push_config_subfield(&mut lines, "provider", model.provider.clone());
        push_config_subfield(&mut lines, "model_id", model.model_id.clone());
        push_config_subfield(&mut lines, "temperature", model.temperature.to_string());
        push_config_subfield(
            &mut lines,
            "thinking_budget",
            model
                .thinking_budget
                .as_ref()
                .map(|budget| budget.as_str().to_string())
                .unwrap_or_else(|| "default".to_string()),
        );
        push_config_subfield(
            &mut lines,
            "rpm",
            model
                .rpm
                .map(|rpm| rpm.to_string())
                .unwrap_or_else(|| "unlimited".to_string()),
        );
        push_config_subfield(
            &mut lines,
            "request_timeout",
            format!("{}s", model.request_timeout_secs),
        );
        push_config_subfield(
            &mut lines,
            "stream_idle_timeout",
            format!("{}s", model.stream_idle_timeout_secs),
        );
        push_config_subfield(
            &mut lines,
            "context_window",
            model.context_window_tokens.to_string(),
        );
        push_config_subfield(
            &mut lines,
            "effective_window",
            format!(
                "{} tokens ({}%)",
                model.effective_context_window_tokens(),
                model.effective_context_window_percent()
            ),
        );
        push_config_subfield(
            &mut lines,
            "auto_compact_limit",
            model.auto_compact_token_limit().to_string(),
        );
        push_config_subfield(
            &mut lines,
            "max_completion",
            model.max_completion_tokens.to_string(),
        );
        push_config_subfield(
            &mut lines,
            "tool_output_max",
            model.tool_output_max_tokens.to_string(),
        );
        push_config_subfield(
            &mut lines,
            "supports_vision",
            model
                .supports_vision
                .map(|value| value.to_string())
                .unwrap_or_else(|| "auto".to_string()),
        );
    }

    push_config_section(&mut lines, crate::tr!(locale, "config.judge_heading"));
    let judge_model = config
        .judge
        .model
        .as_deref()
        .unwrap_or(&config.efficient_model);
    push_config_field(&mut lines, "enabled", config.judge.enabled.to_string());
    push_config_field(&mut lines, "model", judge_model.to_string());
    push_config_field(
        &mut lines,
        "pairwise_candidates",
        config.judge.max_pairwise_candidates.to_string(),
    );
    push_config_field(
        &mut lines,
        "pairwise_cases",
        config.judge.max_pairwise_cases.to_string(),
    );

    push_config_section(&mut lines, crate::tr!(locale, "config.telegram_heading"));
    let token_status = if config.telegram.has_real_credentials() {
        mask_secret(&config.telegram.bot_token)
    } else {
        crate::tr!(locale, "config.telegram_token_missing")
    };
    let active_status = if config.telegram.enabled && config.telegram.has_real_credentials() {
        crate::tr!(locale, "config.telegram_active")
    } else if config.telegram.enabled {
        crate::tr!(locale, "config.telegram_waiting_for_token")
    } else {
        crate::tr!(locale, "config.telegram_disabled")
    };
    push_config_field(&mut lines, "status", active_status);
    push_config_field(&mut lines, "enabled", config.telegram.enabled.to_string());
    push_config_field(&mut lines, "token", token_status);
    push_config_field(
        &mut lines,
        "poll_timeout_secs",
        config.telegram.poll_timeout_secs.to_string(),
    );

    lines
}

/// `config` without a subcommand: interactive menu.
pub async fn run_config_menu() -> Result<()> {
    let initial_locale = crate::config::load_config()
        .await
        .ok()
        .map(|config| config.locale)
        .unwrap_or_default();
    let mut ui = PromptUi::new(initial_locale)?;
    loop {
        let mut locale = ui.locale();
        let has_config = crate::config::config_file_exists().await;
        if has_config && let Ok(cfg) = crate::config::load_config().await {
            locale = cfg.locale;
            ui.set_locale(locale);
        }

        let items = [
            crate::tr!(locale, "config.add_provider"),
            crate::tr!(locale, "config.add_model"),
            crate::tr!(locale, "config.change_main_model"),
            crate::tr!(locale, "config.change_efficient_model"),
            crate::tr!(locale, "config.configure_telegram"),
            crate::tr!(locale, "config.exit"),
        ];

        let idx = match ui.select_compact(&items, 0) {
            Ok(idx) => idx,
            Err(err) => match config_menu_navigation_from_prompt_error(&err, true) {
                Some(ConfigMenuPromptNavigation::ExitMenu) => break,
                Some(ConfigMenuPromptNavigation::ReturnToMenu) => continue,
                None => return Err(err),
            },
        };

        if idx == items.len() - 1 {
            break;
        }

        let action_result: Result<()> = async {
            match idx {
                0 => {
                    let mut config = crate::config::load_config().await.map_err(|e| {
                        miette!(
                            "{}",
                            crate::tr!(locale, "common.config_load_failed", error = e)
                        )
                    })?;
                    let existing: Vec<String> = config.providers.keys().cloned().collect();
                    let (name, provider) = prompt_provider(&mut ui, &existing).await?;
                    if config.providers.contains_key(&name)
                        && !ui.confirm(
                            &crate::tr!(locale, "common.overwrite_provider", name = name.clone()),
                            false,
                        )?
                    {
                        Ok(())
                    } else {
                        config.providers.insert(name, provider);
                        write_config(&config).await?;
                        Ok(())
                    }
                }
                1 => {
                    let mut config = crate::config::load_config().await.map_err(|e| {
                        miette!(
                            "{}",
                            crate::tr!(locale, "common.config_load_failed", error = e)
                        )
                    })?;
                    let provider_names: Vec<String> = config.providers.keys().cloned().collect();
                    if provider_names.is_empty() {
                        ui.suspend();
                        return Err(miette!("{}", crate::tr!(locale, "common.no_providers")));
                    }
                    let provider_idx = if provider_names.len() == 1 {
                        0
                    } else {
                        ui.select(
                            &crate::tr!(locale, "config.bind_provider"),
                            &provider_names,
                            0,
                        )?
                    };
                    let provider_name = &provider_names[provider_idx];
                    let provider_config = config.providers.get(provider_name).unwrap();
                    let (name, model) =
                        prompt_model(&mut ui, provider_name, provider_config).await?;
                    if config.models.contains_key(&name)
                        && !ui.confirm(
                            &crate::tr!(locale, "common.overwrite_model", name = name.clone()),
                            false,
                        )?
                    {
                        Ok(())
                    } else {
                        config.models.insert(name.clone(), model);
                        if ui.confirm(
                            &crate::tr!(locale, "config.set_as_main", name = name.clone()),
                            false,
                        )? {
                            config.main_model = name;
                        }
                        write_config(&config).await?;
                        Ok(())
                    }
                }
                2 => {
                    let mut config = crate::config::load_config().await.map_err(|e| {
                        miette!(
                            "{}",
                            crate::tr!(locale, "common.config_load_failed", error = e)
                        )
                    })?;
                    let model_names: Vec<String> = config.models.keys().cloned().collect();
                    if model_names.is_empty() {
                        ui.suspend();
                        return Err(miette!("{}", crate::tr!(locale, "common.no_models")));
                    }
                    let current_idx = model_names
                        .iter()
                        .position(|n| n == &config.main_model)
                        .unwrap_or(0);
                    let idx = ui.select(
                        &crate::tr!(locale, "config.select_main_model"),
                        &model_names,
                        current_idx,
                    )?;
                    config.main_model = model_names[idx].clone();
                    write_config(&config).await?;
                    Ok(())
                }
                3 => {
                    let mut config = crate::config::load_config().await.map_err(|e| {
                        miette!(
                            "{}",
                            crate::tr!(locale, "common.config_load_failed", error = e)
                        )
                    })?;
                    let model_names: Vec<String> = config.models.keys().cloned().collect();
                    if model_names.is_empty() {
                        ui.suspend();
                        return Err(miette!("{}", crate::tr!(locale, "common.no_models")));
                    }
                    let current_idx = model_names
                        .iter()
                        .position(|n| n == &config.efficient_model)
                        .unwrap_or(0);
                    let idx = ui.select(
                        &crate::tr!(locale, "config.select_efficient_model"),
                        &model_names,
                        current_idx,
                    )?;
                    config.efficient_model = model_names[idx].clone();
                    write_config(&config).await?;
                    Ok(())
                }
                4 => {
                    let mut config = crate::config::load_config().await.map_err(|e| {
                        miette!(
                            "{}",
                            crate::tr!(locale, "common.config_load_failed", error = e)
                        )
                    })?;
                    config.telegram = prompt_telegram_config(&mut ui, Some(&config.telegram))?;
                    write_config(&config).await?;
                    Ok(())
                }
                _ => Ok(()),
            }
        }
        .await;

        match action_result {
            Ok(()) => {}
            Err(err) => match config_menu_navigation_from_prompt_error(&err, false) {
                Some(ConfigMenuPromptNavigation::ExitMenu) => break,
                Some(ConfigMenuPromptNavigation::ReturnToMenu) => continue,
                None => return Err(err),
            },
        }
    }
    Ok(())
}

fn prompt_telegram_config(
    ui: &mut PromptUi,
    current: Option<&TelegramConfig>,
) -> Result<TelegramConfig> {
    let locale = ui.locale();
    let default = TelegramConfig::default();
    let enabled = ui.confirm(
        &crate::tr!(locale, "config.telegram_enable"),
        current.map(|config| config.enabled).unwrap_or(false),
    )?;
    let poll_timeout_secs = current
        .map(|config| config.poll_timeout_secs)
        .unwrap_or(default.poll_timeout_secs);
    let existing_token = current
        .map(|config| config.bot_token.clone())
        .unwrap_or_else(|| default.bot_token.clone());

    if !enabled {
        return Ok(TelegramConfig {
            enabled,
            bot_token: existing_token,
            poll_timeout_secs,
        });
    }

    let has_existing_token = current
        .map(|config| config.has_real_credentials())
        .unwrap_or(false);
    let mut token_options = Vec::new();
    if has_existing_token {
        token_options.push(crate::tr!(locale, "config.telegram_keep_existing_token"));
    }
    token_options.push(crate::tr!(locale, "config.telegram_env_token"));
    token_options.push(crate::tr!(locale, "config.telegram_manual_token"));

    let token_idx = ui.select(
        &crate::tr!(locale, "config.telegram_token_source"),
        &token_options,
        0,
    )?;
    let bot_token = if has_existing_token && token_idx == 0 {
        existing_token
    } else {
        let adjusted_idx = if has_existing_token {
            token_idx.saturating_sub(1)
        } else {
            token_idx
        };
        match adjusted_idx {
            0 => {
                let name = ui.text(
                    &crate::tr!(locale, "config.telegram_token_env_name"),
                    Some("TELEGRAM_BOT_TOKEN"),
                )?;
                let name = if name.trim().is_empty() {
                    "TELEGRAM_BOT_TOKEN"
                } else {
                    name.trim()
                };
                format!("${name}")
            }
            _ => ui.password(&crate::tr!(locale, "config.telegram_bot_token"))?,
        }
    };

    let poll_timeout_secs = ui.usize(
        &crate::tr!(locale, "config.telegram_poll_timeout_secs"),
        usize::try_from(poll_timeout_secs).unwrap_or(30),
    )? as u64;

    Ok(TelegramConfig {
        enabled,
        bot_token,
        poll_timeout_secs,
    })
}

/// `config set-efficient-model` subcommand.
pub async fn run_set_efficient_model() -> Result<()> {
    let mut config = crate::config::load_config().await.map_err(|e| {
        miette!(
            "{}",
            crate::tr!(Locale::default(), "common.config_load_failed", error = e)
        )
    })?;
    let locale = config.locale;
    let mut ui = PromptUi::new(locale)?;

    let model_names: Vec<String> = config.models.keys().cloned().collect();
    if model_names.is_empty() {
        return Err(miette!("{}", crate::tr!(locale, "common.no_models")));
    }

    let current_idx = model_names
        .iter()
        .position(|n| n == &config.efficient_model)
        .unwrap_or(0);

    let idx = ui.select(
        &crate::tr!(locale, "config.select_efficient_model"),
        &model_names,
        current_idx,
    )?;

    config.efficient_model = model_names[idx].clone();
    write_config(&config).await?;
    ui.detail(
        &crate::tr!(locale, "config.select_efficient_model"),
        &[crate::tr!(
            locale,
            "config.efficient_model_set",
            name = config.efficient_model.clone()
        )],
    )?;
    Ok(())
}

/// `config set-telegram` subcommand.
pub async fn run_set_telegram() -> Result<()> {
    let mut config = crate::config::load_config().await.map_err(|e| {
        miette!(
            "{}",
            crate::tr!(Locale::default(), "common.config_load_failed", error = e)
        )
    })?;
    let locale = config.locale;
    let mut ui = PromptUi::new(locale)?;

    config.telegram = prompt_telegram_config(&mut ui, Some(&config.telegram))?;
    write_config(&config).await?;
    ui.detail(
        &crate::tr!(locale, "config.configure_telegram"),
        &[crate::tr!(locale, "config.telegram_saved")],
    )?;
    Ok(())
}

fn mask_secret(s: &str) -> String {
    let s = s.trim();
    if s.len() <= 8 {
        return "*".repeat(s.len());
    }
    // Show the first and last four characters.
    let prefix = &s[..4];
    let suffix = &s[s.len() - 4..];
    format!("{prefix}...{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_capacity_prefers_detected_values() {
        let capacity = resolve_model_capacity("gpt-4.1", Some(12_345), Some(678), None);

        assert_eq!(
            capacity,
            ModelCapacity {
                context_window_tokens: 12_345,
                max_completion_tokens: 678,
                supports_vision: true,
                supports_tool_call: true,
            }
        );
    }

    #[test]
    fn model_capacity_fills_missing_detected_fields_from_exact_catalog_match() {
        let capacity = resolve_model_capacity("gpt-4.1", Some(12_345), None, None);

        assert_eq!(
            capacity,
            ModelCapacity {
                context_window_tokens: 12_345,
                max_completion_tokens: 32_768,
                supports_vision: true,
                supports_tool_call: true,
            }
        );
    }

    #[test]
    fn model_capacity_uses_conservative_defaults_for_unknown_models() {
        let capacity = resolve_model_capacity("unknown-local-model", None, None, None);

        assert_eq!(capacity, conservative_model_capacity());
    }

    #[test]
    fn model_catalog_does_not_substring_match_similar_model_names() {
        let capacity = resolve_model_capacity("gpt-4.1-custom", None, None, None);

        assert_eq!(capacity, conservative_model_capacity());
    }

    #[test]
    fn model_capacity_uses_detected_vision_over_catalog() {
        let capacity = resolve_model_capacity("gpt-4.1", None, None, Some(false));

        assert!(!capacity.supports_vision);
    }

    #[test]
    fn model_capacity_uses_detected_vision_true_for_unknown() {
        let capacity = resolve_model_capacity("unknown-model", None, None, Some(true));

        assert!(capacity.supports_vision);
    }

    #[test]
    fn urlenc_percent_encodes_utf8_bytes() {
        assert_eq!(urlenc("a b/你"), "a%20b%2F%E4%BD%A0");
    }

    #[test]
    fn codex_authorize_url_matches_codex_callback_flow() {
        let pkce = CodexPkceCodes {
            code_verifier: "verifier".to_string(),
            code_challenge: "challenge".to_string(),
        };
        let url = build_codex_authorize_url("http://localhost:1455/auth/callback", &pkce, "state");

        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
        assert!(url.contains("scope=openid%20profile%20email%20offline_access%20api.connectors.read%20api.connectors.invoke"));
        assert!(url.contains("code_challenge=challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("id_token_add_organizations=true"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("state=state"));
        assert!(url.contains("originator=codex_cli_rs"));
    }

    #[test]
    fn codex_pkce_codes_are_url_safe_and_have_expected_lengths() {
        let pkce = generate_codex_pkce();
        let state = generate_codex_oauth_state();

        assert_eq!(pkce.code_verifier.len(), 86);
        assert_eq!(pkce.code_challenge.len(), 43);
        assert_eq!(state.len(), 43);
        assert!(
            pkce.code_verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert!(
            pkce.code_challenge
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert!(
            state
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn parse_models_response_accepts_codex_models_shape() {
        let models = parse_models_response(Some(serde_json::json!({
            "models": [
                {
                    "slug": "gpt-5.4-mini",
                    "display_name": "GPT-5.4-Mini",
                    "context_window": 272000
                },
                {
                    "slug": "gpt-5.5",
                    "display_name": "GPT-5.5",
                    "max_context_window": 400000,
                    "max_output_tokens": 128000
                },
                {
                    "slug": "gpt-5.3-codex-spark",
                    "display_name": "GPT-5.3-Codex-Spark",
                    "supported_in_api": false
                },
                {
                    "slug": "codex-auto-review",
                    "display_name": "Codex Auto Review",
                    "visibility": "hide"
                }
            ]
        })));

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-5.4-mini");
        assert_eq!(models[0].context_window, Some(272000));
        assert_eq!(models[0].max_output_tokens, None);
        assert_eq!(models[1].id, "gpt-5.5");
        assert_eq!(models[1].context_window, Some(400000));
        assert_eq!(models[1].max_output_tokens, Some(128000));
    }

    #[test]
    fn config_summary_includes_telegram_state() {
        let config = Config {
            telegram: TelegramConfig {
                enabled: true,
                bot_token: "${TELEGRAM_BOT_TOKEN}".to_string(),
                poll_timeout_secs: 45,
            },
            ..Config::default()
        };

        let summary = render_config_summary_lines(&config, Locale::EnUs).join("\n");

        assert!(summary.contains("Telegram"));
        assert!(summary.contains("enabled                  true"));
        assert!(summary.contains("poll_timeout_secs        45"));
        assert!(summary.contains("${TE...KEN}"));
    }

    #[test]
    fn config_summary_expands_model_details_and_masks_secrets() {
        let mut config = Config::default();
        config.providers.insert(
            "custom".to_string(),
            ProviderConfig::OpenaiCompatible {
                base_url: "https://example.test/v1".to_string(),
                api_key: "sk-secret-token".to_string(),
                api_style: None,
            },
        );
        config.models.insert(
            "coder".to_string(),
            ModelConfig {
                provider: "custom".to_string(),
                model_id: "example-coder".to_string(),
                temperature: 0.2,
                thinking_budget: None,
                rpm: Some(60),
                request_timeout_secs: 120,
                stream_idle_timeout_secs: 30,
                context_window_tokens: 200_000,
                auto_compact_token_limit: None,
                effective_context_window_percent: 50,
                max_completion_tokens: 16_000,
                tool_output_max_tokens: 32_000,
                supports_vision: Some(false),
            },
        );
        config.main_model = "coder".to_string();

        let summary = render_config_summary_lines(&config, Locale::EnUs).join("\n");

        assert!(summary.contains("▸ Providers"));
        assert!(summary.contains("base_url             https://example.test/v1"));
        assert!(summary.contains("api_key              sk-s...oken"));
        assert!(!summary.contains("sk-secret-token"));
        assert!(summary.contains("• coder (main)"));
        assert!(summary.contains("temperature          0.2"));
        assert!(summary.contains("effective_window     100000 tokens (50%)"));
        assert!(summary.contains("supports_vision      false"));
    }

    #[test]
    fn detail_scroll_offset_keeps_last_page_full() {
        let rows = detail_body_rows();

        assert_eq!(detail_scroll_offset(0, KeyCode::End, 30, rows), 30 - rows);
        assert_eq!(detail_scroll_offset(0, KeyCode::PageDown, 30, rows), rows);
        assert_eq!(detail_scroll_offset(0, KeyCode::PageUp, 30, rows), 0);
        assert_eq!(detail_scroll_offset(0, KeyCode::End, 2, rows), 0);
    }

    #[test]
    fn config_menu_esc_exits_at_root() {
        let err = prompt_navigate_parent();

        assert_eq!(
            config_menu_navigation_from_prompt_error(&err, true),
            Some(ConfigMenuPromptNavigation::ExitMenu)
        );
    }

    #[test]
    fn select_prompt_hides_option_count() {
        let items = [
            crate::tr!(Locale::EnUs, "config.show_details"),
            crate::tr!(Locale::EnUs, "config.add_provider"),
            crate::tr!(Locale::EnUs, "config.exit"),
        ];
        let compact_text = render_select_prompt_to_text(
            Locale::EnUs,
            "Config management main_model=gpt-5.5 | providers=6 | models=12 | locale=English",
            &items,
            true,
        );
        let full_text = render_select_prompt_to_text(Locale::EnUs, "Provider type", &items, false);

        assert!(!compact_text.contains("Config management main_model"));
        assert!(!compact_text.contains("Config inline"));
        assert!(!compact_text.contains("3 option"));
        assert!(!compact_text.contains("Select  Config management"));
        assert!(!compact_text.lines().any(|line| line.trim() == "Select"));
        assert!(compact_text.contains(&items[0]));
        assert!(compact_text.contains(&crate::tr!(Locale::EnUs, "prompt_ui.help_select")));

        assert!(full_text.contains("Provider type"));
        assert!(!full_text.contains("3 option"));
        assert!(!full_text.contains("option(s)"));
    }
    #[test]
    fn text_prompt_uses_semantic_field_titles() {
        assert_eq!(
            text_prompt_field_title(
                Locale::ZhCn,
                &crate::tr!(Locale::ZhCn, "config.provider_name"),
                false,
                false,
            ),
            "名称"
        );
        assert_eq!(
            text_prompt_field_title(
                Locale::ZhCn,
                &crate::tr!(Locale::ZhCn, "config.openai_api_key"),
                true,
                false,
            ),
            "密钥"
        );
        assert_eq!(
            text_prompt_field_title(Locale::EnUs, "Context window tokens", false, false),
            "Number"
        );
        assert_eq!(
            text_prompt_field_title(
                Locale::ZhCn,
                &crate::tr!(Locale::ZhCn, "config.ollama_host"),
                false,
                false,
            ),
            "主机"
        );

        let provider_name = render_text_prompt_to_text(
            Locale::ZhCn,
            &crate::tr!(Locale::ZhCn, "config.provider_name"),
            "openai",
            false,
            None,
        );
        let api_key = render_text_prompt_to_text(
            Locale::ZhCn,
            &crate::tr!(Locale::ZhCn, "config.openai_api_key"),
            "sk-test",
            true,
            None,
        );

        assert!(provider_name.contains("称"));
        assert!(api_key.contains("钥"));
        assert!(!provider_name.contains("┌值"));
        assert!(!provider_name.contains("Value"));
    }

    #[test]
    fn config_menu_esc_returns_to_menu_below_root() {
        let err = prompt_navigate_parent();

        assert_eq!(
            config_menu_navigation_from_prompt_error(&err, false),
            Some(ConfigMenuPromptNavigation::ReturnToMenu)
        );
    }

    #[test]
    fn config_menu_ctrl_c_is_not_parent_navigation() {
        let err = prompt_cancelled(Locale::EnUs);

        assert_eq!(config_menu_navigation_from_prompt_error(&err, false), None);
        assert_eq!(config_menu_navigation_from_prompt_error(&err, true), None);
    }
}
