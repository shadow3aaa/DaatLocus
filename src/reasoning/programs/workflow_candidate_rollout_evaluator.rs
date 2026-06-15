use daat_locus_macros::model_schema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{
    ir::PromptIR,
    program::Program,
    prompts::{
        PROGRAM_WORKFLOW_CANDIDATE_ROLLOUT_EVALUATOR_INSTRUCTIONS,
        PROGRAM_WORKFLOW_CANDIDATE_ROLLOUT_EVALUATOR_SYSTEM, prompt_bullet_lines,
    },
    signature::Signature,
};

pub struct WorkflowCandidateRolloutEvaluatorProgram;

#[model_schema]
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
        "Evaluate a SOP primitive frontier candidate against a single primitive rollout case."
    }

    fn signature(&self) -> Signature {
        Signature::new("Perform single-case rollout evaluation for a SOP primitive frontier candidate.")
            .input("candidate kind", "patch or merge.")
            .input(
                "rollout target primitive spec",
                "Target primitive spec after the candidate was actually applied.",
            )
            .input(
                "rollout result",
                "Summary of the candidate's actual application result in an isolated primitive store.",
            )
            .input("target reflection", "Target workflow reflection.")
            .input(
                "target rollout case",
                "A concrete target workflow rollout case containing run record, executed steps, and boundary events.",
            )
            .input(
                "source primitive spec",
                "Source primitive spec for merges; otherwise none.",
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
        let mut ir = PromptIR::with_system(PROGRAM_WORKFLOW_CANDIDATE_ROLLOUT_EVALUATOR_SYSTEM);
        for instruction in
            prompt_bullet_lines(PROGRAM_WORKFLOW_CANDIDATE_ROLLOUT_EVALUATOR_INSTRUCTIONS)
        {
            ir.push_instruction(instruction);
        }
        ir.push_section("candidate kind", candidate_kind);
        ir.push_section(
            "rollout target primitive spec",
            rollout_target_workflow_spec,
        );
        ir.push_section("rollout result", rollout_result);
        ir.push_section("target reflection", target_reflection_json);
        ir.push_section("target rollout case", target_rollout_case_json);
        ir.push_section("source primitive spec", source_workflow_spec);
        ir.push_section("source reflection", source_reflection_json);
        ir.push_section("source rollout case", source_rollout_case_json);
        ir.push_section("candidate", candidate_json);
        ir
    }
}
