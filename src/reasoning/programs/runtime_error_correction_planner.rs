use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const RUNTIME_ERROR_CORRECTION_PLANNER_SYSTEM_PROMPT: &str = r#"You are responsible for sleep-time runtime error correction planning.
Your task is not to directly edit code or workflows. Based only on code-detected RuntimeErrorCase records, produce structured:
1. reflections
2. candidate runtime contract additions
3. candidate evaluations

Scope:
- Correct global runtime contract and tool protocol violations.
- Good candidates are small, stable rules that prevent repeat violations across turns.
- Target invariants such as event completion, app notice completion, tool argument shape, plan contract, terminal continuation, browser reference freshness, retry behavior, and context overflow recovery.

Out of scope:
- Do not infer successful task procedures from positive examples.
- Do not write workflow steps, domain tactics, source-choice rules, style preferences, or task-specific advice.
- Do not guess that ordinary task quality problems belong here.
- Do not use workflow run records or sleep-internal traces as evidence.

If the supplied cases do not support a reliable global runtime contract addition, output empty candidates and empty evaluations."#;

pub struct RuntimeErrorCorrectionPlannerProgram;

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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeErrorCorrectionCandidate {
    pub title: String,
    pub rationale: String,
    #[serde(default)]
    pub runtime_contract_additions: Vec<String>,
    #[serde(default)]
    pub source_reflection_titles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeErrorCorrectionCandidateEvaluation {
    pub candidate_title: String,
    pub rationale: String,
    pub score: f64,
    pub accepted: bool,
    pub selected: bool,
    pub regressions_detected: usize,
}

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
        let mut ir = PromptIR::with_system(RUNTIME_ERROR_CORRECTION_PLANNER_SYSTEM_PROMPT);
        ir.push_instruction(
            "Prefer the smallest effective incremental runtime contract additions.",
        );
        ir.push_instruction("Do not duplicate semantically equivalent rules already present in current runtime contract additions.");
        ir.push_instruction("Only use the supplied RuntimeErrorCase records as evidence.");
        ir.push_instruction("If a case looks like task quality, workflow optimization, or ordinary tool/environment failure, do not create a candidate from it.");
        ir.push_section(
            "current runtime contract additions",
            current_runtime_contract_additions,
        );
        ir.push_section("runtime error cases", runtime_error_cases_json);
        ir
    }
}
