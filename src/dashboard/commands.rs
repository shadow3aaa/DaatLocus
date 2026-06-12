use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::DashboardState;
use crate::telegram_acl::TelegramAclHandle;

#[derive(Clone, Debug)]
pub enum DashboardControlCommand {
    RunSleep,
    ClearConversation,
    RestartDaemon,
    ReloadSkills,
    SetSkillAutoUse { path: PathBuf, enabled: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DashboardAction {
    RunSleep,
    ClearConversation,
    RestartDaemon,
    ReloadSkills,
    SetSkillAutoUse { path: PathBuf, enabled: bool },
    ApproveTelegramAccess { chat_id: i64 },
    RejectTelegramAccess { chat_id: i64 },
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

#[async_trait]
pub trait DashboardCommandRunner: Send + Sync {
    async fn run_command(&self, command: &str, state: &DashboardState) -> String;
    async fn run_action(
        &self,
        action: DashboardAction,
        state: &DashboardState,
    ) -> DashboardActionResult;
}

pub(crate) fn execute_dashboard_action(
    action: DashboardAction,
    telegram_acl: &TelegramAclHandle,
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
        DashboardAction::ApproveTelegramAccess { chat_id } => match telegram_acl.approve(chat_id) {
            Ok(()) => DashboardActionResult::ok(format!("approved {chat_id}")),
            Err(err) => {
                DashboardActionResult::error(format!("approve failed for {chat_id}: {err}"))
            }
        },
        DashboardAction::RejectTelegramAccess { chat_id } => match telegram_acl.reject(chat_id) {
            Ok(()) => DashboardActionResult::ok(format!("rejected {chat_id}")),
            Err(err) => DashboardActionResult::error(format!("reject failed for {chat_id}: {err}")),
        },
    }
}
