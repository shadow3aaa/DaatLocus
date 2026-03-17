use crate::{
    context::Context,
    core::Output,
    reasoning::{
        datasets::action_phase as dataset,
        examples::ProgramExample,
        ir::PromptIR,
        program::Program,
        programs::action_phase_common::{ActionPhaseProgramSpec, base_signature},
        prompts::build_device_context_prompt,
    },
    snapshot::Snapshot,
};

#[derive(Clone, Copy)]
pub struct ExploreNewTasksProgram;

impl Program for ExploreNewTasksProgram {
    type Output = Output;

    fn name(&self) -> &'static str {
        "explore_new_tasks"
    }

    fn description(&self) -> &'static str {
        "根据完整快照探索并创建新任务。"
    }

    fn tuning_key(&self) -> String {
        self.suite_name().to_string()
    }

    fn signature(&self) -> crate::reasoning::signature::Signature {
        base_signature(self.phase_title())
    }

    fn examples(&self) -> Vec<ProgramExample<Self::Output>> {
        dataset::examples(self)
    }

    fn build_ir(&self, context: &Context, snapshot: &Snapshot) -> PromptIR {
        self.dataset_ir(build_device_context_prompt(context), snapshot.to_string())
    }
}

impl ActionPhaseProgramSpec for ExploreNewTasksProgram {
    fn suite_name(&self) -> &'static str {
        "action_phase.explore_new_tasks"
    }

    fn phase_title(&self) -> &'static str {
        "探索与规划新任务"
    }

    fn phase_instructions(&self) -> Vec<&'static str> {
        vec![
            "只有在没有待处理义务、没有可执行的下一步动作、也没有需要先规划的活跃项目时，才进入探索。",
            "如果你需要探索 Terminal，但它当前不在前景，请先输出 `FocusDevice` 将 `Terminal` 切到前景。",
            "探索环境时，可在 Terminal 处于前景时输出 `DeviceAction` 来执行探索性命令。",
            "一旦构思好新目标，请立即输出 `TaskAdd` 将计划添加到任务列表中。",
            "你的首要职责是寻找并创建新任务。",
            "如果当前完全空闲，只是在等新的 Telegram 消息或外部输入，请输出 `SilentWait`，不要把这种空转等待写进记忆。",
        ]
    }
}
