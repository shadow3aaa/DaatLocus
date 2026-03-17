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
pub struct PlanFromProjectProgram;

impl Program for PlanFromProjectProgram {
    type Output = Output;

    fn name(&self) -> &'static str {
        "plan_from_project"
    }

    fn description(&self) -> &'static str {
        "根据完整快照为活跃项目生成下一步执行效果。"
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

impl ActionPhaseProgramSpec for PlanFromProjectProgram {
    fn suite_name(&self) -> &'static str {
        "action_phase.plan_from_project"
    }

    fn phase_title(&self) -> &'static str {
        "为项目规划下一步"
    }

    fn phase_instructions(&self) -> Vec<&'static str> {
        vec![
            "查看项目列表，找出最值得优先推进的 `Active` 项目。",
            "为这个项目生成一条具体、可执行、足够小的下一步动作，并用 `TaskAdd` 添加到下一步动作列表。",
            "这条新动作若明确属于某个项目，必须在 `TaskAdd.project_id` 中填写对应项目 id。",
            "下一步动作应尽量直接可执行，避免空泛表述。",
            "如果某个项目当前处于外部等待状态，且确实还不适合生成新的动作，可以输出 `Wait`，但不要因此转去探索无关新任务。",
            "如果某个项目其实已经达到成功标准，不要继续规划新动作，应直接输出 `ProjectComplete`。",
        ]
    }
}
