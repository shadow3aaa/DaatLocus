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

/// Static fallback for `OpenAI` Codex. The `ChatGPT` Codex backend may return an
/// empty `/models` list while still accepting current Codex model slugs.
const CODEX_OAUTH_DEFAULT_MODELS: &[&str] = &[
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
];
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
    CODEX_OAUTH_DEFAULT_MODELS
        .iter()
        .map(|id| {
            let capacity = catalog_model_capacity_for_provider("openai", id);
            DiscoveredModel {
                id: (*id).to_string(),
                context_window: capacity.map(|capacity| capacity.context_window_tokens),
                max_output_tokens: capacity.map(|capacity| capacity.max_completion_tokens),
                supports_vision: capacity.map(|c| c.supports_vision),
                reasoning_options: Some(codex_oauth_reasoning_options()),
            }
        })
        .collect()
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
        ProviderConfig::OpenaiCompatible { base_url, .. } => {
            catalog_provider_id_for_base_url_and_model(base_url, model_id)
        }
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
        ProviderConfig::OpenaiCompatible {
            base_url, api_key, ..
        } => {
            let api_key = resolve_env_reference(api_key);
            fetch_openai_models(base_url, &api_key).await
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
    let url = format!(
        "{}/models?client_version={}",
        normalize_provider_base_url(base_url),
        codex_oauth_client_version()
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| {
            miette!("OpenAI Codex model discovery: failed to build http client: {err}")
        })?;
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
    let models = parse_models_response(Some(json));
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
                supports_vision: None,
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
            .flat_map(|items| items.iter().filter_map(|item| item.as_str()))
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
        ProviderConfig::OpenaiCodexOauth { .. } => codex_oauth_reasoning_options(),
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

fn codex_oauth_reasoning_options() -> Vec<ReasoningOption> {
    vec![ReasoningOption::Effort {
        values: ["none", "minimal", "low", "medium", "high", "xhigh"]
            .into_iter()
            .map(str::to_string)
            .collect(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_oauth_fallback_models_include_current_gpt_5_6_variants() {
        let ids = codex_oauth_fallback_models()
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<_>>();

        assert!(ids.contains(&"gpt-5.6-sol".to_string()));
        assert!(ids.contains(&"gpt-5.6-terra".to_string()));
        assert!(ids.contains(&"gpt-5.6-luna".to_string()));
    }

    #[test]
    fn codex_oauth_reasoning_defaults_include_xhigh() {
        let options = codex_oauth_reasoning_options();
        let ReasoningOption::Effort { values } = &options[0] else {
            panic!("expected Codex reasoning effort options");
        };

        assert!(values.contains(&"xhigh".to_string()));
    }
}
