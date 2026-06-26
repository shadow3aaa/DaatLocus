use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::command_flow::{
    adjusted_popup_scroll, command_blocks_submission, command_feedback_from_action_result,
    command_panel_for_input, dashboard_action_for_input, dashboard_command_body,
    is_clear_command_input, is_dashboard_command_input, matching_commands,
    selected_command_completion, unsupported_dashboard_command_feedback,
};
use super::command_input::{
    expand_paste_placeholders, handle_paste_placeholder, should_insert_newline_on_enter,
};
use super::command_panels::{
    CommandFeedback, CommandFeedbackLevel, CommandPanel, CommandPanelAction,
    DashboardCommandContext, PendingUserInputQueuePanel, SkillsListPanel, SkillsTogglePanel,
    handle_command_panel_key,
};
use super::view_state::{CtrlCReminder, TuiViewState};
use std::path::{Path, PathBuf};

use super::{DashboardAction, DashboardCommandAttachment, DashboardCommandRunner, DashboardState};

pub(super) enum TuiInputOutcome {
    Continue,
    Exit,
    CopySelection {
        text: String,
    },
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
    SavePendingUserInputEdit {
        event_id: uuid::Uuid,
        incoming_text: String,
    },
    SubmitText {
        input: String,
        attachments: Vec<DashboardCommandAttachment>,
    },
}

pub(super) fn handle_key_event(
    key: KeyEvent,
    view: &mut TuiViewState,
    state: &DashboardState,
) -> TuiInputOutcome {
    if is_ctrl_c(key)
        && let Some(text) = view.selected_text()
    {
        return TuiInputOutcome::CopySelection { text };
    }

    if key.code == KeyCode::Esc && view.clear_selection() {
        return TuiInputOutcome::Continue;
    }

    if view.transcript_overlay.is_some() {
        view.handle_transcript_overlay_key(key);
        return TuiInputOutcome::Continue;
    }

    if is_ctrl_t(key) {
        view.open_transcript_overlay(state);
        return TuiInputOutcome::Continue;
    }
    if view.editing_pending_user_input.is_some() {
        return handle_pending_user_input_edit_key(key, view);
    }

    if is_ctrl_p(key) {
        return open_pending_user_input_queue_panel(view, state);
    }

    if view.command_panel.is_some() {
        if is_ctrl_c(key) {
            view.command_panel = None;
            view.command_feedback = None;
            view.ctrl_c_reminder = ctrl_c_reminder_for_state(state);
            return TuiInputOutcome::Continue;
        }
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
            CommandPanelAction::EditPendingUserInput {
                event_id,
                incoming_text,
            } => {
                view.begin_pending_user_input_edit(event_id, incoming_text);
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

    if is_ctrl_c(key) {
        return handle_ctrl_c_key(view, state);
    }

    if key.code == KeyCode::Esc && state.runtime_activity.active_runtime_turn {
        view.command_feedback = None;
        view.clear_ctrl_c_reminder();
        view.reset_command_popup();
        return TuiInputOutcome::RunDashboardAction {
            input: "<esc>".to_string(),
            title: "Interrupt".to_string(),
            action: DashboardAction::InterruptRuntime,
            quiet_success: true,
        };
    }

    if view.command_input.is_empty() {
        if key.code == KeyCode::Enter {
            view.toggle_thinking_expansion(&state.activity_events);
            return TuiInputOutcome::Continue;
        }
        if view.handle_activity_scroll_key(key) {
            return TuiInputOutcome::Continue;
        }
    }

    let command_context = DashboardCommandContext { state };

    match key.code {
        KeyCode::Char(c) => {
            view.command_input.insert_char(c);
            view.command_feedback = None;
            view.clear_ctrl_c_reminder();
            view.reset_command_history_navigation();
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
                view.clear_ctrl_c_reminder();
                view.reset_command_history_navigation();
                view.reset_command_popup();
            }
        }
        KeyCode::Backspace => {
            view.command_input.delete_before_cursor();
            view.command_feedback = None;
            view.clear_ctrl_c_reminder();
            view.reset_command_history_navigation();
            view.reset_command_popup();
        }
        KeyCode::Up => {
            if view.command_input.move_up_line() {
                view.reset_command_popup();
            } else {
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
                } else if view.navigate_command_history_up() {
                    view.command_feedback = None;
                    view.clear_ctrl_c_reminder();
                }
            }
        }
        KeyCode::Down => {
            if view.command_input.move_down_line() {
                view.reset_command_popup();
            } else {
                let matches = matching_commands(view.command_input.as_str(), &command_context);
                if !matches.is_empty() {
                    view.command_popup_selection =
                        (view.command_popup_selection + 1).min(matches.len() - 1);
                    view.command_popup_scroll = adjusted_popup_scroll(
                        view.command_popup_scroll,
                        view.command_popup_selection,
                        matches.len(),
                    );
                } else if view.navigate_command_history_down() {
                    view.command_feedback = None;
                    view.clear_ctrl_c_reminder();
                }
            }
        }
        KeyCode::Esc => {
            view.command_input.clear();
            view.pending_image_attachments.clear();
            view.command_feedback = None;
            view.clear_ctrl_c_reminder();
            view.reset_command_history_navigation();
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

fn is_ctrl_c(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) && c.eq_ignore_ascii_case(&'c'))
}

fn is_ctrl_t(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) && c.eq_ignore_ascii_case(&'t'))
}
fn is_ctrl_p(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) && c.eq_ignore_ascii_case(&'p'))
}

