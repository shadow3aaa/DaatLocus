use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const RUNTIME_RETAIN_PREPROCESSOR_SYSTEM_PROMPT: &str = r#"你现在处于长期记忆 retain 预处理阶段。
你的任务不是执行工具，也不是继续当前 turn，而是把一段 runtime turn 记录整理成更适合长期记忆系统 ingest 的材料。

目标：
1. 过滤低价值、重复、纯协议、纯日志、纯中间输出。
2. 只保留对未来真正可复用的内容：项目事实、用户偏好、失败模式、策略 lesson、稳定边界。
3. 如果一个 turn 里存在多个独立且都值得保留的主题，可以拆成少量 split_items；否则优先输出一个 consolidated digest。

原则：
- 不要复述整段原始轨迹；输出必须明显短于输入。
- 事实要面向未来可复用，而不是一次性流水账。
- 对 tool output、patch、terminal log 只保留“发生了什么、为什么重要、结果是什么”。
- 如果内容只是在推进中间步骤、没有稳定价值、没有明确结果或 lesson，should_retain=false。
- split_items 只在存在多个彼此独立的 durable topic 时使用；不要机械按 token 切块。
- 输出保持简洁、具体、可用于后续 recall/reflect。"#;

pub struct RuntimeRetainPreprocessorProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct RuntimeRetainSplitItem {
    #[serde(default, deserialize_with = "deserialize_lossy_string")]
    pub title: String,
    #[serde(default, deserialize_with = "deserialize_lossy_string")]
    pub content: String,
    #[serde(default, deserialize_with = "deserialize_lossy_string")]
    pub context: String,
    #[serde(default, deserialize_with = "deserialize_lossy_string_vec")]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct RuntimeRetainPreprocessorOutput {
    #[serde(default)]
    pub should_retain: bool,
    #[serde(default, deserialize_with = "deserialize_lossy_string")]
    pub summary: String,
    #[serde(default, deserialize_with = "deserialize_lossy_string_vec")]
    pub facts: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_lossy_string_vec")]
    pub preferences: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_lossy_string_vec")]
    pub failures: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_lossy_string_vec")]
    pub lessons: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_lossy_string_vec")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub split_items: Vec<RuntimeRetainSplitItem>,
    #[serde(default, deserialize_with = "deserialize_lossy_string")]
    pub reason: String,
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

impl Program for RuntimeRetainPreprocessorProgram {
    type Output = RuntimeRetainPreprocessorOutput;

    fn name(&self) -> &'static str {
        "runtime_retain_preprocessor"
    }

    fn description(&self) -> &'static str {
        "把 runtime turn retain 原文过滤、抽取并压缩成更适合长期记忆保留的 digest。"
    }

    fn signature(&self) -> Signature {
        Signature::new("将 runtime turn retain 原文预处理成可保留的长期记忆摘要。")
            .input("current doing", "该 turn 结束时的当前任务主线或 focus。")
            .input("document id", "当前 retain job 的文档 id。")
            .input("existing tags", "当前 job 已有的 tags。")
            .input("raw retain content", "当前 turn 的原始 retain 文本。")
            .output("should_retain", "是否值得把这轮 turn 保留进长期记忆。")
            .output("summary", "对这轮 turn 的简短长期记忆摘要。")
            .output("facts", "值得长期保留的项目事实或状态事实。")
            .output("preferences", "值得长期保留的稳定用户偏好或协作偏好。")
            .output("failures", "值得长期保留的失败模式或风险。")
            .output("lessons", "值得长期保留的策略或经验。")
            .output("tags", "建议追加的 tags。")
            .output("split_items", "当存在多个独立 durable topic 时的拆分结果。")
            .output("reason", "简述是否保留以及如何整理。")
            .rule("如果 should_retain=false，则 summary、facts、preferences、failures、lessons、split_items 应尽量为空。")
            .rule("如果 split_items 非空，每个 item 都必须是独立 durable topic，而不是按 token 机械切分。")
            .rule("输出必须明显短于 raw retain content，不要大段复制原文。")
    }
}

impl RuntimeRetainPreprocessorProgram {
    pub fn dataset_ir(
        &self,
        current_doing: String,
        document_id: String,
        existing_tags: Vec<String>,
        raw_retain_content: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(RUNTIME_RETAIN_PREPROCESSOR_SYSTEM_PROMPT);
        ir.push_instruction("优先抽取 durable knowledge，而不是复述工具协议。");
        ir.push_instruction(
            "如果没有形成稳定结论、偏好、边界、失败模式或策略 lesson，就不要 retain。",
        );
        ir.push_instruction("除非明显存在多个独立主题，否则不要输出 split_items。");
        ir.push_section("current doing", current_doing);
        ir.push_section("document id", document_id);
        ir.push_section(
            "existing tags",
            if existing_tags.is_empty() {
                "none".to_string()
            } else {
                existing_tags.join(", ")
            },
        );
        ir.push_section("raw retain content", raw_retain_content);
        ir
    }
}
