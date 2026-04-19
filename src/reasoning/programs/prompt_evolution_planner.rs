use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const PROMPT_EVOLUTION_PLANNER_SYSTEM_PROMPT: &str = r#"你现在负责 runtime system prompt 的 sleep 优化规划。
你的任务不是直接修改 prompt，而是基于 failure patterns 产出结构化的：
1. reflections
2. candidate prompt patches
3. candidate evaluations

要求：
- 先诊断失败模式，再给候选 patch，再评估候选。
- patch 必须是稳定、可迁移的运行时规则，不要写成 case 特化描述。
- evaluation 必须显式指出哪个 candidate 应被 selected。
- 如果没有可靠 patch，就输出空 candidates 和空 evaluations。"#;

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
        "基于 runtime trace 提炼的 failure patterns，为 runtime system prompt 生成 reflection、candidates 和 evaluations。"
    }

    fn signature(&self) -> Signature {
        Signature::new("为 runtime system prompt 生成基于反思的 sleep 优化规划。")
            .input(
                "current system additions",
                "当前 runtime system prompt 的可演化 additions。",
            )
            .input(
                "failure patterns",
                "从 trace 中提炼出的 failure pattern 列表。",
            )
            .output("reflections", "针对 failure patterns 的结构化反思结果。")
            .output("candidates", "基于 reflections 生成的 prompt patch 候选。")
            .output(
                "evaluations",
                "对每个 candidate 的评估结果，必须标出 selected。",
            )
            .rule("reflections 应先于 candidates，evaluations 应覆盖每个 candidate。")
            .rule("selected=true 的 candidate 最多一个。")
            .rule("如果没有 candidate，就输出空 evaluations。")
    }
}

impl PromptEvolutionPlannerProgram {
    pub fn dataset_ir(
        &self,
        current_system_additions: String,
        failure_patterns_json: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(PROMPT_EVOLUTION_PLANNER_SYSTEM_PROMPT);
        ir.push_instruction("优先提出最小但有效的增量规则。");
        ir.push_instruction("不要重复 current system additions 中已经存在的同义规则。");
        ir.push_instruction(
            "若多个 failure pattern 可被同一条规则覆盖，应合并为更稳定的 candidate。",
        );
        ir.push_section("current system additions", current_system_additions);
        ir.push_section("failure patterns", failure_patterns_json);
        ir
    }
}
