//! Provider model discovery shared by the CLI/TUI config wizard and `WebUI` setup.

use std::time::Duration;

use miette::{Result, miette};

use crate::{
    config::{
        ProviderConfig, normalize_provider_base_url, redact_secret_text, resolve_env_reference,
    },
    model_catalog::{
        ModelCapacity, ReasoningOption, catalog_model_capacity,
        catalog_model_capacity_for_provider, catalog_model_reasoning_options_for_provider,
        catalog_provider_has_model, catalog_provider_ids_for_api_url, conservative_model_capacity,
        parse_reasoning_options,
    },
    providers::{
        codex_oauth_access_from_file, codex_oauth_client_version, codex_oauth_default_base_url,
        codex_oauth_headers,
    },
};

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

/// Narrow snapshot of the official Codex catalog, used only for empty catalogs.
const CODEX_OAUTH_MODEL_CATALOG: &str = include_str!("providers/codex_model_catalog.json");
const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Model metadata returned by the provider API.
#[derive(Debug, Clone)]
pub struct DiscoveredModel {
    pub(crate) id: String,
    pub(crate) context_window: Option<usize>,
    pub(crate) max_output_tokens: Option<usize>,
    pub(crate) supports_vision: Option<bool>,
    pub(crate) reasoning_options: Option<Vec<ReasoningOption>>,
}

fn codex_oauth_fallback_models() -> Vec<DiscoveredModel> {
    parse_models_response_for_auth(
        Some(serde_json::from_str(CODEX_OAUTH_MODEL_CATALOG).expect("valid bundled Codex catalog")),
        true,
    )
}

fn copilot_fallback_models() -> Vec<DiscoveredModel> {
    COPILOT_DEFAULT_MODELS
        .iter()
        .map(|s| DiscoveredModel {
            id: s.to_string(),
            context_window: None,
            max_output_tokens: None,
            supports_vision: None,
            reasoning_options: None,
        })
        .collect()
}

pub fn resolve_model_capacity(
    provider: &ProviderConfig,
    model_id: &str,
    detected_context_window: Option<usize>,
    detected_max_output: Option<usize>,
    detected_supports_vision: Option<bool>,
) -> ModelCapacity {
    let catalog_provider_id = catalog_provider_id_for_model(provider, model_id);
    let catalog = catalog_provider_id.as_deref().map_or_else(
        || catalog_model_capacity(model_id),
        |provider_id| catalog_model_capacity_for_provider(provider_id, model_id),
    );
    let fallback = conservative_model_capacity();

    ModelCapacity {
        context_window_tokens: detected_context_window
            .or_else(|| catalog.map(|capacity| capacity.context_window_tokens))
            .unwrap_or(fallback.context_window_tokens),
        max_completion_tokens: detected_max_output
            .or_else(|| catalog.map(|capacity| capacity.max_completion_tokens))
            .unwrap_or(fallback.max_completion_tokens),
        supports_vision: detected_supports_vision
            .unwrap_or_else(|| catalog.map_or(fallback.supports_vision, |c| c.supports_vision)),
        supports_tool_call: catalog.map_or(fallback.supports_tool_call, |c| c.supports_tool_call),
    }
}

fn catalog_provider_id_for_model(provider: &ProviderConfig, model_id: &str) -> Option<String> {
    match provider {
        ProviderConfig::Openai { base_url, .. } => base_url.as_deref().map_or_else(
            || Some("openai".to_string()),
            |base_url| {
                catalog_provider_id_for_base_url_and_model(base_url, model_id)
                    .or_else(|| Some("openai".to_string()))
            },
        ),
        ProviderConfig::GithubCopilot { .. } => Some("github-copilot".to_string()),
        // models.dev has no separate ChatGPT Codex provider. The model slugs
        // line up with OpenAI entries for capacity metadata; Codex-specific
        // reasoning defaults are handled separately below.
        ProviderConfig::OpenaiCodexOauth { .. } => Some("openai".to_string()),
        ProviderConfig::OpenCodeConsoleOauth { .. } => None,
        ProviderConfig::OpenaiCompatible { base_url, .. } => {
            catalog_provider_id_for_base_url_and_model(base_url, model_id)
        }
        // Anthropic-compatible relays are keyed by model slug; fall back to the
        // provider-agnostic catalog lookup in `resolve_model_capacity`.
        ProviderConfig::AnthropicCompatible { .. } => None,
        ProviderConfig::Ollama { .. } => None,
    }
}

