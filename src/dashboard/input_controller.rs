use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::command_flow::{
    adjusted_popup_scroll, command_blocks_submission, command_feedback_from_action_result,
    command_panel_for_input, dashboard_action_for_input, dashboard_command_body,
    is_clear_command_input, is_dashboard_command_input, matching_commands,
    selected_command_completion, telegram_access_command_panel,
    unsupported_dashboard_command_feedback,
};
use super::command_input::{
    expand_paste_placeholders, handle_paste_placeholder, should_insert_newline_on_enter,
};
use super::command_panels::{
    CommandFeedbackLevel, CommandPanel, CommandPanelAction, DashboardCommandContext,
    SkillsListPanel, SkillsTogglePanel, handle_command_panel_key,
};
use super::view_state::TuiViewState;
use super::{DashboardAction, DashboardCommandRunner, DashboardState};

pub(super) enum TuiInputOutcome {
    Continue,
    Exit,
    RunPanelAction {
        title: String,
        action: DashboardAction,
        keep_panel: bool,
    },
    RunDashboardAction {
        input: String,
        title: String,
        action: DashboardAction,
        quiet_success: bool,
    },
    SubmitText {
        input: String,
    },
}

pub(super) fn handle_key_event(
    key: KeyEvent,
    view: &mut TuiViewState,
    state: &DashboardState,
) -> TuiInputOutcome {
    let pending_requests = state.pending_access_requests.clone();

    if view.command_panel.is_some() {
        let action = view
            .command_panel
            .as_mut()
            .map(|panel| handle_command_panel_key(panel, key))
            .unwrap_or(CommandPanelAction::None);
        match action {
            CommandPanelAction::None => {}
            CommandPanelAction::Close => {
                view.command_panel = None;
            }
            CommandPanelAction::Replace(panel) => {
                view.command_panel = Some(panel);
                view.command_feedback = None;
            }
            CommandPanelAction::OpenSkillsList => {
                view.command_panel =
                    Some(CommandPanel::SkillsList(SkillsListPanel::from_state(state)));
            }
            CommandPanelAction::OpenSkillsToggle => {
                view.command_panel = Some(CommandPanel::SkillsToggle(
                    SkillsTogglePanel::from_state(state),
                ));
            }
            CommandPanelAction::OpenTelegramAccess(action) => {
                view.command_panel = Some(telegram_access_command_panel(action, &pending_requests));
            }
            CommandPanelAction::RunAction {
                title,
                action,
                keep_panel,
            } => {
                if !keep_panel {
                    view.command_panel = None;
                }
                return TuiInputOutcome::RunPanelAction {
                    title,
                    action,
                    keep_panel,
                };
            }
        }
        return TuiInputOutcome::Continue;
    }

    if view.command_input.is_empty() {
        if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
            view.toggle_thinking_expansion(&state.activity_cells);
            return TuiInputOutcome::Continue;
        }
        if view.handle_activity_scroll_key(key) {
            return TuiInputOutcome::Continue;
        }
    }

    let command_context = DashboardCommandContext {
        requests: &pending_requests,
        state,
    };

    match key.code {
        KeyCode::Char(c) => {
            view.command_input.insert_char(c);
            view.command_feedback = None;
            view.reset_command_popup();
        }
        KeyCode::Tab => {
            if let Some(completion) = selected_command_completion(
                view.command_input.as_str(),
                view.command_popup_selection,
                &command_context,
            ) {
                view.command_input.set_text(completion);
                view.command_feedback = None;
                view.reset_command_popup();
            }
        }
        KeyCode::Backspace => {
            view.command_input.delete_before_cursor();
            view.command_feedback = None;
            view.reset_command_popup();
        }
        KeyCode::Up => {
            let matches = matching_commands(view.command_input.as_str(), &command_context);
            if !matches.is_empty() {
                view.command_popup_selection = view
                    .command_popup_selection
                    .saturating_sub(1)
                    .min(matches.len() - 1);
                view.command_popup_scroll = adjusted_popup_scroll(
                    view.command_popup_scroll,
                    view.command_popup_selection,
                    matches.len(),
                );
            }
        }
        KeyCode::Down => {
            let matches = matching_commands(view.command_input.as_str(), &command_context);
            if !matches.is_empty() {
                view.command_popup_selection =
                    (view.command_popup_selection + 1).min(matches.len() - 1);
                view.command_popup_scroll = adjusted_popup_scroll(
                    view.command_popup_scroll,
                    view.command_popup_selection,
                    matches.len(),
                );
            }
        }
        KeyCode::Esc => {
            view.command_input.clear();
            view.command_feedback = None;
            view.reset_command_popup();
        }
        KeyCode::Enter => {
            return handle_enter_key(key, view, command_context);
        }
        KeyCode::Left => {
            view.command_input.move_left();
            view.reset_command_popup();
        }
        KeyCode::Right => {
            view.command_input.move_right();
            view.reset_command_popup();
        }
        KeyCode::Home => {
            view.command_input.move_home();
            view.reset_command_popup();
        }
        KeyCode::End => {
            view.command_input.move_end();
            view.reset_command_popup();
        }
        _ => {}
    }

    TuiInputOutcome::Continue
}

