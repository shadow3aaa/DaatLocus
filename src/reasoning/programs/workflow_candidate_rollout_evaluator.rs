use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const WORKFLOW_CANDIDATE_ROLLOUT_EVALUATOR_SYSTEM_PROMPT: &str = r#"You are responsible for single-case rollout evaluation of a workflow frontier candidate.
The candidate may be a patch or a merge. You will see:
- target workflow spec after rollout
- rollout result summary
- target reflection
- a concrete target rollout case containing flushed run record, executed steps, and boundary events
- for merges, source workflow spec, source reflection, and one source rollout case
- the candidate itself

Your task is to judge:
- whether the candidate may outperform the current baseline on this concrete case
- whether there is obvious regression risk

Output requirements:
- `score`: overall score for this case
- `accepted_case`: whether this case supports keeping the candidate
- `improves_upon_baseline`: whether it improves upon the current baseline
- `regression_risk`: whether there is obvious regression risk
- `reason`: rationale based on this case"#;

pub struct WorkflowCandidateRolloutEvaluatorProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowCandidateRolloutEvaluatorOutput {
    pub score: f64,
    pub accepted_case: bool,
    pub improves_upon_baseline: bool,
    pub regression_risk: bool,
    pub reason: String,
}

impl Program for WorkflowCandidateRolloutEvaluatorProgram {
    type Output = WorkflowCandidateRolloutEvaluatorOutput;

    fn name(&self) -> &'static str {
        "workflow_candidate_rollout_evaluator"
    }

    fn description(&self) -> &'static str {
        "Evaluate a workflow frontier candidate against a single workflow rollout case."
    }

    fn signature(&self) -> Signature {
        Signature::new("Perform single-case rollout evaluation for a workflow frontier candidate.")
            .input("candidate kind", "patch or merge.")
            .input(
                "rollout target workflow spec",
                "Target workflow spec after the candidate was actually applied.",
            )
            .input(
                "rollout result",
                "Summary of the candidate's actual application result in an isolated workflow store.",
            )
            .input("target reflection", "Target workflow reflection.")
            .input(
                "target rollout case",
                "A concrete target workflow rollout case containing run record, executed steps, and boundary events.",
            )
            .input(
                "source workflow spec",
                "Source workflow spec for merges; otherwise none.",
            )
            .input(
                "source reflection",
                "Source reflection for merges; otherwise none.",
            )
            .input(
                "source rollout case",
                "Source rollout case for merges; otherwise none. It also contains run record, executed steps, and boundary events.",
            )
            .input("candidate", "Patch or merge candidate to evaluate.")
            .output("score", "Overall score for this case.")
            .output("accepted_case", "Whether this case supports keeping the candidate.")
            .output("improves_upon_baseline", "Whether it improves upon the current baseline.")
            .output("regression_risk", "Whether there is obvious regression risk.")
            .output("reason", "Rationale based on the case.")
            .rule("A patch candidate must improve a concrete weakness shown by this case.")
            .rule("A merge candidate must preserve compatible task boundaries and process skeleton on this case.")
    }
}

impl WorkflowCandidateRolloutEvaluatorProgram {
    #[allow(clippy::too_many_arguments)]
    pub fn dataset_ir(
        &self,
        candidate_kind: String,
        rollout_target_workflow_spec: String,
        rollout_result: String,
        target_reflection_json: String,
        target_rollout_case_json: String,
        source_workflow_spec: String,
        source_reflection_json: String,
        source_rollout_case_json: String,
        candidate_json: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(WORKFLOW_CANDIDATE_ROLLOUT_EVALUATOR_SYSTEM_PROMPT);
        ir.push_instruction(
            "The goal is to evaluate candidate behavior on a concrete rollout case; rollout target workflow spec is the real result after applying the candidate.",
        );
        ir.push_section("candidate kind", candidate_kind);
        ir.push_section("rollout target workflow spec", rollout_target_workflow_spec);
        ir.push_section("rollout result", rollout_result);
        ir.push_section("target reflection", target_reflection_json);
        ir.push_section("target rollout case", target_rollout_case_json);
        ir.push_section("source workflow spec", source_workflow_spec);
        ir.push_section("source reflection", source_reflection_json);
        ir.push_section("source rollout case", source_rollout_case_json);
        ir.push_section("candidate", candidate_json);
        ir
    }
}