fn catalog_provider_id_for_base_url_and_model(base_url: &str, model_id: &str) -> Option<String> {
    if normalize_provider_base_url(base_url) == OPENAI_DEFAULT_BASE_URL {
        return Some("openai".to_string());
    }

    let provider_ids = catalog_provider_ids_for_api_url(base_url);
    if provider_ids.len() == 1 {
        return provider_ids.into_iter().next();
    }

    let model_matches: Vec<String> = provider_ids
        .into_iter()
        .filter(|provider_id| catalog_provider_has_model(provider_id, model_id))
        .collect();
    if model_matches.len() == 1 {
        model_matches.into_iter().next()
    } else {
        None
    }
}

/// Fetch provider model IDs. Failures return an empty list.
pub async fn fetch_model_ids(
    provider_name: &str,
    provider: &ProviderConfig,
) -> Vec<DiscoveredModel> {
    match discover_model_ids(provider_name, provider).await {
        Ok(models) => models,
        Err(err) => {
            tracing::warn!("model discovery failed: {err}");
            if matches!(provider, ProviderConfig::GithubCopilot { .. }) {
                copilot_fallback_models()
            } else {
                Vec::new()
            }
        }
    }
}

/// Discover provider model IDs. Failures are returned to callers.
pub async fn discover_model_ids(
    _provider_name: &str,
    provider: &ProviderConfig,
) -> Result<Vec<DiscoveredModel>> {
    match provider {
        ProviderConfig::GithubCopilot { github_token } => {
            discover_copilot_models(github_token).await
        }
        ProviderConfig::Openai { api_key, base_url } => {
            let base = base_url.as_deref().unwrap_or("https://api.openai.com/v1");
            let api_key = resolve_env_reference(api_key);
            fetch_openai_models(base, &api_key).await
        }
        ProviderConfig::OpenaiCodexOauth {
            base_url,
            auth_file,
        } => {
            let base = base_url
                .as_deref()
                .unwrap_or(codex_oauth_default_base_url());
            fetch_codex_oauth_models(auth_file, base).await
        }
        ProviderConfig::OpenCodeConsoleOauth {
            base_url,
            auth_file,
        } => fetch_opencode_console_models(base_url.as_deref(), auth_file).await,
        ProviderConfig::OpenaiCompatible {
            base_url, api_key, ..
        } => {
            let api_key = resolve_env_reference(api_key);
            fetch_openai_models(base_url, &api_key).await
        }
        ProviderConfig::AnthropicCompatible { base_url, api_key } => {
            let api_key = resolve_env_reference(api_key);
            fetch_anthropic_models(base_url, &api_key).await
        }
        ProviderConfig::Ollama { host, .. } => {
            let host = host.as_deref().map_or_else(
                || "http://127.0.0.1:11434".to_string(),
                std::string::ToString::to_string,
            );
            fetch_ollama_models(&host).await
        }
    }
}

/// Discover Copilot models via the internal session-token API.
async fn discover_copilot_models(github_token: &str) -> Result<Vec<DiscoveredModel>> {
    let token = resolve_env_reference(github_token);
    if token.is_empty() {
        return Err(miette!("copilot model discovery: github token is empty"));
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| miette!("copilot model discovery: http client error: {err}"))?;

    let models = try_fetch_via_session_token(&client, &token).await?;
    tracing::info!(
        "copilot model discovery: {} models via internal API",
        models.len()
    );
    Ok(models)
}

