use std::collections::HashMap;

use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    context_budget::{
        DEFAULT_AUTO_COMPACT_THRESHOLD_TOKENS, DEFAULT_CONTEXT_WINDOW_TOKENS,
        DEFAULT_MAX_COMPLETION_TOKENS, DEFAULT_TOOL_OUTPUT_MAX_TOKENS,
    },
    daat_locus_paths::daat_locus_paths,
    i18n::Locale,
};

const CONFIG_FILE_NAME: &str = "config.toml";
const DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT: i64 = 95;

pub fn normalize_provider_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

pub fn resolve_env_reference(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(name) = env_ref_name(trimmed) {
        std::env::var(name).unwrap_or_else(|_| trimmed.to_string())
    } else {
        trimmed.to_string()
    }
}

// ---------------------------------------------------------------------------
// Provider credentials
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ProviderConfig {
    Openai {
        api_key: String,
        #[serde(default)]
        base_url: Option<String>,
    },
    GithubCopilot {
        github_token: String,
    },
    OpenaiCompatible {
        base_url: String,
        api_key: String,
    },
}

// ---------------------------------------------------------------------------
// Model capabilities
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    /// Key reference into Config.providers.
    pub provider: String,
    /// Model identifier sent to the provider API.
    pub model_id: String,
    pub temperature: f64,
    pub thinking_budget: Option<String>,
    pub rpm: Option<u32>,
    pub request_timeout_secs: u64,
    pub stream_idle_timeout_secs: u64,
    pub context_window_tokens: usize,
    #[serde(default)]
    pub auto_compact_token_limit: Option<usize>,
    pub effective_context_window_percent: i64,
    pub max_completion_tokens: usize,
    pub tool_output_max_tokens: usize,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: "openai".to_string(),
            model_id: "gpt-4.1".to_string(),
            temperature: 1.0,
            thinking_budget: None,
            rpm: None,
            request_timeout_secs: 300,
            stream_idle_timeout_secs: 45,
            context_window_tokens: DEFAULT_CONTEXT_WINDOW_TOKENS,
            auto_compact_token_limit: Some(DEFAULT_AUTO_COMPACT_THRESHOLD_TOKENS),
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            max_completion_tokens: DEFAULT_MAX_COMPLETION_TOKENS,
            tool_output_max_tokens: DEFAULT_TOOL_OUTPUT_MAX_TOKENS,
        }
    }
}

impl ModelConfig {
    pub fn thinking_budget(&self) -> Option<String> {
        let budget = self.thinking_budget.as_deref()?.trim();
        if budget.is_empty() {
            None
        } else {
            Some(budget.to_string())
        }
    }

    pub fn rpm(&self) -> Option<usize> {
        self.rpm
            .and_then(|r| usize::try_from(r).ok())
            .filter(|r| *r > 0)
    }

    pub fn request_timeout_secs(&self) -> u64 {
        self.request_timeout_secs.max(1)
    }

    pub fn stream_idle_timeout_secs(&self) -> u64 {
        self.stream_idle_timeout_secs.max(1)
    }

    pub fn context_window_tokens(&self) -> usize {
        self.context_window_tokens.max(1)
    }

    pub fn effective_context_window_percent(&self) -> i64 {
        self.effective_context_window_percent.clamp(1, 100)
    }

    pub fn effective_context_window_tokens(&self) -> usize {
        let cw = self.context_window_tokens();
        let effective =
            (cw as u128).saturating_mul(self.effective_context_window_percent() as u128) / 100;
        usize::try_from(effective).unwrap_or(cw).clamp(1, cw)
    }

    pub fn auto_compact_token_limit(&self) -> usize {
        let cw = self.context_window_tokens();
        let default_limit = usize::try_from((cw as u128).saturating_mul(9) / 10).unwrap_or(cw);
        let configured = self.auto_compact_token_limit.unwrap_or(default_limit);
        configured
            .min(default_limit.max(1))
            .min(self.effective_context_window_tokens())
            .max(1)
    }

    pub fn max_completion_tokens(&self) -> usize {
        self.max_completion_tokens
            .clamp(1, self.context_window_tokens())
    }
}

// ---------------------------------------------------------------------------
// Judge configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JudgeConfig {
    pub enabled: bool,
    /// None = use main_model.
    pub model: Option<String>,
    pub max_pairwise_candidates: usize,
    pub max_pairwise_cases: usize,
}

impl Default for JudgeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            max_pairwise_candidates: 4,
            max_pairwise_cases: 4,
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub locale: Locale,
    /// Provider credential registry keyed by user-defined names.
    pub providers: HashMap<String, ProviderConfig>,
    /// Model definition registry keyed by user-defined names.
    pub models: HashMap<String, ModelConfig>,
    /// Main model name; key reference into models.
    pub main_model: String,
    pub daemon: DaemonConfig,
    pub judge: JudgeConfig,
    pub hindsight: HindsightConfig,
    pub telegram: TelegramConfig,
}

impl Default for Config {
    fn default() -> Self {
        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            ProviderConfig::Openai {
                api_key: "your-api-key".to_string(),
                base_url: None,
            },
        );

