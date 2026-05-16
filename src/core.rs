use async_trait::async_trait;
use chrono::Local;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    app::AppId,
    context::Context,
    events::EventDisposition,
    plan::PlanStatus,
    reasoning::runtime::{AgentTurnRequest, AgentTurnStreamResult, PromptRequest},
};

const MAX_DAILY_TOKEN_USAGE_DAYS: usize = 30;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FocusAppArgs {
    pub app: AppId,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct PutAwayAppArgs {}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TerminalExecArgs {
    pub command: String,
    /// Explicit session to reuse; omitted means create a new session.
    pub session_id: Option<String>,
    pub workdir: Option<String>,
    pub yield_time_ms: Option<u64>,
    pub max_chars: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TerminalWriteStdinArgs {
    pub session_id: String,
    pub text: String,
    pub yield_time_ms: Option<u64>,
    pub max_chars: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TerminalTerminateArgs {
    pub session_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserOpenArgs {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserSnapshotArgs {
    pub page_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserWaitArgs {
    pub page_id: String,
    /// `dom` waits for any parsed DOM, `load` waits for complete readyState.
    pub state: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserClickArgs {
    pub page_id: String,
    pub element_ref: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserFillArgs {
    pub page_id: String,
    pub element_ref: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserBackArgs {
    pub page_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserForwardArgs {
    pub page_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserReloadArgs {
    pub page_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserClosePageArgs {
    pub page_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct EventResolveArgs {
    pub disposition: EventDisposition,
    pub reply_message: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct NoticeResolvedArgs {
    pub app: AppId,
    pub reason: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct UpdatePlanStepArgs {
    pub step: String,
    pub status: PlanStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct UpdatePlanArgs {
    pub explanation: Option<String>,
    pub plan: Vec<UpdatePlanStepArgs>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CreateWorkflowArgs {
    pub id: String,
    #[serde(default)]
    pub when_to_use: Vec<String>,
    #[serde(default)]
    pub preconditions: Vec<String>,
    #[serde(default)]
    pub workflow_steps: Vec<String>,
    #[serde(default)]
    pub done_criteria: Vec<String>,
    #[serde(default)]
    pub recovery: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ActivateWorkflowArgs {
    pub workflow_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ReadWorkflowArgs {
    pub workflow_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct UpdateWorkflowArgs {
    pub workflow_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    pub when_to_use: Vec<String>,
    pub preconditions: Vec<String>,
    pub workflow_steps: Vec<String>,
    pub done_criteria: Vec<String>,
    pub recovery: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct TokenUsage {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct DailyTokenUsage {
    pub date: String,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct TokenUsageInfo {
    pub total_token_usage: TokenUsage,
    pub last_token_usage: TokenUsage,
    pub model_context_window: Option<i64>,
    #[serde(default)]
    pub daily_token_usage: Vec<DailyTokenUsage>,
}

impl TokenUsage {
    pub fn is_zero(&self) -> bool {
        self.total_tokens == 0
            && self.input_tokens == 0
            && self.cached_input_tokens == 0
            && self.output_tokens == 0
            && self.reasoning_output_tokens == 0
    }

    pub fn add_assign(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_output_tokens += other.reasoning_output_tokens;
        self.total_tokens += other.total_tokens;
    }
}

impl TokenUsageInfo {
    pub fn append_last_usage(&mut self, last: TokenUsage) {
        self.total_token_usage.add_assign(&last);
        self.append_daily_usage(&last);
        self.last_token_usage = last;
    }

    fn append_daily_usage(&mut self, usage: &TokenUsage) {
        let date = Local::now().date_naive().to_string();

        if let Some(day) = self
            .daily_token_usage
            .iter_mut()
            .find(|day| day.date == date)
        {
            day.usage.add_assign(usage);
        } else {
            self.daily_token_usage.push(DailyTokenUsage {
                date,
                usage: usage.clone(),
            });
        }

        if self.daily_token_usage.len() > MAX_DAILY_TOKEN_USAGE_DAYS {
            let excess = self.daily_token_usage.len() - MAX_DAILY_TOKEN_USAGE_DAYS;
            self.daily_token_usage.drain(0..excess);
        }
    }
}

/// LLM provider abstraction.
#[async_trait]
pub trait Llm {
    /// Execute a structured program request and return the raw JSON argument object.
    async fn run_json(
        &self,
        context: &Context,
        request: PromptRequest,
    ) -> Result<serde_json::Value>;

    /// Execute one tool-driven agent turn and return assistant text or tool calls.
    async fn run_agent_turn(
        &self,
        _: &Context,
        _: AgentTurnRequest,
    ) -> Result<AgentTurnStreamResult> {
        Err(miette!(
            "run_agent_turn is not implemented for this provider"
        ))
    }

    fn token_usage_info(&self) -> Option<TokenUsageInfo> {
        None
    }

    fn model_name(&self) -> Option<String> {
        None
    }
}
