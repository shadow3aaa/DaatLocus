use std::sync::Arc;

use miette::{Result, miette};
use serde::Deserialize;

use crate::reasoning::{
    bench::programs::continuity_guard::{ContinuityGuardOutput, ContinuityGuardProgram},
    dataset_store::decode_dataset_json,
    eval::EvalCase,
    examples::{ExampleField, ProgramExample},
};

const DATASET_FILE: &str = "bench/continuity_guard.json";
const DATASET_JSON: &str = include_str!("continuity_guard.json");

#[derive(Deserialize)]
struct ContinuityGuardDataset {
    examples: Vec<ContinuityGuardExample>,
    train_cases: Vec<ContinuityGuardEvalCase>,
    dev_cases: Vec<ContinuityGuardEvalCase>,
}

#[derive(Deserialize)]
struct ContinuityGuardExample {
    title: String,
    inputs: Vec<ExampleField>,
    output: ContinuityGuardOutput,
}

#[derive(Deserialize)]
struct ContinuityGuardEvalCase {
    name: String,
    current_projects: String,
    current_work: String,
    recent_history: String,
    recalled_memories: String,
    question: String,
    expected_should_continue: bool,
    expected_project_title: Option<String>,
    required_ids: Vec<String>,
    forbidden_ids: Vec<String>,
    reason_must_include: Vec<String>,
    bootstrap_output: Option<ContinuityGuardOutput>,
}

pub fn examples() -> Vec<ProgramExample<ContinuityGuardOutput>> {
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

pub fn train_eval_cases(program: &ContinuityGuardProgram) -> Vec<EvalCase<ContinuityGuardOutput>> {
    to_eval_cases(program, load_dataset().train_cases)
}

pub fn dev_eval_cases(program: &ContinuityGuardProgram) -> Vec<EvalCase<ContinuityGuardOutput>> {
    to_eval_cases(program, load_dataset().dev_cases)
}

fn to_eval_cases(
    program: &ContinuityGuardProgram,
    cases: Vec<ContinuityGuardEvalCase>,
) -> Vec<EvalCase<ContinuityGuardOutput>> {
    cases
        .into_iter()
        .map(|case| {
            let expected_should_continue = case.expected_should_continue;
            let expected_project_title = case.expected_project_title;
            let required_ids = case.required_ids;
            let forbidden_ids = case.forbidden_ids;
            let reason_must_include = case.reason_must_include;
            EvalCase {
                name: Box::leak(case.name.into_boxed_str()),
                ir: program.dataset_ir(
                    case.current_projects,
                    case.current_work,
                    case.recent_history,
                    case.recalled_memories,
                    case.question,
                ),
                check: Arc::new(move |output| {
                    check_expected_project(
                        output,
                        expected_should_continue,
                        expected_project_title.as_deref(),
                    )?;
                    check_required_ids(output, &required_ids)?;
                    check_forbidden_ids(output, &forbidden_ids)?;
                    check_reason_contains(output, &reason_must_include)
                }),
            }
        })
        .collect()
}

pub fn bootstrap_examples(case_names: &[&str]) -> Vec<ProgramExample<ContinuityGuardOutput>> {
    load_dataset()
        .train_cases
        .into_iter()
        .filter(|case| case_names.iter().any(|name| *name == case.name))
        .filter_map(|case| {
            case.bootstrap_output.map(|output| ProgramExample {
                title: format!("Bootstrap from {}", case.name),
                inputs: vec![
                    ExampleField {
                        name: "当前项目状态".to_string(),
                        value: case.current_projects,
                    },
                    ExampleField {
                        name: "当前工作状态".to_string(),
                        value: case.current_work,
                    },
                    ExampleField {
                        name: "近期历史".to_string(),
                        value: case.recent_history,
                    },
                    ExampleField {
                        name: "召回记忆".to_string(),
                        value: case.recalled_memories,
                    },
                    ExampleField {
                        name: "问题".to_string(),
                        value: case.question,
                    },
                ],
                output,
            })
        })
        .collect()
}

fn load_dataset() -> ContinuityGuardDataset {
    decode_dataset_json(DATASET_FILE, DATASET_JSON).expect("continuity_guard dataset must be valid")
}

fn check_expected_project(
    output: &ContinuityGuardOutput,
    expected_should_continue: bool,
    expected_project_title: Option<&str>,
) -> Result<()> {
    if output.should_continue_project != expected_should_continue {
        return Err(miette!(
            "expected should_continue_project={}, got {}",
            expected_should_continue,
            output.should_continue_project
        ));
    }

    match (expected_project_title, output.project_title.as_deref()) {
        (Some(expected), Some(actual)) if expected == actual => Ok(()),
        (None, None) => Ok(()),
        (Some(expected), actual) => Err(miette!(
            "expected project_title {:?}, got {:?}",
            expected,
            actual
        )),
        (None, actual) => Err(miette!("expected empty project_title, got {:?}", actual)),
    }
}

fn check_required_ids(output: &ContinuityGuardOutput, required_ids: &[String]) -> Result<()> {
    for required_id in required_ids {
        if !output
            .supporting_memory_ids
            .iter()
            .any(|id| id == required_id)
        {
            return Err(miette!(
                "expected supporting_memory_ids to contain {}, got {:?}",
                required_id,
                output.supporting_memory_ids
            ));
        }
    }
    Ok(())
}

fn check_forbidden_ids(output: &ContinuityGuardOutput, forbidden_ids: &[String]) -> Result<()> {
    for forbidden_id in forbidden_ids {
        if output
            .supporting_memory_ids
            .iter()
            .any(|id| id == forbidden_id)
        {
            return Err(miette!(
                "expected supporting_memory_ids to avoid noise id {}, got {:?}",
                forbidden_id,
                output.supporting_memory_ids
            ));
        }
    }
    Ok(())
}

fn check_reason_contains(output: &ContinuityGuardOutput, required_parts: &[String]) -> Result<()> {
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
