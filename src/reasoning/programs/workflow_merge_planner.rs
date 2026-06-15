use daat_locus_macros::model_schema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{
    ir::PromptIR,
    program::Program,
    prompts::{
        PROGRAM_WORKFLOW_MERGE_PLANNER_INSTRUCTIONS, PROGRAM_WORKFLOW_MERGE_PLANNER_SYSTEM,
        prompt_bullet_lines,
    },
    signature::Signature,
};

pub struct WorkflowMergePlannerProgram;

#[model_schema]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowMergePlannerOutput {
    pub should_merge: bool,
    pub rationale: String,
    pub confidence: f64,
    pub accepted: bool,
    pub selected: bool,
}

impl Program for WorkflowMergePlannerProgram {
    type Output = WorkflowMergePlannerOutput;

    fn name(&self) -> &'static str {
        "workflow_merge_planner"
    }

    fn description(&self) -> &'static str {
        "Judge whether two SOP primitives should be merged based on their specs, reflections, and evidence."
    }

    fn signature(&self) -> Signature {
        Signature::new("Judge whether two SOP primitives should be merged.")
            .input("target workflow id", "Candidate target primitive id.")
            .input("target primitive spec", "Target primitive spec.")
            .input("target primitive reflection", "Target primitive reflection.")
            .input("target run evidence", "Run evidence for the target primitive.")
            .input("source workflow id", "Candidate source primitive id.")
            .input("source primitive spec", "Source primitive spec.")
            .input("source primitive reflection", "Source primitive reflection.")
            .input("source run evidence", "Run evidence for the source primitive.")
            .output("should_merge", "Whether these two primitives should be merged.")
            .output("rationale", "Rationale for merging or rejecting the merge.")
            .output("confidence", "Confidence from 0 to 1.")
            .output("accepted", "Whether this merge candidate is accepted.")
            .output("selected", "Whether this merge candidate is selected for this round.")
            .rule("If should_merge=false, accepted and selected must also be false.")
            .rule("Set should_merge=true only when task boundaries and process skeleton are compatible.")
    }
}

impl WorkflowMergePlannerProgram {
    #[allow(clippy::too_many_arguments)]
    pub fn dataset_ir(
        &self,
        target_workflow_id: String,
        target_workflow_spec: String,
        target_workflow_reflection: String,
        target_run_evidence: String,
        source_workflow_id: String,
        source_workflow_spec: String,
        source_workflow_reflection: String,
        source_run_evidence: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(PROGRAM_WORKFLOW_MERGE_PLANNER_SYSTEM);
        for instruction in prompt_bullet_lines(PROGRAM_WORKFLOW_MERGE_PLANNER_INSTRUCTIONS) {
            ir.push_instruction(instruction);
        }
        ir.push_section("target workflow id", target_workflow_id);
        ir.push_section("target primitive spec", target_workflow_spec);
        ir.push_section("target primitive reflection", target_workflow_reflection);
        ir.push_section("target run evidence", target_run_evidence);
        ir.push_section("source workflow id", source_workflow_id);
        ir.push_section("source primitive spec", source_workflow_spec);
        ir.push_section("source primitive reflection", source_workflow_reflection);
        ir.push_section("source run evidence", source_run_evidence);
        ir
    }
}
