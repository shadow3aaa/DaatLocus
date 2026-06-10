//! Model capacity catalog backed by models.dev API JSON.
//!
//! Three-layer fallback:
//! 1. Local cache at `~/.daat-locus/cache/models-dev-api.json`
//! 2. Built-in copy compiled into the binary
//! 3. Conservative defaults
//!
//! The cache is refreshed from `https://models.dev/api.json` on daemon
//! startup and during the config wizard.

use std::sync::OnceLock;

use crate::daat_locus_paths::{daat_locus_paths, daat_locus_paths_sync};

const BUILTIN_API_JSON: &str = include_str!("../assets/models-dev-api.json");

const CONSERVATIVE_CONTEXT_WINDOW_TOKENS: usize = 32_768;
const CONSERVATIVE_MAX_COMPLETION_TOKENS: usize =
    crate::context_budget::DEFAULT_MAX_COMPLETION_TOKENS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelCapacity {
    pub context_window_tokens: usize,
    pub max_completion_tokens: usize,
    pub supports_vision: bool,
    pub supports_tool_call: bool,
}

pub fn conservative_model_capacity() -> ModelCapacity {
    ModelCapacity {
        context_window_tokens: CONSERVATIVE_CONTEXT_WINDOW_TOKENS,
        max_completion_tokens: CONSERVATIVE_MAX_COMPLETION_TOKENS,
        supports_vision: false,
        supports_tool_call: true,
    }
}

/// Load the best available model catalog: cached file > built-in.
fn load_catalog_json() -> serde_json::Value {
    static CATALOG: OnceLock<serde_json::Value> = OnceLock::new();
    CATALOG
        .get_or_init(|| {
            let paths = daat_locus_paths_sync();
            if let Ok(text) = std::fs::read_to_string(paths.models_dev_cache())
                && let Ok(root) = serde_json::from_str::<serde_json::Value>(&text)
            {
                return root;
            }
            serde_json::from_str(BUILTIN_API_JSON).unwrap_or(serde_json::Value::Null)
        })
        .clone()
}

/// Refresh the local cache from models.dev. Returns Ok if cache was written.
pub async fn refresh_models_dev_cache() -> Result<(), String> {
    let response = reqwest::get("https://models.dev/api.json")
        .await
        .map_err(|e| format!("fetch models.dev failed: {e}"))?
        .text()
        .await
        .map_err(|e| format!("read models.dev response failed: {e}"))?;
    let _: serde_json::Value = serde_json::from_str(&response)
        .map_err(|e| format!("models.dev returned invalid JSON: {e}"))?;
    let paths = daat_locus_paths().await;
    let cache_path = paths.models_dev_cache();
    if let Some(parent) = cache_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("create cache dir failed: {e}"))?;
    }
    tokio::fs::write(&cache_path, &response)
        .await
        .map_err(|e| format!("write cache file failed: {e}"))?;
    tracing::info!("refreshed models.dev cache ({} bytes)", response.len());
    Ok(())
}

fn input_modalities_suggest_vision(modalities: &serde_json::Value) -> bool {
    let Some(inputs) = modalities["input"].as_array() else {
        return false;
    };
    inputs.iter().any(|v| {
        let s = v.as_str().unwrap_or_default();
        matches!(s, "image" | "video" | "pdf" | "audio")
    })
}

fn normalize_catalog_key(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn normalize_catalog_api_url(value: &str) -> Option<String> {
    let normalized = value.trim().trim_end_matches('/');
    (!normalized.is_empty()).then(|| normalized.to_string())
}

/// Reasoning configuration option discovered from models.dev.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReasoningOption {
    Toggle,
    Effort { values: Vec<String> },
    BudgetTokens { min: usize, max: Option<usize> },
}

/// Search all provider sections for reasoning options for a matching model ID.
/// Falls back to a basic toggle when `reasoning: true` but no explicit options.
pub fn catalog_model_reasoning_options(model_id: &str) -> Vec<ReasoningOption> {
    let root = load_catalog_json();
    let normalized = normalize_catalog_key(model_id);
    for section in root.as_object().into_iter().flat_map(|o| o.values()) {
        if let Some(model) = lookup_model_value_in_section(section, &normalized) {
            return reasoning_options_for_model(model);
        }
    }
    Vec::new()
}

pub fn catalog_model_reasoning_options_for_provider(
    provider_id: &str,
    model_id: &str,
) -> Option<Vec<ReasoningOption>> {
    let root = load_catalog_json();
    lookup_provider_section(&root, provider_id)
        .and_then(|section| {
            lookup_model_value_in_section(section, &normalize_catalog_key(model_id))
        })
        .map(reasoning_options_for_model)
}

fn reasoning_options_for_model(model: &serde_json::Value) -> Vec<ReasoningOption> {
    let options = parse_reasoning_options(&model["reasoning_options"]);
    if !options.is_empty() {
        return options;
    }
    // Fallback: model declares reasoning support but lacks explicit options.
    if model["reasoning"].as_bool() == Some(true) {
        return vec![ReasoningOption::Toggle];
    }
    Vec::new()
}

