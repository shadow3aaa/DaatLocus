use std::{collections::HashMap, path::Path};

use miette::Diagnostic;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    context_budget::{
        DEFAULT_CONTEXT_WINDOW_TOKENS, DEFAULT_MAX_COMPLETION_TOKENS,
        DEFAULT_TOOL_OUTPUT_MAX_TOKENS,
    },
    i18n::Locale,
    persistence::{PersistenceFileMode, PersistenceStore, write_bytes_atomic},
    sandbox::StrongFilesystemSandboxMode,
};

const CONFIG_FILE_NAME: &str = "config.toml";
const DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT: i64 = 95;
const DEFAULT_STREAM_IDLE_TIMEOUT_SECS: u64 = 15 * 60;

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

pub fn redact_secret_text(text: &str, secret: &str) -> String {
    let secret = secret.trim();
    if secret.is_empty() {
        text.to_string()
    } else {
        text.replace(secret, "[redacted]")
    }
}

// ---------------------------------------------------------------------------
// Provider credentials
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
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
    OpenaiCodexOauth {
        #[serde(default)]
        base_url: Option<String>,
    },
    OpenaiCompatible {
        base_url: String,
        api_key: String,
        #[serde(default)]
        api_style: Option<String>,
    },
    Ollama {
        #[serde(default)]
        host: Option<String>,
        #[serde(default)]
        api_key: Option<String>,
        #[serde(default)]
        keep_alive: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Model capabilities
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ThinkingBudget(String);

impl ThinkingBudget {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ModelConfig {
    /// Key reference into Config.providers.
    pub provider: String,
    /// Model identifier sent to the provider API.
    pub model_id: String,
    pub temperature: f64,
    pub thinking_budget: Option<ThinkingBudget>,
    pub rpm: Option<u32>,
    pub request_timeout_secs: u64,
    pub stream_idle_timeout_secs: u64,
    pub context_window_tokens: usize,
    #[serde(default)]
    pub auto_compact_token_limit: Option<usize>,
    pub effective_context_window_percent: i64,
    pub max_completion_tokens: usize,
    pub tool_output_max_tokens: usize,
    /// Explicitly set whether this model accepts image attachments in messages.
    /// When `None`, vision support is inferred from the model name via the
    /// built-in catalog heuristic.  Set to `false` to unconditionally strip
    /// images before sending; set to `true` to skip runtime detection and
    /// always include images.
    #[serde(default)]
    pub supports_vision: Option<bool>,
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
            stream_idle_timeout_secs: DEFAULT_STREAM_IDLE_TIMEOUT_SECS,
            context_window_tokens: DEFAULT_CONTEXT_WINDOW_TOKENS,
            auto_compact_token_limit: None,
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            max_completion_tokens: DEFAULT_MAX_COMPLETION_TOKENS,
            tool_output_max_tokens: DEFAULT_TOOL_OUTPUT_MAX_TOKENS,
            supports_vision: None,
        }
    }
}

