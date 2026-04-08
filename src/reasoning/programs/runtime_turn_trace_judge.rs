use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const RUNTIME_TURN_TRACE_JUDGE_SYSTEM_PROMPT: &str = r#"你现在不是执行者，而是 runtime turn trace 的评审器。
你的任务是根据给定的 turn demo 目标，判断当前 system prompt 是否会诱导出正确的多轮 ReAct 行为。

要求：
- 只根据给定 prompt、turn demo 和 turn trace 做判断，不要假设不存在的工具或额外上下文。
- 优先评估：是否过早终止、是否错误地把阶段性话术当成最终答复、是否遗漏了必要工具推进。
- `passed=true` 只在当前 trace 已明显符合 demo 期望行为时给出。
- `needed_changes` 只写最小必要 patch，不要整段重写 prompt。 "#;

pub struct RuntimeTurnTraceJudgeProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeTurnTraceJudgeOutput {
    pub passed: bool,
    pub regression_detected: bool,
    pub confidence: f64,
    pub needed_changes: Vec<String>,
    pub reason: String,
}

impl Program for RuntimeTurnTraceJudgeProgram {
    type Output = RuntimeTurnTraceJudgeOutput;

    fn name(&self) -> &'static str {
        "runtime_turn_trace_judge"
    }

    fn description(&self) -> &'static str {
        "根据完整 turn trace 判断当前 runtime system prompt 是否诱导出了正确的 ReAct 停止与终局行为。"
    }

    fn signature(&self) -> Signature {
        Signature::new("评估当前 runtime system prompt 是否通过 turn rollout demo。")
            .input("current system prompt", "当前正在评估的 system prompt。")
            .input(
                "previous system prompt",
                "上一版 system prompt；没有则写 none。",
            )
            .input("demo title", "当前 turn demo 标题。")
            .input("scenario summary", "turn demo 场景摘要。")
            .input("expected behavior", "该 demo 期望的多轮行为。")
            .input("judge focus", "本 demo 的评审重点。")
            .input("turn trace", "本次真实 rollout 的 trace 渲染文本。")
            .output("passed", "当前 prompt 是否通过该 turn demo。")
            .output("regression_detected", "相对 previous prompt 是否出现退化。")
            .output("confidence", "0 到 1 之间的置信度。")
            .output("needed_changes", "若未通过，需要增加或修改的最小提示语。")
            .output("reason", "简洁说明判断依据。")
            .rule("如果 previous system prompt 为 none，则 regression_detected 必须为 false。")
            .rule("needed_changes 应尽量是 prompt patch，而不是完整重写。")
            .rule("不要把阶段性计划、承诺或'接下来我会继续'类文本视为天然合格的最终答复。")
    }
}

impl RuntimeTurnTraceJudgeProgram {
    #[allow(clippy::too_many_arguments)]
    pub fn dataset_ir(
        &self,
        current_system_prompt: String,
        previous_system_prompt: String,
        demo_title: String,
        scenario_summary: String,
        expected_behavior: String,
        judge_focus: String,
        turn_trace: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(RUNTIME_TURN_TRACE_JUDGE_SYSTEM_PROMPT);
        ir.push_instruction("优先关注 turn 是否在正确时机停止，以及最后 assistant 是否是可直接交付的 terminal answer。");
        ir.push_instruction("如果只是缺少一句规则或约束，请在 needed_changes 中给出最小 patch。");
        ir.push_section("current system prompt", current_system_prompt);
        ir.push_section("previous system prompt", previous_system_prompt);
        ir.push_section("demo title", demo_title);
        ir.push_section("scenario summary", scenario_summary);
        ir.push_section("expected behavior", expected_behavior);
        ir.push_section("judge focus", judge_focus);
        ir.push_section("turn trace", turn_trace);
        ir
    }
}
