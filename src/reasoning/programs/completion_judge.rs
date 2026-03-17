use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    reasoning::{ir::PromptIR, program::Program, signature::Signature},
    snapshot::Snapshot,
};

const SYSTEM_PROMPT: &str = r#"你正在判断一个训练任务当前是否还应继续探索、已经可以开始修改、应该进入验证、还是已经完成。
你不能发明不存在的结果，只能基于当前步骤、终端状态和任务理解做保守判断。"#;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CompletionJudgeOutput {
    pub state: String,
    pub reason: String,
    pub next_check: Option<String>,
}

pub struct CompletionJudgeProgram {
    pub task_goal: String,
    pub done_criteria: Vec<String>,
    pub recent_steps: Vec<String>,
    pub current_terminal: String,
    pub validation_summary: String,
}

impl Program for CompletionJudgeProgram {
    type Output = CompletionJudgeOutput;

    fn name(&self) -> &'static str {
        "completion_judge"
    }

    fn description(&self) -> &'static str {
        "判断当前训练任务应继续探索、开始修改、进入验证、完成或阻塞。"
    }

    fn signature(&self) -> Signature {
        Signature::new("判断训练任务当前所处的完成阶段。")
            .input("任务目标", "压缩后的任务目标。")
            .input("完成标准", "done criteria。")
            .input("最近步骤", "最近几步做了什么。")
            .input("当前终端状态", "终端当前显示与最近观察。")
            .input("验证摘要", "已有 validation 结果；若没有则写 none。")
            .output("state", "investigate / change / verify / finish / blocked")
            .output("reason", "为什么这样判断。")
            .output("next_check", "如果还不能完成，最值得做的下一项检查。")
            .rule("只有在已经满足 done criteria 时才输出 finish。")
            .rule("如果只是理解清楚了修改点但还没修改，应输出 change。")
            .rule("如果修改已完成且应跑测试/验证，应输出 verify。")
    }

    fn build_ir(&self, _context: &Context, _snapshot: &Snapshot) -> PromptIR {
        let mut ir = PromptIR::with_system(SYSTEM_PROMPT);
        ir.push_instruction("保守判断，不要因为看到了可疑代码就直接判 finish。");
        ir.push_instruction("使用通用工作阶段，不要输出领域专用术语。investigate 表示继续调查，change 表示开始做实质修改，verify 表示应进入验证，finish 表示可以收尾。");
        ir.push_instruction("如果最近步骤只是重复阅读/grep，应优先判断是否已到 change。");
        ir.push_section("任务目标", self.task_goal.clone());
        ir.push_section("完成标准", self.done_criteria.join("\n"));
        ir.push_section("最近步骤", self.recent_steps.join("\n"));
        ir.push_section("当前终端状态", self.current_terminal.clone());
        ir.push_section("验证摘要", self.validation_summary.clone());
        ir
    }
}
