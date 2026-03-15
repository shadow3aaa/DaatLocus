use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    reasoning::{
        bench::datasets::memory_recall as dataset, examples::ProgramExample, ir::PromptIR,
        program::Program, signature::Signature,
    },
    snapshot::Snapshot,
};

const BENCH_SYSTEM_PROMPT: &str = r#"你正在执行一个离线 benchmark program，用来评估记忆呈现是否足够清晰。
你只能根据输入中的“当前目标”“近期经历”“联想回忆”和“问题”作答。
不要发明不存在的记忆，不要把无关等待噪声当成关键事实。"#;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MemoryRecallOutput {
    pub relevant_memory_ids: Vec<String>,
    pub answer: String,
}

pub struct MemoryRecallProgram;

impl MemoryRecallProgram {
    pub fn suite_name(&self) -> &'static str {
        "bench.memory_recall"
    }

    pub fn dataset_ir(
        &self,
        current_goal: String,
        recent_trail: String,
        associated_memories: String,
        question: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(BENCH_SYSTEM_PROMPT);
        ir.push_instruction("先从近期经历和联想回忆中挑出最相关的 1-3 条记忆 id。");
        ir.push_instruction("优先保留承诺、项目连续性、明确事实线索；忽略纯等待、空转和无关寒暄。");
        ir.push_instruction("`relevant_memory_ids` 只能填写输入里实际出现过的记忆 id。");
        ir.push_instruction(
            "`answer` 必须简洁说明这些记忆为什么关键，以及它们支持什么下一步判断。",
        );
        ir.push_section("当前目标", current_goal);
        ir.push_section("近期经历", recent_trail);
        ir.push_section("联想回忆", associated_memories);
        ir.push_section("问题", question);
        ir
    }

    pub fn train_eval_cases(&self) -> Vec<crate::reasoning::eval::EvalCase<MemoryRecallOutput>> {
        dataset::train_eval_cases(self)
    }

    pub fn dev_eval_cases(&self) -> Vec<crate::reasoning::eval::EvalCase<MemoryRecallOutput>> {
        dataset::dev_eval_cases(self)
    }
}

impl Program for MemoryRecallProgram {
    type Output = MemoryRecallOutput;

    fn name(&self) -> &'static str {
        "memory_recall"
    }

    fn description(&self) -> &'static str {
        "根据当前目标、近期经历和联想回忆，找出最相关的记忆并给出简洁结论。"
    }

    fn tuning_key(&self) -> String {
        self.suite_name().to_string()
    }

    fn signature(&self) -> Signature {
        Signature::new("从候选记忆中挑出最相关的事实，保持任务连续性，并回答当前问题。")
            .input("当前目标", "当前正在推进的目标或问题。")
            .input("近期经历", "L1 风格的近期轨迹，可能包含噪声等待。")
            .input(
                "联想回忆",
                "L2 风格的相关历史记忆，可能包含有用线索或无关干扰。",
            )
            .input("问题", "需要基于上述记忆回答的问题。")
            .output("relevant_memory_ids", "最相关的 1-3 条记忆 id。")
            .output("answer", "基于这些记忆给出的简洁结论。")
            .rule("只能引用输入里实际出现过的记忆 id。")
            .rule("优先保留承诺连续性、项目信息和明确事实线索，忽略纯等待噪声。")
            .rule("如果存在明显的长期承诺或项目线索，不要被近期无关消息带偏。")
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
        )
    }
}
