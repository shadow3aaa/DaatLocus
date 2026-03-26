use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::get_spinova_home;

const CONFIG_FILE_NAME: &str = "config.toml";

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
    pub default_recall_budget: String,
    pub default_reflect_budget: String,
}

impl Default for HindsightConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:8888".to_string(),
            api_key: String::new(),
            namespace: "default".to_string(),
            bank_id: "spinova".to_string(),
            request_timeout_secs: 120,
            default_recall_budget: "mid".to_string(),
            default_reflect_budget: "low".to_string(),
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
}

impl Default for MainModelConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            model_name: "gpt-4.1".to_string(),
            api_key: "your-api-key".to_string(),
            temperature: 1.0,
        }
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
    let config_path = get_spinova_home().await.join(CONFIG_FILE_NAME);

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
