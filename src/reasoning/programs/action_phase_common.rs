use crate::{
    core::Output,
    reasoning::{
        datasets::action_phase as dataset,
        ir::PromptIR,
        program::Program,
        prompts::SYSTEM_PROMPT,
        signature::Signature,
    },
};

pub trait ActionPhaseProgramSpec: Program<Output = Output> {
    fn suite_name(&self) -> &'static str;
    fn phase_title(&self) -> &'static str;
    fn phase_instructions(&self) -> Vec<&'static str>;

    fn dataset_ir(&self, device_context: String, snapshot_text: String) -> PromptIR {
        let mut ir = PromptIR::with_system(SYSTEM_PROMPT);
        for instruction in self.phase_instructions() {
            ir.push_instruction(instruction);
        }
        ir.push_section("当前阶段", self.phase_title());
        ir.push_section("设备上下文", device_context);
        ir.push_section("完整快照", snapshot_text);
        ir
    }

    fn train_eval_cases(&self) -> Vec<crate::reasoning::eval::EvalCase<Output>> {
        dataset::train_eval_cases(self)
    }

    fn dev_eval_cases(&self) -> Vec<crate::reasoning::eval::EvalCase<Output>> {
        dataset::dev_eval_cases(self)
    }

    fn acceptance_eval_cases(&self) -> Vec<crate::reasoning::eval::EvalCase<Output>> {
        dataset::acceptance_eval_cases(self)
    }

    fn stress_eval_cases(&self) -> Vec<crate::reasoning::eval::EvalCase<Output>> {
        dataset::stress_eval_cases(self)
    }
}

pub fn base_signature(phase: &'static str) -> Signature {
    Signature::new(format!("基于当前阶段、设备上下文和完整快照，在“{phase}”阶段选择一条最合适的全局动作。"))
        .input("当前阶段", "当前推理阶段。")
        .input("设备上下文", "当前前景设备、可操作约束和后台提醒摘要。")
        .input(
            "完整快照",
            "世界状态的完整文本视图，包含记忆、义务、项目、下一步动作和设备画面。",
        )
        .output(
            "observation",
            "本轮从快照中提炼出的具体事实、结论或新信息。",
        )
        .output(
            "description",
            "为何选择该执行效果，以及它与当前阶段目标的关系。",
        )
        .output("current_doing", "正在持续推进的高层行为。")
        .output("effect", "一条合法的执行效果，必须能直接交给执行层处理。")
        .rule("输出必须与当前阶段目标一致，不要越阶段做无关决策。")
        .rule("优先使用快照中出现的 UUID 作为 task_id、obligation_id、project_id。")
        .rule("不要把 bookkeeping 解释成 observation；observation 必须包含环境事实。")
}
