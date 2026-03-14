use std::{collections::HashSet, path::PathBuf, sync::Arc};

use miette::{Result, miette};
use serde_json::Value;

use crate::core::TelegramResolution;

use super::{
    eval::EvalCase,
    examples::{ExampleField, ProgramExample},
    optimizer::{CandidateConfig, PromptTuningConfig},
    programs::resolve_telegram::{
        ResolveTelegramChatProgram, ResolveTelegramProgramAction, ResolveTelegramProgramOutput,
    },
    runtime::{PromptRequest, PromptRole},
    trace::{ProgramTraceRecord, TraceOrigin},
};

const TRACE_FILE_NAME: &str = "reasoning_traces.jsonl";
const MAX_TRACE_EVAL_CASES: usize = 6;
const MAX_TRACE_BOOTSTRAP_EXAMPLES: usize = 3;

pub fn derive_resolve_telegram_eval_cases(
    program: &ResolveTelegramChatProgram,
) -> Vec<EvalCase<ResolveTelegramProgramOutput>> {
    let records = match load_trace_records() {
        Ok(records) => records,
        Err(_) => return Vec::new(),
    };

    let mut dedup = HashSet::new();
    let mut cases = Vec::new();

    for sample in trace_samples_from_records(records)
        .into_iter()
        .filter(|sample| sample.from_failure)
    {
        if cases.len() >= MAX_TRACE_EVAL_CASES {
            break;
        }
        let Some(key) = sample.dedup_key() else {
            continue;
        };
        if !dedup.insert(key) {
            continue;
        }
        cases.push(sample.into_eval_case(program));
    }

    cases
}

pub fn propose_resolve_telegram_candidates(
    base: &PromptTuningConfig<ResolveTelegramProgramOutput>,
) -> Vec<CandidateConfig<ResolveTelegramProgramOutput>> {
    let records = match load_trace_records() {
        Ok(records) => records,
        Err(_) => return Vec::new(),
    };
    let samples = trace_samples_from_records(records);
    if samples.is_empty() {
        return Vec::new();
    }

    let mut candidates = Vec::new();

    let bootstrap_examples = samples
        .iter()
        .filter_map(|sample| sample.to_program_example())
        .fold(
            (HashSet::new(), Vec::new()),
            |(mut seen, mut items), example| {
                if items.len() < MAX_TRACE_BOOTSTRAP_EXAMPLES {
                    let key = format!(
                        "{}|{}",
                        example.title,
                        serde_json::to_string(&example.output).unwrap_or_default()
                    );
                    if seen.insert(key) {
                        items.push(example);
                    }
                }
                (seen, items)
            },
        )
        .1;

    if !bootstrap_examples.is_empty() {
        let mut examples = base.examples.clone();
        examples.extend(bootstrap_examples.clone());
        candidates.push(CandidateConfig {
            name: "trace_bootstrap_examples".to_string(),
            config: PromptTuningConfig {
                extra_instructions: base.extra_instructions.clone(),
                examples,
            },
        });
        candidates.push(CandidateConfig {
            name: "trace_bootstrap_only".to_string(),
            config: PromptTuningConfig {
                extra_instructions: base.extra_instructions.clone(),
                examples: bootstrap_examples,
            },
        });
    }

    let saw_reply_alias = samples.iter().any(|sample| sample.reply_alias);
    let saw_content_fallback = samples.iter().any(|sample| sample.content_json_fallback);

    if saw_reply_alias || saw_content_fallback {
        let mut extra_instructions = base.extra_instructions.clone();
        if saw_reply_alias {
            extra_instructions.push(
                "如果输出 `ReplyInCurrentChat`，字段名必须是 `text`，不要写成 `reply`。"
                    .to_string(),
            );
        }
        if saw_content_fallback {
            extra_instructions.push(
                "输出时保持单个干净 JSON 对象，不要夹带额外解释、markdown 或代码块。".to_string(),
            );
        }
        candidates.push(CandidateConfig {
            name: "trace_failure_bias".to_string(),
            config: PromptTuningConfig {
                extra_instructions,
                examples: base.examples.clone(),
            },
        });
    }

    candidates
}

#[derive(Clone)]
struct TraceResolveTelegramSample {
    pending_text: String,
    focus: String,
    snapshot_text: String,
    expectation: TraceExpectation,
    output: ResolveTelegramProgramOutput,
    from_failure: bool,
    content_json_fallback: bool,
    reply_alias: bool,
}

#[derive(Clone)]
enum TraceExpectation {
    FocusTelegram,
    AcceptAsProject { chat_id: String },
    ReplyInCurrentChat,
}

