use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    reasoning::{
        bench::datasets::terminal_completion as dataset, examples::ProgramExample, ir::PromptIR,
        program::Program, signature::Signature,
    },
    snapshot::Snapshot,
};

const BENCH_SYSTEM_PROMPT: &str = r#"你正在执行一个离线 benchmark program，用来评估 agent 是否会正确理解 PTY 终端状态。
你只能根据输入中的“当前任务”“终端画面”和“问题”作答。
不要把窗口截断误判成命令仍在运行，也不要把交互式提示误判成普通输出。"#;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCompletionStatus {
    Finished,
    StillRunning,
    ViewportTruncated,
    InteractivePrompt,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TerminalCompletionOutput {
    pub status: TerminalCompletionStatus,
    pub reason: String,
}

pub struct TerminalCompletionProgram;

impl TerminalCompletionProgram {
    pub fn suite_name(&self) -> &'static str {
        "bench.terminal_completion"
    }

    pub fn dataset_ir(&self, task: String, terminal_view: String, question: String) -> PromptIR {
        let mut ir = PromptIR::with_system(BENCH_SYSTEM_PROMPT);
        ir.push_instruction("先判断终端现在属于：命令已结束、命令仍在运行、只是窗口截断、或进入交互式提示。");
        ir.push_instruction("如果终端底部已经回到 shell prompt，优先判断为 finished。");
        ir.push_instruction("如果内容只是高度不够导致看不全，但已经回到 prompt，应判断为 viewport_truncated，而不是 still_running。");
        ir.push_instruction("如果画面停在明确的交互式提问或登录向导，应判断为 interactive_prompt。");
        ir.push_section("当前任务", task);
        ir.push_section("终端画面", terminal_view);
        ir.push_section("问题", question);
        ir
    }

    pub fn train_eval_cases(
        &self,
    ) -> Vec<crate::reasoning::eval::EvalCase<TerminalCompletionOutput>> {
        dataset::train_eval_cases(self)
    }

    pub fn dev_eval_cases(
        &self,
    ) -> Vec<crate::reasoning::eval::EvalCase<TerminalCompletionOutput>> {
        dataset::dev_eval_cases(self)
    }
}

impl Program for TerminalCompletionProgram {
    type Output = TerminalCompletionOutput;

    fn name(&self) -> &'static str {
        "terminal_completion"
    }

    fn description(&self) -> &'static str {
        "根据 PTY 终端画面判断命令是否结束、仍在运行、只是窗口截断，或进入交互式提示。"
    }

    fn tuning_key(&self) -> String {
        self.suite_name().to_string()
    }

    fn signature(&self) -> Signature {
        Signature::new("根据 PTY 终端画面判断当前 shell 状态。")
            .input("当前任务", "agent 当前想完成的事情。")
            .input("终端画面", "当前 PTY 里能看到的终端内容。")
            .input("问题", "需要回答的状态判断问题。")
            .output("status", "finished/still_running/viewport_truncated/interactive_prompt 之一。")
            .output("reason", "简洁说明为什么是这个判断。")
            .rule("看到 shell prompt 返回时，不要误判为命令仍在运行。")
            .rule("窗口高度不够导致的截断不等于 still_running。")
            .rule("明确的交互式问题或登录向导应判断为 interactive_prompt。")
    }

    fn examples(&self) -> Vec<ProgramExample<Self::Output>> {
        dataset::examples()
    }

    fn build_ir(&self, _context: &Context, _snapshot: &Snapshot) -> PromptIR {
        self.dataset_ir("无".to_string(), "无".to_string(), "无".to_string())
    }
}
