use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const PROMPT_EVOLUTION_PLANNER_SYSTEM_PROMPT: &str = r#"You are responsible for sleep-time optimization planning for the runtime system prompt.
Your task is not to directly edit the prompt. Instead, based on failure patterns, produce structured:
1. reflections
2. candidate prompt patches
3. candidate evaluations

Requirements:
- Diagnose failure modes first, then propose candidate patches, then evaluate candidates.
- Each patch must be a stable, transferable runtime rule, not a case-specific description.
- Each evaluation must explicitly state which candidate should be selected.
- If no reliable patch exists, output empty candidates and empty evaluations."#;

pub struct PromptEvolutionPlannerProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PromptPlannerReflection {
    pub title: String,
    pub rationale: String,
    #[serde(default)]
    pub missing_instructions: Vec<String>,
    #[serde(default)]
    pub over_constraints: Vec<String>,
    #[serde(default)]
    pub source_pattern_ids: Vec<String>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PromptPlannerCandidate {
    pub title: String,
    pub rationale: String,
    #[serde(default)]
    pub prompt_patches: Vec<String>,
    #[serde(default)]
    pub source_reflection_titles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PromptPlannerCandidateEvaluation {
    pub candidate_title: String,
    pub rationale: String,
    pub score: f64,
    pub accepted: bool,
    pub selected: bool,
    pub regressions_detected: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PromptEvolutionPlannerOutput {
    #[serde(default)]
    pub reflections: Vec<PromptPlannerReflection>,
    #[serde(default)]
    pub candidates: Vec<PromptPlannerCandidate>,
    #[serde(default)]
    pub evaluations: Vec<PromptPlannerCandidateEvaluation>,
}

impl Program for PromptEvolutionPlannerProgram {
    type Output = PromptEvolutionPlannerOutput;

    fn name(&self) -> &'static str {
        "prompt_evolution_planner"
    }

    fn description(&self) -> &'static str {
        "Generate reflections, candidates, and evaluations for the runtime system prompt from failure patterns extracted from runtime traces."
    }

    fn signature(&self) -> Signature {
        Signature::new(
            "Generate reflection-based sleep optimization planning for the runtime system prompt.",
        )
        .input(
            "current system additions",
            "Current evolvable additions in the runtime system prompt.",
        )
        .input(
            "failure patterns",
            "Failure patterns extracted from traces.",
        )
        .output(
            "reflections",
            "Structured reflections over the failure patterns.",
        )
        .output(
            "candidates",
            "Prompt patch candidates generated from reflections.",
        )
        .output(
            "evaluations",
            "Evaluation results for each candidate, explicitly marking selected.",
        )
        .rule(
            "Reflections should precede candidates, and evaluations should cover every candidate.",
        )
        .rule("At most one candidate may have selected=true.")
        .rule("If there are no candidates, output empty evaluations.")
    }
}

impl PromptEvolutionPlannerProgram {
    pub fn dataset_ir(
        &self,
        current_system_additions: String,
        failure_patterns_json: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(PROMPT_EVOLUTION_PLANNER_SYSTEM_PROMPT);
        ir.push_instruction("Prefer the smallest effective incremental rules.");
        ir.push_instruction("Do not duplicate semantically equivalent rules already present in current system additions.");
        ir.push_instruction(
            "If multiple failure patterns can be covered by one rule, merge them into a more stable candidate.",
        );
        ir.push_section("current system additions", current_system_additions);
        ir.push_section("failure patterns", failure_patterns_json);
        ir
    }
}
