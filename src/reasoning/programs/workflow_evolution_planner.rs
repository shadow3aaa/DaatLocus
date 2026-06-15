use daat_locus_macros::model_schema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{
    ir::PromptIR,
    program::Program,
    prompts::{
        PROGRAM_WORKFLOW_EVOLUTION_PLANNER_INSTRUCTIONS, PROGRAM_WORKFLOW_EVOLUTION_PLANNER_SYSTEM,
        prompt_bullet_lines,
    },
    signature::Signature,
};

pub struct WorkflowEvolutionPlannerProgram;

#[model_schema]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowPlannerReflection {
    pub rationale: String,
    #[serde(default)]
    pub missing_preconditions: Vec<String>,
    #[serde(default)]
    pub weak_primitive_steps: Vec<String>,
    #[serde(default)]
    pub weak_done_criteria: Vec<String>,
    #[serde(default)]
    pub weak_recovery: Vec<String>,
    #[serde(default)]
    pub recurring_failure_patterns: Vec<String>,
    pub confidence: f64,
}

#[model_schema]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowPlannerPatchCandidate {
    pub title: String,
    pub rationale: String,
    #[serde(default)]
    pub when_to_use_additions: Vec<String>,
    #[serde(default)]
    pub precondition_additions: Vec<String>,
    #[serde(default)]
    pub workflow_step_additions: Vec<String>,
    #[serde(default)]
    pub done_criteria_additions: Vec<String>,
    #[serde(default)]
    pub recovery_additions: Vec<String>,
    pub confidence: f64,
}

#[model_schema]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowPlannerCandidateEvaluation {
    pub candidate_title: String,
    pub rationale: String,
    pub score: f64,
    pub accepted: bool,
    pub selected: bool,
}

#[model_schema]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowEvolutionPlannerOutput {
    pub should_optimize: bool,
    pub reflection: WorkflowPlannerReflection,
    #[serde(default)]
    pub patch_candidates: Vec<WorkflowPlannerPatchCandidate>,
    #[serde(default)]
    pub evaluations: Vec<WorkflowPlannerCandidateEvaluation>,
}

impl Program for WorkflowEvolutionPlannerProgram {
    type Output = WorkflowEvolutionPlannerOutput;

    fn name(&self) -> &'static str {
        "workflow_evolution_planner"
    }

    fn description(&self) -> &'static str {
        "Generate reflection, patch candidates, and evaluations from a primitive spec and run evidence."
    }

    fn signature(&self) -> Signature {
        Signature::new("Generate sleep optimization planning for a single SOP primitive.")
            .input("workflow id", "Current primitive id.")
            .input("primitive spec", "Current primitive spec.")
            .input(
                "workflow run evidence",
                "Recent run-level evidence for this primitive.",
            )
            .output(
                "should_optimize",
                "Whether the current primitive is worth optimizing.",
            )
            .output(
                "reflection",
                "Structured reflection on weaknesses in the primitive spec.",
            )
            .output(
                "patch_candidates",
                "Patch candidates generated from the reflection.",
            )
            .output(
                "evaluations",
                "Evaluation results for patch candidates, explicitly marking selected.",
            )
            .rule("At most one patch candidate may have selected=true.")
            .rule("If should_optimize=false, patch_candidates and evaluations should be empty.")
    }
}

impl WorkflowEvolutionPlannerProgram {
    pub fn dataset_ir(
        &self,
        workflow_id: String,
        workflow_spec_markdown: String,
        workflow_run_evidence_json: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(PROGRAM_WORKFLOW_EVOLUTION_PLANNER_SYSTEM);
        for instruction in prompt_bullet_lines(PROGRAM_WORKFLOW_EVOLUTION_PLANNER_INSTRUCTIONS) {
            ir.push_instruction(instruction);
        }
        ir.push_section("workflow id", workflow_id);
        ir.push_section("primitive spec", workflow_spec_markdown);
        ir.push_section("workflow run evidence", workflow_run_evidence_json);
        ir
    }
}
