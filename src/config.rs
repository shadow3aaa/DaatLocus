use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::get_spinova_home;

const CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub main_model: MainModelConfig,
    pub telegram: TelegramConfig,
    // pub embedding_model: EmbeddingModelConfig, // 目前使用内置的模型
}

impl Default for Config {
    fn default() -> Self {
        Self {
            main_model: MainModelConfig::default(),
            telegram: TelegramConfig::default(),
            /* embedding_model: EmbeddingModelConfig {
                base_url: "https://api.openai.com/v1".to_string(),
                model_name: "text-embedding-3-small".to_string(),
                api_key: "your-api-key".to_string(),
            }, */
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
pub struct TelegramConfig {
    pub enabled: bool,
    pub bot_token: String,
    pub allowed_chat_ids: Vec<i64>,
    pub poll_timeout_secs: u64,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: "your-telegram-bot-token".to_string(),
            allowed_chat_ids: Vec::new(),
            poll_timeout_secs: 30,
        }
    }
}

impl TelegramConfig {
    pub fn has_real_credentials(&self) -> bool {
        !self.bot_token.trim().is_empty() && self.bot_token != "your-telegram-bot-token"
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct EmbeddingModelConfig {
    pub base_url: String,
    pub model_name: String,
    pub api_key: String,
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
        .map_err(|e| ConfigError::IO(e))?;

    let ret: Config = toml::from_str(&content).map_err(|e| ConfigError::Syntax(e.to_string()))?;
    Ok(ret)
}
