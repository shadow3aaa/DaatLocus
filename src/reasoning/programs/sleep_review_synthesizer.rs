use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{
    context::Context,
    reasoning::{ir::PromptIR, program::Program, signature::Signature},
    snapshot::Snapshot,
};

const SLEEP_REVIEW_SYNTHESIZER_SYSTEM_PROMPT: &str = r#"你现在处于睡眠整理阶段。
你的任务是阅读一个 review unit 的结果，并把它抽象成可复用的学习结论。

review unit 可能来自：
1. train/eval episode
2. 正常 runtime span

目标不是复述轨迹，而是提炼：
1. 这次行为片段暴露了什么稳定模式
2. 下次遇到类似情况应如何更快收敛
3. 哪些 compile 产物值得生成（demo / instruction / stress）
4. 是否值得保留为长期复盘经验

重要原则：
- 优先把 lesson 落成可验证的 runtime demo 或 stress case；只有无法 case 化时，才退回 instruction hypothesis。
- 优先学习“如何围绕明确错误对象或状态边界收敛”的策略，而不是只记录表面现象。
- 成功或常规推进不默认 retain；只有模式明显可迁移、可复用、能稳定指导未来任务时，才允许 retain_reflection=true。
- 如果 review unit 已经给出明确错误对象（导入错误、路径错误、入口错误、命令未触发、环境缺失、状态推进冲突），应围绕该错误对象总结下一步策略。
- 输出必须简洁、抽象、可迁移，不要长篇复述整个轨迹。 "#;

pub struct SleepReviewSynthesizerProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum ReflectionKind {
    #[serde(alias = "TerminationPolicy")]
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
pub struct SleepReviewSynthesizerOutput {
    #[serde(
        default,
        deserialize_with = "deserialize_lossy_string",
        alias = "summary"
    )]
    pub synthesized_summary: String,
    #[serde(
        default,
        deserialize_with = "deserialize_lossy_string",
        alias = "strategy"
    )]
    pub strategy_lesson: String,
    #[serde(default)]
    pub create_failure_pattern: bool,
    #[serde(
        default,
        deserialize_with = "deserialize_lossy_string",
        alias = "failure_pattern"
    )]
    pub failure_pattern_summary: String,
    #[serde(
        default,
        deserialize_with = "deserialize_lossy_string",
        alias = "fix_kind",
        alias = "suggested_fix"
    )]
    pub suggested_fix_kind: String,
    #[serde(default, alias = "create_instruction")]
    pub create_instruction_hypothesis: bool,
    #[serde(
        default,
        deserialize_with = "deserialize_lossy_string",
        alias = "instruction"
    )]
    pub instruction_text: String,
    #[serde(default, alias = "demo")]
    pub create_bootstrap_demo: bool,
    #[serde(
        default,
        deserialize_with = "deserialize_lossy_string",
        alias = "demo_title"
    )]
    pub bootstrap_demo_title: String,
    #[serde(
        default,
        deserialize_with = "deserialize_lossy_string",
        alias = "demo_summary"
    )]
    pub bootstrap_demo_summary: String,
    #[serde(default, alias = "stress_case")]
    pub create_stress_case: bool,
    #[serde(
        default,
        deserialize_with = "deserialize_lossy_string",
        alias = "stress_name"
    )]
    pub stress_case_name: String,
    #[serde(default, deserialize_with = "deserialize_lossy_string_vec")]
    pub stress_constraints: Vec<String>,
    #[serde(default)]
    pub retain_reflection: bool,
    #[serde(default, alias = "kind", alias = "reflect_kind")]
    pub reflection_kind: ReflectionKind,
    #[serde(default, alias = "stability")]
    pub reflection_stability: ReflectionStability,
    #[serde(
        default,
        deserialize_with = "deserialize_lossy_string",
        alias = "lesson"
    )]
    pub reflection_lesson: String,
    #[serde(
        default,
        deserialize_with = "deserialize_lossy_string",
        alias = "evidence"
    )]
    pub reflection_evidence_summary: String,
    #[serde(default, deserialize_with = "deserialize_lossy_string")]
    pub reflection_retrieval_text: String,
    #[serde(
        default,
        deserialize_with = "deserialize_lossy_f64",
        alias = "confidence"
    )]
    pub reflection_confidence: f64,
    #[serde(default, deserialize_with = "deserialize_lossy_string")]
    pub reason: String,
}

impl Default for ReflectionKind {
    fn default() -> Self {
        Self::General
    }
}

impl Default for ReflectionStability {
    fn default() -> Self {
        Self::Tentative
    }
}

