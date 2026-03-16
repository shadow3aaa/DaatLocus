use std::sync::Arc;

use miette::{Result, miette};
use serde::Deserialize;

use crate::reasoning::{
    bench::programs::memory_encoding::{MemoryEncodingOutput, MemoryEncodingProgram},
    dataset_store::decode_dataset_json,
    eval::EvalCase,
    examples::{ExampleField, ProgramExample},
};

const DATASET_FILE: &str = "bench/memory_encoding.json";
const DATASET_JSON: &str = include_str!("memory_encoding.json");

#[derive(Deserialize)]
struct MemoryEncodingDataset {
    examples: Vec<MemoryEncodingExample>,
    train_cases: Vec<MemoryEncodingEvalCase>,
    dev_cases: Vec<MemoryEncodingEvalCase>,
}

#[derive(Deserialize)]
struct MemoryEncodingExample {
    title: String,
    inputs: Vec<ExampleField>,
    output: MemoryEncodingOutput,
}

#[derive(Deserialize)]
struct MemoryEncodingEvalCase {
    name: String,
    thread_focus: String,
    observation: String,
    action_description: String,
    evidence: String,
    expected_thread_effect: String,
    thread_focus_must_include: Vec<String>,
    event_must_include: Vec<String>,
    required_anchors: Vec<String>,
    forbidden_anchors: Vec<String>,
}

pub fn examples() -> Vec<ProgramExample<MemoryEncodingOutput>> {
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

pub fn train_eval_cases(program: &MemoryEncodingProgram) -> Vec<EvalCase<MemoryEncodingOutput>> {
    to_eval_cases(program, load_dataset().train_cases)
}

pub fn dev_eval_cases(program: &MemoryEncodingProgram) -> Vec<EvalCase<MemoryEncodingOutput>> {
    to_eval_cases(program, load_dataset().dev_cases)
}

fn to_eval_cases(
    program: &MemoryEncodingProgram,
    cases: Vec<MemoryEncodingEvalCase>,
) -> Vec<EvalCase<MemoryEncodingOutput>> {
    cases
        .into_iter()
        .map(|case| {
            let expected_thread_effect = case.expected_thread_effect;
            let thread_focus_must_include = case.thread_focus_must_include;
            let event_must_include = case.event_must_include;
            let required_anchors = case.required_anchors;
            let forbidden_anchors = case.forbidden_anchors;
            EvalCase {
                name: Box::leak(case.name.into_boxed_str()),
                ir: program.dataset_ir(
                    case.thread_focus,
                    case.observation,
                    case.action_description,
                    case.evidence,
                ),
                check: Arc::new(move |output| {
                    check_exact_effect(output, &expected_thread_effect)?;
                    check_thread_focus(output, &thread_focus_must_include)?;
                    check_event_summary(output, &event_must_include)?;
                    check_required_anchors(output, &required_anchors)?;
                    check_forbidden_anchors(output, &forbidden_anchors)
                }),
            }
        })
        .collect()
}

pub fn bootstrap_examples(_case_names: &[&str]) -> Vec<ProgramExample<MemoryEncodingOutput>> {
    Vec::new()
}

fn load_dataset() -> MemoryEncodingDataset {
    decode_dataset_json(DATASET_FILE, DATASET_JSON).expect("memory_encoding dataset must be valid")
}

fn check_exact_effect(output: &MemoryEncodingOutput, expected: &str) -> Result<()> {
    if output.thread_effect.trim() != expected {
        return Err(miette!(
            "expected thread_effect `{}`, got `{}`",
            expected,
            output.thread_effect
        ));
    }
    Ok(())
}

fn check_thread_focus(output: &MemoryEncodingOutput, required_parts: &[String]) -> Result<()> {
    for required in required_parts {
        if !output.thread_focus.contains(required) {
            return Err(miette!(
                "expected thread_focus to contain `{}`, got `{}`",
                required,
                output.thread_focus
            ));
        }
    }
    Ok(())
}

fn check_event_summary(output: &MemoryEncodingOutput, required_parts: &[String]) -> Result<()> {
    for required in required_parts {
        if !output.event_summary.contains(required) {
            return Err(miette!(
                "expected event_summary to contain `{}`, got `{}`",
                required,
                output.event_summary
            ));
        }
    }
    Ok(())
}

fn check_required_anchors(
    output: &MemoryEncodingOutput,
    required_anchors: &[String],
) -> Result<()> {
    for required in required_anchors {
        if !output
            .anchors
            .iter()
            .any(|anchor| anchor.contains(required))
        {
            return Err(miette!(
                "expected anchors to contain `{}`, got {:?}",
                required,
                output.anchors
            ));
        }
    }
    Ok(())
}

fn check_forbidden_anchors(
    output: &MemoryEncodingOutput,
    forbidden_anchors: &[String],
) -> Result<()> {
    for forbidden in forbidden_anchors {
        if output
            .anchors
            .iter()
            .any(|anchor| anchor.contains(forbidden))
        {
            return Err(miette!(
                "expected anchors to avoid `{}`, got {:?}",
                forbidden,
                output.anchors
            ));
        }
    }
    Ok(())
}