fn handle_enter_key(
    key: KeyEvent,
    view: &mut TuiViewState,
    command_context: DashboardCommandContext<'_>,
) -> TuiInputOutcome {
    if should_insert_newline_on_enter(key.modifiers) {
        view.command_input.insert_char('\n');
        view.reset_command_popup();
        return TuiInputOutcome::Continue;
    }

    if !view.pending_pastes.is_empty() {
        view.command_input.set_text(expand_paste_placeholders(
            view.command_input.as_str(),
            &view.pending_pastes,
        ));
        view.pending_pastes.clear();
    }

    let input = view.command_input.as_str().trim().to_string();
    if !input.is_empty() {
        if matches!(dashboard_command_body(&input), Some("quit" | "q" | "exit")) {
            return TuiInputOutcome::Exit;
        }
        if let Some(panel) = command_panel_for_input(&input, &command_context) {
            view.command_panel = Some(panel);
            view.command_feedback = None;
            view.command_input.clear();
            view.reset_command_popup();
            return TuiInputOutcome::Continue;
        }
        match dashboard_action_for_input(&input, &command_context) {
            Ok(Some(invocation)) => {
                return TuiInputOutcome::RunDashboardAction {
                    input,
                    title: invocation.title,
                    action: invocation.action,
                    quiet_success: invocation.quiet_success,
                };
            }
            Ok(None) => {}
            Err(feedback) => {
                view.command_panel = None;
                view.command_feedback = Some(feedback);
                view.reset_command_popup();
                return TuiInputOutcome::Continue;
            }
        }
    }

    if let Some(completion) = selected_command_completion(
        view.command_input.as_str(),
        view.command_popup_selection,
        &command_context,
    ) && completion != view.command_input.as_str()
    {
        view.command_input.set_text(completion);
        view.command_feedback = None;
        view.reset_command_popup();
        return TuiInputOutcome::Continue;
    }

    if !input.is_empty() {
        if is_dashboard_command_input(&input) {
            view.command_panel = None;
            view.command_feedback = Some(
                command_blocks_submission(&input, &command_context)
                    .unwrap_or_else(|| unsupported_dashboard_command_feedback(&input)),
            );
            view.reset_command_popup();
            return TuiInputOutcome::Continue;
        }
        return TuiInputOutcome::SubmitText { input };
    }
    view.command_input.clear();
    view.reset_command_popup();
    TuiInputOutcome::Continue
}

pub(super) async fn execute_input_outcome(
    outcome: TuiInputOutcome,
    view: &mut TuiViewState,
    state: &DashboardState,
    command_runner: &dyn DashboardCommandRunner,
) -> bool {
    match outcome {
        TuiInputOutcome::Continue => false,
        TuiInputOutcome::Exit => true,
        TuiInputOutcome::RunPanelAction {
            title,
            action,
            keep_panel,
        } => {
            let result = command_runner.run_action(action, state).await;
            let feedback = command_feedback_from_action_result(title, result);
            if keep_panel {
                if let Some(panel) = view.command_panel.as_mut() {
                    if matches!(feedback.level, CommandFeedbackLevel::Error) {
                        panel.set_error_feedback(feedback);
                    } else {
                        panel.clear_feedback();
                    }
                }
            } else {
                view.command_feedback = Some(feedback);
            }
            false
        }
        TuiInputOutcome::RunDashboardAction {
            input,
            title,
            action,
            quiet_success,
        } => {
            let result = command_runner.run_action(action, state).await;
            if is_clear_command_input(&input) && result.success {
                view.clear_visible_activity();
            }
            view.command_feedback = if quiet_success && result.success {
                None
            } else {
                Some(command_feedback_from_action_result(title, result))
            };
            view.command_input.clear();
            view.reset_command_popup();
            false
        }
        TuiInputOutcome::SubmitText { input } => {
            let _ = command_runner.run_command(&input, state).await;
            view.command_panel = None;
            view.command_feedback = None;
            view.command_input.clear();
            view.reset_command_popup();
            false
        }
    }
}

pub(super) fn handle_paste_event(text: &str, view: &mut TuiViewState) {
    handle_paste_placeholder(text, &mut view.command_input.text, &mut view.pending_pastes);
    view.command_input.move_end();
    view.command_feedback = None;
}