impl ModelConfig {
    pub fn thinking_budget(&self) -> Option<&ThinkingBudget> {
        self.thinking_budget.as_ref()
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
        self.stream_idle_timeout_secs
            .max(DEFAULT_STREAM_IDLE_TIMEOUT_SECS)
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

    pub fn reserved_output_tokens(&self) -> usize {
        self.effective_context_window_tokens()
            .saturating_sub(self.auto_compact_token_limit())
    }
}

// ---------------------------------------------------------------------------
// Judge configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
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

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Config {
    pub locale: Locale,
    /// Provider credential registry keyed by user-defined names.
    pub providers: HashMap<String, ProviderConfig>,
    /// Model definition registry keyed by user-defined names.
    pub models: HashMap<String, ModelConfig>,
    /// Main model name; key reference into models.
    pub main_model: String,
    /// Efficient model name; key reference into models.
    /// Default for non-main-loop operations such as judge and compaction.
    /// When not set explicitly, defaults to the same value as main_model for backward compatibility.
    pub efficient_model: String,
    pub daemon: DaemonConfig,
    pub judge: JudgeConfig,
    pub sandbox: SandboxConfig,
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
            efficient_model: "default".to_string(),
            daemon: DaemonConfig::default(),
            judge: JudgeConfig::default(),
            sandbox: SandboxConfig::default(),
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

    /// Return the judge model config, falling back through efficient model then to main model.
    pub fn judge_model_config(&self) -> &ModelConfig {
        let key = self.judge.model.as_deref().unwrap_or(&self.efficient_model);
        self.models
            .get(key)
            .unwrap_or_else(|| panic!("judge model '{}' not found in models", key))
    }

    /// Return the efficient model config. Missing keys panic because startup validation should catch them.
    pub fn efficient_model_config(&self) -> &ModelConfig {
        self.models.get(&self.efficient_model).unwrap_or_else(|| {
            panic!(
                "efficient_model '{}' not found in models",
                self.efficient_model
            )
        })
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
                ProviderConfig::OpenaiCodexOauth { .. } => {}
                ProviderConfig::Ollama { api_key, .. } => {
                    if let Some(key) = api_key {
                        push_secret_env_ref(&mut vars, key);
                    }
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

        let efficient = self.models.get(&self.efficient_model).ok_or_else(|| {
            format!(
                "efficient_model '{}' not found in [models]",
                self.efficient_model
            )
        })?;
        self.providers.get(&efficient.provider).ok_or_else(|| {
            format!(
                "efficient_model '{}' references unknown provider '{}'",
                self.efficient_model, efficient.provider
            )
        })?;

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
    } else {
        trimmed.strip_prefix('$')?
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

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct DaemonConfig {
    pub port: u16,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self { port: 53825 }
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SandboxConfig {
    pub enabled: bool,
    pub strong_filesystem: StrongFilesystemSandboxMode,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            strong_filesystem: StrongFilesystemSandboxMode::Off,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
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
    PersistenceStore::runtime()
        .await
        .config_file(CONFIG_FILE_NAME)
        .exists()
}

/// Serialize Config and write it to config.toml.
pub async fn write_config(config: &Config) -> Result<(), ConfigError> {
    let config_path = PersistenceStore::runtime()
        .await
        .config_file(CONFIG_FILE_NAME);
    write_config_to_path(&config_path, config).await
}

async fn write_config_to_path(config_path: &Path, config: &Config) -> Result<(), ConfigError> {
    let toml_str =
        toml::to_string_pretty(config).map_err(|e| ConfigError::Syntax(e.to_string()))?;
    write_bytes_atomic(
        config_path.to_path_buf(),
        toml_str.into_bytes(),
        PersistenceFileMode::Private,
    )
    .await
    .map_err(ConfigError::IO)?;
    Ok(())
}

/// Load config.toml. Missing files return an IO error; defaults are not auto-created.
pub async fn load_config() -> Result<Config, ConfigError> {
    let config_path = PersistenceStore::runtime()
        .await
        .config_file(CONFIG_FILE_NAME);

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
    use super::{
        Config, DEFAULT_STREAM_IDLE_TIMEOUT_SECS, ModelConfig, ProviderConfig, ThinkingBudget,
        normalize_provider_base_url, resolve_env_reference,
    };
    use crate::sandbox::StrongFilesystemSandboxMode;

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
                api_style: None,
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

    #[test]
    fn redact_secret_text_replaces_secret_values() {
        assert_eq!(
            super::redact_secret_text("Bearer secret-token", "secret-token"),
            "Bearer [redacted]"
        );
        assert_eq!(super::redact_secret_text("unchanged", ""), "unchanged");
    }

    #[test]
    fn sandbox_config_defaults_to_no_strong_filesystem_backend() {
        let config: Config = toml::from_str(
            r#"
main_model = "default"

[providers.openai]
type = "openai"
api_key = "test"

[models.default]
provider = "openai"
model_id = "gpt-4.1"
"#,
        )
        .expect("parse config");

        assert!(config.sandbox.enabled);
        assert_eq!(
            config.sandbox.strong_filesystem,
            StrongFilesystemSandboxMode::Off
        );
    }

    #[test]
    fn sandbox_config_parses_strong_filesystem_mode() {
        let config: Config = toml::from_str(
            r#"
main_model = "default"

[providers.openai]
type = "openai"
api_key = "test"

[models.default]
provider = "openai"
model_id = "gpt-4.1"

[sandbox]
strong_filesystem = "required"
"#,
        )
        .expect("parse config");

        assert_eq!(
            config.sandbox.strong_filesystem,
            StrongFilesystemSandboxMode::Required
        );
    }

    #[test]
    fn sandbox_config_parses_enabled_flag() {
        let config: Config = toml::from_str(
            r#"
main_model = "default"

[providers.openai]
type = "openai"
api_key = "test"

[models.default]
provider = "openai"
model_id = "gpt-4.1"

[sandbox]
enabled = false
"#,
        )
        .expect("parse config");

        assert!(!config.sandbox.enabled);
        assert_eq!(
            config.sandbox.strong_filesystem,
            StrongFilesystemSandboxMode::Off
        );
    }

    #[test]
    fn codex_oauth_config_ignores_auth_file_field() {
        let config: Config = toml::from_str(
            r#"
main_model = "default"

[providers.codex-oauth]
type = "openai-codex-oauth"
auth_file = 'C:\Users\example\.daat-locus\config\old.json'

[models.default]
provider = "codex-oauth"
model_id = "gpt-5.5"
"#,
        )
        .expect("parse config");

        assert!(matches!(
            config.providers["codex-oauth"],
            ProviderConfig::OpenaiCodexOauth { .. }
        ));
        let serialized = toml::to_string_pretty(&config).expect("serialize config");
        assert!(!serialized.contains("auth_file"));
    }

    #[test]
    fn thinking_budget_preserves_configured_string() {
        let config: Config = toml::from_str(
            r#"
main_model = "default"

[providers.openai]
type = "openai"
api_key = "test"

[models.default]
provider = "openai"
model_id = "gpt-4.1"
thinking_budget = "max"
"#,
        )
        .expect("parse config");

        assert_eq!(
            config.models["default"]
                .thinking_budget()
                .map(ThinkingBudget::as_str),
            Some("max")
        );
    }

    #[test]
    fn thinking_budget_accepts_custom_values() {
        let config: Config = toml::from_str(
            r#"
main_model = "default"

[providers.openai]
type = "openai"
api_key = "test"

[models.default]
provider = "openai"
model_id = "gpt-4.1"
thinking_budget = "xhigh"
"#,
        )
        .expect("parse config");

        assert_eq!(
            config.models["default"]
                .thinking_budget()
                .map(ThinkingBudget::as_str),
            Some("xhigh")
        );
    }

    #[test]
    fn reserved_output_tokens_tracks_effective_window_headroom() {
        let model = ModelConfig {
            context_window_tokens: 272_000,
            effective_context_window_percent: 95,
            max_completion_tokens: 128_000,
            ..Default::default()
        };

        assert_eq!(model.effective_context_window_tokens(), 258_400);
        assert_eq!(model.auto_compact_token_limit(), 244_800);
        assert_eq!(model.reserved_output_tokens(), 13_600);
        assert_eq!(model.max_completion_tokens(), 128_000);
    }

    #[test]
    fn reserved_output_tokens_can_be_zero_when_auto_limit_matches_effective_window() {
        let model = ModelConfig {
            context_window_tokens: 200_000,
            effective_context_window_percent: 50,
            ..Default::default()
        };

        assert_eq!(model.effective_context_window_tokens(), 100_000);
        assert_eq!(model.auto_compact_token_limit(), 100_000);
        assert_eq!(model.reserved_output_tokens(), 0);
    }

    #[test]
    fn stream_idle_timeout_has_long_reasoning_floor() {
        let model = ModelConfig {
            stream_idle_timeout_secs: 45,
            ..Default::default()
        };

        assert_eq!(
            model.stream_idle_timeout_secs(),
            DEFAULT_STREAM_IDLE_TIMEOUT_SECS
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_config_sets_private_permissions_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("config.toml");

        super::write_config_to_path(&path, &Config::default())
            .await
            .expect("write config");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
