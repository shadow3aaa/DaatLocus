use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    reasoning::{ir::PromptIR, program::Program, signature::Signature},
    snapshot::Snapshot,
};

const SLEEP_ARTIFACT_BUILDER_SYSTEM_PROMPT: &str = r#"你现在处于睡眠整理阶段。
你的任务是把运行期 failure pattern 和相关记忆，转成 compile 可消费的优化产物建议。

你可以生成三类产物：
1. instruction hypothesis
2. bootstrap demo
3. stress case

原则：
- 只在 pattern 具有重复性、可迁移性时生成。
- 优先学习“如何从失败中收敛”的策略，而不是只复述表面现象。
- 优先把可迁移经验落成 bootstrap demo 或 stress case；只有难以 case 化时，才生成 instruction hypothesis。
- 如果 failure pattern 已经给出明确错误对象（如导入错误、路径错误、入口错误、命令未触发），优先生成围绕错误对象收敛的 instruction hypothesis。
- 优先复用给定的 canonical case 名称，不要编造新的 case 名称。
- reference_case_names 应尽量少，但要能覆盖这个 failure pattern。
- 如果没有合适的产物，就把对应 create_* 设为 false。
- 输出必须简洁，面向后续优化，不要复述整段 trace。 "#;

pub struct SleepArtifactBuilderProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SleepArtifactBuilderOutput {
    pub create_instruction_hypothesis: bool,
    pub instruction_text: String,
    pub create_bootstrap_demo: bool,
    pub bootstrap_demo_title: String,
    pub bootstrap_demo_summary: String,
    pub create_stress_case: bool,
    pub stress_case_name: String,
    pub stress_constraints: Vec<String>,
    pub reference_case_names: Vec<String>,
    pub confidence: f64,
    pub reason: String,
}

impl Program for SleepArtifactBuilderProgram {
    type Output = SleepArtifactBuilderOutput;

    fn name(&self) -> &'static str {
        "sleep_artifact_builder"
    }

    fn description(&self) -> &'static str {
        "把 failure pattern 与相关记忆整理成可供 optimize 使用的睡眠产物建议。"
    }

    fn signature(&self) -> Signature {
        Signature::new("根据 failure pattern 生成 instruction/demo/stress 的睡眠优化产物。")
            .input("suite", "pattern 所属 suite。")
            .input("pattern id", "pattern 的稳定标识。")
            .input("pattern description", "pattern 的人类可读说明。")
            .input("frequency", "pattern 出现频率。")
            .input("severity", "pattern 严重程度。")
            .input("suggested fix kind", "当前推荐的修复方向。")
            .input("supporting traces", "支持该 pattern 的 trace id。")
            .input("related memories", "从 L2 检索到的相关记忆。")
            .input(
                "available canonical cases",
                "可引用的 canonical case 名称列表。",
            )
            .output(
                "create_instruction_hypothesis",
                "是否生成 instruction hypothesis。",
            )
            .output("instruction_text", "生成的 instruction hypothesis 文本。")
            .output("create_bootstrap_demo", "是否生成 bootstrap demo。")
            .output("bootstrap_demo_title", "bootstrap demo 标题。")
            .output("bootstrap_demo_summary", "bootstrap demo 的简短摘要。")
            .output("create_stress_case", "是否生成 stress case。")
            .output("stress_case_name", "stress case 名称。")
            .output("stress_constraints", "stress case 的判别性约束。")
            .output("reference_case_names", "应引用的 canonical case 名称。")
            .output("confidence", "0 到 1 之间的置信度。")
            .output("reason", "为什么这样生成。")
            .rule("不要编造不存在的 canonical case 名称。")
            .rule("reference_case_names 最好 1 到 3 个。")
            .rule("如果 pattern 能稳定转成 worked example 或 stress case，就优先生成它们，不要默认生成 instruction hypothesis。")
            .rule("如果 pattern 更适合通过 worked example 修复，就生成 bootstrap demo。")
            .rule("如果 pattern 更适合拉开候选差异，就生成 stress case。")
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
            String::new(),
            String::new(),
        )
    }
}

impl SleepArtifactBuilderProgram {
    #[allow(clippy::too_many_arguments)]
    pub fn dataset_ir(
        &self,
        suite: String,
        pattern_id: String,
        description: String,
        frequency: usize,
        severity: u8,
        suggested_fix_kind: String,
        supporting_traces: String,
        related_memories: String,
        available_canonical_cases: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(SLEEP_ARTIFACT_BUILDER_SYSTEM_PROMPT);
        ir.push_instruction("优先提出最小但有效的优化产物，不要一次生成太多东西。");
        ir.push_instruction("如果没有足够依据，就保守一些。");
        ir.push_section("suite", suite);
        ir.push_section("pattern id", pattern_id);
        ir.push_section("pattern description", description);
        ir.push_section("frequency", frequency.to_string());
        ir.push_section("severity", severity.to_string());
        ir.push_section("suggested fix kind", suggested_fix_kind);
        ir.push_section("supporting traces", supporting_traces);
        ir.push_section("related memories", related_memories);
        ir.push_section("available canonical cases", available_canonical_cases);
        ir
    }
}