async fn try_fetch_via_session_token(
    client: &reqwest::Client,
    github_token: &str,
) -> Result<Vec<DiscoveredModel>> {
    let resp = client
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Authorization", format!("Bearer {github_token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("Editor-Version", "vscode/1.96.2")
        .header("X-Github-Api-Version", "2025-04-01")
        .send()
        .await
        .map_err(|err| miette!("copilot session token request failed: {err}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let body = redact_secret_text(&body, github_token);
        return Err(miette!(
            "copilot session token request returned HTTP {status}: {body}"
        ));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|err| miette!("copilot session token response parse failed: {err}"))?;
    let session_token = json["token"]
        .as_str()
        .ok_or_else(|| miette!("copilot session token response missing token"))?
        .to_string();

    let base_url = crate::providers::copilot_base_url_from_session_token(&session_token)
        .unwrap_or_else(|| "https://api.individual.githubcopilot.com".to_string());

    let models =
        fetch_copilot_internal_models(client, &format!("{base_url}/models"), &session_token)
            .await?;
    if models.is_empty() {
        Err(miette!(
            "copilot internal models response did not include models"
        ))
    } else {
        Ok(models)
    }
}

async fn fetch_openai_models(base_url: &str, api_key: &str) -> Result<Vec<DiscoveredModel>> {
    let url = format!("{}/models", normalize_provider_base_url(base_url));
    fetch_openai_models_path(&url, api_key).await
}

async fn fetch_openai_models_path(url: &str, api_key: &str) -> Result<Vec<DiscoveredModel>> {
    let url = url.to_string();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| miette!("fetch_openai_models: failed to build http client: {err}"))?;
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .map_err(|err| miette!("fetch_openai_models: request to {url} failed: {err}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body = redact_secret_text(&body, api_key);
        return Err(miette!(
            "fetch_openai_models: request to {url} returned HTTP {status}: {body}"
        ));
    }
    let json = resp
        .json()
        .await
        .map_err(|err| miette!("fetch_openai_models: response parse failed: {err}"))?;
    Ok(parse_models_response(Some(json)))
}

/// Discover OpenCode Console model IDs.
///
/// The Console-issued inference endpoint (`/inference/openai/v1`) exposes no
/// `/models` route, so the catalog comes from the Console config endpoint
/// (`GET {server}/api/config`), which the official client also consults.
async fn fetch_opencode_console_models(
    base_url: Option<&str>,
    auth_file: &str,
) -> Result<Vec<DiscoveredModel>> {
    let access = crate::providers::opencode_console_access(std::path::Path::new(auth_file)).await?;
    let server = access
        .server
        .as_deref()
        .unwrap_or(crate::providers::OPENCODE_OAUTH_DEFAULT_SERVER);
    let base =
        normalize_provider_base_url(base_url.unwrap_or(crate::providers::OPENCODE_ZEN_BASE_URL));
    let url = format!("{}/api/config", server.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| {
            miette!("fetch_opencode_console_models: failed to build http client: {err}")
        })?;
    let mut request = client.get(&url).bearer_auth(&access.access_token);
    if let Some(org_id) = access.org_id.as_deref() {
        request = request.header("x-org-id", org_id);
    }
    let resp = request
        .send()
        .await
        .map_err(|err| miette!("fetch_opencode_console_models: request to {url} failed: {err}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body = redact_secret_text(&body, &access.access_token);
        return Err(miette!(
            "fetch_opencode_console_models: request to {url} returned HTTP {status}: {body}"
        ));
    }
    let json = resp
        .json()
        .await
        .map_err(|err| miette!("fetch_opencode_console_models: response parse failed: {err}"))?;
    Ok(parse_opencode_console_models(&json, &base))
}

/// Parse the `config.provider.*.models` catalog returned by `GET /api/config`,
/// preferring the provider whose `api` matches the configured inference base.
fn parse_opencode_console_models(json: &serde_json::Value, base_url: &str) -> Vec<DiscoveredModel> {
    let Some(providers) = json["config"]["provider"].as_object() else {
        return Vec::new();
    };
    let matches_base = |value: &serde_json::Value| {
        value["api"]
            .as_str()
            .is_some_and(|api| normalize_provider_base_url(api) == base_url)
    };
    let provider = providers
        .values()
        .find(|value| matches_base(value))
        .or_else(|| providers.get("opencode"));
    let Some(provider) = provider else {
        return Vec::new();
    };
    let Some(models) = provider["models"].as_object() else {
        return Vec::new();
    };
    let mut discovered: Vec<DiscoveredModel> = models
        .iter()
        .filter(|(_, model)| model["disabled"].as_bool() != Some(true))
        .map(|(id, model)| {
            let limit = &model["limit"];
            DiscoveredModel {
                id: id.clone(),
                context_window: limit["context"]
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok()),
                max_output_tokens: limit["output"]
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok()),
                supports_vision: model["modalities"]["input"]
                    .as_array()
                    .map(|inputs| inputs.iter().any(|input| input.as_str() == Some("image"))),
                reasoning_options: None,
            }
        })
        .collect();
    discovered.sort_by(|a, b| a.id.cmp(&b.id));
    discovered
}

/// Discover Anthropic-compatible model IDs via `GET /v1/models`, which
/// authenticates with `x-api-key` rather than a bearer token.
async fn fetch_anthropic_models(base_url: &str, api_key: &str) -> Result<Vec<DiscoveredModel>> {
    let base = normalize_provider_base_url(base_url);
    let url = if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| miette!("fetch_anthropic_models: failed to build http client: {err}"))?;
    let resp = client
        .get(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .map_err(|err| miette!("fetch_anthropic_models: request to {url} failed: {err}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body = redact_secret_text(&body, api_key);
        return Err(miette!(
            "fetch_anthropic_models: request to {url} returned HTTP {status}: {body}"
        ));
    }
    let json = resp
        .json()
        .await
        .map_err(|err| miette!("fetch_anthropic_models: response parse failed: {err}"))?;
    Ok(parse_models_response(Some(json)))
}

async fn fetch_ollama_models(host: &str) -> Result<Vec<DiscoveredModel>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| miette!("fetch_ollama_models: failed to build http client: {err}"))?;

    let tags_url = format!("{host}/api/tags");
    let resp = client
        .get(&tags_url)
        .send()
        .await
        .map_err(|err| miette!("fetch_ollama_models: request to {tags_url} failed: {err}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(miette!(
            "fetch_ollama_models: request to {tags_url} returned HTTP {status}"
        ));
    }
    let tags_json: serde_json::Value = resp
        .json()
        .await
        .map_err(|err| miette!("fetch_ollama_models: response parse failed: {err}"))?;
    let Some(model_list) = tags_json.get("models").and_then(|m| m.as_array()) else {
        return Err(miette!(
            "fetch_ollama_models: response missing models array"
        ));
    };

    let model_ids: Vec<String> = model_list
        .iter()
        .filter_map(|m| {
            m.get("model")
                .and_then(|v| v.as_str())
                .map(std::string::ToString::to_string)
        })
        .collect();

    let Ok(show_client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
    else {
        return Ok(model_ids
            .into_iter()
            .map(|id| DiscoveredModel {
                id,
                context_window: None,
                max_output_tokens: None,
                supports_vision: None,
                reasoning_options: None,
            })
            .collect());
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
                reasoning_options: None,
            });
        }
    }
    discovered.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(discovered)
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
        if let Some(n) = val.as_u64().and_then(|value| usize::try_from(value).ok()) {
            return Some(n);
        }
        if let Some(n) = val.as_i64()
            && n > 0
        {
            return usize::try_from(n).ok();
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

async fn fetch_codex_oauth_models(auth_file: &str, base_url: &str) -> Result<Vec<DiscoveredModel>> {
    let auth_file = std::path::Path::new(auth_file);
    let access = codex_oauth_access_from_file(auth_file)
        .await
        .map_err(|err| {
            miette!(
                "OpenAI Codex model discovery: auth unavailable at {}: {err}",
                auth_file.display()
            )
        })?;
    let client_version = codex_oauth_client_version();
    let url = format!("{}/models", normalize_provider_base_url(base_url));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| {
            miette!("OpenAI Codex model discovery: failed to build http client: {err}")
        })?;
    let mut request = client
        .get(&url)
        .query(&[("client_version", &client_version)])
        .header("Authorization", format!("Bearer {}", access.access_token))
        .headers(codex_oauth_headers(&client_version));
    if let Some(account_id) = access.account_id.as_deref() {
        request = request.header("ChatGPT-Account-ID", account_id);
    }
    if access.is_fedramp_account {
        request = request.header("X-OpenAI-Fedramp", "true");
    }
    let resp = request
        .send()
        .await
        .map_err(|err| miette!("OpenAI Codex model discovery request to {url} failed: {err}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body = redact_secret_text(&body, &access.access_token);
        return Err(miette!(
            "OpenAI Codex model discovery request to {url} returned HTTP {status}: {body}"
        ));
    }
    let json = resp
        .json()
        .await
        .map_err(|err| miette!("OpenAI Codex model discovery response parse failed: {err}"))?;
    let models = parse_models_response_for_auth(Some(json), true);
    if models.is_empty() {
        Ok(codex_oauth_fallback_models())
    } else {
        Ok(models)
    }
}

async fn fetch_copilot_internal_models(
    client: &reqwest::Client,
    url: &str,
    session_token: &str,
) -> Result<Vec<DiscoveredModel>> {
    let resp = client
        .get(url)
        .header("Authorization", format!("Bearer {session_token}"))
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("Editor-Version", "vscode/1.96.2")
        .header("X-Github-Api-Version", "2025-04-01")
        .send()
        .await
        .map_err(|err| miette!("copilot internal models request to {url} failed: {err}"))?;
    if !resp.status().is_success() {
        let s = resp.status();
        let b = resp.text().await.unwrap_or_default();
        let b = redact_secret_text(&b, session_token);
        return Err(miette!(
            "copilot internal models request to {url} returned HTTP {s}: {b}"
        ));
    }
    let json = resp
        .json()
        .await
        .map_err(|err| miette!("copilot internal models response parse failed: {err}"))?;
    Ok(parse_models_response(Some(json)))
}

pub fn parse_models_response(json: Option<serde_json::Value>) -> Vec<DiscoveredModel> {
    parse_models_response_for_auth(json, false)
}

fn parse_models_response_for_auth(
    json: Option<serde_json::Value>,
    chatgpt_mode: bool,
) -> Vec<DiscoveredModel> {
    let Some(json) = json else { return vec![] };
    let items = json
        .get("data")
        .or_else(|| json.get("models"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut models: Vec<DiscoveredModel> = items
        .iter()
        .filter_map(|m| {
            if (!chatgpt_mode && m["supported_in_api"].as_bool() == Some(false))
                || m["visibility"].as_str().is_some_and(|visibility| {
                    if chatgpt_mode {
                        visibility != "list"
                    } else {
                        visibility == "hide"
                    }
                })
            {
                return None;
            }
            let id = m["id"].as_str().or_else(|| m["slug"].as_str())?.to_string();
            let limits = &m["capabilities"]["limits"];
            let context_window = limits["max_context_window_tokens"]
                .as_u64()
                .or_else(|| m["context_window"].as_u64())
                .or_else(|| m["max_context_window"].as_u64())
                .and_then(|value| usize::try_from(value).ok());
            let max_output_tokens = limits["max_output_tokens"]
                .as_u64()
                .or_else(|| m["max_output_tokens"].as_u64())
                .and_then(|value| usize::try_from(value).ok());
            let reasoning_options = discovered_reasoning_options(m);
            Some(DiscoveredModel {
                id,
                context_window,
                max_output_tokens,
                supports_vision: m["input_modalities"].as_array().map(|modalities| {
                    modalities
                        .iter()
                        .any(|modality| modality.as_str() == Some("image"))
                }),
                reasoning_options,
            })
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

fn discovered_reasoning_options(model: &serde_json::Value) -> Option<Vec<ReasoningOption>> {
    let options = parse_reasoning_options(&model["reasoning_options"]);
    if !options.is_empty() {
        return Some(options);
    }

    [
        &model["supported_reasoning_levels"],
        &model["supported_reasoning_efforts"],
        &model["reasoning_efforts"],
        &model["reasoning"]["efforts"],
        &model["capabilities"]["reasoning_efforts"],
        &model["capabilities"]["reasoning"]["efforts"],
    ]
    .into_iter()
    .find_map(|raw| {
        let values: Vec<String> = raw
            .as_array()
            .into_iter()
            .flat_map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().or_else(|| item["effort"].as_str()))
            })
            .map(str::to_string)
            .collect();
        (!values.is_empty()).then_some(vec![ReasoningOption::Effort { values }])
    })
}

pub fn reasoning_options_for_prompt(
    provider: &ProviderConfig,
    model_id: &str,
    detected_options: Option<&[ReasoningOption]>,
) -> Vec<ReasoningOption> {
    if let Some(options) = detected_options
        && !options.is_empty()
    {
        return options.to_vec();
    }

    let provider_defaults = match provider {
        ProviderConfig::OpenaiCodexOauth { .. } => codex_oauth_reasoning_options(model_id),
        _ => Vec::new(),
    };
    if !provider_defaults.is_empty() {
        return provider_defaults;
    }

    if let Some(provider_id) = catalog_provider_id_for_model(provider, model_id) {
        return catalog_model_reasoning_options_for_provider(&provider_id, model_id)
            .unwrap_or_default();
    }

    crate::model_catalog::catalog_model_reasoning_options(model_id)
}

fn codex_oauth_reasoning_options(model_id: &str) -> Vec<ReasoningOption> {
    if let Some(options) = codex_oauth_fallback_models()
        .into_iter()
        .find(|model| model.id == model_id)
        .and_then(|model| model.reasoning_options)
    {
        return options;
    }
    if let Some(options) = catalog_model_reasoning_options_for_provider("openai", model_id)
        && !options.is_empty()
    {
        return options;
    }
    vec![ReasoningOption::Effort {
        values: ["low", "medium", "high", "xhigh"]
            .into_iter()
            .map(str::to_string)
            .collect(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn codex_discovery_sends_current_identity_and_preserves_login_only_models() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let temp = tempfile::tempdir().unwrap();
        let auth_file = temp.path().join("auth.json");
        tokio::fs::write(&auth_file, serde_json::to_vec(&serde_json::json!({
            "id_token": "test-id", "access_token": "test-access", "refresh_token": "test-refresh", "account_id": "test-account"
        })).unwrap()).await.unwrap();
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let mut chunk = [0; 4096];
                let count = socket.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&chunk[..count]);
            }
            let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
            assert!(request.contains(&format!(
                "/models?client_version={}",
                codex_oauth_client_version().to_ascii_lowercase()
            )));
            assert!(request.contains("originator: codex_cli_rs"));
            assert!(request.contains("user-agent: codex_cli_rs/"));
            assert!(request.contains("authorization: bearer test-access"));
            assert!(request.contains("chatgpt-account-id: test-account"));
            let body = serde_json::json!({"models": [{"slug": "gpt-6.1-sol", "visibility": "list", "supported_in_api": false,
                "supported_reasoning_levels": [{"effort": "max"}, {"effort": "ultra"}], "input_modalities": ["text", "image"]}]}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
        let models =
            fetch_codex_oauth_models(auth_file.to_str().unwrap(), &format!("http://{address}"))
                .await
                .unwrap();
        server.await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-6.1-sol");
        assert_eq!(models[0].supports_vision, Some(true));
    }

    #[test]
    fn codex_oauth_fallback_models_include_current_gpt_6_and_5_6_variants() {
        let ids = codex_oauth_fallback_models()
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<_>>();

        assert!(ids.contains(&"gpt-5.6-sol".to_string()));
        assert!(ids.contains(&"gpt-5.6-terra".to_string()));
        assert!(ids.contains(&"gpt-5.6-luna".to_string()));
        for id in ["gpt-6-astra", "gpt-6.1-sol", "gpt-6-sol", "gpt-6-luna"] {
            assert!(ids.contains(&id.to_string()));
        }
    }

    #[test]
    fn codex_oauth_reasoning_defaults_include_xhigh() {
        let options = codex_oauth_reasoning_options("gpt-6.1-sol");
        let ReasoningOption::Effort { values } = &options[0] else {
            panic!("expected Codex reasoning effort options");
        };

        assert!(values.contains(&"xhigh".to_string()));
        assert!(values.contains(&"max".to_string()));
        assert!(values.contains(&"ultra".to_string()));
        assert!(!values.contains(&"none".to_string()));
        assert!(!values.contains(&"minimal".to_string()));
    }

    #[test]
    fn codex_catalog_uses_login_visibility_and_native_capability_fields() {
        let catalog = serde_json::json!({ "models": [
            { "slug": "gpt-6.1-sol", "visibility": "list", "supported_in_api": false,
              "context_window": 272000, "max_context_window": 872000,
              "input_modalities": ["text", "image"],
              "supported_reasoning_levels": [{"effort": "low"}, {"effort": "max"}, {"effort": "ultra"}] },
            { "slug": "text-only", "visibility": "list", "supported_in_api": true,
              "input_modalities": ["text"] },
            { "slug": "hidden", "visibility": "hide", "supported_in_api": true },
            { "slug": "internal", "visibility": "unlisted", "supported_in_api": true }
        ] });
        let models = parse_models_response_for_auth(Some(catalog.clone()), true);
        assert_eq!(models.len(), 2);
        let sol = models
            .iter()
            .find(|model| model.id == "gpt-6.1-sol")
            .unwrap();
        assert_eq!(sol.context_window, Some(272000));
        assert_eq!(sol.supports_vision, Some(true));
        let Some(options) = &sol.reasoning_options else {
            panic!("missing efforts")
        };
        let ReasoningOption::Effort { values } = &options[0] else {
            panic!("missing efforts")
        };
        assert_eq!(values, &["low", "max", "ultra"]);
        assert_eq!(
            models
                .iter()
                .find(|model| model.id == "text-only")
                .unwrap()
                .supports_vision,
            Some(false)
        );
        assert!(
            !parse_models_response(Some(catalog))
                .iter()
                .any(|model| model.id == "gpt-6.1-sol")
        );
    }

    #[test]
    fn codex_reasoning_fallback_is_model_specific_and_live_catalog_wins() {
        let sol = codex_oauth_reasoning_options("gpt-6.1-sol");
        let luna = codex_oauth_reasoning_options("gpt-6-luna");
        let ReasoningOption::Effort { values: sol } = &sol[0] else {
            panic!("efforts")
        };
        let ReasoningOption::Effort { values: luna } = &luna[0] else {
            panic!("efforts")
        };
        assert!(sol.iter().any(|value| value == "ultra"));
        assert!(!luna.iter().any(|value| value == "ultra"));
        let provider = ProviderConfig::OpenaiCodexOauth {
            base_url: None,
            auth_file: "unused".into(),
        };
        let live = vec![ReasoningOption::Effort {
            values: vec!["new-effort".into()],
        }];
        let result = reasoning_options_for_prompt(&provider, "gpt-6.1-sol", Some(&live));
        let ReasoningOption::Effort { values } = &result[0] else {
            panic!("efforts")
        };
        assert_eq!(values, &["new-effort"]);
    }

    #[test]
    fn copilot_session_token_proxy_ep_uses_key_value_parse() {
        let token = "exp=1; proxy-ep=proxy.individual.githubcopilot.com; sku=x";
        assert_eq!(
            crate::providers::copilot_base_url_from_session_token(token).as_deref(),
            Some("https://api.individual.githubcopilot.com")
        );
        assert!(crate::providers::copilot_base_url_from_session_token("proxy-ep=").is_none());
    }

    #[test]
    fn opencode_console_config_parses_models_and_skips_disabled() {
        let json = serde_json::json!({
            "config": { "provider": {
                "opencode": {
                    "api": "https://opencode.ai/inference/openai/v1",
                    "models": {
                        "gpt-5.1": {
                            "limit": { "context": 400000, "output": 128000 },
                            "modalities": { "input": ["text", "image"] }
                        },
                        "deepseek-v4-pro": {
                            "limit": { "context": 200000, "output": 64000 },
                            "modalities": { "input": ["text"] }
                        },
                        "retired-model": {
                            "disabled": true,
                            "limit": { "context": 1, "output": 1 }
                        }
                    },
                },
                "opencode-go": {
                    "api": "https://opencode.ai/inference/go/openai/v1",
                    "models": { "glm-5.3": { "limit": { "context": 200000, "output": 64000 } } }
                }
            }}
        });

        let models =
            parse_opencode_console_models(&json, "https://opencode.ai/inference/openai/v1");
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, vec!["deepseek-v4-pro", "gpt-5.1"]);

        let gpt = models.iter().find(|model| model.id == "gpt-5.1").unwrap();
        assert_eq!(gpt.context_window, Some(400000));
        assert_eq!(gpt.max_output_tokens, Some(128000));
        assert_eq!(gpt.supports_vision, Some(true));

        let deepseek = models
            .iter()
            .find(|model| model.id == "deepseek-v4-pro")
            .unwrap();
        assert_eq!(deepseek.supports_vision, Some(false));
    }

    #[test]
    fn opencode_console_config_selects_provider_by_api_base() {
        let json = serde_json::json!({
            "config": { "provider": {
                "opencode": { "api": "https://opencode.ai/inference/openai/v1", "models": { "a": {} } },
                "opencode-go": { "api": "https://opencode.ai/inference/go/openai/v1", "models": { "b": {} } }
            }}
        });

        let models =
            parse_opencode_console_models(&json, "https://opencode.ai/inference/go/openai/v1");
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, vec!["b"]);
    }

    #[test]
    fn opencode_console_config_without_providers_is_empty() {
        let json = serde_json::json!({ "config": {} });
        assert!(
            parse_opencode_console_models(&json, "https://opencode.ai/inference/openai/v1")
                .is_empty()
        );
    }
}
