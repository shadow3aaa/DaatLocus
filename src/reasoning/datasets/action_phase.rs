use std::sync::Arc;

use miette::{Result, miette};
use serde::Deserialize;

use crate::{
    core::{Action, Output},
    device::DeviceId,
    reasoning::{
        dataset_store::decode_dataset_json,
        eval::EvalCase,
        examples::ProgramExample,
        programs::action_phase::{ActionPhase, ActionPhaseProgram},
    },
};

const DATASET_FILE: &str = "action_phase.json";
const DATASET_JSON: &str = include_str!("action_phase.json");

#[derive(Deserialize)]
struct ActionPhaseDataset {
    attend_notifications: ActionPhaseSection,
    execute_task: ActionPhaseSection,
    plan_from_project: ActionPhaseSection,
    explore_new_tasks: ActionPhaseSection,
}

#[derive(Deserialize)]
struct ActionPhaseSection {
    examples: Vec<ProgramExample<Output>>,
    eval_cases: Vec<ActionPhaseEvalCase>,
}

#[derive(Deserialize)]
struct ActionPhaseEvalCase {
    name: String,
    device_context: String,
    snapshot_text: String,
    expectation: ActionPhaseExpectation,
}

#[derive(Deserialize)]
#[serde(tag = "kind")]
enum ActionPhaseExpectation {
    #[serde(rename = "focus_telegram")]
    FocusTelegram,
    #[serde(rename = "select_task")]
    SelectTask { task_id: String },
    #[serde(rename = "add_project_task")]
    AddProjectTask { project_id: String },
    #[serde(rename = "focus_terminal")]
    FocusTerminal,
}

pub fn examples(phase: ActionPhase) -> Vec<ProgramExample<Output>> {
    section_for_phase(load_dataset(), phase).examples
}

pub fn eval_cases(program: &ActionPhaseProgram) -> Vec<EvalCase<Output>> {
    section_for_phase(load_dataset(), program.phase())
        .eval_cases
        .into_iter()
        .map(|case| {
            let expectation = case.expectation;
            EvalCase {
                name: Box::leak(case.name.into_boxed_str()),
                ir: program.dataset_ir(case.device_context, case.snapshot_text),
                check: match expectation {
                    ActionPhaseExpectation::FocusTelegram => Arc::new(check_focus_telegram),
                    ActionPhaseExpectation::SelectTask { task_id } => {
                        register_select_task_check(task_id)
                    }
                    ActionPhaseExpectation::AddProjectTask { project_id } => {
                        register_add_project_task_check(project_id)
                    }
                    ActionPhaseExpectation::FocusTerminal => Arc::new(check_focus_terminal),
                },
            }
        })
        .collect()
}

fn load_dataset() -> ActionPhaseDataset {
    decode_dataset_json(DATASET_FILE, DATASET_JSON).expect("action_phase dataset must be valid")
}

fn section_for_phase(dataset: ActionPhaseDataset, phase: ActionPhase) -> ActionPhaseSection {
    match phase {
        ActionPhase::AttendNotifications => dataset.attend_notifications,
        ActionPhase::ExecuteTask => dataset.execute_task,
        ActionPhase::PlanFromProject => dataset.plan_from_project,
        ActionPhase::ExploreNewTasks => dataset.explore_new_tasks,
    }
}

fn check_focus_telegram(output: &Output) -> Result<()> {
    match &output.action {
        Action::FocusDevice {
            device: DeviceId::Telegram,
        } => Ok(()),
        other => Err(miette!("expected FocusDevice(Telegram), got {:?}", other)),
    }
}

fn check_focus_terminal(output: &Output) -> Result<()> {
    match &output.action {
        Action::FocusDevice {
            device: DeviceId::Terminal,
        } => Ok(()),
        other => Err(miette!("expected FocusDevice(Terminal), got {:?}", other)),
    }
}

fn register_select_task_check(
    expected_task_id: String,
) -> Arc<dyn Fn(&Output) -> Result<()> + Send + Sync> {
    Arc::new(move |output: &Output| match &output.action {
        Action::TaskSelect { task_id } if task_id == &expected_task_id => Ok(()),
        other => Err(miette!(
            "expected TaskSelect on task {}, got {:?}",
            expected_task_id,
            other
        )),
    })
}

fn register_add_project_task_check(
    expected_project_id: String,
) -> Arc<dyn Fn(&Output) -> Result<()> + Send + Sync> {
    Arc::new(move |output: &Output| match &output.action {
        Action::TaskAdd {
            description,
            project_id: Some(project_id),
        } if project_id == &expected_project_id && !description.trim().is_empty() => Ok(()),
        other => Err(miette!(
            "expected TaskAdd with project_id={}, got {:?}",
            expected_project_id,
            other
        )),
    })
}
