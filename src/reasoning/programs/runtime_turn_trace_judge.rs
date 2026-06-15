//! Runtime turn-trace judge program used in offline evaluation.
#![allow(dead_code)]

use daat_locus_macros::model_schema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{
    ir::PromptIR,
    program::Program,
    prompts::{
        PROGRAM_RUNTIME_TURN_TRACE_JUDGE_INSTRUCTIONS, PROGRAM_RUNTIME_TURN_TRACE_JUDGE_SYSTEM,
        prompt_bullet_lines,
    },
    signature::Signature,
};

pub struct RuntimeTurnTraceJudgeProgram;

#[model_schema]
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
        let mut ir = PromptIR::with_system(PROGRAM_RUNTIME_TURN_TRACE_JUDGE_SYSTEM);
        for instruction in prompt_bullet_lines(PROGRAM_RUNTIME_TURN_TRACE_JUDGE_INSTRUCTIONS) {
            ir.push_instruction(instruction);
        }
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
