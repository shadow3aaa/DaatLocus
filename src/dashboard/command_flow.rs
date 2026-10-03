use std::path::PathBuf;

use super::command_panels::{
    CommandFeedback, CommandFeedbackLevel, CommandPanel, CommandSelectionAction,
    CommandSelectionItem, CommandSelectionPanel, CommandSuggestion, DashboardActionInvocation,
    DashboardCommandContext, SkillsListPanel, detail_panel,
};
use super::command_registry::{
    app_status_command_accepts, clear_command_accepts, dashboard_command_is_known,
    dashboard_commands, debug_command_accepts, quit_command_accepts, restart_command_accepts,
    skills_command_accepts, sleep_command_accepts, status_command_accepts, think_command_accepts,
    workflows_command_accepts,
};
use super::command_text::{
    fallback_output, render_app_status_text, render_available_app_statuses, render_skill_detail,
    render_skills_list, resolve_skill_target, skill_detail_text, skill_status_description,
    truncate_command_text,
};
use super::{
    DashboardAction, DashboardActionResult, DashboardControlCommand, DashboardState,
    DashboardWorkflowLoadError, DashboardWorkflowSummary,
};
use crate::{
    openskills::OpenSkillDashboardSummary,
    reasoning::turn_compile::{
        PromptPersonaSpec, prompt_persona_path_sync, render_prompt_persona_markdown,
    },
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
    let parts = command_parts_ref(&parts);
    match parts.as_slice() {
        ["status"] => Some(detail_panel(
            "STATUS",
            fallback_output(&context.state.status_output),
        )),
        ["debug"] => Some(debug_command_panel(context.state)),
        ["debug", "persona"] => Some(debug_persona_panel()),
        ["debug", "system-prompt" | "system_prompt"] => {
            Some(debug_system_prompt_panel(context.state))
        }
        ["debug", "context" | "preturn-context" | "preturn_context"] => {
            Some(debug_context_panel(context.state))
        }
        ["sleep"] => Some(sleep_command_panel(context.state)),
        ["sleep", "status"] => Some(sleep_status_panel(context.state)),
        [verb] if app_status_command_accepts(verb) => {
            Some(app_status_selection_panel(context.state))
        }
        [verb, target] if app_status_command_accepts(verb) => {
            Some(app_status_detail_panel(context.state, target))
        }
        [verb] if workflows_command_accepts(verb) => Some(workflows_command_panel(context.state)),
        [verb, ..] if workflows_command_accepts(verb) => None,
        ["skills"] => Some(skills_command_panel(context.state)),
        ["skills", "list" | "show"] => Some(CommandPanel::SkillsList(SkillsListPanel::from_state(
            context.state,
        ))),
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
    let parts = command_parts_ref(&parts);
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
        ["sleep", "auto" | "toggle"] | ["sleep", "auto" | "toggle", "on" | "off"] => {
            let enabled = match parts.as_slice() {
                ["sleep", "auto" | "toggle"] => !context.state.runtime_optimization.enabled,
                _ => parts[2] == "on",
            };
            DashboardActionInvocation {
                title: "SLEEP".to_string(),
                action: DashboardAction::SetSleepEnabled { enabled },
                quiet_success: false,
            }
        }
        ["skills", "reload"] => DashboardActionInvocation {
            title: "SKILLS".to_string(),
            action: DashboardAction::ReloadSkills,
            quiet_success: false,
        },
        ["skills", "enable" | "disable", target] => {
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
        ["ask", ..] => {
            return Err(CommandFeedback {
                title: "ASK".to_string(),
                message: "/ask needs a dashboard composer or `daat-locus send` input.".to_string(),
                detail: Some("Type /ask followed by the discussion text there.".to_string()),
                level: CommandFeedbackLevel::Error,
            });
        }
        _ => return Ok(None),
    };
    Ok(Some(invocation))
}

/// Returns the discussion text of an `/ask` command, or `None` when the input
/// is not an ask command. An empty string means `/ask` was used without text.
pub(crate) fn ask_message_text(input: &str) -> Option<&str> {
    let rest = input.trim().strip_prefix("/ask")?;
    if rest.starts_with(|ch: char| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') {
        return None;
    }
    Some(rest.trim())
}

pub fn execute_control_command(
    command: &str,
    state: &DashboardState,
    control_tx: &tokio::sync::mpsc::UnboundedSender<DashboardControlCommand>,
) -> String {
    let command = command.trim().trim_start_matches('/').trim();
    if command.is_empty() {
        return "empty command".to_string();
    }
    let input = format!("/{command}");
    let context = DashboardCommandContext { state };
    let Some(owned_parts) = dashboard_command_parts(&input) else {
        return "empty command".to_string();
    };
    let parts = command_parts_ref(&owned_parts);

    if matches!(parts.as_slice(), ["quit" | "q" | "exit"]) {
        return "quit command is only available in the local dashboard".to_string();
    }

    match dashboard_action_for_input(&input, &context) {
        Ok(Some(invocation)) => {
            let result = super::execute_dashboard_action(invocation.action, control_tx);
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
        ["debug", "system-prompt" | "system_prompt"] => {
            fallback_output(&state.system_prompt_output)
        }
        ["debug", "context" | "preturn-context" | "preturn_context"] => {
            fallback_output(&state.preturn_context_output)
        }
        [verb] if app_status_command_accepts(verb) => render_available_app_statuses(state),
        [verb, target] if app_status_command_accepts(verb) => render_app_status_text(state, target),
        ["sleep"] => "available actions: status, run".to_string(),
        ["sleep", "status"] => fallback_output(&state.sleep_status_output),
        [verb] if workflows_command_accepts(verb) => render_workflows_list(state),
        ["skills"] | ["skills", "list" | "show"] => render_skills_list(state),
        ["skills", "show", target] => render_skill_detail(state, target),
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
            },
            CommandSelectionItem {
                name: "System prompt".to_string(),
                description: "show current runtime system prompt".to_string(),
                action: CommandSelectionAction::ShowDetail {
                    title: "DEBUG SYSTEM PROMPT".to_string(),
                    text: fallback_output(&state.system_prompt_output),
                },
            },
            CommandSelectionItem {
                name: "Runtime context".to_string(),
                description: "show latest pre-turn runtime context".to_string(),
                action: CommandSelectionAction::ShowDetail {
                    title: "DEBUG CONTEXT".to_string(),
                    text: fallback_output(&state.preturn_context_output),
                },
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
        Err(_) => render_prompt_persona_markdown(&PromptPersonaSpec::default()),
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
            },
            CommandSelectionItem {
                name: "Start sleep run".to_string(),
                description: "start a background sleep run".to_string(),
                action: CommandSelectionAction::RunAction {
                    title: "SLEEP".to_string(),
                    action: DashboardAction::RunSleep,
                    keep_panel: false,
                },
            },
            CommandSelectionItem {
                name: "Automatic sleep".to_string(),
                description: if state.runtime_optimization.enabled {
                    "automatic sleep is enabled; select to disable it".to_string()
                } else {
                    "automatic sleep is disabled; select to enable it".to_string()
                },
                action: CommandSelectionAction::RunAction {
                    title: "SLEEP".to_string(),
                    action: DashboardAction::SetSleepEnabled {
                        enabled: !state.runtime_optimization.enabled,
                    },
                    keep_panel: false,
                },
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

fn workflows_command_panel(state: &DashboardState) -> CommandPanel {
    let mut items = state
        .workflows
        .iter()
        .map(workflow_selection_item)
        .collect::<Vec<_>>();
    items.extend(
        state
            .workflow_errors
            .iter()
            .map(workflow_error_selection_item),
    );
    if items.is_empty() {
        return detail_panel(
            "WORKFLOWS",
            "No Lua workflows are loaded. Add a .lua file under ~/.daat-locus/workflows, then reload the session.",
        );
    }
    CommandPanel::Selection(CommandSelectionPanel {
        title: "Workflows".to_string(),
        subtitle: Some(format!(
            "{} loaded workflow{}, {} load error{}",
            state.workflows.len(),
            if state.workflows.len() == 1 { "" } else { "s" },
            state.workflow_errors.len(),
            if state.workflow_errors.len() == 1 {
                ""
            } else {
                "s"
            },
        )),
        items,
        selected: 0,
        scroll: 0,
    })
}

fn workflow_selection_item(workflow: &DashboardWorkflowSummary) -> CommandSelectionItem {
    CommandSelectionItem {
        name: workflow.id.clone(),
        description: format!(
            "{} input field{}",
            workflow.input_fields.len(),
            if workflow.input_fields.len() == 1 {
                ""
            } else {
                "s"
            },
        ),
        action: CommandSelectionAction::OpenWorkflowForm {
            workflow: workflow.clone(),
        },
    }
}

fn workflow_error_selection_item(error: &DashboardWorkflowLoadError) -> CommandSelectionItem {
    CommandSelectionItem {
        name: format!("Load error: {}", error.path),
        description: truncate_command_text(&error.message, 100),
        action: CommandSelectionAction::ShowDetail {
            title: "WORKFLOW LOAD ERROR".to_string(),
            text: format!("Path: {}\nError: {}", error.path, error.message),
        },
    }
}

fn render_workflows_list(state: &DashboardState) -> String {
    let mut lines = state
        .workflows
        .iter()
        .map(|workflow| {
            format!(
                "{} — {} input field{}",
                workflow.id,
                workflow.input_fields.len(),
                if workflow.input_fields.len() == 1 {
                    ""
                } else {
                    "s"
                },
            )
        })
        .collect::<Vec<_>>();
    lines.extend(
        state
            .workflow_errors
            .iter()
            .map(|error| format!("load error: {} — {}", error.path, error.message)),
    );
    if lines.is_empty() {
        "No Lua workflows are loaded.".to_string()
    } else {
        lines.join("\n")
    }
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
            },
            CommandSelectionItem {
                name: "Enable/Disable Skills".to_string(),
                description: "toggle whether skills may be selected automatically".to_string(),
                action: CommandSelectionAction::OpenSkillsToggle,
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
        skill_detail_text(skill),
    ))
}

pub(super) fn is_clear_command_input(input: &str) -> bool {
    dashboard_command_parts(input)
        .is_some_and(|parts| parts.first().is_some_and(|verb| verb == "clear"))
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

/// Tokenize POSIX-like shell words: single quotes, double quotes, and backslash escapes.
/// Returns `None` when a quote or escape is left open.
pub(crate) fn tokenize_shell_words(input: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_word = false;
    let mut quote: Option<char> = None;

    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    current.push(ch);
                }
                in_word = true;
            }
            Some('"') => {
                if ch == '\\' {
                    match chars.next() {
                        Some(escaped @ ('"' | '\\' | '$' | '`')) => current.push(escaped),
                        Some('\n') => {}
                        Some(other) => {
                            current.push('\\');
                            current.push(other);
                        }
                        None => return None,
                    }
                    in_word = true;
                } else if ch == '"' {
                    quote = None;
                    in_word = true;
                } else {
                    current.push(ch);
                    in_word = true;
                }
            }
            None if ch == '\\' => match chars.next() {
                Some('\n') => {}
                Some(escaped) => {
                    current.push(escaped);
                    in_word = true;
                }
                None => return None,
            },
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                in_word = true;
            }
            None if ch.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            None => {
                current.push(ch);
                in_word = true;
            }
            Some(_) => unreachable!(),
        }
    }

    if quote.is_some() {
        return None;
    }
    if in_word {
        words.push(current);
    }
    Some(words)
}