fn handle_ctrl_c_key(view: &mut TuiViewState, state: &DashboardState) -> TuiInputOutcome {
    if composer_has_input(view) {
        let previous_input = view.command_input.as_str().to_string();
        view.command_panel = None;
        view.command_input.clear();
        view.pending_pastes.clear();
        view.pending_image_attachments.clear();
        view.command_feedback = None;
        view.ctrl_c_reminder = ctrl_c_reminder_for_state(state);
        view.reset_command_history_navigation();
        view.record_command_history(&previous_input);
        view.reset_command_popup();
        return TuiInputOutcome::Continue;
    }

    if state.runtime_activity.active_runtime_turn {
        view.command_feedback = None;
        view.clear_ctrl_c_reminder();
        view.reset_command_popup();
        return TuiInputOutcome::RunDashboardAction {
            input: "<ctrl-c>".to_string(),
            title: "Interrupt".to_string(),
            action: DashboardAction::InterruptRuntime,
            quiet_success: true,
        };
    }

    view.command_feedback = None;
    view.clear_ctrl_c_reminder();
    view.reset_command_popup();
    TuiInputOutcome::Continue
}

fn composer_has_input(view: &TuiViewState) -> bool {
    !view.command_input.is_empty()
        || !view.pending_pastes.is_empty()
        || !view.pending_image_attachments.is_empty()
}

fn ctrl_c_reminder_for_state(state: &DashboardState) -> Option<CtrlCReminder> {
    if state.runtime_activity.active_runtime_turn {
        Some(CtrlCReminder::Interrupt)
    } else {
        None
    }
}

fn open_pending_user_input_queue_panel(
    view: &mut TuiViewState,
    state: &DashboardState,
) -> TuiInputOutcome {
    if let Some(panel) = PendingUserInputQueuePanel::from_state(state) {
        view.command_panel = Some(CommandPanel::PendingUserInputQueue(panel));
        view.command_feedback = None;
    } else {
        view.command_feedback = Some(CommandFeedback {
            title: "QUEUED INPUTS".to_string(),
            message: "no queued follow-up inputs".to_string(),
            detail: None,
            level: CommandFeedbackLevel::Info,
        });
    }
    view.clear_ctrl_c_reminder();
    view.reset_command_history_navigation();
    view.reset_command_popup();
    TuiInputOutcome::Continue
}

