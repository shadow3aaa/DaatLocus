use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const EVALUATION_ARTIFACT_BUILDER_SYSTEM_PROMPT: &str = r#"You are in the evaluation artifact consolidation stage.
Your task is to convert runtime failure patterns and related memories into optimization artifact proposals that compile can consume.

You may generate three artifact types:
1. instruction hypothesis
2. bootstrap demo
3. stress case

Principles:
- Generate artifacts only when the pattern is repeated and transferable.
- Prefer learning strategies for converging from failure, not merely restating surface symptoms.
- Prefer turning transferable lessons into bootstrap demos or stress cases; generate an instruction hypothesis only when the lesson is hard to case-ify.
- If the failure pattern identifies a concrete error object, such as an import, path, entrypoint, or missing command trigger, prefer an instruction hypothesis centered on converging around that object.
- Reuse provided canonical case names when possible; do not invent new canonical case names.
- Keep reference_case_names small while still covering the failure pattern.
- If no suitable artifact exists, set the corresponding create_* field to false.
- Keep output concise and optimization-oriented. Do not restate the entire trace."#;

pub struct EvaluationArtifactBuilderProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EvaluationArtifactBuilderOutput {
    pub create_instruction_hypothesis: bool,
    pub instruction_text: String,
    pub create_bootstrap_demo: bool,
    pub bootstrap_demo_title: String,
    pub bootstrap_demo_summary: String,
    pub create_stress_case: bool,
    pub stress_case_name: String,
    pub stress_constraints: Vec<String>,
    pub reference_case_names: Vec<String>,
    pub confidence: f64,
    pub reason: String,
}

impl Program for EvaluationArtifactBuilderProgram {
    type Output = EvaluationArtifactBuilderOutput;

    fn name(&self) -> &'static str {
        "evaluation_artifact_builder"
    }

    fn description(&self) -> &'static str {
        "Convert failure patterns and related memories into evaluation artifact proposals for optimization."
    }

    fn signature(&self) -> Signature {
        Signature::new("Generate instruction/demo/stress evaluation artifacts from a failure pattern.")
            .input("suite", "Suite the pattern belongs to.")
            .input("pattern id", "Stable identifier for the pattern.")
            .input("pattern description", "Human-readable pattern description.")
            .input("frequency", "Observed pattern frequency.")
            .input("severity", "Pattern severity.")
            .input("suggested fix kind", "Currently recommended fix direction.")
            .input("supporting traces", "Trace ids supporting this pattern.")
            .input("related memories", "Relevant memories retrieved from L2.")
            .input(
                "available canonical cases",
                "Canonical case names available for reference.",
            )
            .output(
                "create_instruction_hypothesis",
                "Whether to generate an instruction hypothesis.",
            )
            .output("instruction_text", "Generated instruction hypothesis text.")
            .output("create_bootstrap_demo", "Whether to generate a bootstrap demo.")
            .output("bootstrap_demo_title", "Bootstrap demo title.")
            .output("bootstrap_demo_summary", "Short bootstrap demo summary.")
            .output("create_stress_case", "Whether to generate a stress case.")
            .output("stress_case_name", "Stress case name.")
            .output("stress_constraints", "Discriminative stress case constraints.")
            .output("reference_case_names", "Canonical case names to reference.")
            .output("confidence", "Confidence from 0 to 1.")
            .output("reason", "Why this artifact proposal was generated.")
            .rule("Do not invent nonexistent canonical case names.")
            .rule("reference_case_names should usually contain 1 to 3 names.")
            .rule("If the pattern can become a stable worked example or stress case, prefer those and do not default to an instruction hypothesis.")
            .rule("If the pattern is better fixed through a worked example, generate a bootstrap demo.")
            .rule("If the pattern is better at separating candidate behavior, generate a stress case.")
    }
}

impl EvaluationArtifactBuilderProgram {
    #[allow(clippy::too_many_arguments)]
    pub fn dataset_ir(
        &self,
        suite: String,
        pattern_id: String,
        description: String,
        frequency: usize,
        severity: u8,
        suggested_fix_kind: String,
        supporting_traces: String,
        related_memories: String,
        available_canonical_cases: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(EVALUATION_ARTIFACT_BUILDER_SYSTEM_PROMPT);
        ir.push_instruction("Prefer minimal but effective optimization artifacts; do not generate too many at once.");
        ir.push_instruction("Be conservative when evidence is insufficient.");
        ir.push_section("suite", suite);
        ir.push_section("pattern id", pattern_id);
        ir.push_section("pattern description", description);
        ir.push_section("frequency", frequency.to_string());
        ir.push_section("severity", severity.to_string());
        ir.push_section("suggested fix kind", suggested_fix_kind);
        ir.push_section("supporting traces", supporting_traces);
        ir.push_section("related memories", related_memories);
        ir.push_section("available canonical cases", available_canonical_cases);
        ir
    }
}
