use daat_locus_macros::model_schema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{
    ir::PromptIR,
    program::Program,
    prompts::{
        PROGRAM_RUNTIME_ERROR_CORRECTION_PLANNER_INSTRUCTIONS,
        PROGRAM_RUNTIME_ERROR_CORRECTION_PLANNER_SYSTEM, prompt_bullet_lines,
    },
    signature::Signature,
};

pub struct RuntimeErrorCorrectionPlannerProgram;

#[model_schema]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeErrorCorrectionReflection {
    pub title: String,
    pub rationale: String,
    #[serde(default)]
    pub missing_runtime_contracts: Vec<String>,
    #[serde(default)]
    pub over_constraints: Vec<String>,
    #[serde(default)]
    pub source_case_ids: Vec<String>,
    pub confidence: f64,
}

#[model_schema]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeErrorCorrectionCandidate {
    pub title: String,
    pub rationale: String,
    #[serde(default)]
    pub runtime_contract_additions: Vec<String>,
    #[serde(default)]
    pub source_reflection_titles: Vec<String>,
}

#[model_schema]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeErrorCorrectionCandidateEvaluation {
    pub candidate_title: String,
    pub rationale: String,
    pub score: f64,
    pub accepted: bool,
    pub selected: bool,
    pub regressions_detected: usize,
}

#[model_schema]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeErrorCorrectionPlannerOutput {
    #[serde(default)]
    pub reflections: Vec<RuntimeErrorCorrectionReflection>,
    #[serde(default)]
    pub candidates: Vec<RuntimeErrorCorrectionCandidate>,
    #[serde(default)]
    pub evaluations: Vec<RuntimeErrorCorrectionCandidateEvaluation>,
}

impl Program for RuntimeErrorCorrectionPlannerProgram {
    type Output = RuntimeErrorCorrectionPlannerOutput;

    fn name(&self) -> &'static str {
        "runtime_error_correction_planner"
    }

    fn description(&self) -> &'static str {
        "Generate runtime contract reflections, candidates, and evaluations from code-detected RuntimeErrorCase records."
    }

    fn signature(&self) -> Signature {
        Signature::new("Generate sleep-time runtime error correction planning.")
            .input(
                "current runtime contract additions",
                "Current evolvable additions in the runtime system prompt.",
            )
            .input(
                "runtime error cases",
                "Code-detected RuntimeErrorCase records from daytime runtime execution.",
            )
            .output(
                "reflections",
                "Structured reflections over runtime/tool protocol errors.",
            )
            .output(
                "candidates",
                "Runtime contract addition candidates generated from reflections.",
            )
            .output(
                "evaluations",
                "Evaluation results for each candidate, explicitly marking selected.",
            )
            .rule("Reflections should precede candidates, and evaluations should cover every candidate.")
            .rule("At most one candidate may have selected=true.")
            .rule("If there are no candidates, output empty evaluations.")
            .rule("Every candidate addition must be a global runtime/tool protocol rule, not a workflow or task procedure.")
    }
}

impl RuntimeErrorCorrectionPlannerProgram {
    pub fn dataset_ir(
        &self,
        current_runtime_contract_additions: String,
        runtime_error_cases_json: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(PROGRAM_RUNTIME_ERROR_CORRECTION_PLANNER_SYSTEM);
        for instruction in
            prompt_bullet_lines(PROGRAM_RUNTIME_ERROR_CORRECTION_PLANNER_INSTRUCTIONS)
        {
            ir.push_instruction(instruction);
        }
        ir.push_section(
            "current runtime contract additions",
            current_runtime_contract_additions,
        );
        ir.push_section("runtime error cases", runtime_error_cases_json);
        ir
    }
}
