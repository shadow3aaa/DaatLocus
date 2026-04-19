use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const WORKFLOW_CANDIDATE_ROLLOUT_EVALUATOR_SYSTEM_PROMPT: &str = r#"你现在负责对 workflow frontier candidate 做单 case rollout 评估。
候选可能是 patch，也可能是 merge。你会看到：
- rollout 后的 target workflow spec
- rollout result summary
- target reflection
- 一个具体 target rollout case，其中包含 flushed run record、executed steps 和 boundary events
- 如果是 merge，还会看到 source workflow spec、source reflection 和一个 source rollout case
- candidate 本身

你的任务是判断：
- 在这个具体 case 上，candidate 是否可能优于当前基线
- 是否存在明显的回归风险

输出要求：
- `score`：该 case 上的综合分数
- `accepted_case`：该 case 是否支持保留该 candidate
- `improves_upon_baseline`：是否优于当前基线
- `regression_risk`：是否存在明显回归风险
- `reason`：基于该 case 的判断依据"#;

pub struct WorkflowCandidateRolloutEvaluatorProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowCandidateRolloutEvaluatorOutput {
    pub score: f64,
    pub accepted_case: bool,
    pub improves_upon_baseline: bool,
    pub regression_risk: bool,
    pub reason: String,
}

impl Program for WorkflowCandidateRolloutEvaluatorProgram {
    type Output = WorkflowCandidateRolloutEvaluatorOutput;

    fn name(&self) -> &'static str {
        "workflow_candidate_rollout_evaluator"
    }

    fn description(&self) -> &'static str {
        "基于单个 workflow rollout case 对 workflow frontier candidate 做评估。"
    }

    fn signature(&self) -> Signature {
        Signature::new("对 workflow frontier candidate 做单 case rollout 评估。")
            .input("candidate kind", "patch 或 merge。")
            .input(
                "rollout target workflow spec",
                "candidate 真正应用后的 target workflow spec。",
            )
            .input(
                "rollout result",
                "candidate 在隔离 workflow store 中的真实应用结果摘要。",
            )
            .input("target reflection", "target workflow reflection。")
            .input(
                "target rollout case",
                "一个具体 target workflow rollout case，包含 run record、executed steps 和 boundary events。",
            )
            .input(
                "source workflow spec",
                "merge 时的 source workflow spec；否则写 none。",
            )
            .input(
                "source reflection",
                "merge 时的 source reflection；否则写 none。",
            )
            .input(
                "source rollout case",
                "merge 时的 source rollout case；否则写 none。source rollout case 同样包含 run record、executed steps 和 boundary events。",
            )
            .input("candidate", "要评估的 patch 或 merge candidate。")
            .output("score", "该 case 上的综合分数。")
            .output("accepted_case", "该 case 是否支持保留 candidate。")
            .output("improves_upon_baseline", "是否优于当前基线。")
            .output("regression_risk", "是否存在明显回归风险。")
            .output("reason", "基于 case 的判断依据。")
            .rule("patch candidate 必须在该 case 的具体弱点上体现改进。")
            .rule("merge candidate 必须在该 case 上保持任务边界与流程骨架兼容。")
    }
}

impl WorkflowCandidateRolloutEvaluatorProgram {
    #[allow(clippy::too_many_arguments)]
    pub fn dataset_ir(
        &self,
        candidate_kind: String,
        rollout_target_workflow_spec: String,
        rollout_result: String,
        target_reflection_json: String,
        target_rollout_case_json: String,
        source_workflow_spec: String,
        source_reflection_json: String,
        source_rollout_case_json: String,
        candidate_json: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(WORKFLOW_CANDIDATE_ROLLOUT_EVALUATOR_SYSTEM_PROMPT);
        ir.push_instruction(
            "当前目标是评估 candidate 在具体 rollout case 上的表现；rollout target workflow spec 已经是真实应用 candidate 后得到的结果。",
        );
        ir.push_section("candidate kind", candidate_kind);
        ir.push_section("rollout target workflow spec", rollout_target_workflow_spec);
        ir.push_section("rollout result", rollout_result);
        ir.push_section("target reflection", target_reflection_json);
        ir.push_section("target rollout case", target_rollout_case_json);
        ir.push_section("source workflow spec", source_workflow_spec);
        ir.push_section("source reflection", source_reflection_json);
        ir.push_section("source rollout case", source_rollout_case_json);
        ir.push_section("candidate", candidate_json);
        ir
    }
}
