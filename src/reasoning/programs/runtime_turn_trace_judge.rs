use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const RUNTIME_TURN_TRACE_JUDGE_SYSTEM_PROMPT: &str = r#"You are not the executor; you are the reviewer for a runtime turn trace.
Your task is to judge, based on the given turn demo objective, whether the current system prompt induces correct multi-turn ReAct behavior.

Requirements:
- Judge only from the given prompt, turn demo, and turn trace. Do not assume nonexistent tools or extra context.
- Prioritize whether the trace stops too early, treats interim wording as a final answer, or misses required tool-driven progress.
- Set `passed=true` only when the trace clearly satisfies the demo's expected behavior.
- In `needed_changes`, provide only the minimal necessary patch, not a full prompt rewrite."#;

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
        "Judge whether the current runtime system prompt induced correct ReAct stopping and terminal behavior from a complete turn trace."
    }

    fn signature(&self) -> Signature {
        Signature::new("Evaluate whether the current runtime system prompt passes a turn rollout demo.")
            .input("current system prompt", "System prompt currently under evaluation.")
            .input(
                "previous system prompt",
                "Previous system prompt, or none if unavailable.",
            )
            .input("demo title", "Current turn demo title.")
            .input("scenario summary", "Turn demo scenario summary.")
            .input("expected behavior", "Multi-turn behavior expected by this demo.")
            .input("judge focus", "Primary review focus for this demo.")
            .input("turn trace", "Rendered trace from the actual rollout.")
            .output("passed", "Whether the current prompt passes this turn demo.")
            .output("regression_detected", "Whether behavior regressed relative to the previous prompt.")
            .output("confidence", "Confidence from 0 to 1.")
            .output("needed_changes", "Minimal prompt changes needed if the trace failed.")
            .output("reason", "Concise rationale for the judgment.")
            .rule("If previous system prompt is none, regression_detected must be false.")
            .rule("needed_changes should be prompt patches, not a full rewrite.")
            .rule("Do not treat interim plans, promises, or 'I will continue next' wording as a valid final answer by default.")
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
        ir.push_instruction("Focus on whether the turn stops at the right time and whether the final assistant message is a directly deliverable terminal answer.");
        ir.push_instruction(
            "If only one rule or constraint is missing, put the minimal patch in needed_changes.",
        );
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
