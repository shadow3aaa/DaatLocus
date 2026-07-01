use daat_locus_macros::model_schema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{
    ir::PromptIR,
    program::Program,
    prompts::{
        PROGRAM_SKILL_IMPROVEMENT_PLANNER_INSTRUCTIONS, PROGRAM_SKILL_IMPROVEMENT_PLANNER_SYSTEM,
        prompt_bullet_lines,
    },
    signature::Signature,
};

pub struct SkillImprovementPlannerProgram;

#[model_schema]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillImprovementReflection {
    pub skill_name: String,
    pub rationale: String,
    #[serde(default)]
    pub weak_steps: Vec<String>,
    #[serde(default)]
    pub missing_guidance: Vec<String>,
    #[serde(default)]
    pub recurring_failure_patterns: Vec<String>,
    pub should_improve: bool,
    pub confidence: f64,
}

#[model_schema]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillImprovementPatch {
    pub title: String,
    pub rationale: String,
    /// Lines to append or clarify in the skill body. These are natural-language
    /// additions that should appear under relevant sections of the SKILL.md.
    #[serde(default)]
    pub additions: Vec<String>,
    pub confidence: f64,
    pub selected: bool,
}

#[model_schema]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillImprovementPlannerOutput {
    pub skill_name: String,
    pub should_improve: bool,
    pub reflection: SkillImprovementReflection,
    #[serde(default)]
    pub patches: Vec<SkillImprovementPatch>,
}

impl Program for SkillImprovementPlannerProgram {
    type Output = SkillImprovementPlannerOutput;

    fn name(&self) -> &'static str {
        "skill_improvement_planner"
    }

    fn description(&self) -> &'static str {
        "Analyze SkillRunRecord evidence for a skill and propose targeted improvements to its SKILL.md."
    }

    fn output_schema(&self) -> serde_json::Value {
        crate::schema_utils::model_schema_for::<SkillImprovementPlannerOutput>()
    }

    fn signature(&self) -> Signature {
        Signature::new(
            "Analyze skill run evidence and suggest targeted improvements to a skill's SKILL.md.",
        )
        .input("skill name", "The name of the skill being analyzed.")
        .input(
            "skill content",
            "The full current content of the skill's SKILL.md.",
        )
        .input(
            "skill run evidence",
            "JSON array of SkillRunRecord entries recording how the skill was used.",
        )
        .output(
            "should_improve",
            "Whether the evidence justifies updating this skill.",
        )
        .output(
            "reflection",
            "Analysis of weaknesses found in the skill content.",
        )
        .output(
            "patches",
            "Proposed additions to improve the skill, with at most one having selected=true.",
        )
        .rule("Only propose additions that are clearly missing from the current skill content.")
        .rule("At most one patch may have selected=true.")
        .rule("If should_improve=false, patches should be empty.")
        .rule("Patches must add reusable operational guidance, not record one-off task details.")
    }
}

impl SkillImprovementPlannerProgram {
    pub fn dataset_ir(
        &self,
        skill_name: String,
        skill_content: String,
        run_evidence_json: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(PROGRAM_SKILL_IMPROVEMENT_PLANNER_SYSTEM);
        for instruction in prompt_bullet_lines(PROGRAM_SKILL_IMPROVEMENT_PLANNER_INSTRUCTIONS) {
            ir.push_instruction(instruction);
        }
        ir.push_section("skill name", skill_name);
        ir.push_section("skill content", skill_content);
        ir.push_section("skill run evidence", run_evidence_json);
        ir
    }
}
