use async_trait::async_trait;
use miette::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    device::{DeviceAction, DeviceId},
    reasoning::runtime::PromptRequest,
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
        /// 接下项目后立即要执行的第一条下一步动作
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
#[serde(tag = "type")]
/// 你做出的行动，必须是下面几种之一：
/// 1. TaskAdd
/// 2. TaskDelete
/// 3. TaskSelect
/// 4. ResolveTelegramChat
/// 5. ObligationSatisfy
/// 6. CommitToProject
/// 7. ProjectComplete
/// 8. FocusDevice
/// 9. PutAwayDevice
/// 10. DeviceAction
/// 11. Wait
/// 12. SilentWait
pub enum Effect {
    /// TaskAdd: 添加一个新的任务
    TaskAdd {
        /// 任务的描述
        description: String,
        /// 若这条下一步动作属于某个项目，可填写项目 id；否则留空。优先填写快照里显示的项目 UUID，不要填写项目描述。
        project_id: Option<String>,
    },
    /// TaskDelete: 删除一个任务
    TaskDelete {
        /// 要删除的下一步动作 id。优先填写快照里显示的 UUID，不要填写动作描述。
        task_id: String,
    },
    /// TaskSelect: 选中要执行的任务
    TaskSelect {
        /// 要选中的下一步动作 id。优先填写快照里显示的 UUID，不要填写动作描述。
        task_id: String,
    },
    /// ResolveTelegramChat: 对某个 Telegram 会话中的最新来信做语义判断，并由系统自动落地后续 bookkeeping
    ResolveTelegramChat {
        /// 要处理的 Telegram 会话 id。优先填写设备视图里显示的 chat id；若标题能唯一匹配，也可填写标题。
        chat_id: String,
        /// 你对这条会话来信的判断结果与执行所需负载
        resolution: TelegramResolution,
    },
    /// ObligationSatisfy: 将一条已经妥善处理完的义务标记为完成
    ObligationSatisfy {
        /// 要完成的义务 id。优先填写快照里显示的 UUID，不要填写义务摘要。
        obligation_id: String,
    },
    /// CommitToProject: 明确接受某项义务，将其升级为项目
    CommitToProject {
        /// 要升级的义务 id。优先填写快照里显示的 UUID，不要填写义务摘要。
        obligation_id: String,
        /// 新项目的标题
        title: String,
        /// 如何判断该项目完成
        success_criteria: String,
        /// 接受承诺后，立即要执行的第一条下一步动作
        initial_next_action: Option<String>,
        /// 如果需要对外明确承诺，可在这里附上确认消息
        acknowledgment: Option<String>,
    },
    /// ProjectComplete: 将项目标记为完成，并可附上结果摘要
    ProjectComplete {
        /// 要完成的项目 id。优先填写快照里显示的 UUID，不要填写项目标题。
        project_id: String,
        /// 项目已完成的结果摘要，可用于后续回报
        summary: String,
    },
    /// FocusDevice: 将某个设备切到前景
    FocusDevice {
        /// 设备id
        device: DeviceId,
    },
    /// PutAwayDevice: 将当前前景设备放回后台
    PutAwayDevice,
    /// DeviceAction: 对当前前景设备执行动作
    DeviceAction {
        /// 设备动作
        action: DeviceAction,
    },
    /// Wait: 不进行操作，不思观望
    Wait,
    /// SilentWait: 静默等待，不写入记忆；适用于空闲等待用户消息、新任务或新外部输入
    SilentWait,
}

/// LLM 输出
#[derive(Clone, Deserialize, Serialize, JsonSchema)]
pub struct Output {
    /// 你从当前快照、终端输出、消息内容、报错或文件内容中观察到并归纳出的关键信息。
    /// 必须写出具体得到的事实、结论或内容摘要，而不是只写“我看了某个文件/执行了某个命令”。
    pub observation: String,
    /// 对于本次动作决定、分析结论和你为什么这样做的描述
    pub description: String,
    /// 对当前正在进行的连续行为的描述。区别于description对单次行为的描述
    pub current_doing: String,
    /// 本次进行的动作
    pub effect: Effect,
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
}
