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
    let normalized = model_id.trim().to_ascii_lowercase();
    for section in root.as_object().into_iter().flat_map(|o| o.values()) {
        if let Some(models) = section["models"].as_object()
            && let Some(model) = models.get(&normalized)
        {
            let options = parse_reasoning_options(&model["reasoning_options"]);
            if !options.is_empty() {
                return options;
            }
            // Fallback: model declares reasoning support but lacks explicit options
            if model["reasoning"].as_bool() == Some(true) {
                return vec![ReasoningOption::Toggle];
            }
            return Vec::new();
        }
    }
    Vec::new()
}

fn parse_reasoning_options(raw: &serde_json::Value) -> Vec<ReasoningOption> {
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
        let models = section["models"].as_object()?;
        if let Some(model) = models.get(normalized) {
            let limit = &model["limit"];
            let context = limit["context"].as_u64().map(|v| v as usize)?;
            let output = limit["output"].as_u64().map(|v| v as usize)?;
            let modalities = &model["modalities"];
            return Some(ModelCapacity {
                context_window_tokens: context,
                max_completion_tokens: output,
                supports_vision: input_modalities_suggest_vision(modalities),
                supports_tool_call: model["tool_call"].as_bool().unwrap_or(false),
            });
        }
    }
    None
}

pub fn catalog_model_capacity(model_id: &str) -> Option<ModelCapacity> {
    let root = load_catalog_json();
    let normalized = model_id.trim().to_ascii_lowercase();
    lookup_model_in_json(&root, &normalized)
}

pub fn fetch_models_dev_capacity(model_id: &str) -> Option<ModelCapacity> {
    let response = reqwest::blocking::get("https://models.dev/api.json").ok()?;
    let text = response.text().ok()?;
    let root: serde_json::Value = serde_json::from_str(&text).ok()?;
    let normalized = model_id.trim().to_ascii_lowercase();
    lookup_model_in_json(&root, &normalized)
}
