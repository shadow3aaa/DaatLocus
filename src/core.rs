use async_trait::async_trait;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    device::DeviceId,
    reasoning::runtime::{AgentTurnRequest, AgentTurnResponse, PromptRequest},
};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "kind")]
pub enum TelegramResolution {
    ReplyOnly {
        /// 仅做简短回复时要发送的内容
        reply: String,
    },
    AcceptAsProject {
        /// 如果需要先对外确认接下该工作，可填写回复内容；也可以留空，稍后再回
        reply: Option<String>,
        /// 新项目的标题
        project_title: String,
        /// 如何判断该项目完成
        success_criteria: String,
        /// 接下项目后立即要聚焦的第一条当前工作目标
        first_next_action: Option<String>,
    },
    AskClarification {
        /// 需要进一步澄清时发送的追问内容
        reply: String,
    },
    Decline {
        /// 拒绝时发送的回复内容
        reply: String,
    },
    NoReplyNeeded,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SetWorkObjectiveArgs {
    pub description: String,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ClearWorkObjectiveArgs {}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FocusDeviceArgs {
    pub device: DeviceId,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct PutAwayDeviceArgs {}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TerminalExecArgs {
    pub command: String,
    pub session_id: Option<String>,
    #[serde(default)]
    pub create_new_session: bool,
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
pub struct ApplyPatchArgs {
    pub patch: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TelegramSelectChatArgs {
    pub chat_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TelegramListChatsArgs {}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TelegramReadChatArgs {
    pub chat_id: Option<String>,
    pub max_messages: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TelegramSendMessageArgs {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ResolveTelegramChatArgs {
    pub chat_id: String,
    pub resolution: TelegramResolution,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ObligationSatisfyArgs {
    pub obligation_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CommitToProjectArgs {
    pub obligation_id: String,
    pub title: String,
    pub success_criteria: String,
    pub initial_next_action: Option<String>,
    pub acknowledgment: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ProjectCompleteArgs {
    pub project_id: String,
    pub summary: String,
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
        _context: &Context,
        _request: AgentTurnRequest,
    ) -> Result<AgentTurnResponse> {
        Err(miette!(
            "run_agent_turn is not implemented for this provider"
        ))
    }
}
