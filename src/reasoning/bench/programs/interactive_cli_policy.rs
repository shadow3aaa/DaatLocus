use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    reasoning::{
        bench::datasets::interactive_cli_policy as dataset, examples::ProgramExample,
        ir::PromptIR, program::Program, signature::Signature,
    },
    snapshot::Snapshot,
};

const BENCH_SYSTEM_PROMPT: &str = r#"你正在执行一个离线 benchmark program，用来评估 agent 是否会正确处理交互式 CLI 工具。
你只能根据输入中的“当前任务”“终端画面”和“问题”作答。
不要把登录向导、全屏程序或 REPL 误判成普通 shell 输出。"#;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InteractiveCliPolicy {
    InterruptAndSwitchNoninteractive,
    ContinueInteraction,
    Wait,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InteractiveCliPolicyOutput {
    pub policy: InteractiveCliPolicy,
    pub next_input: Option<String>,
    pub reason: String,
}

pub struct InteractiveCliPolicyProgram;

impl InteractiveCliPolicyProgram {
    pub fn suite_name(&self) -> &'static str {
        "bench.interactive_cli_policy"
    }

    pub fn dataset_ir(&self, task: String, terminal_view: String, question: String) -> PromptIR {
        let mut ir = PromptIR::with_system(BENCH_SYSTEM_PROMPT);
        ir.push_instruction("如果终端进入登录向导、授权向导、全屏程序或 REPL，而任务并不要求人工交互，优先选择 interrupt_and_switch_noninteractive。");
        ir.push_instruction("只有在当前交互本身就是任务目标的一部分，而且存在明确安全、短小、确定的输入时，才选择 continue_interaction。");
        ir.push_instruction("如果当前只是后台命令还在自然输出，没有出现需要输入的提示，才选择 wait。");
        ir.push_instruction("`next_input` 只有在 policy=continue_interaction 时才应填写。");
        ir.push_section("当前任务", task);
        ir.push_section("终端画面", terminal_view);
        ir.push_section("问题", question);
        ir
    }

    pub fn train_eval_cases(
        &self,
    ) -> Vec<crate::reasoning::eval::EvalCase<InteractiveCliPolicyOutput>> {
        dataset::train_eval_cases(self)
    }

    pub fn dev_eval_cases(
        &self,
    ) -> Vec<crate::reasoning::eval::EvalCase<InteractiveCliPolicyOutput>> {
        dataset::dev_eval_cases(self)
    }
}

impl Program for InteractiveCliPolicyProgram {
    type Output = InteractiveCliPolicyOutput;

    fn name(&self) -> &'static str {
        "interactive_cli_policy"
    }

    fn description(&self) -> &'static str {
        "根据终端画面判断遇到交互式 CLI 时应该中断、继续输入还是等待。"
    }

    fn tuning_key(&self) -> String {
        self.suite_name().to_string()
    }

    fn signature(&self) -> Signature {
        Signature::new("在交互式 CLI 画面里选择最合理的处理策略。")
            .input("当前任务", "agent 当前试图完成的任务。")
            .input("终端画面", "当前 PTY 终端画面。")
            .input("问题", "需要回答的策略判断问题。")
            .output("policy", "interrupt_and_switch_noninteractive/continue_interaction/wait 之一。")
            .output("next_input", "如果应该继续交互，则给出下一次输入。")
            .output("reason", "简洁说明为什么采取这个策略。")
            .rule("登录向导、授权向导和与任务无关的交互式工具通常应中断并改用非交互方案。")
            .rule("只有在存在明确、安全、短小、确定输入时才继续交互。")
            .rule("如果 policy 不是 continue_interaction，则 next_input 应为空。")
    }

    fn examples(&self) -> Vec<ProgramExample<Self::Output>> {
        dataset::examples()
    }

    fn build_ir(&self, _context: &Context, _snapshot: &Snapshot) -> PromptIR {
        self.dataset_ir("无".to_string(), "无".to_string(), "无".to_string())
    }
}
