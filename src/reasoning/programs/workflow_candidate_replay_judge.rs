use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const WORKFLOW_CANDIDATE_REPLAY_JUDGE_SYSTEM_PROMPT: &str = r#"你现在负责对 workflow frontier candidate 做 replay-style 复评。
候选可能是 patch，也可能是 merge。你要根据 workflow spec、reflection、run evidence 和 candidate 本身，判断它是否仍然值得保留在 frontier 中。

要求：
- patch 要看是否仍覆盖当前主要 workflow 弱点。
- merge 要看两个 workflow 在任务边界和流程骨架上是否仍兼容。
- 如果 candidate 已经被当前 workflow spec 吸收，不应给高分。
- 输出 score、accepted、reason。"#;

pub struct WorkflowCandidateReplayJudgeProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowCandidateReplayJudgeOutput {
    pub score: f64,
    pub accepted: bool,
    pub reason: String,
}

impl Program for WorkflowCandidateReplayJudgeProgram {
    type Output = WorkflowCandidateReplayJudgeOutput;

    fn name(&self) -> &'static str {
        "workflow_candidate_replay_judge"
    }

    fn description(&self) -> &'static str {
        "基于 workflow spec / reflection / run evidence 对 workflow frontier candidate 做复评。"
    }

    fn signature(&self) -> Signature {
        Signature::new("对 workflow frontier candidate 做 replay-style 复评。")
            .input("candidate kind", "patch 或 merge。")
            .input("target workflow spec", "target workflow spec。")
            .input("target reflection", "target workflow reflection。")
            .input("target run evidence", "target workflow 的 run 证据。")
            .input(
                "source workflow spec",
                "merge 时的 source workflow spec；否则写 none。",
            )
            .input(
                "source reflection",
                "merge 时的 source reflection；否则写 none。",
            )
            .input(
                "source run evidence",
                "merge 时的 source run evidence；否则写 none。",
            )
            .input("candidate", "要复评的 patch 或 merge candidate。")
            .output("score", "candidate 的综合分数。")
            .output("accepted", "candidate 是否应保留在 frontier 中。")
            .output("reason", "复评依据。")
            .rule("merge candidate 必须在任务边界和流程骨架兼容时才 accepted。")
            .rule("如果 patch 已被 target workflow spec 吸收，不应给高分。")
    }
}

impl WorkflowCandidateReplayJudgeProgram {
    #[allow(clippy::too_many_arguments)]
    pub fn dataset_ir(
        &self,
        candidate_kind: String,
        target_workflow_spec: String,
        target_reflection_json: String,
        target_run_evidence_json: String,
        source_workflow_spec: String,
        source_reflection_json: String,
        source_run_evidence_json: String,
        candidate_json: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(WORKFLOW_CANDIDATE_REPLAY_JUDGE_SYSTEM_PROMPT);
        ir.push_instruction(
            "当前目标是判断 candidate 的 frontier 保留价值，不是直接改写 workflow。",
        );
        ir.push_section("candidate kind", candidate_kind);
        ir.push_section("target workflow spec", target_workflow_spec);
        ir.push_section("target reflection", target_reflection_json);
        ir.push_section("target run evidence", target_run_evidence_json);
        ir.push_section("source workflow spec", source_workflow_spec);
        ir.push_section("source reflection", source_reflection_json);
        ir.push_section("source run evidence", source_run_evidence_json);
        ir.push_section("candidate", candidate_json);
        ir
    }
}
