use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    device::{DeviceAction, DeviceId},
    snapshot::Snapshot,
};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type")]
/// 你做出的行动，必须是下面几种之一：
/// 1. TaskAdd
/// 2. TaskDelete
/// 3. TaskSelect
/// 4. CommitToProject
/// 5. ProjectComplete
/// 6. FocusDevice
/// 7. PutAwayDevice
/// 8. DeviceAction
/// 9. Wait
pub enum Action {
    /// TaskAdd: 添加一个新的任务
    TaskAdd {
        /// 任务的描述
        description: String,
        /// 若这条下一步动作属于某个项目，可填写项目 id；否则留空
        project_id: Option<String>,
    },
    /// TaskDelete: 删除一个任务
    TaskDelete {
        /// 任务的id
        task_id: String,
    },
    /// TaskSelect: 选中要执行的任务
    TaskSelect {
        /// 任务的id
        task_id: String,
    },
    /// CommitToProject: 明确接受某项义务，将其升级为项目
    CommitToProject {
        /// 要升级的义务 id
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
        /// 要完成的项目 id
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
}

/// LLM 输出
#[derive(Deserialize, Serialize, JsonSchema)]
pub struct Output {
    /// 你从当前快照、终端输出、消息内容、报错或文件内容中观察到并归纳出的关键信息。
    /// 必须写出具体得到的事实、结论或内容摘要，而不是只写“我看了某个文件/执行了某个命令”。
    pub observation: String,
    /// 对于本次动作决定、分析结论和你为什么这样做的描述
    pub description: String,
    /// 对当前正在进行的连续行为的描述。区别于description对单次行为的描述
    pub current_doing: String,
    /// 本次进行的动作
    pub action: Action,
}

/// LLM 负责思考
#[async_trait]
pub trait LLM {
    /// 根据输入的快照进行思考
    async fn think(&self, context: &Context, input: &Snapshot, instruction: &str) -> Output;
}
