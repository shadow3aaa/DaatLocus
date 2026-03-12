use serde::{Deserialize, Serialize};

use crate::snapshot::Snapshot;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum Action {
    /// 添加一个新的任务
    TaskAdd {
        /// 任务的描述
        description: String,
    },
    /// 删除一个任务
    TaskDelete {
        /// 任务的编号
        task_id: usize,
    },
    /// 选中要执行的任务
    TaskSelect {
        /// 任务的编号
        task_id: usize,
    },
    /// 输入终端
    TerminalInput { text: String },
    /// 不进行操作，不思观望
    Wait,
}

/// LLM 输出
pub struct Output {
    /// 对于本次行为的描述
    pub description: String,
    /// 对当前正在进行的连续行为的描述。区别于description对本次行为的描述
    pub current_doing: String,
    /// 本次进行的动作
    pub action: Action,
}

/// LLM 负责思考
pub trait LLM {
    /// 根据输入的快照进行思考
    async fn think(&self, input: &Snapshot) -> Output;
}
