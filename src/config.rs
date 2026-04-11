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
    pub default_recall_budget: String,
    pub default_reflect_budget: String,
    pub reflect_mission: String,
    pub retain_mission: String,
    pub retain_extraction_mode: String,
    pub retain_custom_instructions: String,
    pub observations_mission: String,
    pub enable_observations: bool,
    pub disposition_skepticism: u8,
    pub disposition_literalism: u8,
    pub disposition_empathy: u8,
    pub entity_labels: Vec<HindsightEntityLabelGroupConfig>,
    pub entities_allow_free_form: bool,
    pub directives: Vec<HindsightDirectiveConfig>,
    pub mental_models: Vec<HindsightMentalModelTemplateConfig>,
}

impl Default for HindsightConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:8888".to_string(),
            api_key: String::new(),
            namespace: "default".to_string(),
            bank_id: "daat-locus".to_string(),
            request_timeout_secs: 120,
            default_recall_budget: "mid".to_string(),
            default_reflect_budget: "low".to_string(),
            reflect_mission: default_hindsight_reflect_mission(),
            retain_mission: default_hindsight_retain_mission(),
            retain_extraction_mode: "verbose".to_string(),
            retain_custom_instructions: String::new(),
            observations_mission: default_hindsight_observations_mission(),
            enable_observations: true,
            disposition_skepticism: 4,
            disposition_literalism: 4,
            disposition_empathy: 3,
            entity_labels: default_hindsight_entity_labels(),
            entities_allow_free_form: true,
            directives: default_hindsight_directives(),
            mental_models: default_hindsight_mental_models(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HindsightEntityLabelGroupConfig {
    pub key: String,
    pub description: String,
    #[serde(rename = "type")]
    pub label_type: String,
    pub optional: bool,
    pub tag: bool,
    pub values: Vec<HindsightEntityLabelValueConfig>,
}

impl Default for HindsightEntityLabelGroupConfig {
    fn default() -> Self {
        Self {
            key: String::new(),
            description: String::new(),
            label_type: "value".to_string(),
            optional: true,
            tag: false,
            values: Vec::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HindsightEntityLabelValueConfig {
    pub value: String,
    pub description: String,
}

impl Default for HindsightEntityLabelValueConfig {
    fn default() -> Self {
        Self {
            value: String::new(),
            description: String::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HindsightDirectiveConfig {
    pub id: String,
    pub name: String,
    pub content: String,
    pub priority: i64,
    pub is_active: bool,
    pub tags: Vec<String>,
}

impl Default for HindsightDirectiveConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            content: String::new(),
            priority: 0,
            is_active: true,
            tags: Vec::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HindsightMentalModelTemplateConfig {
    pub id: String,
    pub name: String,
    pub source_query: String,
    pub max_tokens: usize,
    pub tags: Vec<String>,
    pub refresh_after_consolidation: bool,
}

impl Default for HindsightMentalModelTemplateConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            source_query: String::new(),
            max_tokens: 2048,
            tags: Vec::new(),
            refresh_after_consolidation: false,
        }
    }
}

fn default_hindsight_reflect_mission() -> String {
    "Reason like a persistent Daat Locus runtime maintainer. Prefer grounded, reviewable judgments about project continuity, runtime boundaries, tool usage, user preferences, and operational risk. Distinguish stable knowledge from transient state, and surface uncertainty when evidence is incomplete.".to_string()
}

fn default_hindsight_retain_mission() -> String {
    "Retain durable engineering knowledge for Daat Locus. Prefer architectural boundaries, event/app semantics, user preferences, failure patterns, tool usage constraints, and decisions with future reuse value. Ignore greetings, transient bookkeeping, redundant retries, and low-signal logs unless they materially explain a durable lesson.".to_string()
}

fn default_hindsight_observations_mission() -> String {
    "Observations should capture stable facts about the project, runtime behavior, user preferences, and recurring engineering patterns. Consolidate repeated evidence into reusable knowledge. Avoid overfitting to one-off events or transient machine state.".to_string()
}

fn default_hindsight_entity_labels() -> Vec<HindsightEntityLabelGroupConfig> {
    vec![
        HindsightEntityLabelGroupConfig {
            key: "kind".to_string(),
            description: "The durable knowledge class represented by this memory.".to_string(),
            label_type: "value".to_string(),
            optional: true,
            tag: true,
            values: vec![
                HindsightEntityLabelValueConfig {
                    value: "project_fact".to_string(),
                    description: "Stable facts about the Daat Locus codebase or runtime."
                        .to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "user_preference".to_string(),
                    description: "Persistent user or operator preferences.".to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "runtime_boundary".to_string(),
                    description: "Behavioral contract or boundary the agent should preserve."
                        .to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "failure_pattern".to_string(),
                    description: "Recurring failure mode or risk pattern.".to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "strategy_lesson".to_string(),
                    description: "Reusable operational lesson or heuristic.".to_string(),
                },
            ],
        },
        HindsightEntityLabelGroupConfig {
            key: "scope".to_string(),
            description: "The runtime surface or subsystem most relevant to the memory."
                .to_string(),
            label_type: "value".to_string(),
            optional: true,
            tag: true,
            values: vec![
                HindsightEntityLabelValueConfig {
                    value: "runtime".to_string(),
                    description: "Core runtime loop behavior.".to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "telegram".to_string(),
                    description: "Telegram event or delivery behavior.".to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "workspace".to_string(),
                    description: "Workspace or code editing behavior.".to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "sleep".to_string(),
                    description: "Sleep-time reflection and self-improvement.".to_string(),
                },
            ],
        },
        HindsightEntityLabelGroupConfig {
            key: "source".to_string(),
            description: "How the memory entered the system.".to_string(),
            label_type: "value".to_string(),
            optional: true,
            tag: true,
            values: vec![
                HindsightEntityLabelValueConfig {
                    value: "runtime_step".to_string(),
                    description: "A runtime step retained from the live agent loop.".to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "sleep_reflection".to_string(),
                    description: "A lesson synthesized during sleep.".to_string(),
                },
            ],
        },
    ]
}

fn default_hindsight_directives() -> Vec<HindsightDirectiveConfig> {
    vec![
        HindsightDirectiveConfig {
            id: "ground-claims-in-evidence".to_string(),
            name: "Ground Claims In Evidence".to_string(),
            content: "Prefer conclusions that can be tied back to retrieved memories, observations, or mental models. If evidence is weak or mixed, say so explicitly instead of overstating certainty.".to_string(),
            priority: 100,
            is_active: true,
            tags: vec!["runtime".to_string(), "reasoning".to_string()],
        },
        HindsightDirectiveConfig {
            id: "respect-stable-boundaries".to_string(),
            name: "Respect Stable Runtime Boundaries".to_string(),
            content: "Preserve stable contracts around App, Event, PendingWork, Plan, Memory, and finish_and_send. Do not collapse distinct runtime concepts or rewrite boundaries based on one-off situations.".to_string(),
            priority: 90,
            is_active: true,
            tags: vec!["runtime".to_string(), "architecture".to_string()],
        },
        HindsightDirectiveConfig {
            id: "avoid-transient-overfitting".to_string(),
            name: "Avoid Transient Overfitting".to_string(),
            content: "Do not elevate transient machine state, temporary confusion, or one-off logs into durable preferences or project facts unless the evidence repeats across turns.".to_string(),
            priority: 80,
            is_active: true,
            tags: vec!["memory".to_string(), "retention".to_string()],
        },
    ]
}

fn default_hindsight_mental_models() -> Vec<HindsightMentalModelTemplateConfig> {
    vec![
        HindsightMentalModelTemplateConfig {
            id: "project-state".to_string(),
            name: "Project State".to_string(),
            source_query: "What is the current project state of Daat Locus, including active workstreams, unresolved technical threads, and recently stabilized decisions?".to_string(),
            max_tokens: 1600,
            tags: vec![
                "mental-model".to_string(),
                "scope:project".to_string(),
                "scope:runtime".to_string(),
            ],
            refresh_after_consolidation: true,
        },
        HindsightMentalModelTemplateConfig {
            id: "runtime-boundaries".to_string(),
            name: "Runtime Boundaries".to_string(),
            source_query: "What stable runtime boundaries and agent-facing contracts define how Daat Locus should treat App, Event, PendingWork, Plan, Memory, and finish_and_send?".to_string(),
            max_tokens: 1400,
            tags: vec![
                "mental-model".to_string(),
                "scope:runtime".to_string(),
                "kind:runtime_boundary".to_string(),
            ],
            refresh_after_consolidation: true,
        },
        HindsightMentalModelTemplateConfig {
            id: "user-preferences".to_string(),
            name: "User Preferences".to_string(),
            source_query: "What stable user preferences, communication expectations, and collaboration patterns should Daat Locus preserve in this workspace?".to_string(),
            max_tokens: 1200,
            tags: vec![
                "mental-model".to_string(),
                "scope:user".to_string(),
                "kind:user_preference".to_string(),
            ],
            refresh_after_consolidation: true,
        },
        HindsightMentalModelTemplateConfig {
            id: "runtime-strategy".to_string(),
            name: "Runtime Strategy".to_string(),
            source_query: "What stable runtime strategies, learned heuristics, and prompt-level lessons should guide Daat Locus when continuing work in this repository?".to_string(),
            max_tokens: 1400,
            tags: vec![
                "mental-model".to_string(),
                "scope:runtime".to_string(),
                "kind:strategy_lesson".to_string(),
            ],
            refresh_after_consolidation: true,
        },
    ]
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MainModelConfig {
    pub base_url: String,
    pub model_name: String,
    pub api_key: String,
    pub temperature: f64,
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
            base_url: "https://api.openai.com/v1".to_string(),
            model_name: "gpt-4.1".to_string(),
            api_key: "your-api-key".to_string(),
            temperature: 1.0,
            context_window_tokens: DEFAULT_CONTEXT_WINDOW_TOKENS,
            auto_compact_token_limit: Some(DEFAULT_AUTO_COMPACT_THRESHOLD_TOKENS),
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            max_completion_tokens: DEFAULT_MAX_COMPLETION_TOKENS,
            tool_output_max_tokens: DEFAULT_TOOL_OUTPUT_MAX_TOKENS,
        }
    }
}

impl MainModelConfig {
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
