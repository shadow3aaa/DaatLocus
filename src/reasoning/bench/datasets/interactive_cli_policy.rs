use std::sync::Arc;

use miette::{Result, miette};
use serde::Deserialize;

use crate::reasoning::{
    bench::programs::interactive_cli_policy::{
        InteractiveCliPolicy, InteractiveCliPolicyOutput, InteractiveCliPolicyProgram,
    },
    dataset_store::decode_dataset_json,
    eval::EvalCase,
    examples::{ExampleField, ProgramExample},
};

const DATASET_FILE: &str = "bench/interactive_cli_policy.json";
const DATASET_JSON: &str = include_str!("interactive_cli_policy.json");

#[derive(Deserialize)]
struct InteractiveCliPolicyDataset {
    examples: Vec<InteractiveCliPolicyExample>,
    train_cases: Vec<InteractiveCliPolicyEvalCase>,
    dev_cases: Vec<InteractiveCliPolicyEvalCase>,
}

#[derive(Deserialize)]
struct InteractiveCliPolicyExample {
    title: String,
    inputs: Vec<ExampleField>,
    output: InteractiveCliPolicyOutput,
}

#[derive(Deserialize)]
struct InteractiveCliPolicyEvalCase {
    name: String,
    task: String,
    terminal_view: String,
    question: String,
    expected_policy: InteractiveCliPolicy,
    expected_next_input: Option<String>,
    reason_must_include: Vec<String>,
}

pub fn examples() -> Vec<ProgramExample<InteractiveCliPolicyOutput>> {
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
    program: &InteractiveCliPolicyProgram,
) -> Vec<EvalCase<InteractiveCliPolicyOutput>> {
    to_eval_cases(program, load_dataset().train_cases)
}

pub fn dev_eval_cases(
    program: &InteractiveCliPolicyProgram,
) -> Vec<EvalCase<InteractiveCliPolicyOutput>> {
    to_eval_cases(program, load_dataset().dev_cases)
}

pub fn bootstrap_examples(_case_names: &[&str]) -> Vec<ProgramExample<InteractiveCliPolicyOutput>> {
    Vec::new()
}

fn to_eval_cases(
    program: &InteractiveCliPolicyProgram,
    cases: Vec<InteractiveCliPolicyEvalCase>,
) -> Vec<EvalCase<InteractiveCliPolicyOutput>> {
    cases
        .into_iter()
        .map(|case| {
            let expected_policy = case.expected_policy;
            let expected_next_input = case.expected_next_input;
            let reason_must_include = case.reason_must_include;
            EvalCase {
                name: Box::leak(case.name.into_boxed_str()),
                ir: program.dataset_ir(case.task, case.terminal_view, case.question),
                check: Arc::new(move |output| {
                    check_policy(output, &expected_policy)?;
                    check_next_input(output, expected_next_input.as_deref())?;
                    check_reason_contains(output, &reason_must_include)
                }),
            }
        })
        .collect()
}

fn load_dataset() -> InteractiveCliPolicyDataset {
    decode_dataset_json(DATASET_FILE, DATASET_JSON)
        .expect("interactive_cli_policy dataset must be valid")
}

fn check_policy(
    output: &InteractiveCliPolicyOutput,
    expected_policy: &InteractiveCliPolicy,
) -> Result<()> {
    if &output.policy != expected_policy {
        return Err(miette!(
            "expected policy {:?}, got {:?}",
            expected_policy,
            output.policy
        ));
    }
    Ok(())
}

fn check_next_input(output: &InteractiveCliPolicyOutput, expected: Option<&str>) -> Result<()> {
    match (expected, output.next_input.as_deref()) {
        (Some(expected), Some(actual)) if expected == actual => Ok(()),
        (None, None) => Ok(()),
        (Some(expected), actual) => Err(miette!(
            "expected next_input {:?}, got {:?}",
            expected,
            actual
        )),
        (None, actual) => Err(miette!("expected empty next_input, got {:?}", actual)),
    }
}

fn check_reason_contains(
    output: &InteractiveCliPolicyOutput,
    required_parts: &[String],
) -> Result<()> {
    for required_part in required_parts {
        if !output.reason.contains(required_part) {
            return Err(miette!(
                "expected reason to contain `{}`, got `{}`",
                required_part,
                output.reason
            ));
        }
    }
    Ok(())
}
