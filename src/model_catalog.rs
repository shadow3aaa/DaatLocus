//! The current catalog is downloaded into Cargo's `OUT_DIR` by `build.rs` and
//! compiled into the binary. A newer runtime cache can override that snapshot.

use std::{
    sync::{Arc, OnceLock, RwLock},
    time::{Duration, Instant},
};

use crate::daat_locus_paths::daat_locus_paths;
#[cfg(not(test))]
use crate::daat_locus_paths::daat_locus_paths_sync;

const COMPILED_API_JSON: &str = include_str!(concat!(env!("OUT_DIR"), "/models-dev-api.json"));

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

pub const fn conservative_model_capacity() -> ModelCapacity {
    ModelCapacity {
        context_window_tokens: CONSERVATIVE_CONTEXT_WINDOW_TOKENS,
        max_completion_tokens: CONSERVATIVE_MAX_COMPLETION_TOKENS,
        supports_vision: false,
        supports_tool_call: true,
    }
}

/// Load the best available model catalog: cached file > compiled snapshot.
///
/// The parsed tree lives behind `Arc` in a `OnceLock`. Callers clone the `Arc`
/// (reference count) instead of deep-copying the `serde_json::Value`.
fn catalog_store() -> &'static RwLock<Arc<serde_json::Value>> {
    static CATALOG: OnceLock<RwLock<Arc<serde_json::Value>>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        #[cfg(not(test))]
        {
            let paths = daat_locus_paths_sync();
            if let Ok(text) = std::fs::read_to_string(paths.models_dev_cache())
                && let Ok(root) = validated_catalog(&text)
            {
                return RwLock::new(Arc::new(root));
            }
        }
        RwLock::new(Arc::new(
            validated_catalog(COMPILED_API_JSON).expect("valid compiled models.dev catalog"),
        ))
    })
}

fn load_catalog_json() -> Arc<serde_json::Value> {
    Arc::clone(&catalog_store().read().expect("catalog lock"))
}

fn validated_catalog(text: &str) -> Result<serde_json::Value, String> {
    let root: serde_json::Value =
        serde_json::from_str(text).map_err(|_| "Invalid model catalog")?;
    if !root.as_object().is_some_and(|providers| {
        providers.values().any(|provider| {
            provider["models"].as_object().is_some_and(|models| {
                models
                    .values()
                    .any(|model| capacity_for_model(model).is_some())
            })
        })
    }) {
        return Err("Empty model catalog".into());
    }
    Ok(root)
}

/// Refresh the local cache from models.dev. Returns Ok if cache was written.
pub async fn refresh_models_dev_cache() -> Result<(), String> {
    let mut response = reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
        .map_err(|_| "Cannot create catalog client")?
        .get("https://models.dev/api.json")
        .send()
        .await
        .map_err(|_| "Cannot fetch model catalog")?
        .error_for_status()
        .map_err(|_| "Catalog service unavailable")?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Cannot read model catalog")?
    {
        if bytes.len() + chunk.len() > 16 * 1024 * 1024 {
            return Err("Model catalog is too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let root = validated_catalog(
        std::str::from_utf8(&bytes).map_err(|_| "Invalid model catalog encoding")?,
    )?;
    let paths = daat_locus_paths().await;
    let cache_path = paths.models_dev_cache();
    if let Some(parent) = cache_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("create cache dir failed: {e}"))?;
    }
    crate::persistence::write_bytes_atomic(
        cache_path,
        bytes,
        crate::persistence::PersistenceFileMode::Default,
    )
    .await
    .map_err(|e| format!("write cache file failed: {e}"))?;
    *catalog_store().write().expect("catalog lock") = Arc::new(root);
    Ok(())
}

/// Refresh once a day, retrying failures after a short backoff. Existing readers
/// retain the last valid tree; a failed download never replaces it or the cache.
pub async fn refresh_models_dev_cache_if_stale() {
    static ATTEMPT: tokio::sync::Mutex<Option<(Instant, Duration)>> =
        tokio::sync::Mutex::const_new(None);
    let mut attempt = ATTEMPT.lock().await;
    if attempt
        .as_ref()
        .is_some_and(|(at, delay)| at.elapsed() < *delay)
    {
        return;
    }
    let paths = daat_locus_paths().await;
    if attempt.is_none()
        && tokio::fs::metadata(paths.models_dev_cache())
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age < Duration::from_secs(86400))
        && tokio::fs::read_to_string(paths.models_dev_cache())
            .await
            .ok()
            .is_some_and(|text| validated_catalog(&text).is_ok())
    {
        *attempt = Some((Instant::now(), Duration::from_secs(86400)));
        return;
    }
    let delay = if refresh_models_dev_cache().await.is_ok() {
        86400
    } else {
        300
    };
    *attempt = Some((Instant::now(), Duration::from_secs(delay)));
}

