use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    memory::{L3EntryKind, L3EntryStability},
    reasoning::{ir::PromptIR, program::Program, signature::Signature},
    snapshot::Snapshot,
};

const SLEEP_SUCCESS_PROMOTER_SYSTEM_PROMPT: &str = r#"你现在处于睡眠整理阶段。
你的任务是判断一个成功的 runtime trace 是否值得提升为长期习得经验（L3 memory）。

只有当成功模式具备可迁移性时才 promote：
1. 它表达的是可复用策略，不是一次性的偶然成功。
2. 它能指导未来在类似环境中更稳定地行动。
3. 它可以被压缩成简洁、抽象的 lesson。

不要只是复述这次动作做了什么，要总结“以后应该怎么做”。"#;

pub struct SleepSuccessL3PromoterProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SleepSuccessL3PromoterOutput {
    pub promote: bool,
    pub kind: L3EntryKind,
    pub stability: L3EntryStability,
    pub lesson: String,
    pub evidence_summary: String,
    pub retrieval_text: String,
    pub confidence: f64,
    pub reason: String,
}

impl Program for SleepSuccessL3PromoterProgram {
    type Output = SleepSuccessL3PromoterOutput;

    fn name(&self) -> &'static str {
        "sleep_success_l3_promoter"
    }

    fn description(&self) -> &'static str {
        "判断成功 runtime trace 是否应提升为 memory.L3 的长期成功经验。"
    }

    fn signature(&self) -> Signature {
        Signature::new("判断成功 runtime trace 是否值得提升为长期成功经验。")
            .input("suite", "成功 trace 所属 suite。")
            .input("trace id", "trace 的稳定标识。")
            .input("input summary", "该成功 trace 的输入摘要。")
            .input("output summary", "该成功 trace 的输出摘要。")
            .input("related memories", "从 L2 检索到的相关记忆。")
            .output("promote", "是否应提升成 L3 经验。")
            .output("kind", "经验所属类别。")
            .output("stability", "经验稳定度。")
            .output("lesson", "一句话长期经验。")
            .output("evidence_summary", "这条经验基于什么成功模式。")
            .output("retrieval_text", "供未来检索和拼接进快照的文本。")
            .output("confidence", "0 到 1 之间的置信度。")
            .output("reason", "为什么 promote 或不 promote。")
            .rule("lesson 必须是未来可复用的策略，不要只复述这次 trace。")
            .rule("如果只是一次性命令结果，不要 promote。")
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

impl SleepSuccessL3PromoterProgram {
    pub fn dataset_ir(
        &self,
        suite: String,
        trace_id: String,
        input_summary: String,
        output_summary: String,
        related_memories: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(SLEEP_SUCCESS_PROMOTER_SYSTEM_PROMPT);
        ir.push_instruction("优先提升 shell/IM/project 连续性等可迁移的成功策略。");
        ir.push_instruction("如果这只是一个偶然输出，不要 promote。");
        ir.push_section("suite", suite);
        ir.push_section("trace id", trace_id);
        ir.push_section("input summary", input_summary);
        ir.push_section("output summary", output_summary);
        ir.push_section("related memories", related_memories);
        ir
    }
}
