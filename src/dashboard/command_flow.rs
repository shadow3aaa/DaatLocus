use std::path::PathBuf;

use super::command_panels::{
    CommandFeedback, CommandFeedbackLevel, CommandPanel, CommandSelectionAction,
    CommandSelectionItem, CommandSelectionPanel, CommandSuggestion, DashboardActionInvocation,
    DashboardCommandContext, SkillsListPanel, TelegramAccessAction, TelegramAccessPicker,
    detail_panel,
};
use super::command_registry::{
    app_status_command_accepts, clear_command_accepts, dashboard_command_is_known,
    dashboard_commands, debug_command_accepts, quit_command_accepts, restart_command_accepts,
    skills_command_accepts, sleep_command_accepts, status_command_accepts,
    telegram_command_accepts,
};
use super::command_text::{
    fallback_output, format_pending_request_choices, render_app_status_text,
    render_available_app_statuses, render_pending_access_requests, render_skill_detail,
    render_skills_list, resolve_skill_target, skill_status_description, truncate_command_text,
};
use super::{DashboardAction, DashboardActionResult, DashboardControlCommand, DashboardState};
use crate::{
    reasoning::turn_compile::{
        load_prompt_persona_spec_sync, prompt_persona_path_sync, render_prompt_persona_markdown,
    },
    telegram_acl::{PendingAccessRequest, TelegramAclHandle},
};

pub(super) fn command_feedback_from_action_result(
    title: String,
    result: DashboardActionResult,
) -> CommandFeedback {
    CommandFeedback {
        title,
        message: result.message,
        detail: result.detail,
        level: if result.success {
            CommandFeedbackLevel::Info
        } else {
            CommandFeedbackLevel::Error
        },
    }
}

pub(super) fn command_panel_for_input(
    input: &str,
    context: &DashboardCommandContext<'_>,
) -> Option<CommandPanel> {
    let parts = dashboard_command_parts(input)?;
    match parts.as_slice() {
        ["status"] => Some(detail_panel(
            "STATUS",
            fallback_output(&context.state.status_output),
        )),
        ["debug"] => Some(debug_command_panel(context.state)),
        ["debug", "persona"] => Some(debug_persona_panel()),
        ["debug", "system-prompt"] | ["debug", "system_prompt"] => {
            Some(debug_system_prompt_panel(context.state))
        }
        ["debug", "context"] | ["debug", "preturn-context"] | ["debug", "preturn_context"] => {
            Some(debug_context_panel(context.state))
        }
        ["sleep"] => Some(sleep_command_panel(context.state)),
        ["sleep", "status"] => Some(sleep_status_panel(context.state)),
        ["telegram"] => Some(telegram_command_panel(context.state, context.requests)),
        ["telegram", "status"] => Some(telegram_status_panel(context.state)),
        ["telegram", "approve"] => Some(
            telegram_access_picker_for_input(input, context.requests)
                .map(CommandPanel::TelegramAccess)
                .unwrap_or_else(|| {
                    telegram_access_command_panel(TelegramAccessAction::Approve, context.requests)
                }),
        ),
        ["telegram", "reject"] => Some(
            telegram_access_picker_for_input(input, context.requests)
                .map(CommandPanel::TelegramAccess)
                .unwrap_or_else(|| {
                    telegram_access_command_panel(TelegramAccessAction::Reject, context.requests)
                }),
        ),
        [verb] if app_status_command_accepts(verb) => {
            Some(app_status_selection_panel(context.state))
        }
        [verb, target] if app_status_command_accepts(verb) => {
            Some(app_status_detail_panel(context.state, target))
        }
        ["skills"] => Some(skills_command_panel(context.state)),
        ["skills", "list"] | ["skills", "show"] => Some(CommandPanel::SkillsList(
            SkillsListPanel::from_state(context.state),
        )),
        ["skills", "show", target] => skill_detail_panel(context.state, target),
        _ => None,
    }
}

