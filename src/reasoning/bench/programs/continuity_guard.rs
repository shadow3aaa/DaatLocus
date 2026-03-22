use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    reasoning::{
        bench::datasets::continuity_guard as dataset, examples::ProgramExample, ir::PromptIR,
        program::Program, signature::Signature,
    },
    snapshot::Snapshot,
};

const BENCH_SYSTEM_PROMPT: &str = r#"你正在执行一个离线 benchmark program，用来评估长期连续性是否会被近期噪声打断。
你只能根据输入中的“当前项目状态”“当前工作状态”“近期历史”“召回记忆”和“问题”作答。
不要把寒暄、无关等待或轻量消息误判成新的主目标。"#;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContinuityGuardOutput {
    pub should_continue_project: bool,
    pub project_title: Option<String>,
    pub supporting_memory_ids: Vec<String>,
    pub reason: String,
}

pub struct ContinuityGuardProgram;

impl ContinuityGuardProgram {
    pub fn suite_name(&self) -> &'static str {
        "bench.continuity_guard"
    }

    pub fn dataset_ir(
        &self,
        current_projects: String,
        current_work: String,
        recent_history: String,
        recalled_memories: String,
        question: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(BENCH_SYSTEM_PROMPT);
        ir.push_instruction("先判断当前是否存在应该继续推进的活跃项目、当前工作承诺或明确阻塞。");
        ir.push_instruction("如果应继续推进，`project_title` 必须填写最该继续的项目标题。");
        ir.push_instruction("`supporting_memory_ids` 只能填写输入里实际出现过的记忆 id，且优先包含项目连续性、明确承诺和阻塞信息。");
        ir.push_instruction("不要把寒暄、空转等待或无关消息当成新的主目标。");
        ir.push_section("当前项目状态", current_projects);
        ir.push_section("当前工作状态", current_work);
        ir.push_section("近期历史", recent_history);
        ir.push_section("召回记忆", recalled_memories);
        ir.push_section("问题", question);
        ir
    }

    pub fn train_eval_cases(&self) -> Vec<crate::reasoning::eval::EvalCase<ContinuityGuardOutput>> {
        dataset::train_eval_cases(self)
    }

    pub fn dev_eval_cases(&self) -> Vec<crate::reasoning::eval::EvalCase<ContinuityGuardOutput>> {
        dataset::dev_eval_cases(self)
    }
}

impl Program for ContinuityGuardProgram {
    type Output = ContinuityGuardOutput;

    fn name(&self) -> &'static str {
        "continuity_guard"
    }

    fn description(&self) -> &'static str {
        "判断当前是否应继续推进已有项目或承诺，并说明支撑这一判断的关键记忆。"
    }

    fn tuning_key(&self) -> String {
        self.suite_name().to_string()
    }

    fn signature(&self) -> Signature {
        Signature::new("在近期噪声和外部消息中，保持对已有项目与承诺的连续性判断。")
            .input("当前项目状态", "当前仍在进行中的项目列表。")
            .input("当前工作状态", "当前已设定、正在推进或最近确认的工作目标。")
            .input(
                "近期历史",
                "近期轨迹，可能包含等待噪声、寒暄或被打断的片段。",
            )
            .input("召回记忆", "相关历史记忆，可能包含承诺、阻塞和调查线索。")
            .input("问题", "需要判断当前是否继续推进哪个项目。")
            .output(
                "should_continue_project",
                "当前是否应该继续推进某个已有项目。",
            )
            .output("project_title", "如果应该继续推进，则填写对应项目标题。")
            .output("supporting_memory_ids", "支持该判断的关键记忆 id。")
            .output("reason", "简洁说明为什么该继续推进或为什么不该继续。")
            .rule("只能引用输入里实际出现过的记忆 id。")
            .rule("如果存在明确的活跃项目、承诺或阻塞信息，不要被近期寒暄或空转等待带偏。")
            .rule("如果 `should_continue_project` 为 false，则 `project_title` 应为空。")
    }

    fn examples(&self) -> Vec<ProgramExample<Self::Output>> {
        dataset::examples()
    }

    fn build_ir(&self, _context: &Context, _snapshot: &Snapshot) -> PromptIR {
        self.dataset_ir(
            "无".to_string(),
            "无".to_string(),
            "无".to_string(),
            "无".to_string(),
            "无".to_string(),
        )
    }
}
