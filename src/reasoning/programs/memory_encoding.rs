use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    reasoning::{ir::PromptIR, program::Program, signature::Signature},
    snapshot::Snapshot,
};

const SYSTEM_PROMPT: &str = r#"你正在把一轮执行结果编码成记忆条目。
目标不是复述全部原文，而是保住：
- 持续推进的主线
- 本轮新增事实
- 未来会影响判断的关键锚点
- 这次事件对主线的影响

你必须优先保住 URL、文件名、命令、对象引用和关键错误信号。"#;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MemoryEncodingOutput {
    pub thread_focus: String,
    pub event_summary: String,
    pub anchors: Vec<String>,
    pub thread_effect: String,
}

pub struct MemoryEncodingProgram {
    pub thread_focus: String,
    pub observation: String,
    pub action_description: String,
    pub evidence: String,
}

impl MemoryEncodingProgram {
    fn build_memory_ir(&self) -> PromptIR {
        let mut ir = PromptIR::with_system(SYSTEM_PROMPT);
        ir.push_instruction("`thread_focus` 表示持续推进的主线，不要退化成这一次点了什么按钮。");
        ir.push_instruction("`event_summary` 只总结本轮新增事实与采取动作。");
        ir.push_instruction(
            "`anchors` 只保留少量关键锚点，例如 URL、文件名、命令、对象引用、关键错误。",
        );
        ir.push_instruction(
            "`thread_effect` 只能是 continue / blocked / clarified / switched / completed。",
        );
        ir.push_instruction(
            "如果本轮的关键问题是链接、文件名、命令或对象引用，这些字面量必须保留进 anchors。",
        );
        ir.push_section("当前主线", self.thread_focus.clone());
        ir.push_section("观察与结论", self.observation.clone());
        ir.push_section("采取动作", self.action_description.clone());
        ir.push_section("证据", self.evidence.clone());
        ir
    }
}

impl Program for MemoryEncodingProgram {
    type Output = MemoryEncodingOutput;

    fn name(&self) -> &'static str {
        "memory_encoding"
    }

    fn description(&self) -> &'static str {
        "把一轮执行结果编码成主线清晰、锚点不丢的记忆条目。"
    }

    fn signature(&self) -> Signature {
        Signature::new("把一轮执行结果编码成适合记忆的结构化条目。")
            .input("当前主线", "持续推进的主线，不是单次动作。")
            .input("观察与结论", "本轮观察到的事实与结论。")
            .input("采取动作", "本轮采取了什么动作。")
            .input(
                "证据",
                "本轮关键字面量证据，如 URL、文件名、命令、对象引用。",
            )
            .output("thread_focus", "持续推进的主线。")
            .output("event_summary", "本轮新增事实与动作的总结。")
            .output("anchors", "应保留的关键锚点。")
            .output(
                "thread_effect",
                "continue / blocked / clarified / switched / completed。",
            )
            .rule("不要把 thread_focus 写成一次性动作。")
            .rule("不要丢掉会影响后续判断的 URL、文件名、命令和对象引用。")
            .rule("anchors 只保留少量关键锚点，不要抄整段原文。")
    }

    fn build_ir(&self, _context: &Context, _snapshot: &Snapshot) -> PromptIR {
        self.build_memory_ir()
    }
}