impl TraceResolveTelegramSample {
    fn dedup_key(&self) -> Option<String> {
        let expectation = match &self.expectation {
            TraceExpectation::FocusTelegram => "focus_telegram".to_string(),
            TraceExpectation::AcceptAsProject { chat_id } => {
                format!("accept_as_project:{chat_id}")
            }
            TraceExpectation::ReplyInCurrentChat => "reply_in_current_chat".to_string(),
        };
        Some(format!(
            "{}||{}||{}||{}",
            self.pending_text, self.focus, self.snapshot_text, expectation
        ))
    }

    fn into_eval_case(
        self,
        program: &ResolveTelegramChatProgram,
    ) -> EvalCase<ResolveTelegramProgramOutput> {
        let expectation = self.expectation;
        let name = format!(
            "trace_resolve_telegram_{}",
            stable_case_suffix(&self.pending_text, &self.focus, &self.snapshot_text)
        );
        EvalCase {
            name: Box::leak(name.into_boxed_str()),
            ir: program.dataset_ir(self.pending_text, self.focus, self.snapshot_text),
            check: match expectation {
                TraceExpectation::FocusTelegram => Arc::new(check_focus_telegram),
                TraceExpectation::AcceptAsProject { chat_id } => {
                    register_accept_project_check(chat_id)
                }
                TraceExpectation::ReplyInCurrentChat => Arc::new(check_reply_in_current_chat),
            },
        }
    }

    fn to_program_example(&self) -> Option<ProgramExample<ResolveTelegramProgramOutput>> {
        let mut example = ProgramExample {
            title: trace_example_title(&self.expectation),
            inputs: vec![
                ExampleField {
                    name: "待判断会话".to_string(),
                    value: self.pending_text.clone(),
                },
                ExampleField {
                    name: "当前前景设备".to_string(),
                    value: self.focus.clone(),
                },
                ExampleField {
                    name: "完整快照".to_string(),
                    value: self.snapshot_text.clone(),
                },
            ],
            output: self.output.clone(),
        };

        if example
            .inputs
            .iter()
            .all(|field| field.value.trim().is_empty())
        {
            return None;
        }

        if self.from_failure {
            example.title = format!("来自 trace 的恢复案例：{}", example.title);
        }
        Some(example)
    }
}

fn trace_samples_from_records(records: Vec<ProgramTraceRecord>) -> Vec<TraceResolveTelegramSample> {
    let mut samples = Vec::new();

    for record in records.into_iter().rev() {
        if record.program_name != "resolve_telegram_chat" || record.origin != TraceOrigin::Runtime {
            continue;
        }
        let Some((pending_text, focus, snapshot_text)) = extract_resolve_sections(&record.request)
        else {
            continue;
        };
        let from_failure = record.deserialization_error.is_some();
        let content_json_fallback = is_content_json_fallback(
            &record.raw_response,
            record.deserialization_error.as_deref(),
        );
        let reply_alias = uses_reply_alias(
            &record.raw_response,
            record.parsed_output.as_ref(),
            record.deserialization_error.as_deref(),
        );
        let Some(output) = extract_resolve_output(&record) else {
            continue;
        };
        let Some(expectation) = infer_expectation(&output) else {
            continue;
        };
        samples.push(TraceResolveTelegramSample {
            pending_text,
            focus,
            snapshot_text,
            expectation,
            output,
            from_failure,
            content_json_fallback,
            reply_alias,
        });
    }

    samples
}

fn load_trace_records() -> Result<Vec<ProgramTraceRecord>> {
    let path = trace_file_path()?;
    let bytes = match std::fs::read_to_string(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(miette!(
                "failed to read reasoning trace file {}: {err}",
                path.display()
            ));
        }
    };

    let mut records = Vec::new();
    for line in bytes.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<ProgramTraceRecord>(line) {
            records.push(record);
        }
    }
    Ok(records)
}

fn trace_file_path() -> Result<PathBuf> {
    let home = std::env::home_dir().ok_or_else(|| miette!("home directory is unavailable"))?;
    Ok(home.join(".spinova").join(TRACE_FILE_NAME))
}

fn extract_resolve_sections(request: &PromptRequest) -> Option<(String, String, String)> {
    let user_content = request
        .messages
        .iter()
        .find(|message| {
            matches!(message.role, PromptRole::User)
                && message.content.contains("## 待判断会话")
                && message.content.contains("## 当前前景设备")
                && message.content.contains("## 完整快照")
        })?
        .content
        .clone();

    let pending_text = extract_section(&user_content, "待判断会话")?;
    let focus = extract_section(&user_content, "当前前景设备")?;
    let snapshot_text = extract_section(&user_content, "完整快照")?;
    Some((pending_text, focus, snapshot_text))
}

