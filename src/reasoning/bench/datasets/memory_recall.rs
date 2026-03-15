use std::sync::Arc;

use miette::{Result, miette};
use serde::Deserialize;

use crate::reasoning::{
    bench::programs::memory_recall::{MemoryRecallOutput, MemoryRecallProgram},
    dataset_store::decode_dataset_json,
    eval::EvalCase,
    examples::{ExampleField, ProgramExample},
};

const DATASET_FILE: &str = "bench/memory_recall.json";
const DATASET_JSON: &str = include_str!("memory_recall.json");

#[derive(Deserialize)]
struct MemoryRecallDataset {
    examples: Vec<MemoryRecallExample>,
    eval_cases: Vec<MemoryRecallEvalCase>,
}

#[derive(Deserialize)]
struct MemoryRecallExample {
    title: String,
    inputs: Vec<ExampleField>,
    output: MemoryRecallOutput,
}

#[derive(Deserialize)]
struct MemoryRecallEvalCase {
    name: String,
    current_goal: String,
    recent_trail: String,
    associated_memories: String,
    question: String,
    required_ids: Vec<String>,
    forbidden_ids: Vec<String>,
    answer_must_include: Vec<String>,
}

pub fn examples() -> Vec<ProgramExample<MemoryRecallOutput>> {
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

pub fn eval_cases(program: &MemoryRecallProgram) -> Vec<EvalCase<MemoryRecallOutput>> {
    load_dataset()
        .eval_cases
        .into_iter()
        .map(|case| {
            let required_ids = case.required_ids;
            let forbidden_ids = case.forbidden_ids;
            let answer_must_include = case.answer_must_include;
            EvalCase {
                name: Box::leak(case.name.into_boxed_str()),
                ir: program.dataset_ir(
                    case.current_goal,
                    case.recent_trail,
                    case.associated_memories,
                    case.question,
                ),
                check: Arc::new(move |output| {
                    check_required_ids(output, &required_ids)?;
                    check_forbidden_ids(output, &forbidden_ids)?;
                    check_answer_contains(output, &answer_must_include)
                }),
            }
        })
        .collect()
}

fn load_dataset() -> MemoryRecallDataset {
    decode_dataset_json(DATASET_FILE, DATASET_JSON).expect("memory_recall dataset must be valid")
}

fn check_required_ids(output: &MemoryRecallOutput, required_ids: &[String]) -> Result<()> {
    for required_id in required_ids {
        if !output
            .relevant_memory_ids
            .iter()
            .any(|id| id == required_id)
        {
            return Err(miette!(
                "expected relevant_memory_ids to contain {}, got {:?}",
                required_id,
                output.relevant_memory_ids
            ));
        }
    }
    Ok(())
}

fn check_forbidden_ids(output: &MemoryRecallOutput, forbidden_ids: &[String]) -> Result<()> {
    for forbidden_id in forbidden_ids {
        if output
            .relevant_memory_ids
            .iter()
            .any(|id| id == forbidden_id)
        {
            return Err(miette!(
                "expected relevant_memory_ids to avoid noise id {}, got {:?}",
                forbidden_id,
                output.relevant_memory_ids
            ));
        }
    }
    Ok(())
}

fn check_answer_contains(output: &MemoryRecallOutput, required_parts: &[String]) -> Result<()> {
    for required_part in required_parts {
        if !output.answer.contains(required_part) {
            return Err(miette!(
                "expected answer to contain `{}`, got `{}`",
                required_part,
                output.answer
            ));
        }
    }
    Ok(())
}
