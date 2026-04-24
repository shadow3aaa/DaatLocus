use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const WORKFLOW_EVOLUTION_PLANNER_SYSTEM_PROMPT: &str = r#"You are responsible for sleep-time optimization planning for a single workflow.
Based on the workflow spec and its corresponding WorkflowRunRecord evidence, produce:
1. one structured reflection
2. patch candidates
3. evaluations for those patch candidates

Requirements:
- Diagnose which workflow sections are insufficient before proposing patch candidates.
- Patches must express incremental spec changes only; do not rewrite the entire workflow.
- Evaluations must explicitly state which candidate should be selected.
- If the current workflow is not worth changing, output should_optimize=false."#;

pub struct WorkflowEvolutionPlannerProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowPlannerReflection {
    pub rationale: String,
    #[serde(default)]
    pub missing_preconditions: Vec<String>,
    #[serde(default)]
    pub weak_workflow_steps: Vec<String>,
    #[serde(default)]
    pub weak_done_criteria: Vec<String>,
    #[serde(default)]
    pub weak_recovery: Vec<String>,
    #[serde(default)]
    pub recurring_failure_patterns: Vec<String>,
    pub confidence: f64,
}

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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowPlannerCandidateEvaluation {
    pub candidate_title: String,
    pub rationale: String,
    pub score: f64,
    pub accepted: bool,
    pub selected: bool,
}

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
        "Generate reflection, patch candidates, and evaluations from a workflow spec and run evidence."
    }

    fn signature(&self) -> Signature {
        Signature::new("Generate sleep optimization planning for a single workflow.")
            .input("workflow id", "Current workflow id.")
            .input("workflow spec", "Current workflow spec.")
            .input(
                "workflow run evidence",
                "Recent run-level evidence for this workflow.",
            )
            .output(
                "should_optimize",
                "Whether the current workflow is worth optimizing.",
            )
            .output(
                "reflection",
                "Structured reflection on weaknesses in the workflow spec.",
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
        let mut ir = PromptIR::with_system(WORKFLOW_EVOLUTION_PLANNER_SYSTEM_PROMPT);
        ir.push_instruction(
            "Focus on the preconditions, workflow steps, done criteria, and recovery sections.",
        );
        ir.push_instruction("Patch candidates should be small and precise; do not repeat the same meaning across multiple sections.");
        ir.push_instruction(
            "If the evidence shows the workflow itself is stable, set should_optimize=false.",
        );
        ir.push_section("workflow id", workflow_id);
        ir.push_section("workflow spec", workflow_spec_markdown);
        ir.push_section("workflow run evidence", workflow_run_evidence_json);
        ir
    }
}
