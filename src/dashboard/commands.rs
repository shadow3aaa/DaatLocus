use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::DashboardState;

#[derive(Clone, Debug)]
pub enum DashboardControlCommand {
    RunSleep,
    ClearConversation,
    InterruptRuntime,
    RestartDaemon,
    ReloadSkills,
    SetSkillAutoUse { path: PathBuf, enabled: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DashboardPendingUserInputMoveDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DashboardAction {
    RunSleep,
    ClearConversation,
    InterruptRuntime,
    RestartDaemon,
    ReloadSkills,
    SetSkillAutoUse {
        path: PathBuf,
        enabled: bool,
    },
    DismissPendingUserInput {
        event_id: Uuid,
    },
    ClearPendingUserInputs,
    UpdatePendingUserInput {
        event_id: Uuid,
        incoming_text: String,
    },
    MovePendingUserInput {
        event_id: Uuid,
        direction: DashboardPendingUserInputMoveDirection,
    },
    MovePendingUserInputToPosition {
        event_id: Uuid,
        target_position: usize,
    },
    PreemptPendingUserInput {
        event_id: Uuid,
    },
}

pub(crate) fn dashboard_action_is_manager_owned(_action: &DashboardAction) -> bool {
    false
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardActionResult {
    pub success: bool,
    pub message: String,
    #[serde(default)]
    pub detail: Option<String>,
}

impl DashboardActionResult {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
            detail: None,
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            detail: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardCommandAttachment {
    pub placeholder: String,
    pub name: String,
    pub path: PathBuf,
    pub media_type: String,
}

#[async_trait]
pub trait DashboardCommandRunner: Send + Sync {
    async fn run_command(
        &self,
        command: &str,
        attachments: Vec<DashboardCommandAttachment>,
        state: &DashboardState,
    ) -> String;
    async fn run_action(
        &self,
        action: DashboardAction,
        state: &DashboardState,
    ) -> DashboardActionResult;
}

pub(crate) fn execute_dashboard_action(
    action: DashboardAction,
    control_tx: &tokio::sync::mpsc::UnboundedSender<DashboardControlCommand>,
) -> DashboardActionResult {
    match action {
        DashboardAction::RunSleep => match control_tx.send(DashboardControlCommand::RunSleep) {
            Ok(()) => DashboardActionResult::ok("queued sleep run"),
            Err(err) => DashboardActionResult::error(format!("failed to queue sleep run: {err}")),
        },
        DashboardAction::ClearConversation => {
            match control_tx.send(DashboardControlCommand::ClearConversation) {
                Ok(()) => DashboardActionResult::ok("queued runtime clear"),
                Err(err) => DashboardActionResult::error(format!("failed to queue clear: {err}")),
            }
        }
        DashboardAction::InterruptRuntime => {
            match control_tx.send(DashboardControlCommand::InterruptRuntime) {
                Ok(()) => DashboardActionResult::ok("queued runtime interrupt"),
                Err(err) => {
                    DashboardActionResult::error(format!("failed to queue interrupt: {err}"))
                }
            }
        }
        DashboardAction::RestartDaemon => {
            match control_tx.send(DashboardControlCommand::RestartDaemon) {
                Ok(()) => DashboardActionResult::ok("queued daemon restart"),
                Err(err) => {
                    DashboardActionResult::error(format!("failed to queue daemon restart: {err}"))
                }
            }
        }
        DashboardAction::ReloadSkills => {
            match control_tx.send(DashboardControlCommand::ReloadSkills) {
                Ok(()) => DashboardActionResult::ok("queued skills reload"),
                Err(err) => {
                    DashboardActionResult::error(format!("failed to queue skills reload: {err}"))
                }
            }
        }
        DashboardAction::SetSkillAutoUse { path, enabled } => {
            match control_tx.send(DashboardControlCommand::SetSkillAutoUse { path, enabled }) {
                Ok(()) => {
                    let action = if enabled { "enable" } else { "disable" };
                    DashboardActionResult::ok(format!("queued skills auto-use {action}"))
                }
                Err(err) => {
                    DashboardActionResult::error(format!("failed to queue skills auto-use: {err}"))
                }
            }
        }
        DashboardAction::DismissPendingUserInput { .. }
        | DashboardAction::ClearPendingUserInputs
        | DashboardAction::UpdatePendingUserInput { .. }
        | DashboardAction::MovePendingUserInput { .. }
        | DashboardAction::MovePendingUserInputToPosition { .. }
        | DashboardAction::PreemptPendingUserInput { .. } => {
            DashboardActionResult::error("pending user input actions require a target session")
        }
    }
}
