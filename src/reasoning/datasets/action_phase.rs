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
    bootstrap_output: Option<Output>,
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
    #[serde(rename = "cancel_interactive_prompt")]
    CancelInteractivePrompt,
    #[serde(rename = "focus_terminal")]
    FocusTerminal,
    #[serde(rename = "silent_wait")]
    SilentWait,
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
                    ActionPhaseExpectation::CancelInteractivePrompt => {
                        Arc::new(check_cancel_interactive_prompt)
                    }
                    ActionPhaseExpectation::FocusTerminal => Arc::new(check_focus_terminal),
                    ActionPhaseExpectation::SilentWait => Arc::new(check_silent_wait),
                },
            }
        })
        .collect()
}

pub fn bootstrap_examples(phase: ActionPhase, case_names: &[&str]) -> Vec<ProgramExample<Output>> {
    section_for_phase(load_dataset(), phase)
        .eval_cases
        .into_iter()
        .filter(|case| case_names.iter().any(|name| *name == case.name))
        .filter_map(|case| {
            case.bootstrap_output.map(|output| ProgramExample {
                title: format!("Bootstrap from {}", case.name),
                inputs: vec![
                    crate::reasoning::examples::ExampleField {
                        name: "设备上下文".to_string(),
                        value: case.device_context,
                    },
                    crate::reasoning::examples::ExampleField {
                        name: "完整快照".to_string(),
                        value: case.snapshot_text,
                    },
                ],
                output,
            })
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

fn check_silent_wait(output: &Output) -> Result<()> {
    match &output.action {
        Action::SilentWait => Ok(()),
        other => Err(miette!("expected SilentWait, got {:?}", other)),
    }
}

fn check_cancel_interactive_prompt(output: &Output) -> Result<()> {
    match &output.action {
        Action::DeviceAction {
            action: crate::device::DeviceAction::TerminalInput { text },
        } if text.contains('\u{3}') => Ok(()),
        other => Err(miette!(
            "expected TerminalInput containing Ctrl+C to cancel interactive prompt, got {:?}",
            other
        )),
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
