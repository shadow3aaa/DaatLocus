use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const WORKFLOW_MERGE_PLANNER_SYSTEM_PROMPT: &str = r#"You are responsible for judging whether two workflows should be merged.
Based on the two workflow specs, their reflections, and run evidence, output:
1. should_merge
2. merge rationale
3. confidence
4. accepted / selected

Requirements:
- Set should_merge=true only when the two workflows actually describe the same kind of reusable process.
- Do not rely on surface wording similarity; compare task boundaries, failure modes, and process skeleton compatibility.
- Explicitly reject high-risk merges."#;

pub struct WorkflowMergePlannerProgram;

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
        "Judge whether two workflows should be merged based on their specs, reflections, and evidence."
    }

    fn signature(&self) -> Signature {
        Signature::new("Judge whether two workflows should be merged.")
            .input("target workflow id", "Candidate target workflow id.")
            .input("target workflow spec", "Target workflow spec.")
            .input("target workflow reflection", "Target workflow reflection.")
            .input("target run evidence", "Run evidence for the target workflow.")
            .input("source workflow id", "Candidate source workflow id.")
            .input("source workflow spec", "Source workflow spec.")
            .input("source workflow reflection", "Source workflow reflection.")
            .input("source run evidence", "Run evidence for the source workflow.")
            .output("should_merge", "Whether these two workflows should be merged.")
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
        let mut ir = PromptIR::with_system(WORKFLOW_MERGE_PLANNER_SYSTEM_PROMPT);
        ir.push_instruction("Do not merge because wording is similar; confirm convergence in task boundaries and process structure.");
        ir.push_instruction("Reject the merge if the workflows only share recovery tactics or local steps but have different overall purposes.");
        ir.push_section("target workflow id", target_workflow_id);
        ir.push_section("target workflow spec", target_workflow_spec);
        ir.push_section("target workflow reflection", target_workflow_reflection);
        ir.push_section("target run evidence", target_run_evidence);
        ir.push_section("source workflow id", source_workflow_id);
        ir.push_section("source workflow spec", source_workflow_spec);
        ir.push_section("source workflow reflection", source_workflow_reflection);
        ir.push_section("source run evidence", source_run_evidence);
        ir
    }
}