pub fn catalog_model_ids(provider_ids: &[String]) -> Vec<String> {
    let root = load_catalog_json();
    let mut ids = std::collections::BTreeSet::new();
    if let Some(providers) = root.as_object() {
        for (id, provider) in providers {
            if !provider_ids.is_empty() && !provider_ids.contains(id) {
                continue;
            }
            if let Some(models) = provider["models"].as_object() {
                ids.extend(models.keys().cloned());
            }
        }
    }
    ids.into_iter().take(10000).collect()
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningOption {
    Toggle,
    Effort { values: Vec<String> },
    BudgetTokens { min: usize, max: Option<usize> },
}

/// Search all provider sections for reasoning options for a matching model ID.
/// Falls back to a basic toggle when `reasoning: true` but no explicit options.
pub fn catalog_model_reasoning_options(model_id: &str) -> Vec<ReasoningOption> {
    let root = load_catalog_json();
    reasoning_options_in_json(&root, &normalize_catalog_key(model_id))
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

fn reasoning_options_in_json(
    root: &serde_json::Value,
    normalized_model_id: &str,
) -> Vec<ReasoningOption> {
    root.as_object()
        .into_iter()
        .flat_map(|providers| providers.values())
        .filter_map(|section| {
            lookup_model_value_in_section(section, normalized_model_id)
                .map(reasoning_options_candidate)
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, options)| options)
        .unwrap_or_default()
}

fn reasoning_options_candidate(model: &serde_json::Value) -> (usize, Vec<ReasoningOption>) {
    let options = parse_reasoning_options(&model["reasoning_options"]);
    if !options.is_empty() {
        let score = options
            .iter()
            .map(|option| match option {
                ReasoningOption::Toggle => 100,
                ReasoningOption::BudgetTokens { .. } => 200,
                ReasoningOption::Effort { values } => 300 + values.len(),
            })
            .max()
            .unwrap_or_default();
        return (score, options);
    }
    if model["reasoning"].as_bool() == Some(true) {
        return (1, vec![ReasoningOption::Toggle]);
    }
    (0, Vec::new())
}

pub fn parse_reasoning_options(raw: &serde_json::Value) -> Vec<ReasoningOption> {
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
                min: opt["min"]
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(0),
                max: opt["max"]
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok()),
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
    let context = usize::try_from(limit["context"].as_u64()?).ok()?;
    let output = usize::try_from(limit["output"].as_u64()?).ok()?;
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
    fn invalid_or_empty_catalog_cannot_replace_the_snapshot() {
        for invalid in [
            "not json",
            "null",
            "{}",
            r#"{"error":"unavailable"}"#,
            r#"{"openai":{"models":{}}}"#,
        ] {
            assert!(validated_catalog(invalid).is_err());
        }
        assert!(validated_catalog(COMPILED_API_JSON).is_ok());
        assert!(!catalog_model_ids(&["openai".into()]).is_empty());
    }

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

    #[test]
    fn global_reasoning_lookup_prefers_rich_explicit_options_over_toggle_fallback() {
        let root = serde_json::json!({
            "toggle-only": {
                "models": {
                    "gpt-5.5": {
                        "reasoning": true
                    }
                }
            },
            "partial-effort": {
                "models": {
                    "gpt-5.5": {
                        "reasoning": true,
                        "reasoning_options": [{
                            "type": "effort",
                            "values": ["low", "medium", "high"]
                        }]
                    }
                }
            },
            "full-effort": {
                "models": {
                    "gpt-5.5": {
                        "reasoning": true,
                        "reasoning_options": [{
                            "type": "effort",
                            "values": ["none", "low", "medium", "high", "xhigh"]
                        }]
                    }
                }
            }
        });

        assert_eq!(
            reasoning_options_in_json(&root, "gpt-5.5"),
            vec![ReasoningOption::Effort {
                values: ["none", "low", "medium", "high", "xhigh"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            }]
        );
    }

    #[test]
    fn compiled_catalog_parses_typical_openai_model() {
        let capacity = catalog_model_capacity_for_provider("openai", "gpt-4o")
            .expect("compiled catalog should contain openai/gpt-4o");

        assert!(capacity.context_window_tokens > 0);
        assert!(capacity.max_completion_tokens > 0);
        assert!(capacity.supports_vision);
        assert!(capacity.supports_tool_call);
    }
}