fn handle_pending_user_input_edit_key(key: KeyEvent, view: &mut TuiViewState) -> TuiInputOutcome {
    if should_insert_newline_on_enter(key.modifiers) && key.code == KeyCode::Enter {
        view.command_input.insert_char('\n');
        view.command_feedback = None;
        view.reset_command_history_navigation();
        view.reset_command_popup();
        return TuiInputOutcome::Continue;
    }

    if key.code == KeyCode::Esc || is_ctrl_c(key) {
        view.cancel_pending_user_input_edit();
        return TuiInputOutcome::Continue;
    }

    match key.code {
        KeyCode::Char(c) => {
            view.command_input.insert_char(c);
            view.command_feedback = None;
            view.reset_command_history_navigation();
            view.reset_command_popup();
        }
        KeyCode::Backspace => {
            view.command_input.delete_before_cursor();
            view.command_feedback = None;
            view.reset_command_history_navigation();
            view.reset_command_popup();
        }
        KeyCode::Up => {
            if view.command_input.move_up_line() {
                view.reset_command_popup();
            }
        }
        KeyCode::Down => {
            if view.command_input.move_down_line() {
                view.reset_command_popup();
            }
        }
        KeyCode::Enter => {
            if !view.pending_image_attachments.is_empty() {
                view.command_feedback = Some(CommandFeedback {
                    title: "QUEUED INPUTS".to_string(),
                    message: "pending input edits cannot add image attachments".to_string(),
                    detail: Some("Remove image placeholders before saving the edit.".to_string()),
                    level: CommandFeedbackLevel::Error,
                });
                return TuiInputOutcome::Continue;
            }
            if !view.pending_pastes.is_empty() {
                view.command_input.set_text(expand_paste_placeholders(
                    view.command_input.as_str(),
                    &view.pending_pastes,
                ));
                view.pending_pastes.clear();
            }
            let Some(editing) = view.editing_pending_user_input.as_ref() else {
                return TuiInputOutcome::Continue;
            };
            let Ok(event_id) = editing.event_id.parse() else {
                view.command_feedback = Some(CommandFeedback {
                    title: "QUEUED INPUTS".to_string(),
                    message: "queued input id is invalid".to_string(),
                    detail: Some(editing.event_id.clone()),
                    level: CommandFeedbackLevel::Error,
                });
                return TuiInputOutcome::Continue;
            };
            return TuiInputOutcome::SavePendingUserInputEdit {
                event_id,
                incoming_text: view.command_input.as_str().to_string(),
            };
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
        view.reset_command_history_navigation();
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
    let attachments =
        pending_attachments_for_input(view.command_input.as_str(), &view.pending_image_attachments);
    if !input.is_empty() {
        if !attachments.is_empty() && is_dashboard_command_input(&input) {
            view.command_panel = None;
            view.command_feedback = Some(CommandFeedback {
                title: "ATTACHMENTS".to_string(),
                message: "dashboard commands cannot include image attachments".to_string(),
                detail: Some(
                    "Remove image placeholders before running a slash command.".to_string(),
                ),
                level: CommandFeedbackLevel::Error,
            });
            view.reset_command_popup();
            return TuiInputOutcome::Continue;
        }
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
        return TuiInputOutcome::SubmitText { input, attachments };
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
        TuiInputOutcome::CopySelection { .. } => false,
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
            let clear_input_after_action = !matches!(action, DashboardAction::InterruptRuntime);
            let result = command_runner.run_action(action, state).await;
            if is_clear_command_input(&input) && result.success {
                view.clear_visible_activity();
            }
            view.command_feedback = if quiet_success && result.success {
                None
            } else {
                Some(command_feedback_from_action_result(title, result))
            };
            view.clear_ctrl_c_reminder();
            if clear_input_after_action {
                view.command_input.clear();
                view.pending_image_attachments.clear();
            }
            view.reset_command_popup();
            false
        }
        TuiInputOutcome::SavePendingUserInputEdit {
            event_id,
            incoming_text,
        } => {
            let result = command_runner
                .run_action(
                    DashboardAction::UpdatePendingUserInput {
                        event_id,
                        incoming_text,
                    },
                    state,
                )
                .await;
            if result.success {
                view.editing_pending_user_input = None;
                view.command_feedback = None;
                view.command_input.clear();
                view.pending_pastes.clear();
                view.pending_image_attachments.clear();
                view.reset_command_popup();
            } else {
                view.command_feedback = Some(command_feedback_from_action_result(
                    "Edit queued input".to_string(),
                    result,
                ));
            }
            false
        }
        TuiInputOutcome::SubmitText { input, attachments } => {
            let _ = command_runner.run_command(&input, attachments, state).await;
            view.record_command_history(&input);
            view.command_panel = None;
            view.command_feedback = None;
            view.command_input.clear();
            view.pending_image_attachments.clear();
            view.reset_command_popup();
            false
        }
    }
}

pub(super) fn handle_paste_event(text: &str, view: &mut TuiViewState) {
    if view.transcript_overlay.is_some() {
        return;
    }

    if let Some(attachments) = image_attachments_from_paste(text) {
        if view.editing_pending_user_input.is_some() {
            view.command_feedback = Some(CommandFeedback {
                title: "QUEUED INPUTS".to_string(),
                message: "pending input edits cannot add image attachments".to_string(),
                detail: Some("Paste plain text while editing a queued input.".to_string()),
                level: CommandFeedbackLevel::Error,
            });
            return;
        }
        let placeholders = attachments
            .iter()
            .map(|attachment| attachment.placeholder.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        view.command_input.text.push_str(&placeholders);
        view.pending_image_attachments.extend(attachments);
    } else {
        handle_paste_placeholder(text, &mut view.command_input.text, &mut view.pending_pastes);
    }
    view.command_input.move_end();
    view.command_feedback = None;
    view.clear_ctrl_c_reminder();
    view.reset_command_history_navigation();
}

fn pending_attachments_for_input(
    input: &str,
    attachments: &[DashboardCommandAttachment],
) -> Vec<DashboardCommandAttachment> {
    attachments
        .iter()
        .filter(|attachment| input.contains(&attachment.placeholder))
        .cloned()
        .collect()
}

fn image_attachments_from_paste(text: &str) -> Option<Vec<DashboardCommandAttachment>> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let paths = normalized
        .lines()
        .map(normalize_pasted_path)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if paths.is_empty() || paths.len() > 4 {
        return None;
    }
    let mut attachments = Vec::with_capacity(paths.len());
    for (index, path_text) in paths.iter().enumerate() {
        let path = PathBuf::from(path_text);
        let media_type = media_type_for_image_path(&path)?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(path_text)
            .to_string();
        let placeholder = if paths.len() == 1 {
            format!("[Image: {name}]")
        } else {
            format!("[Image {}: {name}]", index + 1)
        };
        attachments.push(DashboardCommandAttachment {
            placeholder,
            name,
            path,
            media_type: media_type.to_string(),
        });
    }
    Some(attachments)
}

fn normalize_pasted_path(line: &str) -> &str {
    line.trim().trim_matches(['"', '\''])
}

fn media_type_for_image_path(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("webp") => Some("image/webp"),
        Some("gif") => Some("image/gif"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::command_input::command_input_selectable_region;
    use crate::dashboard::selection::{SelectableId, SelectableRegion};
    use crate::dashboard::tui_event::TuiMouseSelectionKind;
    use ratatui::layout::Rect;

    struct OkRunner;

    #[async_trait::async_trait]
    impl DashboardCommandRunner for OkRunner {
        async fn run_command(
            &self,
            _command: &str,
            _attachments: Vec<DashboardCommandAttachment>,
            _state: &DashboardState,
        ) -> String {
            String::new()
        }

        async fn run_action(
            &self,
            _action: DashboardAction,
            _state: &DashboardState,
        ) -> crate::dashboard::DashboardActionResult {
            crate::dashboard::DashboardActionResult {
                success: true,
                message: "ok".to_string(),
                detail: None,
            }
        }
    }

    fn state_with_active_runtime_turn() -> DashboardState {
        DashboardState {
            runtime_activity: crate::dashboard::DashboardRuntimeActivity::default()
                .with_runtime_turn(Some("model request".to_string()), Some(1_000)),
            ..DashboardState::default()
        }
    }

    fn pending_user_input(
        event_id: uuid::Uuid,
        text: &str,
    ) -> crate::dashboard::DashboardPendingUserInput {
        crate::dashboard::DashboardPendingUserInput {
            event_id: event_id.to_string(),
            origin: "tui".to_string(),
            incoming_text: text.to_string(),
            arrived_at_ms: 1_000,
            attachment_count: 0,
        }
    }

    fn state_with_pending_user_inputs(
        inputs: Vec<crate::dashboard::DashboardPendingUserInput>,
    ) -> DashboardState {
        DashboardState {
            pending_user_inputs: inputs,
            ..DashboardState::default()
        }
    }

    #[test]
    fn ctrl_t_opens_transcript_overlay() {
        let mut view = TuiViewState::new();
        let state = DashboardState::default();
        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
            &mut view,
            &state,
        );

        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert!(view.command_panel.is_none());
        let Some(overlay) = view.transcript_overlay else {
            panic!("ctrl+t should open a transcript overlay");
        };
        assert!(overlay.cells.is_empty());
        assert!(overlay.live_cells.is_empty());
        assert!(overlay.follow_bottom);
        assert_eq!(overlay.scroll, 0);
    }

    #[test]
    fn ctrl_t_opens_transcript_overlay_from_command_panel() {
        let mut view = TuiViewState::new();
        let state = DashboardState::default();
        view.command_panel = Some(CommandPanel::SkillsList(SkillsListPanel::from_state(
            &state,
        )));

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
            &mut view,
            &state,
        );

        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert!(view.command_panel.is_none());
        assert!(view.transcript_overlay.is_some());
    }

    #[test]
    fn ctrl_t_preserves_composer_draft_when_opening_transcript_overlay() {
        let mut view = TuiViewState::new();
        view.command_input.set_text("keep this draft".to_string());

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
            &mut view,
            &DashboardState::default(),
        );

        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert!(view.transcript_overlay.is_some());
        assert_eq!(view.command_input.as_str(), "keep this draft");
    }

    #[test]
    fn transcript_overlay_handles_close_and_scroll_keys_before_composer() {
        let mut view = TuiViewState::new();
        view.transcript_overlay = Some(crate::dashboard::view_state::TranscriptOverlayState::new(
            Vec::new(),
            Vec::new(),
            0,
        ));
        let overlay = view.transcript_overlay.as_mut().expect("overlay");
        overlay.set_render_metrics(100, 20);

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &mut view,
            &DashboardState::default(),
        );

        assert!(matches!(outcome, TuiInputOutcome::Continue));
        let overlay = view.transcript_overlay.as_ref().expect("overlay");
        assert!(!overlay.follow_bottom);
        assert_eq!(overlay.scroll, 98);
        assert!(view.command_input.is_empty());

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            &mut view,
            &DashboardState::default(),
        );

        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert!(view.transcript_overlay.is_none());
    }

    #[test]
    fn transcript_overlay_ignores_paste_before_composer() {
        let mut view = TuiViewState::new();
        view.transcript_overlay = Some(crate::dashboard::view_state::TranscriptOverlayState::new(
            Vec::new(),
            Vec::new(),
            0,
        ));

        handle_paste_event("hidden draft", &mut view);

        assert_eq!(view.command_input.as_str(), "");
        assert!(view.pending_pastes.is_empty());
        assert!(view.pending_image_attachments.is_empty());
    }

    #[test]
    fn pasted_image_path_becomes_pending_attachment() {
        let mut view = TuiViewState::new();
        handle_paste_event("\"C:/tmp/dashboard.png\"", &mut view);

        assert_eq!(view.command_input.as_str(), "[Image: dashboard.png]");
        assert_eq!(view.pending_image_attachments.len(), 1);
        assert_eq!(view.pending_image_attachments[0].media_type, "image/png");

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut view,
            &DashboardState::default(),
        );

        match outcome {
            TuiInputOutcome::SubmitText { input, attachments } => {
                assert_eq!(input, "[Image: dashboard.png]");
                assert_eq!(attachments.len(), 1);
                assert_eq!(attachments[0].name, "dashboard.png");
            }
            _ => panic!("image paste should submit text with an attachment"),
        }
    }

    #[test]
    fn slash_commands_cannot_submit_image_attachments() {
        let mut view = TuiViewState::new();
        handle_paste_event("C:/tmp/dashboard.png", &mut view);
        view.command_input
            .set_text(format!("/status {}", view.command_input.as_str()));

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut view,
            &DashboardState::default(),
        );

        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert_eq!(
            view.command_feedback
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("dashboard commands cannot include image attachments")
        );
    }

    #[test]
    fn esc_interrupts_active_runtime_turn_without_clearing_composer() {
        let mut view = TuiViewState::new();
        view.command_input.set_text("keep this draft".to_string());
        let state = state_with_active_runtime_turn();

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &mut view,
            &state,
        );

        match outcome {
            TuiInputOutcome::RunDashboardAction { action, .. } => {
                assert_eq!(action, DashboardAction::InterruptRuntime);
            }
            _ => panic!("esc should interrupt an active runtime turn"),
        }
        assert_eq!(view.command_input.as_str(), "keep this draft");
    }

    #[tokio::test]
    async fn executing_interrupt_action_keeps_composer_text() {
        let mut view = TuiViewState::new();
        view.command_input.set_text("keep this draft".to_string());
        let state = state_with_active_runtime_turn();
        let outcome = TuiInputOutcome::RunDashboardAction {
            input: "<esc>".to_string(),
            title: "Interrupt".to_string(),
            action: DashboardAction::InterruptRuntime,
            quiet_success: true,
        };

        let should_exit = execute_input_outcome(outcome, &mut view, &state, &OkRunner).await;

        assert!(!should_exit);
        assert_eq!(view.command_input.as_str(), "keep this draft");
        assert!(view.command_feedback.is_none());
    }

    #[tokio::test]
    async fn up_down_navigate_local_composer_history() {
        let mut view = TuiViewState::new();
        let state = DashboardState::default();
        execute_input_outcome(
            TuiInputOutcome::SubmitText {
                input: "first".to_string(),
                attachments: Vec::new(),
            },
            &mut view,
            &state,
            &OkRunner,
        )
        .await;
        execute_input_outcome(
            TuiInputOutcome::SubmitText {
                input: "second".to_string(),
                attachments: Vec::new(),
            },
            &mut view,
            &state,
            &OkRunner,
        )
        .await;
        view.max_scroll = 100;
        view.auto_scroll = true;

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &mut view,
            &state,
        );
        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert_eq!(view.command_input.as_str(), "second");
        assert_eq!(view.command_input.cursor_pos, 0);

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &mut view,
            &state,
        );
        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert_eq!(view.command_input.as_str(), "first");

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &mut view,
            &state,
        );
        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert_eq!(view.command_input.as_str(), "second");

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &mut view,
            &state,
        );
        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert_eq!(view.command_input.as_str(), "");
    }

    #[tokio::test]
    async fn up_does_not_replace_regular_draft_with_history() {
        let mut view = TuiViewState::new();
        let state = DashboardState::default();
        execute_input_outcome(
            TuiInputOutcome::SubmitText {
                input: "previous".to_string(),
                attachments: Vec::new(),
            },
            &mut view,
            &state,
            &OkRunner,
        )
        .await;
        view.command_input.set_text("draft".to_string());

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &mut view,
            &state,
        );

        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert_eq!(view.command_input.as_str(), "draft");
    }

    #[test]
    fn up_down_move_cursor_between_multiline_composer_lines() {
        let mut view = TuiViewState::new();
        let state = DashboardState::default();
        view.command_input.set_text("hello".to_string());

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            &mut view,
            &state,
        );
        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert_eq!(view.command_input.as_str(), "hello\n");

        for ch in "world".chars() {
            let outcome = handle_key_event(
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                &mut view,
                &state,
            );
            assert!(matches!(outcome, TuiInputOutcome::Continue));
        }
        assert_eq!(view.command_input.as_str(), "hello\nworld");
        assert_eq!(view.command_input.cursor_pos, "hello\nworld".len());

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &mut view,
            &state,
        );
        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert_eq!(view.command_input.cursor_pos, "hello".len());

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &mut view,
            &state,
        );
        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert_eq!(view.command_input.cursor_pos, "hello\nworld".len());
    }

    #[test]
    fn pending_input_edit_up_down_move_multiline_cursor() {
        let event_id = uuid::Uuid::from_u128(7);
        let state = state_with_pending_user_inputs(vec![pending_user_input(event_id, "old")]);
        let mut view = TuiViewState::new();
        view.begin_pending_user_input_edit(event_id.to_string(), "hello\nworld".to_string());

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &mut view,
            &state,
        );
        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert_eq!(view.command_input.cursor_pos, "hello".len());

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &mut view,
            &state,
        );
        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert_eq!(view.command_input.cursor_pos, "hello\nworld".len());
    }

    #[test]
    fn ctrl_c_clears_nonempty_composer_before_interrupting() {
        let mut view = TuiViewState::new();
        view.command_input.set_text("draft".to_string());
        let state = state_with_active_runtime_turn();

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut view,
            &state,
        );

        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert!(view.command_input.is_empty());
        assert_eq!(view.ctrl_c_reminder, Some(CtrlCReminder::Interrupt));
    }

    #[test]
    fn mouse_selection_updates_local_selection_state() {
        let mut view = TuiViewState::new();
        view.set_selectable_regions(vec![SelectableRegion::new(
            SelectableId::new("test"),
            Rect::new(0, 0, 20, 1),
            vec!["selectable text".to_string()],
            0,
        )]);

        assert!(view.handle_selection_mouse_event(TuiMouseSelectionKind::Down, 0, 0));
        assert!(view.handle_selection_mouse_event(TuiMouseSelectionKind::Drag, 10, 0));
        assert!(view.handle_selection_mouse_event(TuiMouseSelectionKind::Up, 10, 0));

        assert_eq!(view.selected_text().as_deref(), Some("selectable"));
    }

    #[test]
    fn ctrl_c_copies_selection_before_interrupting_runtime() {
        let mut view = TuiViewState::new();
        view.set_selectable_regions(vec![SelectableRegion::new(
            SelectableId::new("test"),
            Rect::new(0, 0, 20, 1),
            vec!["copy this".to_string()],
            0,
        )]);
        assert!(view.handle_selection_mouse_event(TuiMouseSelectionKind::Down, 0, 0));
        assert!(view.handle_selection_mouse_event(TuiMouseSelectionKind::Drag, 9, 0));
        assert!(view.handle_selection_mouse_event(TuiMouseSelectionKind::Up, 9, 0));

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut view,
            &state_with_active_runtime_turn(),
        );

        match outcome {
            TuiInputOutcome::CopySelection { text } => assert_eq!(text, "copy this"),
            _ => panic!("ctrl-c should copy an active selection before interrupting"),
        }
    }

    #[test]
    fn ctrl_c_copies_command_input_selection_without_clearing_composer() {
        let mut view = TuiViewState::new();
        view.command_input.set_text("hello".to_string());
        view.set_selectable_regions(vec![
            command_input_selectable_region(view.command_input.as_str(), Rect::new(0, 0, 20, 1), 0)
                .expect("non-empty command input should be selectable"),
        ]);
        assert!(view.handle_selection_mouse_event(TuiMouseSelectionKind::Down, 0, 0));
        assert!(view.handle_selection_mouse_event(TuiMouseSelectionKind::Drag, 7, 0));
        assert!(view.handle_selection_mouse_event(TuiMouseSelectionKind::Up, 7, 0));

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut view,
            &DashboardState::default(),
        );

        match outcome {
            TuiInputOutcome::CopySelection { text } => assert_eq!(text, "hello"),
            _ => panic!("ctrl-c should copy the command input selection"),
        }
        assert_eq!(view.command_input.as_str(), "hello");
    }

    #[test]
    fn ctrl_c_clears_idle_composer_without_quit_reminder() {
        let mut view = TuiViewState::new();
        view.command_input.set_text("draft".to_string());

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut view,
            &DashboardState::default(),
        );

        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert!(view.command_input.is_empty());
        assert_eq!(view.ctrl_c_reminder, None);
    }

    #[test]
    fn ctrl_c_interrupts_active_runtime_turn_when_composer_is_empty() {
        let mut view = TuiViewState::new();
        let state = state_with_active_runtime_turn();

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut view,
            &state,
        );

        match outcome {
            TuiInputOutcome::RunDashboardAction { action, .. } => {
                assert_eq!(action, DashboardAction::InterruptRuntime);
            }
            _ => panic!("ctrl-c should interrupt when an active runtime turn has no draft"),
        }
    }

    #[test]
    fn ctrl_c_is_noop_when_idle_and_composer_is_empty() {
        let mut view = TuiViewState::new();
        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut view,
            &DashboardState::default(),
        );

        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert_eq!(view.ctrl_c_reminder, None);
    }

    #[test]
    fn ctrl_p_opens_pending_user_input_queue_panel() {
        let event_id = uuid::Uuid::from_u128(1);
        let state = state_with_pending_user_inputs(vec![pending_user_input(event_id, "queued")]);
        let mut view = TuiViewState::new();

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &mut view,
            &state,
        );

        assert!(matches!(outcome, TuiInputOutcome::Continue));
        match view.command_panel.as_ref() {
            Some(CommandPanel::PendingUserInputQueue(panel)) => {
                assert_eq!(panel.inputs.len(), 1);
                assert_eq!(panel.inputs[0].event_id, event_id.to_string());
            }
            _ => panic!("ctrl+p should open the queued input panel"),
        }
    }

    #[test]
    fn ctrl_p_without_pending_inputs_shows_feedback() {
        let mut view = TuiViewState::new();

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &mut view,
            &DashboardState::default(),
        );

        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert!(view.command_panel.is_none());
        assert_eq!(
            view.command_feedback
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("no queued follow-up inputs")
        );
    }

    #[test]
    fn pending_input_queue_discard_shortcut_runs_action() {
        let event_id = uuid::Uuid::from_u128(2);
        let state =
            state_with_pending_user_inputs(vec![pending_user_input(event_id, "discard me")]);
        let mut view = TuiViewState::new();
        let _ = handle_key_event(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &mut view,
            &state,
        );

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &mut view,
            &state,
        );

        match outcome {
            TuiInputOutcome::RunPanelAction {
                action, keep_panel, ..
            } => {
                assert!(keep_panel);
                assert_eq!(
                    action,
                    DashboardAction::DismissPendingUserInput { event_id }
                );
            }
            _ => panic!("d should discard the selected queued input"),
        }
    }

    #[test]
    fn pending_input_queue_edit_shortcut_loads_composer() {
        let event_id = uuid::Uuid::from_u128(3);
        let state = state_with_pending_user_inputs(vec![pending_user_input(event_id, "edit me")]);
        let mut view = TuiViewState::new();
        let _ = handle_key_event(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &mut view,
            &state,
        );

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut view,
            &state,
        );

        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert!(view.command_panel.is_none());
        assert_eq!(view.command_input.as_str(), "edit me");
        assert_eq!(
            view.editing_pending_user_input
                .as_ref()
                .map(|editing| editing.event_id.as_str()),
            Some(event_id.to_string().as_str())
        );
    }

    #[test]
    fn pending_input_queue_esc_closes_without_preempting_selected_input() {
        let event_id = uuid::Uuid::from_u128(6);
        let state = state_with_pending_user_inputs(vec![pending_user_input(event_id, "run now")]);
        let mut view = TuiViewState::new();
        let _ = handle_key_event(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &mut view,
            &state,
        );

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &mut view,
            &state,
        );

        assert!(matches!(outcome, TuiInputOutcome::Continue));
        assert!(view.command_panel.is_none());
    }

    #[test]
    fn pending_input_queue_run_shortcut_preempts_selected_input() {
        let event_id = uuid::Uuid::from_u128(7);
        let state = state_with_pending_user_inputs(vec![pending_user_input(event_id, "run now")]);
        let mut view = TuiViewState::new();
        let _ = handle_key_event(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &mut view,
            &state,
        );

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            &mut view,
            &state,
        );

        match outcome {
            TuiInputOutcome::RunPanelAction {
                action, keep_panel, ..
            } => {
                assert!(!keep_panel);
                assert_eq!(
                    action,
                    DashboardAction::PreemptPendingUserInput { event_id }
                );
            }
            _ => panic!("r should preempt the selected queued input"),
        }
    }

    #[test]
    fn pending_input_queue_reorder_shortcut_runs_action() {
        let event_id = uuid::Uuid::from_u128(4);
        let state = state_with_pending_user_inputs(vec![pending_user_input(event_id, "move me")]);
        let mut view = TuiViewState::new();
        let _ = handle_key_event(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &mut view,
            &state,
        );

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT),
            &mut view,
            &state,
        );

        match outcome {
            TuiInputOutcome::RunPanelAction {
                action, keep_panel, ..
            } => {
                assert!(keep_panel);
                assert_eq!(
                    action,
                    DashboardAction::MovePendingUserInput {
                        event_id,
                        direction: crate::dashboard::DashboardPendingUserInputMoveDirection::Down,
                    }
                );
            }
            _ => panic!("shift+down should reorder the selected queued input"),
        }
    }

    #[tokio::test]
    async fn pending_input_edit_enter_saves_update_action() {
        let event_id = uuid::Uuid::from_u128(5);
        let state = state_with_pending_user_inputs(vec![pending_user_input(event_id, "old")]);
        let mut view = TuiViewState::new();
        view.begin_pending_user_input_edit(event_id.to_string(), "old".to_string());
        view.command_input.set_text("updated".to_string());

        let outcome = handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut view,
            &state,
        );

        match &outcome {
            TuiInputOutcome::SavePendingUserInputEdit {
                event_id: saved_event_id,
                incoming_text,
            } => {
                assert_eq!(*saved_event_id, event_id);
                assert_eq!(incoming_text, "updated");
            }
            _ => panic!("enter should save the queued input edit"),
        }

        let should_exit = execute_input_outcome(outcome, &mut view, &state, &OkRunner).await;
        assert!(!should_exit);
        assert!(view.editing_pending_user_input.is_none());
        assert!(view.command_input.is_empty());
    }
}
