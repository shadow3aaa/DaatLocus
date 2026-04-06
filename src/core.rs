use async_trait::async_trait;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    device::DeviceId,
    events::EventDisposition,
    reasoning::runtime::{AgentTurnRequest, AgentTurnStreamResult, PromptRequest},
    todo_board::TodoStatus,
};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FocusDeviceArgs {
    pub device: DeviceId,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct PutAwayDeviceArgs {}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TerminalExecArgs {
    pub command: String,
    /// 显式指定要复用的 session；不填则新建 session
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
pub struct BrowserFindInPageArgs {
    pub page_id: String,
    pub query: String,
    pub max_results: Option<usize>,
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
    pub snapshot_id: String,
    pub element_ref: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserFillArgs {
    pub page_id: String,
    pub snapshot_id: String,
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
    pub event_id: String,
    pub disposition: EventDisposition,
    pub reply_message: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TodoCreateArgs {
    pub title: String,
    pub done_criteria: String,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TodoUpdateArgs {
    pub item_id: String,
    pub title: Option<String>,
    pub done_criteria: Option<String>,
    pub notes: Option<String>,
    pub clear_notes: Option<bool>,
    pub status: Option<TodoStatus>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DeepRecallArgs {
    /// 要交给长期记忆后端进行深度回忆/反思的自然语言问题
    pub query: String,
    /// 可选 budget；不填则使用配置默认值
    pub budget: Option<String>,
    /// 允许返回的最大 token 数
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TodoCompleteArgs {
    pub item_id: String,
    pub summary: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TodoDropArgs {
    pub item_id: String,
    pub note: Option<String>,
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
pub struct TokenUsageInfo {
    pub total_token_usage: TokenUsage,
    pub last_token_usage: TokenUsage,
    pub model_context_window: Option<i64>,
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
        self.last_token_usage = last;
    }
}

/// LLM 负责思考
#[async_trait]
pub trait LLM {
    /// 执行一个结构化 program 请求，返回原始 JSON 参数对象。
    async fn run_json(
        &self,
        context: &Context,
        request: PromptRequest,
    ) -> Result<serde_json::Value>;

    /// 执行一轮工具驱动的 agent turn，返回 assistant 文本或 tool calls。
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
