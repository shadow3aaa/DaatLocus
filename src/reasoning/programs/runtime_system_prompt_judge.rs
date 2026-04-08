use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const RUNTIME_SYSTEM_PROMPT_JUDGE_SYSTEM_PROMPT: &str = r#"你现在不是执行者，而是 runtime system prompt 的评审器。
你的任务是根据给定的 demo 目标，判断当前 system prompt 是否足以诱导出目标行为。

要求：
- 只根据给定 prompt 和 demo 做判断，不要假设不存在的工具或额外上下文。
- `passed=true` 只在 prompt 已明显覆盖该 demo 的关键行为时给出。
- 如果当前 prompt 在该 demo 上明显比 previous prompt 更差，才判定 regression_detected=true。
- `needed_changes` 只写最小必要改动建议，不要整段重写 prompt。"#;

pub struct RuntimeSystemPromptJudgeProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeSystemPromptJudgeOutput {
    pub passed: bool,
    pub regression_detected: bool,
    pub confidence: f64,
    pub needed_changes: Vec<String>,
    pub reason: String,
}

impl Program for RuntimeSystemPromptJudgeProgram {
    type Output = RuntimeSystemPromptJudgeOutput;

    fn name(&self) -> &'static str {
        "runtime_system_prompt_judge"
    }

    fn description(&self) -> &'static str {
        "根据 runtime demo 判断当前 system prompt 是否满足目标行为，并指出需要的最小改动。"
    }

    fn signature(&self) -> Signature {
        Signature::new("评估当前 runtime system prompt 是否通过 demo。")
            .input("current system prompt", "当前正在评估的 system prompt。")
            .input(
                "previous system prompt",
                "上一版 system prompt；没有则写 none。",
            )
            .input("demo title", "当前 demo 标题。")
            .input("scenario summary", "demo 场景摘要。")
            .input("expected behavior", "该 demo 期望的行为。")
            .input("judge focus", "评审重点。")
            .output("passed", "当前 prompt 是否通过该 demo。")
            .output("regression_detected", "相对 previous prompt 是否出现退化。")
            .output("confidence", "0 到 1 之间的置信度。")
            .output("needed_changes", "若未通过，需要增加或修改的最小提示语。")
            .output("reason", "简洁说明判断依据。")
            .rule("如果 previous system prompt 为 none，则 regression_detected 必须为 false。")
            .rule("needed_changes 应尽量是 prompt patch，而不是完整重写。")
    }
}

impl RuntimeSystemPromptJudgeProgram {
    pub fn dataset_ir(
        &self,
        current_system_prompt: String,
        previous_system_prompt: String,
        demo_title: String,
        scenario_summary: String,
        expected_behavior: String,
        judge_focus: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(RUNTIME_SYSTEM_PROMPT_JUDGE_SYSTEM_PROMPT);
        ir.push_instruction("优先关注 prompt 是否明确诱导出 demo 要求的行为边界。");
        ir.push_instruction("如果只是缺少一句规则或约束，请在 needed_changes 中给出最小 patch。");
        ir.push_section("current system prompt", current_system_prompt);
        ir.push_section("previous system prompt", previous_system_prompt);
        ir.push_section("demo title", demo_title);
        ir.push_section("scenario summary", scenario_summary);
        ir.push_section("expected behavior", expected_behavior);
        ir.push_section("judge focus", judge_focus);
        ir
    }
}
