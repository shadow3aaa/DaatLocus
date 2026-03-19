use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    reasoning::{ir::PromptIR, program::Program, signature::Signature},
    snapshot::Snapshot,
};

const SLEEP_EPISODE_SYNTHESIZER_SYSTEM_PROMPT: &str = r#"你现在处于睡眠整理阶段。
你的任务是阅读一个训练/运行 episode 的结果，并把它抽象成可复用的学习结论。

目标不是复述轨迹，而是提炼：
1. 这次为什么成功或失败
2. 下次遇到类似情况应如何更快收敛
3. 哪些 compile 产物值得生成（demo / instruction / stress）
4. 是否值得保留为长期复盘经验

重要原则：
- 优先把 lesson 落成可验证的 bootstrap demo 或 stress case；只有无法 case 化时，才退回 instruction hypothesis。
- 优先学习“如何围绕明确错误对象收敛”的策略，而不是只记录表面现象。
- 成功是常态，只有当成功模式明显可迁移、可复用、能稳定指导未来任务时，才允许 retain_reflection=true。
- 失败更容易产生长期复盘经验，但也必须抽象成未来可执行的策略，不要停留在 case 名称或日志片段。
- 如果 episode 已经给出明确错误对象（导入错误、路径错误、入口错误、命令未触发、环境缺失），应围绕该错误对象总结下一步策略。
- 输出必须简洁、抽象、可迁移，不要长篇复述整个 trace。 "#;

pub struct SleepEpisodeSynthesizerProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum ReflectionKind {
    TerminalPolicy,
    InteractionBoundary,
    ProjectContinuity,
    ToolUsage,
    FailureAvoidance,
    General,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum ReflectionStability {
    Tentative,
    Stable,
    Canonical,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SleepEpisodeSynthesizerOutput {
    pub synthesized_summary: String,
    pub strategy_lesson: String,
    pub create_failure_pattern: bool,
    pub failure_pattern_summary: String,
    pub suggested_fix_kind: String,
    pub create_instruction_hypothesis: bool,
    pub instruction_text: String,
    pub create_bootstrap_demo: bool,
    pub bootstrap_demo_title: String,
    pub bootstrap_demo_summary: String,
    pub create_stress_case: bool,
    pub stress_case_name: String,
    pub stress_constraints: Vec<String>,
    pub retain_reflection: bool,
    pub reflection_kind: ReflectionKind,
    pub reflection_stability: ReflectionStability,
    pub reflection_lesson: String,
    pub reflection_evidence_summary: String,
    pub reflection_retrieval_text: String,
    pub reflection_confidence: f64,
    pub reason: String,
}

impl Program for SleepEpisodeSynthesizerProgram {
    type Output = SleepEpisodeSynthesizerOutput;

    fn name(&self) -> &'static str {
        "sleep_episode_synthesizer"
    }

    fn description(&self) -> &'static str {
        "把单个成功/失败 episode 抽象成可复用策略，并生成 artifacts 与长期复盘提议。"
    }

    fn signature(&self) -> Signature {
        Signature::new("从单个 episode 结果中抽象学习结论。")
            .input("target suite", "当前 episode 最相关的 suite。")
            .input("episode id", "episode 的稳定标识。")
            .input("outcome status", "Succeeded/Failed/Aborted/MaxStepsExceeded。")
            .input("task goal", "任务目标。")
            .input("done criteria", "任务完成标准。")
            .input("recent steps", "最近关键步骤摘要。")
            .input("final observation", "最终总结和快照摘要。")
            .input("related memories", "相关长期记忆。")
            .output("synthesized_summary", "对 episode 的高层抽象总结。")
            .output("strategy_lesson", "下次应如何行动的可迁移策略。")
            .output("create_failure_pattern", "是否生成失败 pattern。")
            .output("failure_pattern_summary", "失败 pattern 的高层描述。")
            .output("suggested_fix_kind", "demo/instruction/stress_case 之一。")
            .output("create_instruction_hypothesis", "是否生成 instruction hypothesis。")
            .output("instruction_text", "生成的 instruction。")
            .output("create_bootstrap_demo", "是否生成 bootstrap demo。")
            .output("bootstrap_demo_title", "demo 标题。")
            .output("bootstrap_demo_summary", "demo 摘要。")
            .output("create_stress_case", "是否生成 stress case。")
            .output("stress_case_name", "stress case 名称。")
            .output("stress_constraints", "stress case 约束。")
            .output("retain_reflection", "是否保留为长期复盘经验。")
            .output("reflection_kind", "复盘经验类型。")
            .output("reflection_stability", "复盘经验稳定度。")
            .output("reflection_lesson", "一句话复盘经验。")
            .output("reflection_evidence_summary", "复盘证据摘要。")
            .output("reflection_retrieval_text", "供长期记忆检索的文本。")
            .output("reflection_confidence", "0 到 1 的置信度。")
            .output("reason", "为什么这样判断。")
            .rule("成功 episode 默认不要 retain_reflection，除非成功模式明显可迁移且足以稳定指导未来任务。")
            .rule("失败 episode 优先输出收敛策略，不要只复述报错原文。")
            .rule("如果已经有明确错误对象，strategy_lesson 必须围绕该错误对象收敛。")
            .rule("如果一个 lesson 可以稳定落成 demo 或 stress case，就不要只生成 instruction。")
    }

    fn build_ir(&self, _context: &Context, _snapshot: &Snapshot) -> PromptIR {
        self.dataset_ir(
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        )
    }
}

impl SleepEpisodeSynthesizerProgram {
    #[allow(clippy::too_many_arguments)]
    pub fn dataset_ir(
        &self,
        target_suite: String,
        episode_id: String,
        outcome_status: String,
        task_goal: String,
        done_criteria: String,
        recent_steps: String,
        final_observation: String,
        related_memories: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(SLEEP_EPISODE_SYNTHESIZER_SYSTEM_PROMPT);
        ir.push_instruction("优先提炼高层策略，避免只记录 tail/grep/cat/pytest 这类表面命令。");
        ir.push_instruction("如果成功只是常规完成，不必保留为长期复盘；成功只有在明显可迁移时才 retain。");
        ir.push_instruction("优先生成可比较效果的 demo/stress；只有无法 case 化时才生成 instruction。");
        ir.push_section("target suite", target_suite);
        ir.push_section("episode id", episode_id);
        ir.push_section("outcome status", outcome_status);
        ir.push_section("task goal", task_goal);
        ir.push_section("done criteria", done_criteria);
        ir.push_section("recent steps", recent_steps);
        ir.push_section("final observation", final_observation);
        ir.push_section("related memories", related_memories);
        ir
    }
}
