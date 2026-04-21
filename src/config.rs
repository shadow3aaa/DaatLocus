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
};

const CONFIG_FILE_NAME: &str = "config.toml";
const DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT: i64 = 95;

// ---------------------------------------------------------------------------
// Provider 凭据层
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

impl ProviderConfig {
    /// 返回该 provider 对应的 base_url（不含路径）。
    pub fn base_url(&self) -> &str {
        match self {
            ProviderConfig::Openai { base_url, .. } => {
                base_url.as_deref().unwrap_or("https://api.openai.com")
            }
            ProviderConfig::GithubCopilot { .. } => {
                // Copilot 的 base_url 由 token 交换后动态获取；这里给出默认值供初始化使用。
                "https://api.githubcopilot.com"
            }
            ProviderConfig::OpenaiCompatible { base_url, .. } => base_url.as_str(),
        }
    }

    /// 返回静态 api_key（Copilot 返回 None，因为需要动态交换）。
    pub fn static_api_key(&self) -> Option<&str> {
        match self {
            ProviderConfig::Openai { api_key, .. } => Some(api_key.as_str()),
            ProviderConfig::GithubCopilot { .. } => None,
            ProviderConfig::OpenaiCompatible { api_key, .. } => Some(api_key.as_str()),
        }
    }
}

// ---------------------------------------------------------------------------
// Model 能力层
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    /// 引用 Config.providers 中的 key
    pub provider: String,
    /// 发送给 API 的 model 标识符
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
// Judge 配置
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JudgeConfig {
    pub enabled: bool,
    /// None = 使用 main_model
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
// 顶层 Config
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Provider 凭据注册表，key 为用户自定义名称
    pub providers: HashMap<String, ProviderConfig>,
    /// Model 定义注册表，key 为用户自定义名称
    pub models: HashMap<String, ModelConfig>,
    /// 主模型名称，引用 models 中的 key
    pub main_model: String,
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
            main_model: "default".to_string(),
            judge: JudgeConfig::default(),
            hindsight: HindsightConfig::default(),
            telegram: TelegramConfig::default(),
        }
    }
}

impl Config {
    /// 返回主模型配置，若 key 不存在则 panic（应在启动时校验）。
    pub fn main_model_config(&self) -> &ModelConfig {
        self.models
            .get(&self.main_model)
            .unwrap_or_else(|| panic!("main_model '{}' not found in models", self.main_model))
    }

    /// 返回主模型对应的 provider 配置。
    pub fn main_provider_config(&self) -> &ProviderConfig {
        let provider_key = &self.main_model_config().provider;
        self.providers
            .get(provider_key)
            .unwrap_or_else(|| panic!("provider '{}' not found in providers", provider_key))
    }

    /// 返回 judge 使用的模型配置（未指定时退回主模型）。
    pub fn judge_model_config(&self) -> &ModelConfig {
        let key = self.judge.model.as_deref().unwrap_or(&self.main_model);
        self.models
            .get(key)
            .unwrap_or_else(|| panic!("judge model '{}' not found in models", key))
    }

    /// 返回 judge 使用的 provider 配置。
    pub fn judge_provider_config(&self) -> &ProviderConfig {
        let provider_key = &self.judge_model_config().provider;
        self.providers
            .get(provider_key)
            .unwrap_or_else(|| panic!("provider '{}' not found in providers", provider_key))
    }

    /// 校验 main_model 和 judge model 引用的 provider/model 都存在。
    pub fn validate(&self) -> Result<(), String> {
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

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 其他子配置（不变）
// ---------------------------------------------------------------------------

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
// 错误 & 加载
// ---------------------------------------------------------------------------

#[derive(Error, Debug, Diagnostic)]
pub enum ConfigError {
    #[error("配置文件读取失败: {0}")]
    IO(#[from] std::io::Error),
    #[error("{0}")]
    #[diagnostic(code(config::syntax_error))]
    Syntax(String),
    #[error("配置校验失败: {0}")]
    #[diagnostic(code(config::validation_error))]
    Validation(String),
}

/// config.toml 是否已存在
pub async fn config_file_exists() -> bool {
    daat_locus_paths()
        .await
        .config_file(CONFIG_FILE_NAME)
        .exists()
}

/// 将 Config 序列化并写入 config.toml
pub async fn write_config(config: &Config) -> Result<(), ConfigError> {
    let config_path = daat_locus_paths().await.config_file(CONFIG_FILE_NAME);
    let toml_str =
        toml::to_string_pretty(config).map_err(|e| ConfigError::Syntax(e.to_string()))?;
    tokio::fs::write(&config_path, toml_str)
        .await
        .map_err(ConfigError::IO)?;
    Ok(())
}

/// 加载 config.toml；若文件不存在，返回 IO 错误（不再自动创建默认值）
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