pub(super) fn dashboard_action_for_input(
    input: &str,
    context: &DashboardCommandContext<'_>,
) -> Result<Option<DashboardActionInvocation>, CommandFeedback> {
    let Some(parts) = dashboard_command_parts(input) else {
        return Ok(None);
    };
    let invocation = match parts.as_slice() {
        ["clear"] => DashboardActionInvocation {
            title: "CLEAR".to_string(),
            action: DashboardAction::ClearConversation,
            quiet_success: true,
        },
        ["restart"] => DashboardActionInvocation {
            title: "RESTART".to_string(),
            action: DashboardAction::RestartDaemon,
            quiet_success: false,
        },
        ["sleep", "run"] => DashboardActionInvocation {
            title: "SLEEP".to_string(),
            action: DashboardAction::RunSleep,
            quiet_success: false,
        },
        ["skills", "reload"] => DashboardActionInvocation {
            title: "SKILLS".to_string(),
            action: DashboardAction::ReloadSkills,
            quiet_success: false,
        },
        ["skills", "enable", target] | ["skills", "disable", target] => {
            let enabled = parts[1] == "enable";
            let skill =
                resolve_skill_target(context.state, target).map_err(|message| CommandFeedback {
                    title: "SKILLS".to_string(),
                    message,
                    detail: None,
                    level: CommandFeedbackLevel::Error,
                })?;
            DashboardActionInvocation {
                title: "SKILLS".to_string(),
                action: DashboardAction::SetSkillAutoUse {
                    path: PathBuf::from(&skill.path),
                    enabled,
                },
                quiet_success: false,
            }
        }
        ["telegram", "approve", chat_id] | ["telegram", "reject", chat_id] => {
            let chat_id = chat_id.parse::<i64>().map_err(|_| CommandFeedback {
                title: "TELEGRAM".to_string(),
                message: format!("invalid chat_id: {chat_id}"),
                detail: None,
                level: CommandFeedbackLevel::Error,
            })?;
            let action = if parts[1] == "approve" {
                DashboardAction::ApproveTelegramAccess { chat_id }
            } else {
                DashboardAction::RejectTelegramAccess { chat_id }
            };
            DashboardActionInvocation {
                title: "TELEGRAM".to_string(),
                action,
                quiet_success: false,
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(invocation))
}

pub(super) fn telegram_access_command_panel(
    action: TelegramAccessAction,
    requests: &[PendingAccessRequest],
) -> CommandPanel {
    if requests.is_empty() {
        return detail_panel(action.title(), "No pending Telegram access requests.");
    }
    CommandPanel::TelegramAccess(TelegramAccessPicker {
        action,
        requests: requests.to_vec(),
        selected: 0,
        scroll: 0,
    })
}

pub(crate) fn execute_control_command(
    command: &str,
    telegram_acl: &TelegramAclHandle,
    state: &DashboardState,
    control_tx: &tokio::sync::mpsc::UnboundedSender<DashboardControlCommand>,
) -> String {
    let command = command.trim().trim_start_matches('/').trim();
    if command.is_empty() {
        return "empty command".to_string();
    }
    let input = format!("/{command}");
    let requests = telegram_acl.pending_requests();
    let context = DashboardCommandContext {
        requests: &requests,
        state,
    };
    let Some(parts) = dashboard_command_parts(&input) else {
        return "empty command".to_string();
    };

    if matches!(parts.as_slice(), ["quit"] | ["q"] | ["exit"]) {
        return "quit command is only available in the local dashboard".to_string();
    }

    match dashboard_action_for_input(&input, &context) {
        Ok(Some(invocation)) => {
            let result =
                super::execute_dashboard_action(invocation.action, telegram_acl, control_tx);
            return result.message;
        }
        Ok(None) => {}
        Err(feedback) => return feedback.message,
    }

    if let Some(feedback) = command_extra_argument_feedback(&parts) {
        return feedback.message;
    }

    match parts.as_slice() {
        ["status"] => fallback_output(&state.status_output),
        ["debug"] => "available views: persona, system-prompt, context".to_string(),
        ["debug", "persona"] => debug_persona_text(),
        ["debug", "system-prompt"] | ["debug", "system_prompt"] => {
            fallback_output(&state.system_prompt_output)
        }
        ["debug", "context"] | ["debug", "preturn-context"] | ["debug", "preturn_context"] => {
            fallback_output(&state.preturn_context_output)
        }
        [verb] if app_status_command_accepts(verb) => render_available_app_statuses(state),
        [verb, target] if app_status_command_accepts(verb) => render_app_status_text(state, target),
        ["sleep"] => "available actions: status, run".to_string(),
        ["sleep", "status"] => fallback_output(&state.sleep_status_output),
        ["skills"] | ["skills", "list"] | ["skills", "show"] => render_skills_list(state),
        ["skills", "show", target] => render_skill_detail(state, target),
        ["telegram"] => "available actions: status, approve, reject".to_string(),
        ["telegram", "status"] => fallback_output(&state.inspect_telegram_output),
        ["telegram", "approve"] => render_pending_access_requests("approve", &requests),
        ["telegram", "reject"] => render_pending_access_requests("reject", &requests),
        [verb, ..] if dashboard_command_is_known(verb) => {
            format!("unsupported command shape: /{}", parts.join(" "))
        }
        [verb, ..] => format!("unknown command: {verb}"),
        [] => "empty command".to_string(),
    }
}

fn debug_command_panel(state: &DashboardState) -> CommandPanel {
    CommandPanel::Selection(CommandSelectionPanel {
        title: "Debug".to_string(),
        subtitle: Some("Inspect internal runtime views.".to_string()),
        items: vec![
            CommandSelectionItem {
                name: "Prompt persona".to_string(),
                description: "show current prompt persona config".to_string(),
                action: CommandSelectionAction::ShowDetail {
                    title: "DEBUG PERSONA".to_string(),
                    text: debug_persona_text(),
                },
                disabled: false,
            },
            CommandSelectionItem {
                name: "System prompt".to_string(),
                description: "show current runtime system prompt".to_string(),
                action: CommandSelectionAction::ShowDetail {
                    title: "DEBUG SYSTEM PROMPT".to_string(),
                    text: fallback_output(&state.system_prompt_output),
                },
                disabled: false,
            },
            CommandSelectionItem {
                name: "Runtime context".to_string(),
                description: "show latest pre-turn runtime context".to_string(),
                action: CommandSelectionAction::ShowDetail {
                    title: "DEBUG CONTEXT".to_string(),
                    text: fallback_output(&state.preturn_context_output),
                },
                disabled: false,
            },
        ],
        selected: 0,
        scroll: 0,
    })
}

fn debug_persona_text() -> String {
    let path = prompt_persona_path_sync();
    match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(_) => render_prompt_persona_markdown(&load_prompt_persona_spec_sync()),
    }
}

fn debug_persona_panel() -> CommandPanel {
    detail_panel("DEBUG PERSONA", debug_persona_text())
}

fn debug_system_prompt_panel(state: &DashboardState) -> CommandPanel {
    detail_panel(
        "DEBUG SYSTEM PROMPT",
        fallback_output(&state.system_prompt_output),
    )
}

fn debug_context_panel(state: &DashboardState) -> CommandPanel {
    detail_panel(
        "DEBUG CONTEXT",
        fallback_output(&state.preturn_context_output),
    )
}

fn sleep_command_panel(state: &DashboardState) -> CommandPanel {
    CommandPanel::Selection(CommandSelectionPanel {
        title: "Sleep".to_string(),
        subtitle: Some("Inspect sleep state or start a background sleep run.".to_string()),
        items: vec![
            CommandSelectionItem {
                name: "Status".to_string(),
                description: "show sleep status".to_string(),
                action: CommandSelectionAction::ShowDetail {
                    title: "SLEEP STATUS".to_string(),
                    text: fallback_output(&state.sleep_status_output),
                },
                disabled: false,
            },
            CommandSelectionItem {
                name: "Start sleep run".to_string(),
                description: "start a background sleep run".to_string(),
                action: CommandSelectionAction::RunAction {
                    title: "SLEEP".to_string(),
                    action: DashboardAction::RunSleep,
                    keep_panel: false,
                },
                disabled: false,
            },
        ],
        selected: 0,
        scroll: 0,
    })
}

fn sleep_status_panel(state: &DashboardState) -> CommandPanel {
    detail_panel("SLEEP STATUS", fallback_output(&state.sleep_status_output))
}

fn app_status_selection_panel(state: &DashboardState) -> CommandPanel {
    let items = state
        .app_status_outputs
        .iter()
        .map(|(name, output)| CommandSelectionItem {
            name: name.clone(),
            description: truncate_command_text(
                output
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("app state"),
                120,
            ),
            action: CommandSelectionAction::ShowDetail {
                title: format!("APP STATUS {}", name.to_uppercase()),
                text: output.clone(),
            },
            disabled: false,
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        return detail_panel("APP STATUS", "No app state is currently available.");
    }
    CommandPanel::Selection(CommandSelectionPanel {
        title: "App Status".to_string(),
        subtitle: Some("Choose an app to inspect.".to_string()),
        items,
        selected: 0,
        scroll: 0,
    })
}

fn app_status_detail_panel(state: &DashboardState, target: &str) -> CommandPanel {
    let output = render_app_status_text(state, target);
    let target = target.trim().to_ascii_lowercase();
    detail_panel(format!("APP STATUS {}", target.to_uppercase()), output)
}

fn telegram_command_panel(
    state: &DashboardState,
    requests: &[PendingAccessRequest],
) -> CommandPanel {
    CommandPanel::Selection(CommandSelectionPanel {
        title: "Telegram".to_string(),
        subtitle: Some("Inspect transport state or handle access requests.".to_string()),
        items: vec![
            CommandSelectionItem {
                name: "Status".to_string(),
                description: "show Telegram transport details".to_string(),
                action: CommandSelectionAction::ShowDetail {
                    title: "TELEGRAM STATUS".to_string(),
                    text: fallback_output(&state.inspect_telegram_output),
                },
                disabled: false,
            },
            CommandSelectionItem {
                name: "Approve access request".to_string(),
                description: format!("approve one of {} pending requests", requests.len()),
                action: CommandSelectionAction::OpenTelegramAccess(TelegramAccessAction::Approve),
                disabled: requests.is_empty(),
            },
            CommandSelectionItem {
                name: "Reject access request".to_string(),
                description: format!("reject one of {} pending requests", requests.len()),
                action: CommandSelectionAction::OpenTelegramAccess(TelegramAccessAction::Reject),
                disabled: requests.is_empty(),
            },
        ],
        selected: 0,
        scroll: 0,
    })
}

fn telegram_status_panel(state: &DashboardState) -> CommandPanel {
    detail_panel(
        "TELEGRAM STATUS",
        fallback_output(&state.inspect_telegram_output),
    )
}

fn skills_command_panel(state: &DashboardState) -> CommandPanel {
    let auto_count = state
        .skills
        .iter()
        .filter(|skill| skill.auto_use_enabled)
        .count();
    let manual_count = state.skills.len().saturating_sub(auto_count);
    CommandPanel::Selection(CommandSelectionPanel {
        title: "Skills".to_string(),
        subtitle: Some(format!(
            "{} loaded, {auto_count} auto-use, {manual_count} manual-only",
            state.skills.len()
        )),
        items: vec![
            CommandSelectionItem {
                name: "List skills".to_string(),
                description: "show loaded skills and load errors".to_string(),
                action: CommandSelectionAction::OpenSkillsList,
                disabled: false,
            },
            CommandSelectionItem {
                name: "Enable/Disable Skills".to_string(),
                description: "toggle whether skills may be selected automatically".to_string(),
                action: CommandSelectionAction::OpenSkillsToggle,
                disabled: state.skills.is_empty(),
            },
        ],
        selected: 0,
        scroll: 0,
    })
}

fn skill_detail_panel(state: &DashboardState, target: &str) -> Option<CommandPanel> {
    let skill = resolve_skill_target(state, target).ok()?;
    Some(detail_panel(
        format!("SKILL {}", skill.name),
        [
            format!("Name: {}", skill.name),
            format!("Status: {}", skill_status_description(skill)),
            format!("Scope: {}", skill.scope),
            format!("Path: {}", skill.path),
            format!("Description: {}", skill.description),
        ]
        .join("\n"),
    ))
}

pub(super) fn telegram_access_picker_for_input(
    input: &str,
    requests: &[PendingAccessRequest],
) -> Option<TelegramAccessPicker> {
    if requests.is_empty() {
        return None;
    }

    let command = dashboard_command_body(input)?;
    let parts = command.split_whitespace().collect::<Vec<_>>();
    let action = match parts.as_slice() {
        ["telegram", "approve"] => TelegramAccessAction::Approve,
        ["telegram", "reject"] => TelegramAccessAction::Reject,
        _ => return None,
    };

    Some(TelegramAccessPicker {
        action,
        requests: requests.to_vec(),
        selected: 0,
        scroll: 0,
    })
}

pub(super) fn is_clear_command_input(input: &str) -> bool {
    matches!(
        dashboard_command_parts(input).as_deref(),
        Some(["clear", ..])
    )
}

fn debug_subcommand_is_read_only(subcommand: &str) -> bool {
    matches!(
        subcommand,
        "persona"
            | "system-prompt"
            | "system_prompt"
            | "context"
            | "preturn-context"
            | "preturn_context"
    )
}

pub(super) fn dashboard_command_parts(input: &str) -> Option<Vec<&str>> {
    let body = dashboard_command_body(input)?;
    let parts = body.split_whitespace().collect::<Vec<_>>();
    (!parts.is_empty()).then_some(parts)
}

pub(super) fn command_live_feedback(
    input: &str,
    context: &DashboardCommandContext<'_>,
) -> Option<CommandFeedback> {
    let command_input = command_completion_body(input)?;
    let trimmed = command_input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parts = trimmed.split_whitespace().collect::<Vec<_>>();
    let verb = parts.first().copied().unwrap_or_default();
    let command = dashboard_commands()
        .iter()
        .copied()
        .find(|command| command.accepts(verb));
    let Some(_command) = command else {
        if matching_commands(input, context).is_empty() {
            return Some(CommandFeedback {
                title: "UNKNOWN COMMAND".to_string(),
                message: format!("No dashboard command named '{verb}'."),
                detail: Some("Type / to browse available commands.".to_string()),
                level: CommandFeedbackLevel::Error,
            });
        }
        return None;
    };

    if parts.len() == 1 {
        return None;
    }

    if let Some(feedback) = command_extra_argument_feedback(&parts) {
        return Some(feedback);
    }

    if debug_command_accepts(verb) {
        let subcommand = parts[1];
        if !debug_subcommand_is_read_only(subcommand) {
            return Some(unknown_command_part_feedback(
                "DEBUG",
                format!("Unknown debug view '{subcommand}'."),
                "Use /debug to choose a view.",
            ));
        }
    } else if sleep_command_accepts(verb) {
        match parts.as_slice() {
            ["sleep", "run"] | ["sleep", "status"] => {}
            ["sleep", subcommand, ..] => {
                return Some(unknown_command_part_feedback(
                    "SLEEP",
                    format!("Unknown sleep action '{subcommand}'."),
                    "Use /sleep to choose an action.",
                ));
            }
            _ => {}
        }
    } else if skills_command_accepts(verb) {
        match parts.as_slice() {
            ["skills", "list"] | ["skills", "reload"] => {}
            ["skills", "show" | "enable" | "disable", target] => {
                if let Err(message) = resolve_skill_target(context.state, target) {
                    return Some(CommandFeedback {
                        title: "SKILLS".to_string(),
                        message,
                        detail: Some("Use /skills to browse loaded skills.".to_string()),
                        level: CommandFeedbackLevel::Error,
                    });
                }
            }
            ["skills", subcommand, ..] => {
                return Some(unknown_command_part_feedback(
                    "SKILLS",
                    format!("Unknown skills action '{subcommand}'."),
                    "Use /skills to choose an action.",
                ));
            }
            _ => {}
        }
    } else if telegram_command_accepts(verb) {
        match parts.as_slice() {
            ["telegram", "status"] => {}
            ["telegram", "approve" | "reject"] => {
                let subcommand = parts[1];
                if context.requests.is_empty() {
                    return Some(CommandFeedback {
                        title: "TELEGRAM".to_string(),
                        message: format!("No pending Telegram requests to {subcommand}."),
                        detail: Some("Use /telegram to inspect Telegram state.".to_string()),
                        level: CommandFeedbackLevel::Info,
                    });
                }
                return Some(CommandFeedback {
                    title: "TELEGRAM".to_string(),
                    message: format!("Press Enter to choose a request to {subcommand}."),
                    detail: Some(format_pending_request_choices(context.requests)),
                    level: CommandFeedbackLevel::Info,
                });
            }
            ["telegram", "approve" | "reject", chat_id] if chat_id.parse::<i64>().is_err() => {
                return Some(CommandFeedback {
                    title: "TELEGRAM".to_string(),
                    message: format!("Invalid chat_id '{chat_id}'."),
                    detail: None,
                    level: CommandFeedbackLevel::Error,
                });
            }
            ["telegram", "approve" | "reject", _] => {}
            ["telegram", subcommand, ..] => {
                return Some(unknown_command_part_feedback(
                    "TELEGRAM",
                    format!("Unknown Telegram action '{subcommand}'."),
                    "Use /telegram to choose an action.",
                ));
            }
            _ => {}
        }
    } else if app_status_command_accepts(verb) {
        let apps = context
            .state
            .app_status_outputs
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        let target = parts[1].to_ascii_lowercase();
        let known = apps.iter().any(|name| *name == target);
        let possible = apps.iter().any(|name| name.starts_with(&target));
        if !known && !possible {
            return Some(CommandFeedback {
                title: "APP STATUS".to_string(),
                message: format!("Unknown app '{target}'."),
                detail: Some(if apps.is_empty() {
                    "No app state is currently available.".to_string()
                } else {
                    format!("available: {}", apps.join(", "))
                }),
                level: CommandFeedbackLevel::Error,
            });
        }
    }

    None
}

fn unknown_command_part_feedback(
    title: &str,
    message: impl Into<String>,
    detail: impl Into<String>,
) -> CommandFeedback {
    CommandFeedback {
        title: title.to_string(),
        message: message.into(),
        detail: Some(detail.into()),
        level: CommandFeedbackLevel::Error,
    }
}

fn command_extra_argument_feedback(parts: &[&str]) -> Option<CommandFeedback> {
    let verb = parts.first().copied().unwrap_or_default();
    let extra_for_root = |usage: &str| CommandFeedback {
        title: verb.to_uppercase(),
        message: format!("{verb} does not take extra arguments."),
        detail: Some(format!("usage: /{usage}")),
        level: CommandFeedbackLevel::Error,
    };
    if (quit_command_accepts(verb)
        || clear_command_accepts(verb)
        || status_command_accepts(verb)
        || restart_command_accepts(verb))
        && parts.len() > 1
    {
        let usage = if quit_command_accepts(verb) {
            "quit"
        } else if clear_command_accepts(verb) {
            "clear"
        } else if status_command_accepts(verb) {
            "status"
        } else {
            "restart"
        };
        return Some(extra_for_root(usage));
    }

    match parts {
        ["debug", subcommand, ..]
            if parts.len() > 2 && debug_subcommand_is_read_only(subcommand) =>
        {
            Some(CommandFeedback {
                title: "DEBUG".to_string(),
                message: format!("debug {subcommand} does not take extra arguments."),
                detail: Some(format!("usage: /debug {subcommand}")),
                level: CommandFeedbackLevel::Error,
            })
        }
        ["sleep", "run" | "status", ..] if parts.len() > 2 => Some(CommandFeedback {
            title: "SLEEP".to_string(),
            message: format!("sleep {} does not take extra arguments.", parts[1]),
            detail: Some(format!("usage: /sleep {}", parts[1])),
            level: CommandFeedbackLevel::Error,
        }),
        ["skills", "list" | "reload", ..] if parts.len() > 2 => Some(CommandFeedback {
            title: "SKILLS".to_string(),
            message: format!("skills {} does not take extra arguments.", parts[1]),
            detail: Some(format!("usage: /skills {}", parts[1])),
            level: CommandFeedbackLevel::Error,
        }),
        ["skills", "show" | "enable" | "disable"] => Some(CommandFeedback {
            title: "SKILLS".to_string(),
            message: format!("skills {} needs a skill name.", parts[1]),
            detail: Some(format!("usage: /skills {} <skill>", parts[1])),
            level: CommandFeedbackLevel::Warning,
        }),
        ["skills", "show" | "enable" | "disable", ..] if parts.len() > 3 => Some(CommandFeedback {
            title: "SKILLS".to_string(),
            message: format!("skills {} accepts exactly one skill name.", parts[1]),
            detail: Some(format!("usage: /skills {} <skill>", parts[1])),
            level: CommandFeedbackLevel::Error,
        }),
        ["telegram", "status", ..] if parts.len() > 2 => Some(CommandFeedback {
            title: "TELEGRAM".to_string(),
            message: "telegram status does not take extra arguments.".to_string(),
            detail: Some("usage: /telegram status".to_string()),
            level: CommandFeedbackLevel::Error,
        }),
        ["telegram", "approve" | "reject", ..] if parts.len() > 3 => Some(CommandFeedback {
            title: "TELEGRAM".to_string(),
            message: format!("telegram {} accepts at most one chat_id.", parts[1]),
            detail: Some(format!("usage: /telegram {} [chat_id]", parts[1])),
            level: CommandFeedbackLevel::Error,
        }),
        [verb, ..] if app_status_command_accepts(verb) && parts.len() > 2 => {
            Some(CommandFeedback {
                title: "APP STATUS".to_string(),
                message: "app-status accepts exactly one app name.".to_string(),
                detail: Some("usage: /app-status <app>".to_string()),
                level: CommandFeedbackLevel::Error,
            })
        }
        _ => None,
    }
}

pub(super) fn command_blocks_submission(
    input: &str,
    context: &DashboardCommandContext<'_>,
) -> Option<CommandFeedback> {
    let feedback = command_live_feedback(input, context)?;
    match feedback.level {
        CommandFeedbackLevel::Warning | CommandFeedbackLevel::Error => Some(feedback),
        CommandFeedbackLevel::Info => {
            let parts = dashboard_command_parts(input)?;
            if matches!(
                parts.as_slice(),
                ["telegram", "approve"] | ["telegram", "reject"]
            ) {
                Some(feedback)
            } else {
                None
            }
        }
    }
}

pub(super) fn unsupported_dashboard_command_feedback(input: &str) -> CommandFeedback {
    let command = dashboard_command_body(input)
        .and_then(|body| body.split_whitespace().next())
        .unwrap_or_default();
    CommandFeedback {
        title: "COMMAND".to_string(),
        message: if command.is_empty() {
            "Incomplete dashboard command.".to_string()
        } else {
            format!("Dashboard command '/{command}' is incomplete or unsupported here.")
        },
        detail: Some("Use / to choose a top-level command, then press Enter.".to_string()),
        level: CommandFeedbackLevel::Error,
    }
}

pub(super) fn selected_command_completion(
    input: &str,
    selected_index: usize,
    context: &DashboardCommandContext<'_>,
) -> Option<String> {
    let matches = matching_commands(input, context);
    if matches.is_empty() {
        return None;
    }
    let index = selected_index.min(matches.len().saturating_sub(1));
    Some(matches[index].completion.clone())
}

pub(super) fn dashboard_command_body(input: &str) -> Option<&str> {
    let stripped = input.trim_start().strip_prefix('/')?.trim();
    (!stripped.is_empty()).then_some(stripped)
}

pub(super) fn command_completion_body(input: &str) -> Option<&str> {
    input.trim_start().strip_prefix('/')
}

pub(super) fn is_dashboard_command_input(input: &str) -> bool {
    dashboard_command_body(input).is_some()
}

pub(super) fn matching_commands(
    input: &str,
    _context: &DashboardCommandContext<'_>,
) -> Vec<CommandSuggestion> {
    let Some(command_input) = command_completion_body(input) else {
        return Vec::new();
    };
    let trimmed = command_input.trim();
    if trimmed.is_empty() {
        return dashboard_commands()
            .iter()
            .map(|command| CommandSuggestion {
                display: command.primary_verb.to_string(),
                completion: format!("/{}", command.primary_verb),
                description: command.description.to_string(),
            })
            .collect::<Vec<_>>();
    }
    let parts = trimmed.split_whitespace().collect::<Vec<_>>();
    if parts.len() > 1 || command_input.ends_with(' ') {
        return Vec::new();
    }
    dashboard_commands()
        .iter()
        .copied()
        .filter(|command| command.primary_verb.starts_with(parts[0]))
        .map(|command| CommandSuggestion {
            display: command.primary_verb.to_string(),
            completion: format!("/{}", command.primary_verb),
            description: command.description.to_string(),
        })
        .collect::<Vec<_>>()
}

pub(super) fn adjusted_popup_scroll(
    current_scroll: usize,
    selected_index: usize,
    total: usize,
) -> usize {
    if total <= 6 {
        return 0;
    }
    let max_scroll = total.saturating_sub(6);
    if selected_index < current_scroll {
        selected_index
    } else if selected_index >= current_scroll + 6 {
        (selected_index + 1).saturating_sub(6).min(max_scroll)
    } else {
        current_scroll.min(max_scroll)
    }
}

pub(super) fn dashboard_parts_open_panel(parts: &[&str]) -> bool {
    matches!(
        parts,
        ["status"]
            | ["debug"]
            | ["debug", "persona"]
            | ["debug", "system-prompt"]
            | ["debug", "system_prompt"]
            | ["debug", "context"]
            | ["debug", "preturn-context"]
            | ["debug", "preturn_context"]
            | ["sleep"]
            | ["sleep", "status"]
            | ["telegram"]
            | ["telegram", "status"]
            | ["telegram", "approve"]
            | ["telegram", "reject"]
            | ["skills"]
            | ["skills", "list"]
            | ["skills", "show"]
            | ["skills", "show", _]
    ) || matches!(parts, [verb] if app_status_command_accepts(verb))
        || matches!(parts, [verb, _] if app_status_command_accepts(verb))
}

pub(super) fn dashboard_parts_run_action(parts: &[&str]) -> bool {
    matches!(
        parts,
        ["clear"]
            | ["restart"]
            | ["sleep", "run"]
            | ["skills", "reload"]
            | ["skills", "enable", _]
            | ["skills", "disable", _]
            | ["telegram", "approve", _]
            | ["telegram", "reject", _]
    )
}
