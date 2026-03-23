use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    reasoning::{ir::PromptIR, program::Program, signature::Signature},
    snapshot::Snapshot,
};

const SYSTEM_PROMPT: &str = r#"你正在判断一个训练任务当前是否还应继续探索、已经可以开始修改、应该进入验证、还是已经完成。
你不能发明不存在的结果，只能基于当前步骤、终端状态和任务理解做保守判断。"#;

fn trim_lines(text: &str, max_lines: usize) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() <= max_lines {
        return text.to_string();
    }
    lines[lines.len().saturating_sub(max_lines)..].join("\n")
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CompletionJudgeOutput {
    pub state: String,
    pub reason: String,
    pub next_check: Option<String>,
}

pub struct CompletionJudgeProgram {
    pub task_goal: String,
    pub done_criteria: Vec<String>,
    pub key_anchors: Vec<String>,
    pub investigation_plan: Vec<String>,
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
            .input("关键锚点", "已知的路径、函数、参数、报错、协议等关键信号。")
            .input("调查计划", "优先调查步骤。")
            .input("最近步骤", "最近几步做了什么。")
            .input("当前终端状态", "终端当前显示与最近观察。")
            .input("验证摘要", "已有 validation 结果；若没有则写 none。")
            .output("state", "investigate / change / verify / finish / blocked")
            .output("reason", "为什么这样判断。")
            .output("next_check", "如果还不能完成，最值得做的下一项检查。")
            .rule("只有在已经满足 done criteria 时才输出 finish。")
            .rule("如果只是理解清楚了修改点但还没修改，应输出 change。")
            .rule("如果修改已完成且应跑测试/验证，应输出 verify。")
            .rule("如果安装依赖、构建环境或运行测试的命令仍在自然执行，应保持 verify，而不是 blocked。")
    }

    fn build_ir(&self, _: &Context, _: &Snapshot) -> PromptIR {
        let mut ir = PromptIR::with_system(SYSTEM_PROMPT);
        ir.push_instruction("保守判断，不要因为看到了可疑代码就直接判 finish。");
        ir.push_instruction("使用通用工作阶段，不要输出领域专用术语。investigate 表示继续调查，change 表示开始做实质修改，verify 表示应进入验证，finish 表示可以收尾。");
        ir.push_instruction("如果最近步骤已经定位到明确文件、函数、参数逻辑，且后续只是在同一片区域重复 grep/head/cat，应优先判为 change，而不是继续 investigate。");
        ir.push_instruction("done criteria 用于判断 finish，不应用来阻止进入 change。只要修改点和修改条件已经足够清楚，即可进入 change。");
        ir.push_instruction("不是所有任务都需要 change 或 verify。若任务目标本质上是资料搜集、判断、总结或回复，且当前证据已足够支撑最终结论，可以直接输出 finish。");
        ir.push_instruction("只有在当前终端或验证摘要明确显示不可恢复的错误、权限阻塞、缺失关键前提且没有合理下一步时，才输出 blocked。");
        ir.push_instruction("如果最近步骤只是添加 TODO、注释、占位测试文件或其他不改变真实行为的伪修改，不应把它视为已完成修改；此时仍应保持 change，直到出现真实代码变更。");
        ir.push_instruction("如果当前终端正在执行 apt-get、pip install、pytest、tox、nox、uv、poetry install、python -m venv 等安装/构建/测试命令，且还未回到 shell prompt，应优先输出 verify，并要求继续等待。");
        ir.push_instruction("如果测试失败只是提示缺少依赖或环境未就绪，但已有明确补救动作正在执行，也应保持 verify，而不是 blocked。");
        ir.push_section("任务目标", self.task_goal.clone());
        ir.push_section("完成标准", self.done_criteria.join("\n"));
        if !self.key_anchors.is_empty() {
            ir.push_section("关键锚点", self.key_anchors.join("\n"));
        }
        if !self.investigation_plan.is_empty() {
            ir.push_section("调查计划", self.investigation_plan.join("\n"));
        }
        let recent_steps = self
            .recent_steps
            .iter()
            .rev()
            .take(3)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        ir.push_section("最近步骤", recent_steps);
        ir.push_section("当前终端状态", trim_lines(&self.current_terminal, 60));
        ir.push_section("验证摘要", self.validation_summary.clone());
        ir
    }
}
