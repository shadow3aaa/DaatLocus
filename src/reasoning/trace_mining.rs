use std::{collections::HashSet, path::PathBuf, sync::Arc};

use miette::{Result, miette};
use serde_json::Value;

use crate::core::TelegramResolution;

use super::{
    eval::EvalCase,
    programs::resolve_telegram::{
        ResolveTelegramChatProgram, ResolveTelegramProgramAction, ResolveTelegramProgramOutput,
    },
    runtime::{PromptRequest, PromptRole},
    trace::{ProgramTraceRecord, TraceOrigin},
};

const TRACE_FILE_NAME: &str = "reasoning_traces.jsonl";
const MAX_TRACE_EVAL_CASES: usize = 6;

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

#[derive(Clone)]
struct TraceResolveTelegramSample {
    pending_text: String,
    focus: String,
    snapshot_text: String,
    expectation: TraceExpectation,
    from_failure: bool,
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
            from_failure,
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
        .all_messages()
        .into_iter()
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
