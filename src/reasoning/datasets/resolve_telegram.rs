use std::sync::Arc;

use miette::{Result, miette};
use serde::Deserialize;

use crate::{
    core::TelegramResolution,
    reasoning::{
        dataset_store::decode_dataset_json,
        eval::EvalCase,
        examples::{ExampleField, ProgramExample},
        programs::resolve_telegram::{
            ResolveTelegramChatProgram, ResolveTelegramProgramAction, ResolveTelegramProgramOutput,
        },
    },
};

const DATASET_FILE: &str = "resolve_telegram.json";
const DATASET_JSON: &str = include_str!("resolve_telegram.json");

#[derive(Deserialize)]
struct ResolveTelegramDataset {
    examples: Vec<ResolveTelegramExample>,
    train_cases: Vec<ResolveTelegramEvalCase>,
    acceptance_cases: Vec<ResolveTelegramEvalCase>,
    stress_cases: Vec<ResolveTelegramEvalCase>,
}

#[derive(Deserialize)]
struct ResolveTelegramExample {
    title: String,
    inputs: Vec<ExampleField>,
    output: ResolveTelegramProgramOutput,
}

#[derive(Deserialize)]
struct ResolveTelegramEvalCase {
    name: String,
    pending_text: String,
    focus: String,
    snapshot_text: String,
    expectation: ResolveTelegramExpectation,
    bootstrap_output: Option<ResolveTelegramProgramOutput>,
}

#[derive(Deserialize)]
#[serde(tag = "kind")]
enum ResolveTelegramExpectation {
    #[serde(rename = "focus_telegram")]
    FocusTelegram,
    #[serde(rename = "open_chat")]
    OpenChat { chat_id: String },
    #[serde(rename = "accept_as_project")]
    AcceptAsProject { chat_id: String },
    #[serde(rename = "reply_only")]
    ReplyOnly { chat_id: String },
    #[serde(rename = "ask_clarification")]
    AskClarification { chat_id: String },
    #[serde(rename = "decline")]
    Decline { chat_id: String },
    #[serde(rename = "reply_in_current_chat")]
    ReplyInCurrentChat,
}

pub fn examples() -> Vec<ProgramExample<ResolveTelegramProgramOutput>> {
    load_dataset()
        .examples
        .into_iter()
        .map(|example| ProgramExample {
            title: example.title,
            inputs: example.inputs,
            output: example.output,
        })
        .collect()
}

pub fn train_eval_cases(
    program: &ResolveTelegramChatProgram,
) -> Vec<EvalCase<ResolveTelegramProgramOutput>> {
    to_eval_cases(program, load_dataset().train_cases)
}

pub fn dev_eval_cases(
    program: &ResolveTelegramChatProgram,
) -> Vec<EvalCase<ResolveTelegramProgramOutput>> {
    let dataset = load_dataset();
    let mut cases = dataset.acceptance_cases;
    cases.extend(dataset.stress_cases);
    to_eval_cases(program, cases)
}

pub fn acceptance_eval_cases(
    program: &ResolveTelegramChatProgram,
) -> Vec<EvalCase<ResolveTelegramProgramOutput>> {
    to_eval_cases(program, load_dataset().acceptance_cases)
}

pub fn stress_eval_cases(
    program: &ResolveTelegramChatProgram,
) -> Vec<EvalCase<ResolveTelegramProgramOutput>> {
    to_eval_cases(program, load_dataset().stress_cases)
}

pub fn bootstrap_examples(
    case_names: &[&str],
) -> Vec<ProgramExample<ResolveTelegramProgramOutput>> {
    load_dataset()
        .train_cases
        .into_iter()
        .filter(|case| case_names.iter().any(|name| *name == case.name))
        .filter_map(|case| {
            case.bootstrap_output.map(|output| ProgramExample {
                title: format!("Bootstrap from {}", case.name),
                inputs: vec![
                    ExampleField {
                        name: "待判断会话".to_string(),
                        value: case.pending_text,
                    },
                    ExampleField {
                        name: "当前前景设备".to_string(),
                        value: case.focus,
                    },
                    ExampleField {
                        name: "完整快照".to_string(),
                        value: case.snapshot_text,
                    },
                ],
                output,
            })
        })
        .collect()
}

