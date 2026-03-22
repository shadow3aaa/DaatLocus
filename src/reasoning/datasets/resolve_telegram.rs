use std::sync::Arc;

use miette::{Result, miette};
use serde::Deserialize;

use crate::{
    core::TelegramResolution,
    reasoning::{
        dataset_store::decode_dataset_json,
        eval::EvalCase,
        examples::{ExampleField, ProgramExample},
        programs::resolve_telegram::{ResolveTelegramChatProgram, ResolveTelegramProgramOutput},
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

pub fn stress_eval_cases_by_names(
    program: &ResolveTelegramChatProgram,
    case_names: &[String],
) -> Vec<EvalCase<ResolveTelegramProgramOutput>> {
    let dataset = load_dataset();
    let cases = dataset
        .stress_cases
        .into_iter()
        .filter(|case| case_names.iter().any(|name| name == &case.name))
        .collect::<Vec<_>>();
    to_eval_cases(program, cases)
}

pub fn all_case_names() -> Vec<String> {
    let dataset = load_dataset();
    let mut names = Vec::new();
    names.extend(dataset.train_cases.into_iter().map(|case| case.name));
    names.extend(dataset.acceptance_cases.into_iter().map(|case| case.name));
    names.extend(dataset.stress_cases.into_iter().map(|case| case.name));
    names
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
    if output.action_kind == "focus_device" && output.action_summary.contains("Telegram") {
        Ok(())
    } else {
        Err(miette!(
            "expected focus_device(Telegram), got {} ({})",
            output.action_kind,
            output.action_summary
        ))
    }
}

fn check_reply_in_current_chat(output: &ResolveTelegramProgramOutput) -> Result<()> {
    if output.action_kind == "telegram_send_message"
        && output
            .text
            .as_deref()
            .map(|text| !text.trim().is_empty())
            .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(miette!(
            "expected telegram_send_message with non-empty text, got {} ({})",
            output.action_kind,
            output.action_summary
        ))
    }
}

fn register_open_chat_check(
    expected_chat_id: String,
) -> Arc<dyn Fn(&ResolveTelegramProgramOutput) -> Result<()> + Send + Sync> {
    Arc::new(move |output: &ResolveTelegramProgramOutput| {
        if output.action_kind == "telegram_select_chat"
            && output.chat_id.as_deref() == Some(expected_chat_id.as_str())
        {
            Ok(())
        } else {
            Err(miette!(
                "expected telegram_select_chat for chat {}, got {} ({})",
                expected_chat_id,
                output.action_kind,
                output.action_summary
            ))
        }
    })
}

fn register_accept_project_check(
    expected_chat_id: String,
) -> Arc<dyn Fn(&ResolveTelegramProgramOutput) -> Result<()> + Send + Sync> {
    Arc::new(move |output: &ResolveTelegramProgramOutput| {
        match (&output.chat_id, &output.resolution) {
            (
                Some(chat_id),
                Some(TelegramResolution::AcceptAsProject {
                    project_title,
                    success_criteria,
                    ..
                }),
            ) if output.action_kind == "resolve_telegram_chat"
                && chat_id == &expected_chat_id
                && !project_title.trim().is_empty()
                && !success_criteria.trim().is_empty() =>
            {
                Ok(())
            }
            _ => Err(miette!(
                "expected resolve_telegram_chat with AcceptAsProject for chat {}, got {} ({})",
                expected_chat_id,
                output.action_kind,
                output.action_summary
            )),
        }
    })
}

fn register_reply_only_check(
    expected_chat_id: String,
) -> Arc<dyn Fn(&ResolveTelegramProgramOutput) -> Result<()> + Send + Sync> {
    Arc::new(move |output: &ResolveTelegramProgramOutput| {
        match (&output.chat_id, &output.resolution) {
            (Some(chat_id), Some(TelegramResolution::ReplyOnly { reply }))
                if output.action_kind == "resolve_telegram_chat"
                    && chat_id == &expected_chat_id
                    && !reply.trim().is_empty() =>
            {
                Ok(())
            }
            _ => Err(miette!(
                "expected resolve_telegram_chat with ReplyOnly for chat {}, got {} ({})",
                expected_chat_id,
                output.action_kind,
                output.action_summary
            )),
        }
    })
}

fn register_ask_clarification_check(
    expected_chat_id: String,
) -> Arc<dyn Fn(&ResolveTelegramProgramOutput) -> Result<()> + Send + Sync> {
    Arc::new(move |output: &ResolveTelegramProgramOutput| {
        match (&output.chat_id, &output.resolution) {
            (Some(chat_id), Some(TelegramResolution::AskClarification { reply }))
                if output.action_kind == "resolve_telegram_chat"
                    && chat_id == &expected_chat_id
                    && !reply.trim().is_empty() =>
            {
                Ok(())
            }
            _ => Err(miette!(
                "expected resolve_telegram_chat with AskClarification for chat {}, got {} ({})",
                expected_chat_id,
                output.action_kind,
                output.action_summary
            )),
        }
    })
}

fn register_decline_check(
    expected_chat_id: String,
) -> Arc<dyn Fn(&ResolveTelegramProgramOutput) -> Result<()> + Send + Sync> {
    Arc::new(move |output: &ResolveTelegramProgramOutput| {
        match (&output.chat_id, &output.resolution) {
            (Some(chat_id), Some(TelegramResolution::Decline { reply }))
                if output.action_kind == "resolve_telegram_chat"
                    && chat_id == &expected_chat_id
                    && !reply.trim().is_empty() =>
            {
                Ok(())
            }
            _ => Err(miette!(
                "expected resolve_telegram_chat with Decline for chat {}, got {} ({})",
                expected_chat_id,
                output.action_kind,
                output.action_summary
            )),
        }
    })
}
