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

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub main_model: MainModelConfig,
    pub judge: JudgeConfig,
    pub hindsight: HindsightConfig,
    pub telegram: TelegramConfig,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HindsightConfig {
    pub base_url: String,
    pub api_key: String,
    pub namespace: String,
    pub bank_id: String,
    pub request_timeout_secs: u64,
}

impl Default for HindsightConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:8888".to_string(),
            api_key: String::new(),
            namespace: "default".to_string(),
            bank_id: "daat-locus".to_string(),
            request_timeout_secs: 180,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MainModelConfig {
    pub base_url: String,
    pub model_name: String,
    pub api_key: String,
    pub temperature: f64,
    pub thinking_budget: Option<String>,
    pub rpm: Option<u32>,
    pub request_timeout_secs: u64,
    pub stream_idle_timeout_secs: u64,
    pub context_window_tokens: usize,
    #[serde(default, alias = "auto_compact_threshold_tokens")]
    pub auto_compact_token_limit: Option<usize>,
    pub effective_context_window_percent: i64,
    pub max_completion_tokens: usize,
    pub tool_output_max_tokens: usize,
}

impl Default for MainModelConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com".to_string(),
            model_name: "gpt-4.1".to_string(),
            api_key: "your-api-key".to_string(),
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

impl MainModelConfig {
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
            .and_then(|rpm| usize::try_from(rpm).ok())
            .filter(|rpm| *rpm > 0)
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
        let context_window = self.context_window_tokens();
        let effective = (context_window as u128)
            .saturating_mul(self.effective_context_window_percent() as u128)
            / 100;
        usize::try_from(effective)
            .unwrap_or(context_window)
            .clamp(1, context_window)
    }

    pub fn auto_compact_token_limit(&self) -> usize {
        let context_window = self.context_window_tokens();
        let context_default_limit =
            usize::try_from((context_window as u128).saturating_mul(9) / 10)
                .unwrap_or(context_window);
        let configured_limit = self
            .auto_compact_token_limit
            .unwrap_or(context_default_limit);
        configured_limit
            .min(context_default_limit.max(1))
            .min(self.effective_context_window_tokens())
            .max(1)
    }

    pub fn max_completion_tokens(&self) -> usize {
        self.max_completion_tokens
            .clamp(1, self.context_window_tokens())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JudgeConfig {
    pub enabled: bool,
    pub use_main_model: bool,
    pub base_url: String,
    pub model_name: String,
    pub api_key: String,
    pub temperature: f64,
    pub max_pairwise_candidates: usize,
    pub max_pairwise_cases: usize,
}

impl Default for JudgeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            use_main_model: true,
            base_url: String::new(),
            model_name: String::new(),
            api_key: String::new(),
            temperature: 1.0,
            max_pairwise_candidates: 4,
            max_pairwise_cases: 4,
        }
    }
}

impl JudgeConfig {
    pub fn resolved_model(&self, main_model: &MainModelConfig) -> MainModelConfig {
        if self.use_main_model {
            let mut resolved = main_model.clone();
            resolved.temperature = self.temperature;
            return resolved;
        }

        MainModelConfig {
            base_url: if self.base_url.trim().is_empty() {
                main_model.base_url.clone()
            } else {
                self.base_url.clone()
            },
            model_name: if self.model_name.trim().is_empty() {
                main_model.model_name.clone()
            } else {
                self.model_name.clone()
            },
            api_key: if self.api_key.trim().is_empty() {
                main_model.api_key.clone()
            } else {
                self.api_key.clone()
            },
            temperature: self.temperature,
            thinking_budget: main_model.thinking_budget.clone(),
            rpm: main_model.rpm,
            request_timeout_secs: main_model.request_timeout_secs,
            stream_idle_timeout_secs: main_model.stream_idle_timeout_secs,
            context_window_tokens: main_model.context_window_tokens,
            auto_compact_token_limit: main_model.auto_compact_token_limit,
            effective_context_window_percent: main_model.effective_context_window_percent,
            max_completion_tokens: main_model.max_completion_tokens,
            tool_output_max_tokens: main_model.tool_output_max_tokens,
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
            enabled: false,
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

#[derive(Error, Debug, Diagnostic)]
pub enum ConfigError {
    #[error("配置文件读取失败: {0}")]
    IO(#[from] std::io::Error),
    #[error("{0}")]
    #[diagnostic(code(config::syntax_error))]
    Syntax(String),
}

pub async fn load_config() -> Result<Config, ConfigError> {
    let config_path = daat_locus_paths().await.config_file(CONFIG_FILE_NAME);

    if !config_path.exists() {
        let default_config = Config::default();
        let toml_str = toml::to_string_pretty(&default_config).unwrap();
        tokio::fs::write(&config_path, toml_str).await.unwrap();
    }

    let content = tokio::fs::read_to_string(config_path)
        .await
        .map_err(ConfigError::IO)?;

    let ret: Config = toml::from_str(&content).map_err(|e| ConfigError::Syntax(e.to_string()))?;
    Ok(ret)
}
