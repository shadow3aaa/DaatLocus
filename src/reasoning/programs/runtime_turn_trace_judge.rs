//! Runtime turn-trace judge program used in offline evaluation.

use daat_locus_macros::model_schema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{
    program::Program,
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