fn deserialize_lossy_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(match value {
        Value::Null => String::new(),
        Value::String(text) => {
            if text.eq_ignore_ascii_case("null") {
                String::new()
            } else {
                text
            }
        }
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(&value).unwrap_or_default(),
    })
}

fn deserialize_lossy_string_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(match value {
        Value::Null => Vec::new(),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() || text.eq_ignore_ascii_case("null") {
                Vec::new()
            } else {
                vec![text.to_string()]
            }
        }
        Value::Array(items) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::Null => None,
                Value::String(text) => {
                    let text = text.trim();
                    if text.is_empty() || text.eq_ignore_ascii_case("null") {
                        None
                    } else {
                        Some(text.to_string())
                    }
                }
                Value::Bool(flag) => Some(flag.to_string()),
                Value::Number(number) => Some(number.to_string()),
                Value::Array(_) | Value::Object(_) => serde_json::to_string(&item).ok(),
            })
            .collect(),
        other => vec![serde_json::to_string(&other).unwrap_or_default()],
    })
}

fn deserialize_lossy_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(match value {
        Value::Null => 0.0,
        Value::Number(number) => number.as_f64().unwrap_or(0.0),
        Value::String(text) => text.trim().parse::<f64>().unwrap_or(0.0),
        Value::Bool(flag) => {
            if flag {
                1.0
            } else {
                0.0
            }
        }
        Value::Array(_) | Value::Object(_) => 0.0,
    })
}

impl Program for SleepReviewSynthesizerProgram {
    type Output = SleepReviewSynthesizerOutput;

    fn name(&self) -> &'static str {
        "sleep_review_synthesizer"
    }

    fn description(&self) -> &'static str {
        "把单个 train episode 或 runtime review span 抽象成可复用策略，并生成 sleep artifacts 与长期复盘提议。"
    }

    fn signature(&self) -> Signature {
        Signature::new("从单个 review unit 结果中抽象学习结论。")
            .input("review label", "这个 review unit 最相关的标签或能力名。")
            .input("source kind", "train_episode 或 runtime_review。")
            .input("review id", "review unit 的稳定标识。")
            .input(
                "outcome status",
                "Succeeded/Failed/Observed/NoProgress 等高层状态。",
            )
            .input("task goal", "当前目标。")
            .input("done criteria", "完成标准或推进标准。")
            .input("recent steps", "最近关键步骤摘要。")
            .input("final observation", "最终总结和快照摘要。")
            .input("related memories", "相关长期记忆。")
            .output("synthesized_summary", "对 review unit 的高层抽象总结。")
            .output("strategy_lesson", "下次应如何行动的可迁移策略。")
            .output("create_failure_pattern", "是否生成失败 pattern。")
            .output("failure_pattern_summary", "失败 pattern 的高层描述。")
            .output("suggested_fix_kind", "demo/instruction/stress_case 之一。")
            .output(
                "create_instruction_hypothesis",
                "是否生成 instruction hypothesis。",
            )
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
            .rule("优先产出适合后续 runtime system prompt 优化的 demo。")
            .rule("如果一个 lesson 可以稳定落成 demo 或 stress case，就不要只生成 instruction。")
            .rule("失败或卡住的 review unit 优先输出收敛策略，不要只复述报错或日志。")
            .rule("如果已经有明确错误对象或状态冲突，strategy_lesson 必须围绕该对象收敛。")
    }

    fn build_ir(&self, _: &Context, _: &Snapshot) -> PromptIR {
        self.dataset_ir(
            String::new(),
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

impl SleepReviewSynthesizerProgram {
    #[allow(clippy::too_many_arguments)]
    pub fn dataset_ir(
        &self,
        review_label: String,
        source_kind: String,
        review_id: String,
        outcome_status: String,
        task_goal: String,
        done_criteria: String,
        recent_steps: String,
        final_observation: String,
        related_memories: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(SLEEP_REVIEW_SYNTHESIZER_SYSTEM_PROMPT);
        ir.push_instruction("优先提炼高层策略，避免只记录 tail/grep/cat/pytest 这类表面命令。");
        ir.push_instruction(
            "优先生成可比较效果的 runtime demo/stress；只有无法 case 化时才生成 instruction。",
        );
        ir.push_instruction("source kind 为 runtime_review 时，不要强行套 train/eval 完成语义，应更关注状态推进质量与行为边界。");
        ir.push_section("review label", review_label);
        ir.push_section("source kind", source_kind);
        ir.push_section("review id", review_id);
        ir.push_section("outcome status", outcome_status);
        ir.push_section("task goal", task_goal);
        ir.push_section("done criteria", done_criteria);
        ir.push_section("recent steps", recent_steps);
        ir.push_section("final observation", final_observation);
        ir.push_section("related memories", related_memories);
        ir
    }
}
