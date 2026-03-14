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
    eval_cases: Vec<ResolveTelegramEvalCase>,
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
    #[serde(rename = "accept_as_project")]
    AcceptAsProject { chat_id: String },
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

pub fn eval_cases(
    program: &ResolveTelegramChatProgram,
) -> Vec<EvalCase<ResolveTelegramProgramOutput>> {
    load_dataset()
        .eval_cases
        .into_iter()
        .map(|case| {
            let expectation = case.expectation;
            EvalCase {
                name: Box::leak(case.name.into_boxed_str()),
                ir: program.dataset_ir(case.pending_text, case.focus, case.snapshot_text),
                check: match expectation {
                    ResolveTelegramExpectation::FocusTelegram => Arc::new(check_focus_telegram),
                    ResolveTelegramExpectation::AcceptAsProject { chat_id } => {
                        register_accept_project_check(chat_id)
                    }
                    ResolveTelegramExpectation::ReplyInCurrentChat => {
                        Arc::new(check_reply_in_current_chat)
                    }
                },
            }
        })
        .collect()
}

fn load_dataset() -> ResolveTelegramDataset {
    decode_dataset_json(DATASET_FILE, DATASET_JSON).expect("resolve_telegram dataset must be valid")
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