fn extract_section(content: &str, title: &str) -> Option<String> {
    let marker = format!("## {title}\n");
    let start = content.find(&marker)? + marker.len();
    let rest = &content[start..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}

fn extract_resolve_output(record: &ProgramTraceRecord) -> Option<ResolveTelegramProgramOutput> {
    if let Some(parsed) = &record.parsed_output {
        if let Ok(output) = serde_json::from_value::<ResolveTelegramProgramOutput>(parsed.clone()) {
            return Some(output);
        }
    }

    if let Ok(output) =
        serde_json::from_value::<ResolveTelegramProgramOutput>(record.raw_response.clone())
    {
        return Some(output);
    }

    let provider_error = record.raw_response["provider_error"].as_str()?;
    let content_json = extract_json_from_provider_error(provider_error)?;
    serde_json::from_value::<ResolveTelegramProgramOutput>(content_json).ok()
}

fn extract_json_from_provider_error(provider_error: &str) -> Option<Value> {
    let marker = "content=";
    let start = provider_error.find(marker)? + marker.len();
    let rest = &provider_error[start..];
    let end = rest.find("; response=").unwrap_or(rest.len());
    let json_text = rest[..end].trim();
    serde_json::from_str(json_text).ok()
}

fn infer_expectation(output: &ResolveTelegramProgramOutput) -> Option<TraceExpectation> {
    match &output.action {
        ResolveTelegramProgramAction::FocusTelegram => Some(TraceExpectation::FocusTelegram),
        ResolveTelegramProgramAction::ResolveChat {
            chat_id,
            resolution: TelegramResolution::AcceptAsProject { .. },
        } => Some(TraceExpectation::AcceptAsProject {
            chat_id: chat_id.clone(),
        }),
        ResolveTelegramProgramAction::ReplyInCurrentChat { text } if !text.trim().is_empty() => {
            Some(TraceExpectation::ReplyInCurrentChat)
        }
        _ => None,
    }
}

fn is_content_json_fallback(raw_response: &Value, error: Option<&str>) -> bool {
    raw_response.get("provider_error").is_some()
        && error
            .unwrap_or_default()
            .contains("llm response did not include tool_calls")
}

fn uses_reply_alias(
    raw_response: &Value,
    parsed_output: Option<&Value>,
    error: Option<&str>,
) -> bool {
    if error.unwrap_or_default().contains("missing field `text`") {
        return true;
    }

    let Some(action) = raw_response
        .get("action")
        .or_else(|| parsed_output.and_then(|value| value.get("action")))
    else {
        return false;
    };

    action.get("type").and_then(Value::as_str) == Some("ReplyInCurrentChat")
        && action.get("reply").is_some()
}

fn trace_example_title(expectation: &TraceExpectation) -> String {
    match expectation {
        TraceExpectation::FocusTelegram => "待判断会话在后台时，先切到 Telegram。".to_string(),
        TraceExpectation::AcceptAsProject { .. } => {
            "明确要求持续工作的来信，应接受为项目。".to_string()
        }
        TraceExpectation::ReplyInCurrentChat => {
            "语义已判断完且只差回复时，直接在当前会话发送消息。".to_string()
        }
    }
}

fn stable_case_suffix(pending_text: &str, focus: &str, snapshot_text: &str) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(format!("{pending_text}|{focus}|{snapshot_text}"));
    digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn check_focus_telegram(output: &ResolveTelegramProgramOutput) -> Result<()> {
    match &output.action {
        ResolveTelegramProgramAction::FocusTelegram => Ok(()),
        other => Err(miette!("expected FocusTelegram, got {:?}", other)),
    }
}

fn check_reply_in_current_chat(output: &ResolveTelegramProgramOutput) -> Result<()> {
    match &output.action {
        ResolveTelegramProgramAction::ReplyInCurrentChat { text } if !text.trim().is_empty() => {
            Ok(())
        }
        other => Err(miette!(
            "expected ReplyInCurrentChat with non-empty text, got {:?}",
            other
        )),
    }
}

fn register_accept_project_check(
    expected_chat_id: String,
) -> Arc<dyn Fn(&ResolveTelegramProgramOutput) -> Result<()> + Send + Sync> {
    Arc::new(
        move |output: &ResolveTelegramProgramOutput| match &output.action {
            ResolveTelegramProgramAction::ResolveChat {
                chat_id,
                resolution:
                    TelegramResolution::AcceptAsProject {
                        project_title,
                        success_criteria,
                        ..
                    },
            } if chat_id == &expected_chat_id
                && !project_title.trim().is_empty()
                && !success_criteria.trim().is_empty() =>
            {
                Ok(())
            }
            other => Err(miette!(
                "expected ResolveChat with AcceptAsProject for chat {}, got {:?}",
                expected_chat_id,
                other
            )),
        },
    )
}
