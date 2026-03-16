use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    reasoning::{
        bench::datasets::memory_encoding as dataset, examples::ProgramExample, ir::PromptIR,
        program::Program, signature::Signature,
    },
    snapshot::Snapshot,
};

const BENCH_SYSTEM_PROMPT: &str = r#"你正在执行一个离线 benchmark program，用来评估记忆编码是否能保住关键主线与锚点。
你要把当前主线、本轮事件、关键锚点和主线影响拆开表示。
不要记录整段原文，也不要丢掉 URL、文件名、命令、对象引用这类关键锚点。"#;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MemoryEncodingOutput {
    pub thread_focus: String,
    pub event_summary: String,
    pub anchors: Vec<String>,
    pub thread_effect: String,
}

pub struct MemoryEncodingProgram;

impl MemoryEncodingProgram {
    pub fn suite_name(&self) -> &'static str {
        "bench.memory_encoding"
    }

    pub fn dataset_ir(
        &self,
        thread_focus: String,
        observation: String,
        action_description: String,
        evidence: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(BENCH_SYSTEM_PROMPT);
        ir.push_instruction("`thread_focus` 表示持续推进的主线，不要退化成一次性动作。");
        ir.push_instruction("`event_summary` 只总结本轮新增事实和采取动作。");
        ir.push_instruction("`anchors` 只保留未来会影响判断的关键锚点，例如 URL、文件名、命令、对象引用；不要塞无关长句。");
        ir.push_instruction(
            "`thread_effect` 只能是 continue / blocked / clarified / switched / completed 之一。",
        );
        ir.push_instruction("如果证据里出现具体 URL、文件名或执行命令，优先把它们保留到 anchors。");
        ir.push_section("当前主线", thread_focus);
        ir.push_section("观察与结论", observation);
        ir.push_section("采取动作", action_description);
        ir.push_section("证据", evidence);
        ir
    }

    pub fn train_eval_cases(&self) -> Vec<crate::reasoning::eval::EvalCase<MemoryEncodingOutput>> {
        dataset::train_eval_cases(self)
    }

    pub fn dev_eval_cases(&self) -> Vec<crate::reasoning::eval::EvalCase<MemoryEncodingOutput>> {
        dataset::dev_eval_cases(self)
    }
}

impl Program for MemoryEncodingProgram {
    type Output = MemoryEncodingOutput;

    fn name(&self) -> &'static str {
        "memory_encoding"
    }

    fn description(&self) -> &'static str {
        "把当前主线、单轮事件、关键锚点和主线影响编码成适合 L1/L2 的记忆条目。"
    }

    fn tuning_key(&self) -> String {
        self.suite_name().to_string()
    }

    fn signature(&self) -> Signature {
        Signature::new("把一轮执行结果编码成主线清晰、锚点不丢的记忆。")
            .input("当前主线", "当前持续推进的主线，不是单次动作。")
            .input("观察与结论", "这一轮新观察到的事实与结论。")
            .input("采取动作", "这一轮采取了什么动作。")
            .input(
                "证据",
                "这一轮的关键字面量证据，如 URL、文件名、命令、对象引用。",
            )
            .output("thread_focus", "持续推进的主线。")
            .output("event_summary", "本轮新增事实与动作的简洁总结。")
            .output("anchors", "应保留的关键锚点。")
            .output(
                "thread_effect",
                "continue / blocked / clarified / switched / completed。",
            )
            .rule("不要把 thread_focus 写成一次性动作。")
            .rule("不要丢掉会影响后续判断的 URL、文件名、命令和对象引用。")
            .rule("anchors 只保留少量关键锚点，不要抄整段原文。")
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
