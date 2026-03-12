use crate::snapshot::Snapshot;

/// LLM 输出
pub struct Output {
    /// 对于本次行为的描述
    pub description: String,
    /// 对当前正在进行的连续行为的描述。区别于description对本次行为的描述
    pub current_doing: String,
    // todo: 工具调用
}

/// LLM 负责思考
pub trait LLM {
    async fn think(&self, input: &Snapshot) -> Output;
}
