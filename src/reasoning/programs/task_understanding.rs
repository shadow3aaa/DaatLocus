use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    reasoning::{ir::PromptIR, program::Program, signature::Signature},
    snapshot::Snapshot,
};

const SYSTEM_PROMPT: &str = r#"你正在把一个原始训练任务压缩成可执行的理解结果。
目标不是重复整段 issue，而是明确：
- 当前主线要解决什么
- 接下来优先查什么
- 哪些锚点不能丢
- 什么迹象说明已经可以开始修改
- 什么迹象说明可以进入验证或完成"#;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskUnderstandingOutput {
    pub thread_focus: String,
    pub task_goal: String,
    pub key_anchors: Vec<String>,
    pub investigation_plan: Vec<String>,
    pub done_criteria: Vec<String>,
}

pub struct TaskUnderstandingProgram {
    pub title: String,
    pub instruction: String,
    pub success_criteria: Vec<String>,
    pub metadata: Vec<String>,
}

impl Program for TaskUnderstandingProgram {
    type Output = TaskUnderstandingOutput;

    fn name(&self) -> &'static str {
        "task_understanding"
    }

    fn description(&self) -> &'static str {
        "把原始训练任务压缩成可执行的主线、调查计划和完成标准。"
    }

    fn signature(&self) -> Signature {
        Signature::new("理解一个原始训练任务，并将其压缩成可执行计划。")
            .input("任务标题", "任务标题或实例 id。")
            .input("原始任务", "完整的原始任务描述。")
            .input("成功标准", "训练任务自带的 success criteria。")
            .input("任务元信息", "repo、base_commit 等环境信息。")
            .output("thread_focus", "持续推进的任务主线。")
            .output("task_goal", "用一两句话说明最终要达成什么。")
            .output(
                "key_anchors",
                "后续不能丢失的关键锚点，如路径、函数名、报错、参数。",
            )
            .output("investigation_plan", "优先执行的短步骤计划。")
            .output("done_criteria", "可操作的完成判定，而不是泛泛描述。")
            .rule("不要复述整段 issue。")
            .rule("计划应优先帮助从阅读切到定位、修改、验证。")
            .rule("优先保留路径、函数名、参数、错误信号等锚点。")
    }

    fn build_ir(&self, _: &Context, _: &Snapshot) -> PromptIR {
        let mut ir = PromptIR::with_system(SYSTEM_PROMPT);
        ir.push_instruction("把任务压缩成短而有执行性的理解结果，不要重复原文。");
        ir.push_instruction("investigation_plan 应是 3-5 条可执行步骤。");
        ir.push_instruction("done_criteria 应包含何时该开始 patch、何时该验证、何时可视为完成。");
        ir.push_section("任务标题", self.title.clone());
        ir.push_section("原始任务", self.instruction.clone());
        ir.push_section("成功标准", self.success_criteria.join("\n"));
        ir.push_section("任务元信息", self.metadata.join("\n"));
        ir
    }
}