        let mut models = HashMap::new();
        models.insert("default".to_string(), ModelConfig::default());

        Self {
            providers,
            models,
            locale: Locale::default(),
            main_model: "default".to_string(),
            daemon: DaemonConfig::default(),
            judge: JudgeConfig::default(),
            hindsight: HindsightConfig::default(),
            telegram: TelegramConfig::default(),
        }
    }
}

impl Config {
    /// Return the main model config. Missing keys panic because startup validation should catch them.
    pub fn main_model_config(&self) -> &ModelConfig {
        self.models
            .get(&self.main_model)
            .unwrap_or_else(|| panic!("main_model '{}' not found in models", self.main_model))
    }

    /// Return the judge model config, falling back to the main model when unspecified.
    pub fn judge_model_config(&self) -> &ModelConfig {
        let key = self.judge.model.as_deref().unwrap_or(&self.main_model);
        self.models
            .get(key)
            .unwrap_or_else(|| panic!("judge model '{}' not found in models", key))
    }

    /// Return the hindsight model config, falling back to the main model when unspecified.
    pub fn hindsight_model_config(&self) -> &ModelConfig {
        let key = self.hindsight.model.as_deref().unwrap_or(&self.main_model);
        self.models
            .get(key)
            .unwrap_or_else(|| panic!("hindsight model '{}' not found in models", key))
    }

    /// Return the provider config used by hindsight.
    pub fn hindsight_provider_config(&self) -> &ProviderConfig {
        let provider_key = &self.hindsight_model_config().provider;
        self.providers
            .get(provider_key)
            .unwrap_or_else(|| panic!("provider '{}' not found in providers", provider_key))
    }

    pub fn protected_secret_env_vars(&self) -> Vec<String> {
        let mut vars = Vec::new();
        for provider in self.providers.values() {
            match provider {
                ProviderConfig::Openai { api_key, .. }
                | ProviderConfig::OpenaiCompatible { api_key, .. } => {
                    push_secret_env_ref(&mut vars, api_key);
                }
                ProviderConfig::GithubCopilot { github_token } => {
                    push_secret_env_ref(&mut vars, github_token);
                }
            }
        }
        push_secret_env_ref(&mut vars, &self.telegram.bot_token);
        vars.sort_by_key(|name| name.to_ascii_uppercase());
        vars.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        vars
    }

    /// Validate provider and model references.
    pub fn validate(&self) -> Result<(), String> {
        if self.daemon.port == 0 {
            return Err("daemon.port must be greater than 0".to_string());
        }

        let main = self
            .models
            .get(&self.main_model)
            .ok_or_else(|| format!("main_model '{}' not found in [models]", self.main_model))?;
        self.providers.get(&main.provider).ok_or_else(|| {
            format!(
                "main_model '{}' references unknown provider '{}'",
                self.main_model, main.provider
            )
        })?;

        if let Some(judge_model_key) = &self.judge.model {
            let judge = self.models.get(judge_model_key).ok_or_else(|| {
                format!("judge.model '{}' not found in [models]", judge_model_key)
            })?;
            self.providers.get(&judge.provider).ok_or_else(|| {
                format!(
                    "judge.model '{}' references unknown provider '{}'",
                    judge_model_key, judge.provider
                )
            })?;
        }

        if let Some(hindsight_model_key) = &self.hindsight.model {
            let h = self.models.get(hindsight_model_key).ok_or_else(|| {
                format!(
                    "hindsight.model '{}' not found in [models]",
                    hindsight_model_key
                )
            })?;
            self.providers.get(&h.provider).ok_or_else(|| {
                format!(
                    "hindsight.model '{}' references unknown provider '{}'",
                    hindsight_model_key, h.provider
                )
            })?;
        }

        Ok(())
    }
}

fn push_secret_env_ref(vars: &mut Vec<String>, value: &str) {
    if let Some(name) = env_ref_name(value) {
        vars.push(name);
    }
}

fn env_ref_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let name = if let Some(inner) = trimmed
        .strip_prefix("${")
        .and_then(|inner| inner.strip_suffix('}'))
    {
        inner
    } else if let Some(inner) = trimmed.strip_prefix("env:") {
        inner
    } else if let Some(inner) = trimmed.strip_prefix('$') {
        inner
    } else {
        return None;
    };
    let name = name.trim();
    if is_valid_env_ref_name(name) {
        Some(name.to_string())
    } else {
        None
    }
}

fn is_valid_env_ref_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first == '_' || first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

// ---------------------------------------------------------------------------
// Other sub-configs
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub port: u16,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self { port: 53825 }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HindsightConfig {
    pub namespace: String,
    pub bank_id: String,
    pub request_timeout_secs: u64,
    /// Pinned version passed to `uvx hindsight-embed@<version>`.
    /// Empty string means "latest".
    pub embed_version: String,
    /// Profile name used by hindsight-embed. Defaults to "daat-locus".
    pub profile: String,
    /// Port the managed daemon listens on.
    pub port: u16,
    /// Model used for hindsight LLM operations (reflect/retain).
    /// None = use main_model.
    pub model: Option<String>,
}

