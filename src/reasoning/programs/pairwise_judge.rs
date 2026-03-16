use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    reasoning::{ir::PromptIR, program::Program, signature::Signature},
    snapshot::Snapshot,
};

const JUDGE_SYSTEM_PROMPT: &str = r#"你现在不是执行者，而是评审器。
你的任务是比较两个候选输出，判断哪个更好地满足给定 case 的目标和 rubric。
你不能发明第三个候选，也不能改写输入。你只能在 A、B、Tie 三者中选择其一。
如果两者都明显满足要求且没有实质差异，应输出 Tie。
优先依据：是否更符合 case 意图、是否更直接、是否更少引入不必要复杂度、是否更符合安全与边界约束。"#;

pub struct PairwiseJudgeProgram;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PairwiseWinner {
    A,
    B,
    Tie,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct PairwiseJudgeOutput {
    pub winner: PairwiseWinner,
    pub confidence: f64,
    pub reason: String,
}

impl Program for PairwiseJudgeProgram {
    type Output = PairwiseJudgeOutput;

    fn name(&self) -> &'static str {
        "pairwise_judge"
    }

    fn description(&self) -> &'static str {
        "比较两个候选输出，判断哪一个更符合给定 case 的目标与 rubric。"
    }

    fn signature(&self) -> Signature {
        Signature::new("比较候选 A 和候选 B，选出更优者，或判断两者等价。")
            .input("suite/case", "当前评测的 suite 名称与 case 名称。")
            .input("case context", "该 case 的上下文与输入条件。")
            .input("rubric", "用于区分候选优劣的评审标准。")
            .input("candidate A", "候选 A 的名称与 JSON 输出。")
            .input("candidate B", "候选 B 的名称与 JSON 输出。")
            .output("winner", "只能是 a、b、tie 三者之一。")
            .output("confidence", "0 到 1 之间的置信度。")
            .output("reason", "简短说明为什么这个候选更好，或为什么平手。")
            .rule("不要输出第三种方案。")
            .rule("如果候选间没有实质差异，应输出 tie。")
            .rule("优先比较是否更符合 case 意图与边界，而不是文风。")
    }

    fn build_ir(&self, _context: &Context, _snapshot: &Snapshot) -> PromptIR {
        self.dataset_ir(
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        )
    }
}

impl PairwiseJudgeProgram {
    pub fn dataset_ir(
        &self,
        case_name: String,
        case_context: String,
        rubric: String,
        candidate_a: String,
        candidate_b: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(JUDGE_SYSTEM_PROMPT);
        ir.push_instruction("请严格根据 rubric 评估两个候选输出。");
        ir.push_instruction("如果一方虽然正确，但更直接、更稳、更少不必要动作，可以优先选它。");
        ir.push_instruction("如果两者本质等价，请选择 tie。");
        ir.push_section("suite/case", case_name);
        ir.push_section("case context", case_context);
        ir.push_section("rubric", rubric);
        ir.push_section("candidate A", candidate_a);
        ir.push_section("candidate B", candidate_b);
        ir
    }
}
