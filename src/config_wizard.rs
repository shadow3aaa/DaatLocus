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
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    config::{
        Config, JudgeConfig, ModelConfig, ProviderConfig, normalize_provider_base_url,
        redact_secret_text, resolve_env_reference, write_config,
    },
    i18n::Locale,
    model_catalog::{ModelCapacity, catalog_model_capacity, conservative_model_capacity},
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
async fn run_github_device_flow(locale: Locale) -> Result<String> {
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

    println!("  {}", crate::tr!(locale, "github.request_device_code"));
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

    println!();
    println!("  {}", crate::tr!(locale, "github.authorization"));
    println!(
        "  1. {}",
        crate::tr!(locale, "github.open_url", url = verification_uri)
    );
    println!(
        "  2. {}",
        crate::tr!(locale, "github.enter_code", code = user_code)
    );
    println!();

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
        print!(
            "\r  {}",
            crate::tr!(locale, "github.waiting", dots = ".".repeat(dots + 1))
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());

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
            println!(
                "\r  {}                                  ",
                crate::tr!(locale, "github.success")
            );
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

async fn run_codex_oauth_browser_flow(locale: Locale) -> Result<CodexOAuthTokens> {
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

    println!(
        "  {}",
        crate::tr!(locale, "codex_oauth.request_browser_login")
    );
    println!(
        "  {}",
        crate::tr!(locale, "codex_oauth.open_url", url = auth_url.clone())
    );
    println!(
        "  {}",
        crate::tr!(
            locale,
            "codex_oauth.callback_waiting",
            url = redirect_uri.clone()
        )
    );
    println!();

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
            println!("  {}", crate::tr!(locale, "codex_oauth.success"));
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

async fn run_codex_oauth_device_flow(locale: Locale) -> Result<CodexOAuthTokens> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| miette!("Codex OAuth HTTP client failed: {e}"))?;

    println!(
        "  {}",
        crate::tr!(locale, "codex_oauth.request_device_code")
    );
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

    println!();
    println!("  {}", crate::tr!(locale, "codex_oauth.authorization"));
    println!(
        "  1. {}",
        crate::tr!(locale, "codex_oauth.open_url", url = verification_url)
    );
    println!(
        "  2. {}",
        crate::tr!(locale, "codex_oauth.enter_code", code = device.user_code)
    );
    println!("  {}", crate::tr!(locale, "codex_oauth.code_warning"));
    println!();

    let _ = open_browser(&verification_url);

    let token_response = poll_codex_device_authorization(
        &http,
        locale,
        &device.device_auth_id,
        &device.user_code,
        Duration::from_secs(interval_secs),
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

async fn poll_codex_device_authorization(
    http: &reqwest::Client,
    locale: Locale,
    device_auth_id: &str,
    user_code: &str,
    interval: Duration,
) -> Result<CodexDeviceTokenResponse> {
    let token_url = format!("{CODEX_OAUTH_ISSUER}{CODEX_DEVICE_TOKEN_PATH}");
    let expires_at = std::time::Instant::now() + Duration::from_secs(15 * 60);
    let mut dots = 0usize;

    loop {
        if std::time::Instant::now() >= expires_at {
            return Err(miette!("Codex OAuth device authorization expired"));
        }

        tokio::time::sleep(interval).await;
        dots = (dots + 1) % 4;
        print!(
            "\r  {}",
            crate::tr!(locale, "codex_oauth.waiting", dots = ".".repeat(dots + 1))
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());

        let resp = http
            .post(&token_url)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "device_auth_id": device_auth_id,
                "user_code": user_code,
            }))
            .send()
            .await
            .map_err(|e| miette!("Codex OAuth device polling failed: {e}"))?;

        let status = resp.status();
        if status.is_success() {
            println!(
                "\r  {}                                  ",
                crate::tr!(locale, "codex_oauth.success")
            );
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

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn info(msg: &str) {
    println!("  {msg}");
}

fn header(msg: &str) {
    println!("\n{msg}");
    println!("{}", "─".repeat(msg.len()));
}

fn prompt_cancelled(locale: Locale) -> miette::Report {
    miette!("{}", crate::tr!(locale, "common.cancelled"))
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
                .draw(|frame| render_select_prompt(frame, locale, prompt, items, &mut state))
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
                KeyCode::Esc => return Err(prompt_cancelled(self.locale)),
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

        loop {
            let locale = self.locale;
            self.terminal_mut()?
                .draw(|frame| {
                    render_text_prompt(frame, locale, prompt, &value, cursor, secret, error)
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
                KeyCode::Esc => return Err(prompt_cancelled(self.locale)),
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

    fn detail(&mut self, prompt: &str, lines: &[String]) -> Result<()> {
        loop {
            let locale = self.locale;
            self.terminal_mut()?
                .draw(|frame| render_detail_prompt(frame, locale, prompt, lines))
                .map_err(|e| {
                    miette!(
                        "{}",
                        crate::tr!(locale, "prompt_ui.render_failed", error = e)
                    )
                })?;

            let key = read_prompt_key()?;
            match key.code {
                KeyCode::Esc | KeyCode::Enter => return Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(prompt_cancelled(self.locale));
                }
                _ => {}
            }
        }
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

fn prompt_panel_block(locale: Locale) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Line::from(vec![
            Span::styled(
                "Config",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", crate::tr!(locale, "prompt_ui.inline")),
                Style::default().fg(Color::DarkGray),
            ),
        ]))
}

fn render_select_prompt<T: AsRef<str>>(
    frame: &mut Frame,
    locale: Locale,
    prompt: &str,
    items: &[T],
    state: &mut ListState,
) {
    let block = prompt_panel_block(locale);
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ]);
    let [kind_area, prompt_area, list_area, help_area] = inner.layout(&layout);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                crate::tr!(locale, "prompt_ui.select"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(
                prompt.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        kind_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            crate::tr!(locale, "prompt_ui.option_count", count = items.len()),
            Style::default().fg(Color::Gray),
        ))),
        prompt_area,
    );

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

fn render_text_prompt(
    frame: &mut Frame,
    locale: Locale,
    prompt: &str,
    value: &str,
    cursor: usize,
    secret: bool,
    error: Option<&str>,
) {
    let block = prompt_panel_block(locale);
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ]);
    let [kind_area, prompt_area, input_area, help_area, note_area] = inner.layout(&layout);

    let display = if secret {
        "*".repeat(value.chars().count())
    } else {
        value.to_string()
    };
    let input = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Cyan)),
        Span::raw(display),
    ]);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                if secret {
                    crate::tr!(locale, "prompt_ui.secret")
                } else {
                    crate::tr!(locale, "prompt_ui.input")
                },
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(
                prompt.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        kind_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            crate::tr!(locale, "prompt_ui.enter_confirm"),
            Style::default().fg(Color::Gray),
        ))),
        prompt_area,
    );

    let field_block = Block::default()
        .borders(Borders::ALL)
        .border_style(if error.is_some() {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Cyan)
        })
        .title(Line::from(Span::styled(
            crate::tr!(locale, "prompt_ui.value"),
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
        field_inner.x + 2 + value[..cursor].chars().count() as u16,
        field_inner.y,
    ));

    frame.render_widget(
        Paragraph::new(crate::tr!(locale, "prompt_ui.help_text"))
            .style(Style::default().fg(Color::DarkGray)),
        help_area,
    );
    frame.render_widget(
        Paragraph::new(match error {
            Some(error) => Line::from(Span::styled(
                error.to_string(),
                Style::default().fg(Color::Red),
            )),
            None if secret => Line::from(Span::styled(
                crate::tr!(locale, "prompt_ui.masked"),
                Style::default().fg(Color::DarkGray),
            )),
            None => Line::from(Span::styled(
                crate::tr!(locale, "prompt_ui.plain"),
                Style::default().fg(Color::DarkGray),
            )),
        }),
        note_area,
    );
}

