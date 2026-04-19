use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const PROMPT_CANDIDATE_REPLAY_JUDGE_SYSTEM_PROMPT: &str = r#"你现在负责对一个 runtime prompt candidate 做 replay-style 复评。
你要根据当前 trace 证据、failure patterns、当前 system additions，以及候选 patch 本身，判断这个 candidate 是否仍然值得保留在 frontier 中。

要求：
- 优先判断 candidate 是否覆盖当前主要失败模式。
- 如果 candidate 已经被当前 system additions 吸收，不应给高分。
- 如果 candidate 太宽泛、可能引入回归，应降低分数并提高 regressions_detected。
- 输出 score、accepted、regressions_detected、reason。"#;

pub struct PromptCandidateReplayJudgeProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PromptCandidateReplayJudgeOutput {
    pub score: f64,
    pub accepted: bool,
    pub regressions_detected: usize,
    pub reason: String,
}

impl Program for PromptCandidateReplayJudgeProgram {
    type Output = PromptCandidateReplayJudgeOutput;

    fn name(&self) -> &'static str {
        "prompt_candidate_replay_judge"
    }

    fn description(&self) -> &'static str {
        "基于当前 trace 证据对 runtime prompt candidate 做 replay-style 复评。"
    }

    fn signature(&self) -> Signature {
        Signature::new("对 prompt frontier candidate 做复评。")
            .input(
                "current system additions",
                "当前 runtime system additions。",
            )
            .input("candidate", "要复评的 prompt candidate。")
            .input("failure patterns", "当前 sleep 提炼出的 failure patterns。")
            .input("trace evidence", "当前 trace 的证据摘要。")
            .output("score", "candidate 的综合分数。")
            .output("accepted", "该 candidate 是否应保留在 frontier 中。")
            .output("regressions_detected", "预估的回归风险数量。")
            .output("reason", "复评依据。")
            .rule("如果 candidate 已被 current system additions 完全吸收，不应给高分。")
            .rule("如果 candidate 不能覆盖当前主要 failure patterns，不应 accepted。")
    }
}

impl PromptCandidateReplayJudgeProgram {
    pub fn dataset_ir(
        &self,
        current_system_additions: String,
        candidate_json: String,
        failure_patterns_json: String,
        trace_evidence_summary: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(PROMPT_CANDIDATE_REPLAY_JUDGE_SYSTEM_PROMPT);
        ir.push_instruction("只根据当前证据判断 candidate 的保留价值，不要臆造额外上下文。");
        ir.push_section("current system additions", current_system_additions);
        ir.push_section("candidate", candidate_json);
        ir.push_section("failure patterns", failure_patterns_json);
        ir.push_section("trace evidence", trace_evidence_summary);
        ir
    }
}