pub(crate) fn parse_reasoning_options(raw: &serde_json::Value) -> Vec<ReasoningOption> {
    let Some(arr) = raw.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|opt| match opt["type"].as_str()? {
            "toggle" => Some(ReasoningOption::Toggle),
            "effort" => {
                let values: Vec<String> = opt["values"]
                    .as_array()
                    .into_iter()
                    .flat_map(|v| v.iter().filter_map(|s| s.as_str().map(str::to_string)))
                    .collect();
                (!values.is_empty()).then_some(ReasoningOption::Effort { values })
            }
            "budget_tokens" => Some(ReasoningOption::BudgetTokens {
                min: opt["min"].as_u64().map(|v| v as usize).unwrap_or(0),
                max: opt["max"].as_u64().map(|v| v as usize),
            }),
            _ => None,
        })
        .collect()
}

/// Search all provider sections for a matching model ID.
fn lookup_model_in_json(root: &serde_json::Value, normalized: &str) -> Option<ModelCapacity> {
    for section in root.as_object()?.values() {
        if let Some(model) = lookup_model_value_in_section(section, normalized) {
            return capacity_for_model(model);
        }
    }
    None
}

fn lookup_provider_section<'a>(
    root: &'a serde_json::Value,
    provider_id: &str,
) -> Option<&'a serde_json::Value> {
    let normalized = normalize_catalog_key(provider_id);
    root.as_object()?.get(&normalized)
}

fn lookup_model_value_in_section<'a>(
    section: &'a serde_json::Value,
    normalized_model_id: &str,
) -> Option<&'a serde_json::Value> {
    section["models"].as_object()?.get(normalized_model_id)
}

fn capacity_for_model(model: &serde_json::Value) -> Option<ModelCapacity> {
    let limit = &model["limit"];
    let context = limit["context"].as_u64().map(|v| v as usize)?;
    let output = limit["output"].as_u64().map(|v| v as usize)?;
    let modalities = &model["modalities"];
    Some(ModelCapacity {
        context_window_tokens: context,
        max_completion_tokens: output,
        supports_vision: input_modalities_suggest_vision(modalities),
        supports_tool_call: model["tool_call"].as_bool().unwrap_or(false),
    })
}

pub fn catalog_model_capacity(model_id: &str) -> Option<ModelCapacity> {
    let root = load_catalog_json();
    let normalized = normalize_catalog_key(model_id);
    lookup_model_in_json(&root, &normalized)
}

pub fn catalog_model_capacity_for_provider(
    provider_id: &str,
    model_id: &str,
) -> Option<ModelCapacity> {
    let root = load_catalog_json();
    lookup_provider_section(&root, provider_id)
        .and_then(|section| {
            lookup_model_value_in_section(section, &normalize_catalog_key(model_id))
        })
        .and_then(capacity_for_model)
}

pub fn catalog_provider_ids_for_api_url(base_url: &str) -> Vec<String> {
    let root = load_catalog_json();
    provider_ids_for_api_url_in_json(&root, base_url)
}

fn provider_ids_for_api_url_in_json(root: &serde_json::Value, base_url: &str) -> Vec<String> {
    let Some(normalized_base_url) = normalize_catalog_api_url(base_url) else {
        return Vec::new();
    };
    let Some(providers) = root.as_object() else {
        return Vec::new();
    };
    let mut matches: Vec<String> = providers
        .iter()
        .filter_map(|(provider_id, section)| {
            let api_url = normalize_catalog_api_url(section["api"].as_str()?)?;
            (api_url == normalized_base_url).then(|| provider_id.clone())
        })
        .collect();
    matches.sort();
    matches
}

pub fn catalog_provider_has_model(provider_id: &str, model_id: &str) -> bool {
    let root = load_catalog_json();
    lookup_provider_section(&root, provider_id)
        .and_then(|section| {
            lookup_model_value_in_section(section, &normalize_catalog_key(model_id))
        })
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_api_url_matching_is_exact_after_trailing_slash_trim() {
        let root = serde_json::json!({
            "alpha": {
                "api": "https://example.com/v1",
                "models": {}
            },
            "beta": {
                "api": "https://example.com/v1/chat",
                "models": {}
            },
            "gamma": {
                "api": null,
                "models": {}
            }
        });

        assert_eq!(
            provider_ids_for_api_url_in_json(&root, "https://example.com/v1/"),
            vec!["alpha".to_string()]
        );
    }

    #[test]
    fn provider_api_url_matching_returns_duplicate_providers_sorted() {
        let root = serde_json::json!({
            "beta": {
                "api": "https://example.com/v1",
                "models": {}
            },
            "alpha": {
                "api": "https://example.com/v1/",
                "models": {}
            }
        });

        assert_eq!(
            provider_ids_for_api_url_in_json(&root, "https://example.com/v1"),
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }
}