fn render_loading_prompt(frame: &mut Frame, locale: Locale, prompt: &str, note: &str) {
    let block = prompt_panel_block(locale);
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ]);
    let [kind_area, prompt_area, body_area, help_area] = inner.layout(&layout);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                crate::tr!(locale, "prompt_ui.loading"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(
                prompt.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        kind_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            note.to_string(),
            Style::default().fg(Color::Gray),
        ))),
        prompt_area,
    );
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

fn render_detail_prompt(frame: &mut Frame, locale: Locale, prompt: &str, lines: &[String]) {
    let block = prompt_panel_block(locale);
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ]);
    let [kind_area, prompt_area, body_area, help_area] = inner.layout(&layout);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                crate::tr!(locale, "prompt_ui.detail"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(
                prompt.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        kind_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            crate::tr!(locale, "prompt_ui.line_count", count = lines.len()),
            Style::default().fg(Color::Gray),
        ))),
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
        Paragraph::new(crate::tr!(locale, "prompt_ui.help_detail"))
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
}

impl ProviderKind {
    fn labels(locale: Locale) -> Vec<String> {
        vec![
            "OpenAI".to_string(),
            "OpenAI Codex OAuth".to_string(),
            "GitHub Copilot".to_string(),
            crate::tr!(locale, "config.provider_openai_compatible"),
        ]
    }

