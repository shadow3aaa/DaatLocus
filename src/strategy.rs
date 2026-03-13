//! 本模块定义策略路由，它的作用是根据spinova状态判定下一步应该进入什么阶段

use crate::context::Context;

pub enum Strategy {
    /// 无聊程度适中，将进入任务执行阶段
    ExecuteTask,
    /// 无聊度过高，寻找新的短期任务
    ExploreNewTasks,
}

impl Strategy {
    const BOREDOM_THRESHOLD: f32 = 0.8;

    pub fn route(context: &Context) -> Self {
        let boredom = context.emotion.boredom;
        // TODO: 完成真正的贝叶斯惊奇评估后，去掉context.tasks.is_empty()
        if context.tasks.is_empty() || boredom > Self::BOREDOM_THRESHOLD {
            Self::ExploreNewTasks
        } else {
            Self::ExecuteTask
        }
    }
}
