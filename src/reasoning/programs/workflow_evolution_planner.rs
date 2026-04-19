use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const WORKFLOW_EVOLUTION_PLANNER_SYSTEM_PROMPT: &str = r#"你现在负责单个 workflow 的 sleep 优化规划。
你的任务是基于 workflow spec 与对应的 WorkflowRunRecord 证据，产出：
1. 一个结构化 reflection
2. 若干 patch candidates
3. 对 patch candidates 的 evaluations

要求：
- 先诊断 workflow 哪些 section 不足，再提出 patch candidates。
- patch 必须只表达对 spec 的增量修改，不能重写整份 workflow。
- evaluation 必须显式指出哪个 candidate 应被 selected。
- 如果当前 workflow 不值得改，就输出 should_optimize=false。"#;

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
        "基于 workflow spec 与运行证据生成 reflection、patch candidates 与 evaluations。"
    }

    fn signature(&self) -> Signature {
        Signature::new("为单个 workflow 生成 sleep 优化规划。")
            .input("workflow id", "当前 workflow id。")
            .input("workflow spec", "当前 workflow spec。")
            .input(
                "workflow run evidence",
                "该 workflow 的近期 run-level evidence。",
            )
            .output("should_optimize", "当前 workflow 是否值得优化。")
            .output("reflection", "对 workflow spec 弱点的结构化反思。")
            .output("patch_candidates", "基于 reflection 生成的 patch 候选。")
            .output(
                "evaluations",
                "对 patch candidates 的评估结果，必须标出 selected。",
            )
            .rule("selected=true 的 patch candidate 最多一个。")
            .rule("如果 should_optimize=false，则 patch_candidates 和 evaluations 应为空。")
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
            "重点关注 preconditions、workflow steps、done criteria、recovery 四个 section。",
        );
        ir.push_instruction("patch candidates 要小而精，不要把同一意思重复写进多个 section。");
        ir.push_instruction("如果 evidence 说明 workflow 本身稳定，则 should_optimize=false。");
        ir.push_section("workflow id", workflow_id);
        ir.push_section("workflow spec", workflow_spec_markdown);
        ir.push_section("workflow run evidence", workflow_run_evidence_json);
        ir
    }
}