    fn from_index(i: usize) -> Self {
        match i {
            0 => Self::OpenAI,
            1 => Self::OpenAICodexOauth,
            2 => Self::GithubCopilot,
            _ => Self::OpenAICompatible,
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
        ProviderKind::OpenAICompatible => "local",
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
                ui.suspend();
                let result = run_codex_oauth_browser_flow(locale).await;
                ui.resume()?;
                let tokens = result?;
                write_codex_oauth_tokens(&default_auth_file, &tokens).await?;
                Some(default_auth_file.to_string_lossy().to_string())
            } else if auth_method == 1 {
                ui.suspend();
                let result = run_codex_oauth_device_flow(locale).await;
                ui.resume()?;
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
                    ui.suspend();
                    let result = run_github_device_flow(locale).await;
                    ui.resume()?;
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
                Some("http://localhost:11434/v1"),
            )?;
            let api_key = ui.text(&crate::tr!(locale, "config.local_api_key"), Some("ollama"))?;
            ProviderConfig::OpenaiCompatible {
                base_url: normalize_provider_base_url(&base_url),
                api_key,
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
            }
        })
        .collect()
}

fn resolve_model_capacity(
    model_id: &str,
    detected_context_window: Option<usize>,
    detected_max_output: Option<usize>,
) -> ModelCapacity {
    let catalog = catalog_model_capacity(model_id);
    let fallback = conservative_model_capacity();
    ModelCapacity {
        context_window_tokens: detected_context_window
            .or_else(|| catalog.map(|capacity| capacity.context_window_tokens))
            .unwrap_or(fallback.context_window_tokens),
        max_completion_tokens: detected_max_output
            .or_else(|| catalog.map(|capacity| capacity.max_completion_tokens))
            .unwrap_or(fallback.max_completion_tokens),
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
        ProviderConfig::OpenaiCompatible { base_url, api_key } => {
            let api_key = resolve_env_reference(api_key);
            fetch_openai_models(base_url, &api_key).await
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

    let (model_id, api_ctx, api_out) = if discovered.is_empty() {
        let id = ui.text("Model ID", None)?;
        (id, None, None)
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
            (id, None, None)
        } else {
            let m = &discovered[idx];
            (m.id.clone(), m.context_window, m.max_output_tokens)
        }
    };

    let capacity = resolve_model_capacity(&model_id, api_ctx, api_out);

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

    Ok((
        name,
        ModelConfig {
            provider: provider_name.to_string(),
            model_id,
            context_window_tokens: context_window,
            max_completion_tokens: max_completion,
            ..ModelConfig::default()
        },
    ))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run the first-time setup wizard, write config.toml, and return the generated Config.
pub async fn run_first_time_setup() -> Result<Config> {
    println!();
    println!("Daat Locus setup");
    println!();

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

    println!();
    println!("{}", crate::tr!(locale, "setup.welcome"));
    println!("{}", crate::tr!(locale, "setup.missing_config"));
    println!();

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
        info(&crate::tr!(locale, "setup.default_created"));
        return Ok(config);
    }

    header(&crate::tr!(locale, "setup.provider_step"));
    let (provider_name, provider_config) = prompt_provider(&mut ui, &[]).await?;

    let mut providers = HashMap::new();
    providers.insert(provider_name.clone(), provider_config.clone());

    header(&crate::tr!(locale, "setup.model_step"));
    let (model_name, model_config) =
        prompt_model(&mut ui, &provider_name, &provider_config).await?;

    let mut models = HashMap::new();
    models.insert(model_name.clone(), model_config);

    let config = Config {
        locale,
        providers,
        models,
        main_model: model_name.clone(),
        judge: JudgeConfig::default(),
        ..Config::default()
    };

    write_config(&config).await?;

    println!();
    println!("{}", crate::tr!(locale, "setup.written"));
    println!("  main_model = \"{model_name}\" （provider: {provider_name}）");
    println!();

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

    header(&crate::tr!(locale, "config.add_provider"));
    let existing: Vec<String> = config.providers.keys().cloned().collect();
    let mut ui = PromptUi::new(locale)?;
    let (name, provider) = prompt_provider(&mut ui, &existing).await?;

    if config.providers.contains_key(&name) {
        let overwrite = ui.confirm(
            &crate::tr!(locale, "common.overwrite_provider", name = name.clone()),
            false,
        )?;
        if !overwrite {
            info(&crate::tr!(locale, "common.cancelled_action"));
            return Ok(());
        }
    }

    config.providers.insert(name.clone(), provider);
    write_config(&config).await?;
    info(&crate::tr!(locale, "config.provider_saved", name = name));
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

    header(&crate::tr!(locale, "config.add_model"));
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
            info(&crate::tr!(locale, "common.cancelled_action"));
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
    info(&crate::tr!(locale, "config.model_saved", name = name));
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
    info(&crate::tr!(
        locale,
        "config.main_model_set",
        name = config.main_model.clone()
    ));
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

fn render_config_summary_lines(config: &Config, locale: Locale) -> Vec<String> {
    let mut lines = Vec::new();

    lines.push(crate::tr!(locale, "config.locale_heading"));
    lines.push("──────".to_string());
    lines.push(format!(
        "  {} ({})",
        config.locale.as_str(),
        config.locale.display_name()
    ));
    lines.push(String::new());

    lines.push(crate::tr!(locale, "config.providers_heading"));
    lines.push("─────────".to_string());
    for (name, provider) in &config.providers {
        let desc = match provider {
            ProviderConfig::Openai { api_key, base_url } => {
                let masked = mask_secret(api_key);
                let url = base_url.as_deref().unwrap_or("https://api.openai.com/v1");
                format!("openai  url={url}  key={masked}")
            }
            ProviderConfig::GithubCopilot { github_token } => {
                let masked = mask_secret(github_token);
                format!("github-copilot  token={masked}")
            }
            ProviderConfig::OpenaiCodexOauth {
                auth_file,
                base_url,
            } => {
                let url = base_url
                    .as_deref()
                    .unwrap_or(codex_oauth_default_base_url());
                let auth_file = auth_file.as_deref().unwrap_or("<default>");
                format!("openai-codex-oauth  url={url}  auth_file={auth_file}")
            }
            ProviderConfig::OpenaiCompatible { base_url, api_key } => {
                let masked = mask_secret(api_key);
                format!("openai-compatible  url={base_url}  key={masked}")
            }
        };
        lines.push(format!("  [{name}]  {desc}"));
    }

    lines.push(String::new());
    lines.push(crate::tr!(locale, "config.models_heading"));
    lines.push("──────".to_string());
    for (name, model) in &config.models {
        let main_mark = if name == &config.main_model {
            " ← main"
        } else {
            ""
        };
        lines.push(format!(
            "  [{name}]{main_mark}  provider={}  model_id={}  ctx={}  max_out={}",
            model.provider,
            model.model_id,
            model.context_window_tokens,
            model.max_completion_tokens
        ));
    }

    lines.push(String::new());
    lines.push(crate::tr!(locale, "config.judge_heading"));
    lines.push("─────".to_string());
    let judge_model = config.judge.model.as_deref().unwrap_or(&config.main_model);
    lines.push(format!(
        "  enabled={}  model={}  candidates={}  cases={}",
        config.judge.enabled,
        judge_model,
        config.judge.max_pairwise_candidates,
        config.judge.max_pairwise_cases
    ));

    lines.push(String::new());
    lines.push(crate::tr!(locale, "config.hindsight_heading"));
    lines.push("─────────".to_string());
    let hindsight_model = config
        .hindsight
        .model
        .as_deref()
        .unwrap_or(&config.main_model);
    let fallback_mark = if config.hindsight.model.is_none() {
        crate::tr!(locale, "config.fallback_to_main")
    } else {
        String::new()
    };
    lines.push(format!(
        "  model={}{}  port={}  profile={}",
        hindsight_model, fallback_mark, config.hindsight.port, config.hindsight.profile,
    ));

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
        let status = if has_config {
            match crate::config::load_config().await {
                Ok(cfg) => {
                    locale = cfg.locale;
                    ui.set_locale(locale);
                    crate::tr!(
                        locale,
                        "config.status_configured",
                        main_model = cfg.main_model,
                        providers = cfg.providers.len(),
                        models = cfg.models.len(),
                        locale_name = cfg.locale.display_name()
                    )
                }
                Err(e) => crate::tr!(locale, "config.status_load_error", error = e),
            }
        } else {
            crate::tr!(locale, "config.status_unconfigured")
        };

        let items = [
            crate::tr!(locale, "config.show_details"),
            crate::tr!(locale, "config.add_provider"),
            crate::tr!(locale, "config.add_model"),
            crate::tr!(locale, "config.change_main_model"),
            crate::tr!(locale, "config.change_hindsight_model"),
            crate::tr!(locale, "config.exit"),
        ];

        let idx = ui.select(
            &crate::tr!(locale, "config.menu_title", status = status),
            &items,
            0,
        )?;

        match idx {
            0 => match crate::config::load_config().await {
                Ok(cfg) => ui.detail(
                    &crate::tr!(locale, "config.details_title"),
                    &render_config_summary_lines(&cfg, locale),
                )?,
                Err(e) => ui.detail(
                    &crate::tr!(locale, "config.details_title"),
                    &[crate::tr!(locale, "common.config_load_failed", error = e)],
                )?,
            },
            1 => {
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
                    continue;
                }
                config.providers.insert(name, provider);
                write_config(&config).await?;
            }
            2 => {
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
                let (name, model) = prompt_model(&mut ui, provider_name, provider_config).await?;
                if config.models.contains_key(&name)
                    && !ui.confirm(
                        &crate::tr!(locale, "common.overwrite_model", name = name.clone()),
                        false,
                    )?
                {
                    continue;
                }
                config.models.insert(name.clone(), model);
                if ui.confirm(
                    &crate::tr!(locale, "config.set_as_main", name = name.clone()),
                    false,
                )? {
                    config.main_model = name;
                }
                write_config(&config).await?;
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
                    .position(|n| n == &config.main_model)
                    .unwrap_or(0);
                let idx = ui.select(
                    &crate::tr!(locale, "config.select_main_model"),
                    &model_names,
                    current_idx,
                )?;
                config.main_model = model_names[idx].clone();
                write_config(&config).await?;
            }
            4 => {
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
                let mut items: Vec<String> = model_names.clone();
                let use_main = crate::tr!(locale, "config.use_main_model");
                items.push(use_main.clone());
                let current_idx = config
                    .hindsight
                    .model
                    .as_ref()
                    .and_then(|m| model_names.iter().position(|n| n == m))
                    .unwrap_or(items.len() - 1);
                let idx = ui.select(
                    &crate::tr!(locale, "config.select_hindsight_model"),
                    &items,
                    current_idx,
                )?;
                config.hindsight.model = if items[idx] == use_main {
                    None
                } else {
                    Some(model_names[idx].clone())
                };
                write_config(&config).await?;
            }
            _ => break,
        }
    }
    Ok(())
}

