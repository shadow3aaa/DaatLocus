//! 本模块定义策略路由，它的作用是根据spinova状态判定下一步应该进入什么阶段

use crate::context::Context;

pub enum Strategy {
    /// 后台设备出现需要优先处理的提醒
    AttendNotifications,
    /// 已经存在下一步动作，继续执行
    ExecuteTask,
    /// 尚无下一步动作，但有活跃项目，需要先规划下一步
    PlanFromProject,
    /// 无聊度过高，寻找新的短期任务
    ExploreNewTasks,
}

impl Strategy {
    const BOREDOM_THRESHOLD: f32 = 0.8;

    pub fn route(context: &Context) -> Self {
        if context.devices.requires_attention() || context.obligations.has_pending() {
            return Self::AttendNotifications;
        }

        let boredom = context.emotion.boredom;
        if !context.tasks.is_empty() {
            if boredom > Self::BOREDOM_THRESHOLD && !context.projects.has_active() {
                return Self::ExploreNewTasks;
            }
            return Self::ExecuteTask;
        }

        if context.projects.has_active() {
            return Self::PlanFromProject;
        }

        // TODO: 完成真正的贝叶斯惊奇评估后，细化无项目/无动作时的探索触发条件。
        if boredom > Self::BOREDOM_THRESHOLD || context.tasks.is_empty() {
            Self::ExploreNewTasks
        } else {
            Self::ExploreNewTasks
        }
    }
}