impl Default for HindsightConfig {
    fn default() -> Self {
        Self {
            namespace: "default".to_string(),
            bank_id: "daat-locus".to_string(),
            request_timeout_secs: 180,
            embed_version: String::new(),
            profile: "daat-locus".to_string(),
            port: 8888,
            model: None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub bot_token: String,
    pub poll_timeout_secs: u64,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bot_token: "your-telegram-bot-token".to_string(),
            poll_timeout_secs: 30,
        }
    }
}

impl TelegramConfig {
    pub fn has_real_credentials(&self) -> bool {
        !self.bot_token.trim().is_empty() && self.bot_token != "your-telegram-bot-token"
    }
}

// ---------------------------------------------------------------------------
// Errors and loading
// ---------------------------------------------------------------------------

#[derive(Error, Debug, Diagnostic)]
pub enum ConfigError {
    #[error("failed to read config file: {0}")]
    IO(#[from] std::io::Error),
    #[error("{0}")]
    #[diagnostic(code(config::syntax_error))]
    Syntax(String),
    #[error("config validation failed: {0}")]
    #[diagnostic(code(config::validation_error))]
    Validation(String),
}

/// Return whether config.toml exists.
pub async fn config_file_exists() -> bool {
    daat_locus_paths()
        .await
        .config_file(CONFIG_FILE_NAME)
        .exists()
}

/// Serialize Config and write it to config.toml.
pub async fn write_config(config: &Config) -> Result<(), ConfigError> {
    let config_path = daat_locus_paths().await.config_file(CONFIG_FILE_NAME);
    let toml_str =
        toml::to_string_pretty(config).map_err(|e| ConfigError::Syntax(e.to_string()))?;
    tokio::fs::write(&config_path, toml_str)
        .await
        .map_err(ConfigError::IO)?;
    Ok(())
}

/// Load config.toml. Missing files return an IO error; defaults are not auto-created.
pub async fn load_config() -> Result<Config, ConfigError> {
    let config_path = daat_locus_paths().await.config_file(CONFIG_FILE_NAME);

    let content = tokio::fs::read_to_string(config_path)
        .await
        .map_err(ConfigError::IO)?;

    let config: Config =
        toml::from_str(&content).map_err(|e| ConfigError::Syntax(e.to_string()))?;

    config.validate().map_err(ConfigError::Validation)?;

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::{Config, ProviderConfig, normalize_provider_base_url, resolve_env_reference};

    struct EnvOverride {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvOverride {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvOverride {
        fn drop(&mut self) {
            match &self.previous {
                Some(previous) => unsafe {
                    std::env::set_var(self.key, previous);
                },
                None => unsafe {
                    std::env::remove_var(self.key);
                },
            }
        }
    }

    #[test]
    fn normalize_provider_base_url_only_trims_whitespace_and_slashes() {
        assert_eq!(
            normalize_provider_base_url("https://api.deepseek.com/v1/"),
            "https://api.deepseek.com/v1"
        );
        assert_eq!(
            normalize_provider_base_url(" http://localhost:11434/v1 "),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            normalize_provider_base_url("https://example.com/proxy/v1"),
            "https://example.com/proxy/v1"
        );
    }

    #[test]
    fn protected_secret_env_vars_are_collected_from_config_refs() {
        let mut config = Config::default();
        config.providers.insert(
            "openai".to_string(),
            ProviderConfig::Openai {
                api_key: "${OPENAI_API_KEY}".to_string(),
                base_url: None,
            },
        );
        config.providers.insert(
            "compatible".to_string(),
            ProviderConfig::OpenaiCompatible {
                base_url: "https://example.com/v1".to_string(),
                api_key: "env:COMPATIBLE_TOKEN".to_string(),
            },
        );
        config.providers.insert(
            "copilot".to_string(),
            ProviderConfig::GithubCopilot {
                github_token: "$GITHUB_TOKEN".to_string(),
            },
        );
        config.telegram.bot_token = "$TELEGRAM_BOT_TOKEN".to_string();

        assert_eq!(
            config.protected_secret_env_vars(),
            vec![
                "COMPATIBLE_TOKEN",
                "GITHUB_TOKEN",
                "OPENAI_API_KEY",
                "TELEGRAM_BOT_TOKEN",
            ]
        );
    }

    #[test]
    fn resolve_env_reference_supports_all_config_ref_forms() {
        let _env = EnvOverride::set("DAAT_LOCUS_TEST_SECRET_REF", "resolved-secret");

        assert_eq!(
            resolve_env_reference("$DAAT_LOCUS_TEST_SECRET_REF"),
            "resolved-secret"
        );
        assert_eq!(
            resolve_env_reference("${DAAT_LOCUS_TEST_SECRET_REF}"),
            "resolved-secret"
        );
        assert_eq!(
            resolve_env_reference("env:DAAT_LOCUS_TEST_SECRET_REF"),
            "resolved-secret"
        );
        assert_eq!(
            resolve_env_reference("env:DAAT_LOCUS_TEST_MISSING_SECRET"),
            "env:DAAT_LOCUS_TEST_MISSING_SECRET"
        );
        assert_eq!(resolve_env_reference("literal-secret"), "literal-secret");
    }
}