/// `config set-hindsight-model` subcommand.
pub async fn run_set_hindsight_model() -> Result<()> {
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

    let mut items: Vec<String> = model_names.clone();
    let use_main = crate::tr!(locale, "config.use_main_model");
    items.push(use_main.clone());

    let current_idx = config
        .hindsight
        .model
        .as_ref()
        .and_then(|m| model_names.iter().position(|n| n == m))
        .unwrap_or(items.len() - 1);

    let idx = ui.select(
        &crate::tr!(locale, "config.select_hindsight_model"),
        &items,
        current_idx,
    )?;

    config.hindsight.model = if items[idx] == use_main {
        None
    } else {
        Some(model_names[idx].clone())
    };

    write_config(&config).await?;
    let display = config
        .hindsight
        .model
        .as_deref()
        .unwrap_or(&config.main_model);
    info(&crate::tr!(
        locale,
        "config.hindsight_model_set",
        name = display
    ));
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
        let capacity = resolve_model_capacity("gpt-4.1", Some(12_345), Some(678));

        assert_eq!(
            capacity,
            ModelCapacity {
                context_window_tokens: 12_345,
                max_completion_tokens: 678,
            }
        );
    }

    #[test]
    fn model_capacity_fills_missing_detected_fields_from_exact_catalog_match() {
        let capacity = resolve_model_capacity("gpt-4.1", Some(12_345), None);

        assert_eq!(
            capacity,
            ModelCapacity {
                context_window_tokens: 12_345,
                max_completion_tokens: 32_768,
            }
        );
    }

    #[test]
    fn model_capacity_uses_conservative_defaults_for_unknown_models() {
        let capacity = resolve_model_capacity("unknown-local-model", None, None);

        assert_eq!(capacity, conservative_model_capacity());
    }

    #[test]
    fn model_catalog_does_not_substring_match_similar_model_names() {
        let capacity = resolve_model_capacity("gpt-4.1-custom", None, None);

        assert_eq!(capacity, conservative_model_capacity());
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
}
