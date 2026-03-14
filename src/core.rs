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
/// 你做出的行动，必须是下面五种之一：
/// 1. TaskAdd
/// 2. TaskDelete
/// 3. TaskSelect
/// 4. FocusDevice
/// 5. PutAwayDevice
/// 6. DeviceAction
/// 7. Wait
pub enum Action {
    /// TaskAdd: 添加一个新的任务
    TaskAdd {
        /// 任务的描述
        description: String,
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