pub fn all_bootstrap_examples() -> Vec<ProgramExample<ResolveTelegramProgramOutput>> {
    load_dataset()
        .train_cases
        .into_iter()
        .filter_map(|case| {
            case.bootstrap_output.map(|output| ProgramExample {
                title: format!("Bootstrap from {}", case.name),
                inputs: vec![
                    ExampleField {
                        name: "待判断会话".to_string(),
                        value: case.pending_text,
                    },
                    ExampleField {
                        name: "当前前景设备".to_string(),
                        value: case.focus,
                    },
                    ExampleField {
                        name: "完整快照".to_string(),
                        value: case.snapshot_text,
                    },
                ],
                output,
            })
        })
        .collect()
}

fn load_dataset() -> ResolveTelegramDataset {
    decode_dataset_json(DATASET_FILE, DATASET_JSON).expect("resolve_telegram dataset must be valid")
}

fn to_eval_cases(
    program: &ResolveTelegramChatProgram,
    cases: Vec<ResolveTelegramEvalCase>,
) -> Vec<EvalCase<ResolveTelegramProgramOutput>> {
    cases
        .into_iter()
        .map(|case| {
            let expectation = case.expectation;
            EvalCase {
                name: Box::leak(case.name.into_boxed_str()),
                ir: program.dataset_ir(case.pending_text, case.focus, case.snapshot_text),
                check: match expectation {
                    ResolveTelegramExpectation::FocusTelegram => Arc::new(check_focus_telegram),
                    ResolveTelegramExpectation::OpenChat { chat_id } => {
                        register_open_chat_check(chat_id)
                    }
                    ResolveTelegramExpectation::AcceptAsProject { chat_id } => {
                        register_accept_project_check(chat_id)
                    }
                    ResolveTelegramExpectation::ReplyOnly { chat_id } => {
                        register_reply_only_check(chat_id)
                    }
                    ResolveTelegramExpectation::AskClarification { chat_id } => {
                        register_ask_clarification_check(chat_id)
                    }
                    ResolveTelegramExpectation::Decline { chat_id } => {
                        register_decline_check(chat_id)
                    }
                    ResolveTelegramExpectation::ReplyInCurrentChat => {
                        Arc::new(check_reply_in_current_chat)
                    }
                },
            }
        })
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

fn register_open_chat_check(
    expected_chat_id: String,
) -> Arc<dyn Fn(&ResolveTelegramProgramOutput) -> Result<()> + Send + Sync> {
    Arc::new(
        move |output: &ResolveTelegramProgramOutput| match &output.action {
            ResolveTelegramProgramAction::OpenChat { chat_id } if chat_id == &expected_chat_id => {
                Ok(())
            }
            other => Err(miette!(
                "expected OpenChat for chat {}, got {:?}",
                expected_chat_id,
                other
            )),
        },
    )
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

fn register_reply_only_check(
    expected_chat_id: String,
) -> Arc<dyn Fn(&ResolveTelegramProgramOutput) -> Result<()> + Send + Sync> {
    Arc::new(
        move |output: &ResolveTelegramProgramOutput| match &output.action {
            ResolveTelegramProgramAction::ResolveChat {
                chat_id,
                resolution: TelegramResolution::ReplyOnly { reply },
            } if chat_id == &expected_chat_id && !reply.trim().is_empty() => Ok(()),
            other => Err(miette!(
                "expected ResolveChat with ReplyOnly for chat {}, got {:?}",
                expected_chat_id,
                other
            )),
        },
    )
}

fn register_ask_clarification_check(
    expected_chat_id: String,
) -> Arc<dyn Fn(&ResolveTelegramProgramOutput) -> Result<()> + Send + Sync> {
    Arc::new(
        move |output: &ResolveTelegramProgramOutput| match &output.action {
            ResolveTelegramProgramAction::ResolveChat {
                chat_id,
                resolution: TelegramResolution::AskClarification { reply },
            } if chat_id == &expected_chat_id && !reply.trim().is_empty() => Ok(()),
            other => Err(miette!(
                "expected ResolveChat with AskClarification for chat {}, got {:?}",
                expected_chat_id,
                other
            )),
        },
    )
}

fn register_decline_check(
    expected_chat_id: String,
) -> Arc<dyn Fn(&ResolveTelegramProgramOutput) -> Result<()> + Send + Sync> {
    Arc::new(
        move |output: &ResolveTelegramProgramOutput| match &output.action {
            ResolveTelegramProgramAction::ResolveChat {
                chat_id,
                resolution: TelegramResolution::Decline { reply },
            } if chat_id == &expected_chat_id && !reply.trim().is_empty() => Ok(()),
            other => Err(miette!(
                "expected ResolveChat with Decline for chat {}, got {:?}",
                expected_chat_id,
                other
            )),
        },
    )
}