/// Slash-command verb plus arguments. Quoted and escaped words stay intact.
/// Returns `None` when the body is empty or a quote/escape is left open.
pub(crate) fn dashboard_command_parts(input: &str) -> Option<Vec<String>> {
    let body = dashboard_command_body(input)?;
    let parts = tokenize_shell_words(body)?;
    (!parts.is_empty()).then_some(parts)
}

fn command_parts_ref(parts: &[String]) -> Vec<&str> {
    parts.iter().map(String::as_str).collect()
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
    let owned_parts = tokenize_shell_words(trimmed)?;
    let parts = command_parts_ref(&owned_parts);
    let verb = parts.first().copied().unwrap_or_default();
    let command = dashboard_commands()
        .iter()
        .copied()
        .find(|command| command.accepts(verb));
    let Some(_command) = command else {
        let completing = owned_parts.len() == 1
            && !command_input.ends_with(|ch: char| ch.is_whitespace())
            && dashboard_commands()
                .iter()
                .any(|command| command.primary_verb.starts_with(owned_parts[0].as_str()));
        if !completing {
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
    } else if think_command_accepts(verb) {
        if !matches!(parts.as_slice(), ["think", "show" | "hide"]) {
            return Some(unknown_command_part_feedback(
                "THINKING",
                "Use /think show or /think hide.",
                "This command is available only in the TUI and never writes configuration.",
            ));
        }
    } else if sleep_command_accepts(verb) {
        match parts.as_slice() {
            ["sleep", "run" | "status"] => {}
            ["sleep", "auto" | "toggle"] | ["sleep", "auto" | "toggle", "on" | "off"] => {}
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
            ["skills", "list" | "reload"] => {}
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
    } else if app_status_command_accepts(verb) {
        let target = parts[1].to_ascii_lowercase();
        let outputs = &context.state.app_status_outputs;
        let known = outputs.iter().any(|(name, _)| name == &target);
        let possible = outputs.iter().any(|(name, _)| name.starts_with(&target));
        if !known && !possible {
            let detail = if outputs.is_empty() {
                "No app state is currently available.".to_string()
            } else {
                let mut available = String::new();
                for (index, (name, _)) in outputs.iter().enumerate() {
                    if index > 0 {
                        available.push_str(", ");
                    }
                    available.push_str(name);
                }
                format!("available: {available}")
            };
            return Some(CommandFeedback {
                title: "APP STATUS".to_string(),
                message: format!("Unknown app '{target}'."),
                detail: Some(detail),
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
        ["think"] => Some(CommandFeedback {
            title: "THINKING".to_string(),
            message: "think needs show or hide.".to_string(),
            detail: Some("usage: /think <show|hide>".to_string()),
            level: CommandFeedbackLevel::Warning,
        }),
        ["think", ..] => Some(CommandFeedback {
            title: "THINKING".to_string(),
            message: "think accepts only show or hide.".to_string(),
            detail: Some("usage: /think <show|hide>".to_string()),
            level: CommandFeedbackLevel::Error,
        }),
        ["sleep", "run" | "status", ..] if parts.len() > 2 => Some(CommandFeedback {
            title: "SLEEP".to_string(),
            message: format!("sleep {} does not take extra arguments.", parts[1]),
            detail: Some(format!("usage: /sleep {}", parts[1])),
            level: CommandFeedbackLevel::Error,
        }),
        ["sleep", "auto" | "toggle", extra, ..] if parts.len() > 3 => Some(CommandFeedback {
            title: "SLEEP".to_string(),
            message: format!("sleep {} accepts on or off after it.", parts[1]),
            detail: Some(format!("usage: /sleep {} <on|off>", parts[1])),
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
        [verb, ..] if workflows_command_accepts(verb) && parts.len() > 1 => Some(CommandFeedback {
            title: "WORKFLOWS".to_string(),
            message: "Choose a workflow from the /workflows panel.".to_string(),
            detail: Some("usage: /workflows".to_string()),
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
        CommandFeedbackLevel::Info => None,
    }
}

pub(super) fn unsupported_dashboard_command_feedback(input: &str) -> CommandFeedback {
    let command = dashboard_command_body(input)
        .and_then(tokenize_shell_words)
        .and_then(|parts| parts.into_iter().next())
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

pub(super) struct PreparedCommandInput {
    pub(super) matches: Vec<CommandSuggestion>,
    /// Tokenized slash-command body. `None` for ordinary text or an unclosed quote.
    pub(super) slash_parts: Option<Vec<String>>,
}

pub(super) fn prepare_command_input(
    input: &str,
    context: &DashboardCommandContext<'_>,
) -> PreparedCommandInput {
    if let Some(command_input) = command_completion_body(input) {
        let trimmed = command_input.trim();
        if trimmed.is_empty() {
            return PreparedCommandInput {
                matches: all_slash_suggestions(),
                slash_parts: Some(Vec::new()),
            };
        }
        let Some(parts) = tokenize_shell_words(trimmed) else {
            return PreparedCommandInput {
                matches: Vec::new(),
                slash_parts: None,
            };
        };
        let matches = slash_suggestions_from_parts(command_input, &parts);
        return PreparedCommandInput {
            matches,
            slash_parts: Some(parts),
        };
    }
    PreparedCommandInput {
        matches: matching_skill_mentions(input, context),
        slash_parts: None,
    }
}

pub(super) fn matching_commands(
    input: &str,
    context: &DashboardCommandContext<'_>,
) -> Vec<CommandSuggestion> {
    prepare_command_input(input, context).matches
}

fn all_slash_suggestions() -> Vec<CommandSuggestion> {
    dashboard_commands()
        .iter()
        .map(|command| CommandSuggestion {
            display: command.primary_verb.to_string(),
            completion: format!("/{}", command.primary_verb),
            description: command.description.to_string(),
        })
        .collect()
}

fn slash_suggestions_from_parts(command_input: &str, parts: &[String]) -> Vec<CommandSuggestion> {
    if command_input.trim().is_empty() {
        return all_slash_suggestions();
    }
    if parts.len() != 1 || command_input.ends_with(|ch: char| ch.is_whitespace()) {
        return Vec::new();
    }
    let verb = parts[0].as_str();
    dashboard_commands()
        .iter()
        .copied()
        .filter(|command| command.primary_verb.starts_with(verb))
        .map(|command| CommandSuggestion {
            display: command.primary_verb.to_string(),
            completion: format!("/{}", command.primary_verb),
            description: command.description.to_string(),
        })
        .collect()
}

/// True when tokenized parts are identical to raw whitespace-separated words.
/// Quoted or escaped words do not count, matching `dashboard_command_parts_ref`.
pub(super) fn slash_parts_match_literal_words(input: &str, parts: &[String]) -> bool {
    let Some(body) = dashboard_command_body(input) else {
        return false;
    };
    let raw: Vec<&str> = body.split_whitespace().collect();
    !parts.is_empty()
        && raw.len() == parts.len()
        && raw
            .iter()
            .zip(parts)
            .all(|(raw, word)| *raw == word.as_str())
}

fn matching_skill_mentions(
    input: &str,
    context: &DashboardCommandContext<'_>,
) -> Vec<CommandSuggestion> {
    let Some((mention_start, prefix)) = skill_completion_target(input) else {
        return Vec::new();
    };
    context
        .state
        .skills
        .iter()
        .filter(|skill| skill.name.starts_with(prefix))
        .filter(|skill| skill_name_is_unique(&context.state.skills, &skill.name))
        .map(|skill| CommandSuggestion {
            display: format!("${}", skill.name),
            completion: format!("{}${}", &input[..mention_start], skill.name),
            description: skill_suggestion_description(skill),
        })
        .collect::<Vec<_>>()
}

fn skill_completion_target(input: &str) -> Option<(usize, &str)> {
    let mention_start = input.rfind('$')?;
    let name_start = mention_start + 1;
    let prefix = &input[name_start..];
    if !prefix
        .as_bytes()
        .iter()
        .all(|byte| is_skill_mention_name_char(*byte))
    {
        return None;
    }
    Some((mention_start, prefix))
}

const fn is_skill_mention_name_char(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b':')
}

fn skill_name_is_unique(skills: &[OpenSkillDashboardSummary], name: &str) -> bool {
    skills
        .iter()
        .filter(|skill| skill.name == name)
        .take(2)
        .count()
        == 1
}

fn skill_suggestion_description(skill: &OpenSkillDashboardSummary) -> String {
    let status = skill_status_description(skill);
    if skill.description.is_empty() {
        status
    } else {
        format!("{} — {}", skill.description, status)
    }
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

pub(super) fn dashboard_parts_open_panel(parts: &[impl AsRef<str>]) -> bool {
    let parts = parts.iter().map(AsRef::as_ref).collect::<Vec<_>>();
    matches!(
        parts.as_slice(),
        ["status" | "debug" | "sleep" | "skills" | "workflows"]
            | [
                "debug",
                "persona"
                    | "system-prompt"
                    | "system_prompt"
                    | "context"
                    | "preturn-context"
                    | "preturn_context"
            ]
            | ["sleep", "status"]
            | ["skills", "list" | "show"]
            | ["skills", "show", _]
    ) || matches!(parts.as_slice(), [verb] if app_status_command_accepts(verb))
        || matches!(parts.as_slice(), [verb, _] if app_status_command_accepts(verb))
}

pub(super) fn dashboard_parts_run_action(parts: &[impl AsRef<str>]) -> bool {
    let parts = parts.iter().map(AsRef::as_ref).collect::<Vec<_>>();
    matches!(
        parts.as_slice(),
        ["clear" | "restart"]
            | ["sleep", "run"]
            | ["sleep", "auto" | "toggle"]
            | ["sleep", "auto" | "toggle", "on" | "off"]
            | ["skills", "reload"]
            | ["skills", "enable" | "disable", _]
    )
}

#[cfg(test)]
mod tests {
    use super::ask_message_text;

    #[test]
    fn ask_message_text_parses_only_ask_commands() {
        assert_eq!(
            ask_message_text("/ask explain the design"),
            Some("explain the design")
        );
        assert_eq!(ask_message_text("  /ask   spaced   "), Some("spaced"));
        assert_eq!(ask_message_text("/ask"), Some(""));
        assert_eq!(ask_message_text("/ask\nmulti line"), Some("multi line"));
        assert_eq!(ask_message_text("/asking a question"), None);
        assert_eq!(ask_message_text("hello"), None);
    }

    #[test]
    fn dashboard_command_parts_keep_quotes() {
        assert_eq!(
            super::dashboard_command_parts("/skills show 'my skill'").as_deref(),
            Some(
                [
                    "skills".to_string(),
                    "show".to_string(),
                    "my skill".to_string()
                ]
                .as_slice()
            )
        );
        assert_eq!(
            super::dashboard_command_parts("/skills enable \"quoted name\"").as_deref(),
            Some(
                [
                    "skills".to_string(),
                    "enable".to_string(),
                    "quoted name".to_string()
                ]
                .as_slice()
            )
        );
        assert_eq!(
            super::dashboard_command_parts("/asking now").as_deref(),
            Some(["asking".to_string(), "now".to_string()].as_slice())
        );
        assert_eq!(super::ask_message_text("/asking now"), None);
        assert_eq!(
            super::tokenize_shell_words("echo 'a b' \"c d\" e\\ f").as_deref(),
            Some(
                [
                    "echo".to_string(),
                    "a b".to_string(),
                    "c d".to_string(),
                    "e f".to_string()
                ]
                .as_slice()
            )
        );
        assert!(super::tokenize_shell_words("echo 'unterminated").is_none());
    }
}
