use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    memory::{L3EntryKind, L3EntryStability},
    reasoning::{ir::PromptIR, program::Program, signature::Signature},
    snapshot::Snapshot,
};

const SLEEP_PROMOTER_SYSTEM_PROMPT: &str = r#"你现在处于睡眠整理阶段。
你的任务不是继续执行动作，而是判断一个 failure pattern 是否值得提升为长期习得经验（L3 memory）。

只有同时满足这些条件时才 promote:
1. 这不是一次偶然噪声，而是具有重复性或明显模式。
2. 它可以抽象成对未来有帮助的经验，而不是只适用于某个具体 case。
3. 它能指导未来决策或避免错误。

如果只是临时接口波动、一次性错误、无法泛化的细节，就不要 promote。
输出要简短、抽象、可复用。"#;

pub struct SleepL3PromoterProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SleepL3PromoterOutput {
    pub promote: bool,
    pub kind: L3EntryKind,
    pub stability: L3EntryStability,
    pub lesson: String,
    pub evidence_summary: String,
    pub retrieval_text: String,
    pub confidence: f64,
    pub reason: String,
}

impl Program for SleepL3PromoterProgram {
    type Output = SleepL3PromoterOutput;

    fn name(&self) -> &'static str {
        "sleep_l3_promoter"
    }

    fn description(&self) -> &'static str {
        "判断一个 sleep artifact failure pattern 是否应提升为 memory.L3 的长期经验。"
    }

    fn signature(&self) -> Signature {
        Signature::new("判断 failure pattern 是否值得提升成长期习得经验。")
            .input("suite", "failure pattern 所属 suite。")
            .input("pattern id", "pattern 的稳定标识。")
            .input("pattern description", "pattern 的人类可读说明。")
            .input("frequency", "pattern 在运行中出现的频率。")
            .input("severity", "pattern 的严重程度。")
            .input("suggested fix kind", "当前 sleep artifact 推荐的修复方向。")
            .input("supporting traces", "支持该 pattern 的 trace id 列表。")
            .output("promote", "是否应提升成 L3 经验。")
            .output("kind", "这条经验属于哪类长期经验。")
            .output("stability", "这条经验目前的稳定度。")
            .output("lesson", "一句话长期经验。")
            .output("evidence_summary", "这条经验基于哪些现象。")
            .output("retrieval_text", "供未来检索和拼接进快照的文本。")
            .output("confidence", "0 到 1 之间的置信度。")
            .output("reason", "为什么 promote 或不 promote。")
            .rule("只有能泛化的经验才 promote。")
            .rule("lesson 必须是未来可复用的策略或知识，不要复述原始错误。")
            .rule("如果不 promote，也要给出 reason。")
    }

    fn build_ir(&self, _context: &Context, _snapshot: &Snapshot) -> PromptIR {
        self.dataset_ir(
            String::new(),
            String::new(),
            String::new(),
            0,
            0,
            String::new(),
            String::new(),
        )
    }
}

impl SleepL3PromoterProgram {
    pub fn dataset_ir(
        &self,
        suite: String,
        pattern_id: String,
        description: String,
        frequency: usize,
        severity: u8,
        suggested_fix_kind: String,
        supporting_traces: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(SLEEP_PROMOTER_SYSTEM_PROMPT);
        ir.push_instruction("优先提升可迁移、可复用、可指导未来动作的经验。");
        ir.push_instruction("如果只是一次性 provider 波动或偶然 parse 细节，就不要提升为 L3。");
        ir.push_instruction("lesson 和 retrieval_text 必须面向未来，不要停留在 case 名称级别。");
        ir.push_instruction("如果 failure pattern 来自训练任务或真实运行中非常明确的高质量失败，即使 frequency=1，也可以提升为 Tentative；不要机械地因为只出现一次就拒绝。");
        ir.push_instruction("对于依赖安装未完成就提前验证、交互式命令误判、重复低增益调查、错误环境准备这类明显可迁移的失败，应优先考虑提升到 L3。");
        ir.push_instruction("只有当 pattern 明显只是格式噪声、偶发反序列化抖动或无法指导未来动作时，才因为单次出现而拒绝提升。");
        ir.push_section("suite", suite);
        ir.push_section("pattern id", pattern_id);
        ir.push_section("pattern description", description);
        ir.push_section("frequency", frequency.to_string());
        ir.push_section("severity", severity.to_string());
        ir.push_section("suggested fix kind", suggested_fix_kind);
        ir.push_section("supporting traces", supporting_traces);
        ir
    }
}
