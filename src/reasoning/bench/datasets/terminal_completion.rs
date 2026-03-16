use std::sync::Arc;

use miette::{Result, miette};
use serde::Deserialize;

use crate::reasoning::{
    bench::programs::terminal_completion::{
        TerminalCompletionOutput, TerminalCompletionProgram, TerminalCompletionStatus,
    },
    dataset_store::decode_dataset_json,
    eval::EvalCase,
    examples::{ExampleField, ProgramExample},
};

const DATASET_FILE: &str = "bench/terminal_completion.json";
const DATASET_JSON: &str = include_str!("terminal_completion.json");

#[derive(Deserialize)]
struct TerminalCompletionDataset {
    examples: Vec<TerminalCompletionExample>,
    train_cases: Vec<TerminalCompletionEvalCase>,
    dev_cases: Vec<TerminalCompletionEvalCase>,
}

#[derive(Deserialize)]
struct TerminalCompletionExample {
    title: String,
    inputs: Vec<ExampleField>,
    output: TerminalCompletionOutput,
}

#[derive(Deserialize)]
struct TerminalCompletionEvalCase {
    name: String,
    task: String,
    terminal_view: String,
    question: String,
    expected_status: TerminalCompletionStatus,
    reason_any_of: Vec<String>,
}

pub fn examples() -> Vec<ProgramExample<TerminalCompletionOutput>> {
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
    program: &TerminalCompletionProgram,
) -> Vec<EvalCase<TerminalCompletionOutput>> {
    to_eval_cases(program, load_dataset().train_cases)
}

pub fn dev_eval_cases(
    program: &TerminalCompletionProgram,
) -> Vec<EvalCase<TerminalCompletionOutput>> {
    to_eval_cases(program, load_dataset().dev_cases)
}

pub fn bootstrap_examples(_case_names: &[&str]) -> Vec<ProgramExample<TerminalCompletionOutput>> {
    Vec::new()
}

fn to_eval_cases(
    program: &TerminalCompletionProgram,
    cases: Vec<TerminalCompletionEvalCase>,
) -> Vec<EvalCase<TerminalCompletionOutput>> {
    cases.into_iter()
        .map(|case| {
            let expected_status = case.expected_status;
            let reason_any_of = case.reason_any_of;
            EvalCase {
                name: Box::leak(case.name.into_boxed_str()),
                ir: program.dataset_ir(case.task, case.terminal_view, case.question),
                check: Arc::new(move |output| {
                    check_status(output, &expected_status)?;
                    check_reason_any_of(output, &reason_any_of)
                }),
            }
        })
        .collect()
}

fn load_dataset() -> TerminalCompletionDataset {
    decode_dataset_json(DATASET_FILE, DATASET_JSON)
        .expect("terminal_completion dataset must be valid")
}

fn check_status(
    output: &TerminalCompletionOutput,
    expected_status: &TerminalCompletionStatus,
) -> Result<()> {
    if &output.status != expected_status {
        return Err(miette!(
            "expected status {:?}, got {:?}",
            expected_status,
            output.status
        ));
    }
    Ok(())
}

fn check_reason_any_of(output: &TerminalCompletionOutput, accepted_parts: &[String]) -> Result<()> {
    if accepted_parts.is_empty() {
        return Ok(());
    }
    if accepted_parts
        .iter()
        .any(|accepted_part| output.reason.contains(accepted_part))
    {
        return Ok(());
    }
    Err(miette!(
        "expected reason to contain one of {:?}, got `{}`",
        accepted_parts,
        output.reason
    ))
}
