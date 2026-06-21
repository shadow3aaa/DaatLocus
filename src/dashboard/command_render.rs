#[cfg(test)]
use ratatui::backend::TestBackend;
use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use super::DashboardPendingUserInput;
use super::command_flow::{
    command_completion_body, dashboard_command_parts, dashboard_parts_open_panel,
    dashboard_parts_run_action, is_dashboard_command_input, matching_commands,
    selected_command_completion,
};
use super::command_input::{
    command_input_display_text_for_width, command_input_selectable_region, cursor_display_row,
    cursor_display_xy,
};
use super::command_panels::{
    CommandDetailPanel, CommandFeedback, CommandFeedbackLevel, CommandPanel, CommandSelectionPanel,
    DashboardCommandContext, PendingUserInputQueuePanel, SkillsListPanel, SkillsTogglePanel,
    TELEGRAM_ACCESS_PICKER_VISIBLE_ROWS, TelegramAccessPicker,
};
use super::command_registry::dashboard_command_is_known;
use super::command_text::{truncate_command_text, truncate_display_width};
use super::selection::{SelectableId, SelectableRegion, SelectionRegistry, line_plain_text};
use super::view_state::CtrlCReminder;

impl CommandPanel {
    pub(super) fn desired_height(&self) -> u16 {
        match self {
            CommandPanel::Detail(panel) => {
                let line_count = render_panel_text_lines(&panel.text).len() as u16;
                line_count.saturating_add(3).clamp(5, 16)
            }
            CommandPanel::Selection(panel) => {
                let header = 1 + u16::from(panel.subtitle.is_some());
                header
                    .saturating_add(panel.items.len().min(8) as u16)
                    .saturating_add(2)
                    .clamp(5, 14)
            }
            CommandPanel::SkillsList(panel) => {
                let rows = panel.visible_indices().len().min(8) as u16;
                let error_rows = panel.errors.len().min(2) as u16;
                5u16.saturating_add(rows)
                    .saturating_add(error_rows)
                    .clamp(6, 16)
            }
            CommandPanel::SkillsToggle(panel) => {
                let rows = panel.visible_indices().len().min(8) as u16;
                let feedback_rows = command_feedback_row_count(panel.feedback.as_ref());
                5u16.saturating_add(rows)
                    .saturating_add(feedback_rows)
                    .clamp(6, 16)
            }
            CommandPanel::TelegramAccess(picker) => 4u16
                .saturating_add(
                    picker
                        .requests
                        .len()
                        .min(TELEGRAM_ACCESS_PICKER_VISIBLE_ROWS) as u16,
                )
                .clamp(6, 15),
            CommandPanel::PendingUserInputQueue(panel) => {
                let feedback_rows = command_feedback_row_count(panel.feedback.as_ref());
                4u16.saturating_add(panel.inputs.len().min(8) as u16)
                    .saturating_add(feedback_rows)
                    .clamp(6, 15)
            }
        }
    }
}

pub(super) struct CommandBarRenderState<'a> {
    pub(super) input: &'a str,
    pub(super) cursor_pos: usize,
    pub(super) context: &'a DashboardCommandContext<'a>,
    pub(super) feedback: Option<&'a CommandFeedback>,
    pub(super) footer_context: &'a str,
    pub(super) pending_paste_count: usize,
    pub(super) pending_image_attachment_count: usize,
    pub(super) pending_user_inputs: &'a [DashboardPendingUserInput],
    pub(super) ctrl_c_reminder: Option<CtrlCReminder>,
    pub(super) editing_pending_user_input: bool,
    pub(super) panel: Option<&'a CommandPanel>,
    pub(super) panel_rows: u16,
    pub(super) popup_selection: usize,
    pub(super) popup_scroll: usize,
    pub(super) last_cursor_pos: &'a mut Option<(u16, u16)>,
    pub(super) input_lines: u16,
    pub(super) selection: &'a SelectionRegistry,
    pub(super) selectable_regions: &'a mut Vec<SelectableRegion>,
}

pub(super) fn command_panel_row_count(
    panel: Option<&CommandPanel>,
    terminal_height: u16,
    input_lines: u16,
    popup_rows: u16,
    feedback_rows: u16,
) -> u16 {
    let Some(panel) = panel else {
        return 0;
    };
    let base_rows = 1u16
        .saturating_add(input_lines)
        .saturating_add(popup_rows)
        .saturating_add(feedback_rows);
    let available = terminal_height.saturating_sub(base_rows).saturating_sub(6);
    if available < 5 {
        return 0;
    }
    panel.desired_height().min(available)
}

pub(super) fn command_popup_row_count(input: &str, context: &DashboardCommandContext<'_>) -> u16 {
    let matches = matching_commands(input, context);
    if matches.is_empty() {
        0
    } else {
        matches.len().min(6) as u16
    }
}

pub(super) fn command_feedback_row_count(feedback: Option<&CommandFeedback>) -> u16 {
    match feedback {
        Some(feedback)
            if feedback
                .detail
                .as_ref()
                .is_some_and(|detail| !detail.trim().is_empty()) =>
        {
            2
        }
        Some(_) => 1,
        None => 0,
    }
}

pub(super) fn pending_user_input_preview_row_count(inputs: &[DashboardPendingUserInput]) -> u16 {
    if inputs.is_empty() {
        return 0;
    }
    let visible = inputs.len().min(PENDING_USER_INPUT_PREVIEW_LIMIT);
    let overflow = usize::from(inputs.len() > PENDING_USER_INPUT_PREVIEW_LIMIT);
    (1 + visible + overflow) as u16
}

pub(super) fn render_command_panel(f: &mut Frame, area: Rect, panel: &CommandPanel) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let inner = inset_rect(area, 1, 2);
    match panel {
        CommandPanel::Detail(detail) => render_detail_panel(f, inner, detail),
        CommandPanel::Selection(selection) => render_selection_panel(f, inner, selection),
        CommandPanel::SkillsList(skills) => render_skills_list_panel(f, inner, skills),
        CommandPanel::SkillsToggle(skills) => render_skills_toggle_panel(f, inner, skills),
        CommandPanel::TelegramAccess(picker) => render_telegram_access_panel(f, inner, picker),
        CommandPanel::PendingUserInputQueue(panel) => {
            render_pending_user_input_queue_panel(f, inner, panel)
        }
    }
}

fn command_panel_selectable_region(panel: &CommandPanel, area: Rect) -> Option<SelectableRegion> {
    if area.height == 0 || area.width == 0 {
        return None;
    }
    let inner = inset_rect(area, 1, 2);
    if inner.height == 0 || inner.width == 0 {
        return None;
    }
    let (lines, scroll) = command_panel_copy_lines(panel);
    Some(SelectableRegion::new(
        SelectableId::new("command-panel"),
        inner,
        lines,
        scroll,
    ))
}

fn command_panel_copy_lines(panel: &CommandPanel) -> (Vec<String>, u16) {
    match panel {
        CommandPanel::Detail(panel) => {
            let mut lines = vec![panel.title.clone()];
            lines.extend(
                render_panel_text_lines(&panel.text)
                    .iter()
                    .skip(panel.scroll as usize)
                    .map(line_plain_text),
            );
            (lines, 0)
        }
        CommandPanel::Selection(panel) => {
            let mut lines = vec![panel.title.clone()];
            if let Some(subtitle) = panel.subtitle.as_ref() {
                lines.push(subtitle.clone());
            }
            lines.extend(
                panel
                    .items
                    .iter()
                    .skip(panel.scroll)
                    .map(|item| format!("{}  {}", item.name, item.description)),
            );
            (lines, 0)
        }
        CommandPanel::SkillsList(panel) => {
            let mut lines = vec!["Skills".to_string()];
            lines.push(if panel.items.is_empty() {
                "No skills loaded.".to_string()
            } else {
                format!("{} loaded. Choose a skill to inspect.", panel.items.len())
            });
            lines.push(if panel.search.is_empty() {
                "> Type to search skills".to_string()
            } else {
                format!("> {}", panel.search)
            });
            lines.extend(
                panel
                    .errors
                    .iter()
                    .map(|error| format!("! {}  {}", error.path, error.message)),
            );
            lines.extend(
                panel
                    .visible_indices()
                    .into_iter()
                    .skip(panel.scroll)
                    .filter_map(|idx| {
                        let item = panel.items.get(idx)?;
                        Some(format!(
                            "{}  {}  {}  {}",
                            item.name, item.status, item.scope, item.description
                        ))
                    }),
            );
            (lines, 0)
        }
        CommandPanel::SkillsToggle(panel) => {
            let mut lines = vec!["Skills".to_string()];
            lines.push(if panel.items.is_empty() {
                "No skills loaded.".to_string()
            } else {
                "Toggle automatic use for loaded skills.".to_string()
            });
            lines.push(if panel.search.is_empty() {
                "> Type to search skills".to_string()
            } else {
                format!("> {}", panel.search)
            });
            lines.extend(
                panel
                    .visible_indices()
                    .into_iter()
                    .skip(panel.scroll)
                    .filter_map(|idx| {
                        let item = panel.items.get(idx)?;
                        let checkbox = if item.auto_use_enabled { "[x]" } else { "[ ]" };
                        Some(format!(
                            "{} {}  {}  {}",
                            checkbox,
                            item.name,
                            item.scope,
                            item.status_description()
                        ))
                    }),
            );
            (lines, 0)
        }
        CommandPanel::TelegramAccess(picker) => {
            let mut lines = vec![
                picker.action.title().to_string(),
                format!("Select a pending request to {}.", picker.action.verb()),
            ];
            lines.extend(picker.requests.iter().skip(picker.scroll).map(|request| {
                format!(
                    "{}  {}  {}  {}",
                    request.chat_id, request.title, request.sender, request.last_message_preview
                )
            }));
            (lines, 0)
        }
        CommandPanel::PendingUserInputQueue(panel) => {
            let mut lines = vec!["Queued inputs".to_string()];
            lines.extend(
                panel
                    .inputs
                    .iter()
                    .skip(panel.scroll)
                    .map(pending_user_input_preview_text),
            );
            (lines, 0)
        }
    }
}

pub(super) fn render_command_bar(f: &mut Frame, area: Rect, state: CommandBarRenderState<'_>) {
    let CommandBarRenderState {
        input,
        cursor_pos,
        input_lines,
        context,
        feedback,
        footer_context,
        pending_paste_count,
        pending_image_attachment_count,
        pending_user_inputs,
        ctrl_c_reminder,
        editing_pending_user_input,
        panel,
        panel_rows,
        popup_selection,
        popup_scroll,
        last_cursor_pos,
        selection,
        selectable_regions,
    } = state;

    let completion = if panel.is_none() && !editing_pending_user_input {
        selected_command_completion(input, 0, context)
    } else {
        None
    };
    let hint = if editing_pending_user_input {
        String::new()
    } else {
        command_hint(input, context)
    };
    let popup_rows = if panel.is_some() || editing_pending_user_input {
        0
    } else {
        command_popup_row_count(input, context)
    };
    let feedback_rows = if panel.is_some() {
        0
    } else {
        command_feedback_row_count(feedback)
    };
    let pending_user_input_rows = if panel.is_none() && !editing_pending_user_input {
        pending_user_input_preview_row_count(pending_user_inputs)
    } else {
        0
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints({
            let mut c = Vec::new();
            if panel_rows > 0 {
                c.push(Constraint::Length(panel_rows));
            }
            if feedback_rows > 0 {
                c.push(Constraint::Length(feedback_rows));
            }
            if pending_user_input_rows > 0 {
                c.push(Constraint::Length(pending_user_input_rows));
            }
            c.push(Constraint::Length(input_lines));
            if popup_rows > 0 {
                c.push(Constraint::Length(popup_rows));
            }
            c.push(Constraint::Length(1));
            c
        })
        .split(area);
    let mut row_index = 0usize;
    if let Some(panel) = panel
        && panel_rows > 0
    {
        let panel_area = rows[row_index];
        render_command_panel(f, panel_area, panel);
        if let Some(region) = command_panel_selectable_region(panel, panel_area) {
            if let Some(range) = selection.region_selection(&region.id) {
                region.highlight(&range, f.buffer_mut());
            }
            selectable_regions.push(region);
        }
        row_index += 1;
    }
    if let Some(feedback) = feedback
        && feedback_rows > 0
    {
        render_command_feedback(f, rows[row_index], feedback);
        row_index += 1;
    }
    if pending_user_input_rows > 0 {
        render_pending_user_input_preview(f, rows[row_index], pending_user_inputs);
        row_index += 1;
    }
    let input_row_index = row_index;
    let available_width = area.width.saturating_sub(2).max(1) as usize;
    let wrap_width = area.width.saturating_sub(2).max(1) as usize;
    let cursor_total_row = cursor_display_row(input, cursor_pos, available_width, wrap_width);
    let input_scroll =
        cursor_total_row.saturating_sub(rows[input_row_index].height.saturating_sub(1));
    let input_para = Paragraph::new(command_input_display_text_for_width(
        input,
        completion.as_deref(),
        area.width,
    ))
    .scroll((input_scroll, 0));
    f.render_widget(input_para, rows[input_row_index]);
    if let Some(region) =
        command_input_selectable_region(input, rows[input_row_index], input_scroll)
    {
        if let Some(range) = selection.region_selection(&region.id) {
            region.highlight(&range, f.buffer_mut());
        }
        selectable_regions.push(region);
    }

    let (cursor_x, cursor_y) = cursor_display_xy(
        input,
        cursor_pos,
        available_width,
        wrap_width,
        2,
        rows[input_row_index],
        input_scroll,
    );
    *last_cursor_pos = Some((cursor_x, cursor_y));
    let popup_row_index = input_row_index + 1;
    let footer_row = if popup_rows > 0 {
        render_command_popup(
            f,
            rows[popup_row_index],
            input,
            context,
            popup_selection,
            popup_scroll,
        );
        rows[popup_row_index + 1]
    } else {
        rows[popup_row_index]
    };
    let footer_line = if editing_pending_user_input {
        Line::from(vec![
            Span::styled("editing queued input", Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(
                "Enter save   Shift+Enter newline   Esc cancel",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else if let Some(panel) = panel {
        Line::from(vec![
            Span::styled("panel", Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(panel.footer_hint(), Style::default().fg(Color::DarkGray)),
        ])
    } else {
        render_footer_line(
            footer_context,
            &hint,
            pending_paste_count,
            pending_image_attachment_count,
            ctrl_c_reminder,
            footer_row.width,
        )
    };
    f.render_widget(Paragraph::new(footer_line), footer_row);
}

fn render_footer_line(
    footer_context: &str,
    hint: &str,
    pending_paste_count: usize,
    pending_image_attachment_count: usize,
    ctrl_c_reminder: Option<CtrlCReminder>,
    width: u16,
) -> Line<'static> {
    let mut parts = Vec::new();
    if !footer_context.trim().is_empty() {
        parts.push(footer_context.trim().to_string());
    }
    if pending_paste_count > 0 {
        parts.push(format!(
            "{} pasted block{} queued",
            pending_paste_count,
            if pending_paste_count == 1 { "" } else { "s" }
        ));
    }
    if pending_image_attachment_count > 0 {
        parts.push(format!(
            "{} image attachment{} queued",
            pending_image_attachment_count,
            if pending_image_attachment_count == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    let hint = match ctrl_c_reminder {
        Some(CtrlCReminder::Interrupt) => "ctrl + c again to interrupt",
        None => hint.trim(),
    };
    if !hint.is_empty() {
        parts.push(hint.to_string());
    }
    let text = if parts.is_empty() {
        String::new()
    } else {
        truncate_display_width(&parts.join("  ·  "), width as usize)
    };
    Line::from(vec![Span::styled(
        text,
        Style::default().fg(Color::DarkGray),
    )])
}

const PENDING_USER_INPUT_PREVIEW_LIMIT: usize = 3;

fn render_pending_user_input_preview(
    f: &mut Frame,
    area: Rect,
    inputs: &[DashboardPendingUserInput],
) {
    if area.height == 0 || area.width == 0 || inputs.is_empty() {
        return;
    }
    f.render_widget(
        Paragraph::new(Text::from(pending_user_input_preview_lines(
            inputs,
            area.width as usize,
        ))),
        area,
    );
}

fn pending_user_input_preview_lines(
    inputs: &[DashboardPendingUserInput],
    width: usize,
) -> Vec<Line<'static>> {
    if inputs.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![Line::from(vec![
        Span::styled("•", Style::default().fg(Color::Cyan)),
        Span::raw(" "),
        Span::styled(
            format!("Queued follow-up inputs ({})  Ctrl+P manage", inputs.len()),
            Style::default().fg(Color::DarkGray),
        ),
    ])];
    let preview_width = width.saturating_sub(4);
    lines.extend(
        inputs
            .iter()
            .take(PENDING_USER_INPUT_PREVIEW_LIMIT)
            .map(|input| {
                Line::from(vec![
                    Span::styled("  ↳ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        truncate_display_width(
                            &pending_user_input_preview_text(input),
                            preview_width,
                        ),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ])
            }),
    );
    if inputs.len() > PENDING_USER_INPUT_PREVIEW_LIMIT {
        lines.push(Line::from(vec![Span::styled(
            format!(
                "  … {} more queued inputs",
                inputs.len() - PENDING_USER_INPUT_PREVIEW_LIMIT
            ),
            Style::default().fg(Color::DarkGray),
        )]));
    }
    lines
}

fn render_pending_user_input_queue_panel(
    f: &mut Frame,
    area: Rect,
    panel: &PendingUserInputQueuePanel,
) {
    let mut rest = render_panel_title(
        f,
        area,
        "Queued inputs",
        Some("Esc/q close. Enter edit. r run now. d discard. Shift+Up/Down reorder."),
    );
    if rest.height == 0 {
        return;
    }
    if let Some(feedback) = panel.feedback.as_ref() {
        let rows = command_feedback_row_count(Some(feedback)).min(rest.height);
        if rows > 0 {
            render_command_feedback(
                f,
                Rect {
                    height: rows,
                    ..rest
                },
                feedback,
            );
            rest.y = rest.y.saturating_add(rows);
            rest.height = rest.height.saturating_sub(rows);
        }
    }
    if rest.height == 0 {
        return;
    }
    let row_width = rest.width as usize;
    let lines = panel
        .inputs
        .iter()
        .skip(panel.scroll)
        .take(rest.height as usize)
        .enumerate()
        .map(|(visible_idx, input)| {
            let idx = panel.scroll + visible_idx;
            let selected = idx == panel.selected;
            let marker = if selected { "›" } else { " " };
            let index_text = format!("{}.", idx + 1);
            let fixed_width = 1usize
                .saturating_add(1)
                .saturating_add(UnicodeWidthStr::width(index_text.as_str()))
                .saturating_add(1);
            let preview_width = row_width.saturating_sub(fixed_width.saturating_add(2));
            let preview =
                truncate_display_width(&pending_user_input_preview_text(input), preview_width);
            let preview_style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(index_text, Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled(preview, preview_style),
            ])
        })
        .collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        rest,
    );
}

fn pending_user_input_preview_text(input: &DashboardPendingUserInput) -> String {
    let message = input
        .incoming_text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut text = if message.is_empty() {
        if input.attachment_count > 0 {
            "attachment-only input".to_string()
        } else {
            "empty input".to_string()
        }
    } else {
        message
    };
    if input.attachment_count > 0 && !input.incoming_text.trim().is_empty() {
        if input.attachment_count == 1 {
            text.push_str(" +1 attachment");
        } else {
            text.push_str(&format!(" +{} attachments", input.attachment_count));
        }
    }
    text
}

fn render_panel_text_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut previous_blank = true;

    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        if line.trim().is_empty() {
            lines.push(Line::from(""));
            previous_blank = true;
            continue;
        }

        if is_panel_section_header(line, previous_blank) {
            lines.push(Line::from(vec![Span::styled(
                line.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )]));
            previous_blank = false;
            continue;
        }

        if let Some(content) = line.strip_prefix("• ") {
            lines.push(render_panel_bullet_line(content));
            previous_blank = false;
            continue;
        }

        if let Some((label, value)) = line.split_once(':') {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{label}:"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(value.trim().to_string(), Style::default().fg(Color::Gray)),
            ]));
            previous_blank = false;
            continue;
        }

        lines.push(Line::from(vec![Span::styled(
            line.to_string(),
            Style::default().fg(Color::Gray),
        )]));
        previous_blank = false;
    }

    if lines.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "No data",
            Style::default().fg(Color::DarkGray),
        )]));
    }

    lines
}

fn is_panel_section_header(line: &str, previous_blank: bool) -> bool {
    previous_blank
        && !line.contains(':')
        && !line.starts_with('[')
        && !line.starts_with("• ")
        && line.chars().count() <= 32
}

fn render_panel_bullet_line(content: &str) -> Line<'static> {
    if let Some((label, value)) = content.split_once(':') {
        Line::from(vec![
            Span::styled("•", Style::default().fg(Color::Cyan)),
            Span::raw(" "),
            Span::styled(format!("{label}:"), Style::default().fg(Color::White)),
            Span::raw(" "),
            Span::styled(value.trim().to_string(), Style::default().fg(Color::Gray)),
        ])
    } else {
        Line::from(vec![
            Span::styled("•", Style::default().fg(Color::Cyan)),
            Span::raw(" "),
            Span::styled(content.to_string(), Style::default().fg(Color::White)),
        ])
    }
}

fn render_command_popup(
    f: &mut Frame,
    area: Rect,
    input: &str,
    context: &DashboardCommandContext<'_>,
    selected_index: usize,
    scroll: usize,
) {
    let matches = matching_commands(input, context);
    if matches.is_empty() {
        return;
    }

    let lines = matches
        .into_iter()
        .skip(scroll)
        .take(6)
        .enumerate()
        .map(|(visible_idx, suggestion)| {
            let idx = scroll + visible_idx;
            let selected = idx == selected_index;
            let style = if selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::Gray)
            };
            let desc_style = if selected {
                Style::default().fg(Color::Gray)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Line::from(vec![
                Span::raw("  "),
                Span::styled(suggestion.display, style),
                Span::raw("  "),
                Span::styled(suggestion.description, desc_style),
            ])
        })
        .collect::<Vec<_>>();

    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        area,
    );
}

fn render_command_feedback(f: &mut Frame, area: Rect, feedback: &CommandFeedback) {
    let (marker, marker_style, text_style) = match feedback.level {
        CommandFeedbackLevel::Info => (
            "ok",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(Color::Gray),
        ),
        CommandFeedbackLevel::Warning => (
            "!",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(Color::Gray),
        ),
        CommandFeedbackLevel::Error => (
            "x",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            Style::default().fg(Color::Gray),
        ),
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(marker, marker_style),
        Span::raw("  "),
        Span::styled(feedback.title.clone(), Style::default().fg(Color::White)),
        Span::raw("  "),
        Span::styled(feedback.message.clone(), text_style),
    ])];
    if let Some(detail) = feedback
        .detail
        .as_ref()
        .filter(|detail| !detail.trim().is_empty())
    {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(detail.clone(), Style::default().fg(Color::DarkGray)),
        ]));
    }
    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        area,
    );
}

fn inset_rect(area: Rect, vertical: u16, horizontal: u16) -> Rect {
    Rect {
        x: area.x.saturating_add(horizontal),
        y: area.y.saturating_add(vertical),
        width: area.width.saturating_sub(horizontal.saturating_mul(2)),
        height: area.height.saturating_sub(vertical.saturating_mul(2)),
    }
}

fn render_panel_title(f: &mut Frame, area: Rect, title: &str, subtitle: Option<&str>) -> Rect {
    if area.height == 0 {
        return area;
    }
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            title.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )])),
        Rect { height: 1, ..area },
    );
    let mut rest = Rect {
        y: area.y.saturating_add(1),
        height: area.height.saturating_sub(1),
        ..area
    };
    if let Some(subtitle) = subtitle
        && rest.height > 0
    {
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                subtitle.to_string(),
                Style::default().fg(Color::DarkGray),
            )])),
            Rect { height: 1, ..rest },
        );
        rest.y = rest.y.saturating_add(1);
        rest.height = rest.height.saturating_sub(1);
    }
    rest
}

fn render_detail_panel(f: &mut Frame, area: Rect, panel: &CommandDetailPanel) {
    let body = render_panel_title(f, area, &panel.title, None);
    if body.height == 0 {
        return;
    }
    let lines = render_panel_text_lines(&panel.text);
    let max_scroll = lines.len().saturating_sub(body.height as usize) as u16;
    let scroll = panel.scroll.min(max_scroll);
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((scroll, 0))
            .wrap(Wrap { trim: false }),
        body,
    );
}

fn render_selection_panel(f: &mut Frame, area: Rect, panel: &CommandSelectionPanel) {
    let list_area = render_panel_title(f, area, &panel.title, panel.subtitle.as_deref());
    if list_area.height == 0 {
        return;
    }
    let lines = panel
        .items
        .iter()
        .skip(panel.scroll)
        .take(list_area.height as usize)
        .enumerate()
        .map(|(visible_idx, item)| {
            let idx = panel.scroll + visible_idx;
            let selected = idx == panel.selected;
            let marker = if selected { "›" } else { " " };
            let name_style = if item.disabled {
                Style::default().fg(Color::DarkGray)
            } else if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let description_style = if item.disabled {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(item.name.clone(), name_style),
                Span::raw("  "),
                Span::styled(item.description.clone(), description_style),
            ])
        })
        .collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        list_area,
    );
}

fn render_skills_list_panel(f: &mut Frame, area: Rect, panel: &SkillsListPanel) {
    let subtitle = if panel.items.is_empty() {
        "No skills loaded.".to_string()
    } else {
        format!("{} loaded. Choose a skill to inspect.", panel.items.len())
    };
    let mut rest = render_panel_title(f, area, "Skills", Some(&subtitle));
    if rest.height == 0 {
        return;
    }
    let search_line = if panel.search.is_empty() {
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "Type to search skills",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::DarkGray)),
            Span::styled(panel.search.clone(), Style::default().fg(Color::White)),
        ])
    };
    f.render_widget(Paragraph::new(search_line), Rect { height: 1, ..rest });
    rest.y = rest.y.saturating_add(1);
    rest.height = rest.height.saturating_sub(1);

    if !panel.errors.is_empty() && rest.height > 0 {
        let error_lines = panel
            .errors
            .iter()
            .take(rest.height.min(2) as usize)
            .map(|error| {
                Line::from(vec![
                    Span::styled("!", Style::default().fg(Color::Yellow)),
                    Span::raw(" "),
                    Span::styled(
                        truncate_command_text(&error.path, 42),
                        Style::default().fg(Color::Gray),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        truncate_command_text(&error.message, 120),
                        Style::default().fg(Color::DarkGray),
                    ),
                ])
            })
            .collect::<Vec<_>>();
        let rows = error_lines.len() as u16;
        f.render_widget(
            Paragraph::new(Text::from(error_lines)).wrap(Wrap { trim: false }),
            Rect {
                height: rows,
                ..rest
            },
        );
        rest.y = rest.y.saturating_add(rows);
        rest.height = rest.height.saturating_sub(rows);
    }

    if rest.height == 0 {
        return;
    }
    let visible_indices = panel.visible_indices();
    let row_width = rest.width as usize;
    let lines = visible_indices
        .iter()
        .skip(panel.scroll)
        .take(rest.height as usize)
        .enumerate()
        .filter_map(|(visible_idx, actual_idx)| {
            let item = panel.items.get(*actual_idx)?;
            let idx = panel.scroll + visible_idx;
            let selected = idx == panel.selected;
            let marker = if selected { "›" } else { " " };
            let name_style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let fixed_width = 1usize
                .saturating_add(1)
                .saturating_add(UnicodeWidthStr::width(item.name.as_str()))
                .saturating_add(2)
                .saturating_add(UnicodeWidthStr::width(item.status.as_str()));
            let description_width = row_width.saturating_sub(fixed_width.saturating_add(2));
            let mut spans = vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(item.name.clone(), name_style),
                Span::raw("  "),
                Span::styled(item.status.clone(), Style::default().fg(Color::Gray)),
            ];
            if description_width > 0 {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    truncate_display_width(&item.description, description_width),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Some(Line::from(spans))
        })
        .collect::<Vec<_>>();
    let lines = if lines.is_empty() {
        vec![Line::from(vec![Span::styled(
            "no matches",
            Style::default().fg(Color::DarkGray),
        )])]
    } else {
        lines
    };
    f.render_widget(Paragraph::new(Text::from(lines)), rest);
}

fn render_skills_toggle_panel(f: &mut Frame, area: Rect, panel: &SkillsTogglePanel) {
    let subtitle = if panel.items.is_empty() {
        "No skills loaded.".to_string()
    } else {
        "Toggle automatic use for loaded skills.".to_string()
    };
    let mut rest = render_panel_title(f, area, "Skills", Some(&subtitle));
    if rest.height == 0 {
        return;
    }
    let search_line = if panel.search.is_empty() {
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "Type to search skills",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::DarkGray)),
            Span::styled(panel.search.clone(), Style::default().fg(Color::White)),
        ])
    };
    f.render_widget(Paragraph::new(search_line), Rect { height: 1, ..rest });
    rest.y = rest.y.saturating_add(1);
    rest.height = rest.height.saturating_sub(1);

    if let Some(feedback) = panel.feedback.as_ref() {
        let rows = command_feedback_row_count(Some(feedback)).min(rest.height);
        if rows > 0 {
            render_command_feedback(
                f,
                Rect {
                    height: rows,
                    ..rest
                },
                feedback,
            );
            rest.y = rest.y.saturating_add(rows);
            rest.height = rest.height.saturating_sub(rows);
        }
    }

    if rest.height == 0 {
        return;
    }
    let visible_indices = panel.visible_indices();
    let row_width = rest.width as usize;
    let lines = visible_indices
        .iter()
        .skip(panel.scroll)
        .take(rest.height as usize)
        .enumerate()
        .filter_map(|(visible_idx, actual_idx)| {
            let item = panel.items.get(*actual_idx)?;
            let idx = panel.scroll + visible_idx;
            let selected = idx == panel.selected;
            let marker = if selected { "›" } else { " " };
            let checkbox = if item.auto_use_enabled { "[x]" } else { "[ ]" };
            let name_style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let status = format!("{} - {}", item.scope, item.status_description());
            let fixed_width = 1usize
                .saturating_add(1)
                .saturating_add(UnicodeWidthStr::width(checkbox))
                .saturating_add(1)
                .saturating_add(UnicodeWidthStr::width(item.name.as_str()));
            let status_width = row_width.saturating_sub(fixed_width.saturating_add(2));
            let mut spans = vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(checkbox, Style::default().fg(Color::Gray)),
                Span::raw(" "),
                Span::styled(item.name.clone(), name_style),
            ];
            if status_width > 0 {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    truncate_display_width(&status, status_width),
                    Style::default().fg(Color::Gray),
                ));
            }
            Some(Line::from(spans))
        })
        .collect::<Vec<_>>();
    let lines = if lines.is_empty() {
        vec![Line::from(vec![Span::styled(
            "no matches",
            Style::default().fg(Color::DarkGray),
        )])]
    } else {
        lines
    };
    f.render_widget(Paragraph::new(Text::from(lines)), rest);
}

fn render_telegram_access_panel(f: &mut Frame, area: Rect, picker: &TelegramAccessPicker) {
    let rest = render_panel_title(
        f,
        area,
        picker.action.title(),
        Some(&format!(
            "Select a pending request to {}.",
            picker.action.verb()
        )),
    );
    if rest.height == 0 {
        return;
    }
    let lines = picker
        .requests
        .iter()
        .skip(picker.scroll)
        .take(rest.height as usize)
        .enumerate()
        .map(|(visible_idx, request)| {
            let idx = picker.scroll + visible_idx;
            let selected = idx == picker.selected;
            let marker = if selected { "›" } else { " " };
            let style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(
                    format!(
                        "{}  {}  {}  {}",
                        request.chat_id,
                        request.title,
                        request.sender,
                        truncate_command_text(&request.last_message_preview, 100)
                    ),
                    style,
                ),
            ])
        })
        .collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        rest,
    );
}

fn command_hint(input: &str, context: &DashboardCommandContext<'_>) -> String {
    let matches = matching_commands(input, context);
    if !is_dashboard_command_input(input) {
        if matches.len() == 1 {
            let suggestion = &matches[0];
            return format!("{} — {}", suggestion.display, suggestion.description);
        }
        if matches.len() > 1 {
            return matches
                .iter()
                .take(4)
                .map(|suggestion| suggestion.display.clone())
                .collect::<Vec<_>>()
                .join(" | ");
        }
        if input.trim().is_empty() {
            return "Enter send. Shift+Enter newline. Ctrl+P queued inputs. Prefix / for commands. Esc clear."
                .to_string();
        }
        return "Enter send. Shift+Enter newline. Prefix / for commands.".to_string();
    }
    if command_completion_body(input)
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        return "Up/Down select. Tab accept. Enter run. Shift+Enter newline. Esc clear."
            .to_string();
    }
    if matches.len() == 1 {
        let suggestion = &matches[0];
        return format!("{} — {}", suggestion.display, suggestion.description);
    }
    if matches.len() > 1 {
        return matches
            .iter()
            .take(4)
            .map(|suggestion| suggestion.display.clone())
            .collect::<Vec<_>>()
            .join(" | ");
    }
    if let Some(parts) = dashboard_command_parts(input) {
        if dashboard_parts_open_panel(&parts) {
            return "Enter open panel. Shift+Enter newline. Esc clear.".to_string();
        }
        if dashboard_parts_run_action(&parts) {
            return "Enter run action. Shift+Enter newline. Esc clear.".to_string();
        }
        if dashboard_command_is_known(parts[0]) {
            return "Enter run command. Shift+Enter newline. Esc clear.".to_string();
        }
    }
    "unknown command".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    fn trimmed_buffer_lines(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
        let mut lines = Vec::new();
        for y in 0..buffer.area.height {
            let mut line = String::new();
            for x in 0..buffer.area.width {
                if let Some(cell) = buffer.cell((x, y)) {
                    line.push_str(cell.symbol());
                }
            }
            lines.push(line.trim_end().to_string());
        }
        lines
    }

    #[test]
    fn footer_line_combines_context_queue_and_hint() {
        let line = render_footer_line("gpt-5.5 · 10k/100k used", "Enter send.", 2, 1, None, 120);
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(text.contains("gpt-5.5"));
        assert!(text.contains("2 pasted blocks queued"));
        assert!(text.contains("1 image attachment queued"));
        assert!(text.contains("Enter send."));
    }

    #[test]
    fn pending_user_input_preview_lines_show_first_inputs_without_origin() {
        let inputs = vec![crate::dashboard::DashboardPendingUserInput {
            event_id: uuid::Uuid::nil().to_string(),
            origin: "tui".to_string(),
            incoming_text: "queued follow-up".to_string(),
            arrived_at_ms: 42,
            attachment_count: 0,
        }];
        let text = pending_user_input_preview_lines(&inputs, 120)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("Queued follow-up inputs (1)"));
        assert!(text.contains("queued follow-up"));
        assert!(!text.contains("tui |"));
        assert!(!text.contains("webui |"));
    }

    #[test]
    fn command_bar_wraps_long_input_after_prompt_column() {
        let backend = TestBackend::new(10, 4);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        let state = crate::dashboard::DashboardState::default();
        let context = DashboardCommandContext {
            requests: &[],
            state: &state,
        };
        let mut last_cursor_pos = None;
        let selection = SelectionRegistry::default();
        let mut selectable_regions = Vec::new();
        let input = "abcdefghijklmnopqr";

        terminal
            .draw(|f| {
                render_command_bar(
                    f,
                    f.area(),
                    CommandBarRenderState {
                        input,
                        cursor_pos: input.len(),
                        context: &context,
                        feedback: None,
                        footer_context: "",
                        pending_paste_count: 0,
                        pending_image_attachment_count: 0,
                        pending_user_inputs: &[],
                        ctrl_c_reminder: None,
                        editing_pending_user_input: false,
                        panel: None,
                        panel_rows: 0,
                        popup_selection: 0,
                        popup_scroll: 0,
                        last_cursor_pos: &mut last_cursor_pos,
                        input_lines: 3,
                        selection: &selection,
                        selectable_regions: &mut selectable_regions,
                    },
                );
            })
            .expect("draw command bar");

        let lines = trimmed_buffer_lines(terminal.backend().buffer());
        assert_eq!(lines[0], "› abcdefgh");
        assert_eq!(
            lines[1], "  ijklmnop",
            "wrapped input continuation must stay aligned after the prompt"
        );
        assert_eq!(
            lines[2], "  qr",
            "long input tail must not be truncated after wrapping"
        );
        assert_eq!(last_cursor_pos, Some((4, 2)));
    }
    #[test]
    fn command_bar_records_cursor_overlay_position() {
        let backend = TestBackend::new(10, 4);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        let state = crate::dashboard::DashboardState::default();
        let context = DashboardCommandContext {
            requests: &[],
            state: &state,
        };
        let mut last_cursor_pos = None;
        let selection = SelectionRegistry::default();
        let mut selectable_regions = Vec::new();

        terminal
            .draw(|f| {
                render_command_bar(
                    f,
                    f.area(),
                    CommandBarRenderState {
                        input: "hello",
                        cursor_pos: 5,
                        context: &context,
                        feedback: None,
                        footer_context: "",
                        pending_paste_count: 0,
                        pending_image_attachment_count: 0,
                        pending_user_inputs: &[],
                        ctrl_c_reminder: None,
                        editing_pending_user_input: false,
                        panel: None,
                        panel_rows: 0,
                        popup_selection: 0,
                        popup_scroll: 0,
                        last_cursor_pos: &mut last_cursor_pos,
                        input_lines: 1,
                        selection: &selection,
                        selectable_regions: &mut selectable_regions,
                    },
                );
            })
            .expect("draw command bar");

        assert_eq!(last_cursor_pos, Some((7, 0)));
    }

    #[test]
    fn command_input_selection_ignores_prompt_prefix() {
        let backend = TestBackend::new(20, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        let state = crate::dashboard::DashboardState::default();
        let context = DashboardCommandContext {
            requests: &[],
            state: &state,
        };
        let mut last_cursor_pos = None;
        let selection = SelectionRegistry::default();
        let mut selectable_regions = Vec::new();

        terminal
            .draw(|f| {
                render_command_bar(
                    f,
                    f.area(),
                    CommandBarRenderState {
                        input: "hello",
                        cursor_pos: 5,
                        context: &context,
                        feedback: None,
                        footer_context: "",
                        pending_paste_count: 0,
                        pending_image_attachment_count: 0,
                        pending_user_inputs: &[],
                        ctrl_c_reminder: None,
                        editing_pending_user_input: false,
                        panel: None,
                        panel_rows: 0,
                        popup_selection: 0,
                        popup_scroll: 0,
                        last_cursor_pos: &mut last_cursor_pos,
                        input_lines: 1,
                        selection: &selection,
                        selectable_regions: &mut selectable_regions,
                    },
                );
            })
            .expect("draw command bar");

        let mut registry = SelectionRegistry::default();
        registry.set_regions(selectable_regions);
        assert!(registry.begin(0, 0));
        assert!(registry.drag_to(7, 0));

        assert_eq!(registry.selected_text().as_deref(), Some("hello"));
    }

    #[test]
    fn command_input_selection_across_wrap_copies_logical_input() {
        let backend = TestBackend::new(10, 4);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        let state = crate::dashboard::DashboardState::default();
        let context = DashboardCommandContext {
            requests: &[],
            state: &state,
        };
        let mut last_cursor_pos = None;
        let selection = SelectionRegistry::default();
        let mut selectable_regions = Vec::new();
        let input = "abcdefghijklmnopqr";

        terminal
            .draw(|f| {
                render_command_bar(
                    f,
                    f.area(),
                    CommandBarRenderState {
                        input,
                        cursor_pos: input.len(),
                        context: &context,
                        feedback: None,
                        footer_context: "",
                        pending_paste_count: 0,
                        pending_image_attachment_count: 0,
                        pending_user_inputs: &[],
                        ctrl_c_reminder: None,
                        editing_pending_user_input: false,
                        panel: None,
                        panel_rows: 0,
                        popup_selection: 0,
                        popup_scroll: 0,
                        last_cursor_pos: &mut last_cursor_pos,
                        input_lines: 3,
                        selection: &selection,
                        selectable_regions: &mut selectable_regions,
                    },
                );
            })
            .expect("draw command bar");

        let mut registry = SelectionRegistry::default();
        registry.set_regions(selectable_regions);
        assert!(registry.begin(2, 0));
        assert!(registry.drag_to(4, 2));

        assert_eq!(registry.selected_text().as_deref(), Some(input));
    }

    #[test]
    fn command_input_selection_preserves_manual_newlines() {
        let backend = TestBackend::new(20, 4);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        let state = crate::dashboard::DashboardState::default();
        let context = DashboardCommandContext {
            requests: &[],
            state: &state,
        };
        let mut last_cursor_pos = None;
        let selection = SelectionRegistry::default();
        let mut selectable_regions = Vec::new();

        terminal
            .draw(|f| {
                render_command_bar(
                    f,
                    f.area(),
                    CommandBarRenderState {
                        input: "hello\nworld",
                        cursor_pos: "hello\nworld".len(),
                        context: &context,
                        feedback: None,
                        footer_context: "",
                        pending_paste_count: 0,
                        pending_image_attachment_count: 0,
                        pending_user_inputs: &[],
                        ctrl_c_reminder: None,
                        editing_pending_user_input: false,
                        panel: None,
                        panel_rows: 0,
                        popup_selection: 0,
                        popup_scroll: 0,
                        last_cursor_pos: &mut last_cursor_pos,
                        input_lines: 2,
                        selection: &selection,
                        selectable_regions: &mut selectable_regions,
                    },
                );
            })
            .expect("draw command bar");

        let mut registry = SelectionRegistry::default();
        registry.set_regions(selectable_regions);
        assert!(registry.begin(2, 0));
        assert!(registry.drag_to(7, 1));

        assert_eq!(registry.selected_text().as_deref(), Some("hello\nworld"));
    }

    #[test]
    fn command_input_selection_uses_scrolled_visible_rows() {
        let backend = TestBackend::new(10, 2);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        let state = crate::dashboard::DashboardState::default();
        let context = DashboardCommandContext {
            requests: &[],
            state: &state,
        };
        let mut last_cursor_pos = None;
        let selection = SelectionRegistry::default();
        let mut selectable_regions = Vec::new();
        let input = "abcdefghijklmnopqr";

        terminal
            .draw(|f| {
                render_command_bar(
                    f,
                    f.area(),
                    CommandBarRenderState {
                        input,
                        cursor_pos: input.len(),
                        context: &context,
                        feedback: None,
                        footer_context: "",
                        pending_paste_count: 0,
                        pending_image_attachment_count: 0,
                        pending_user_inputs: &[],
                        ctrl_c_reminder: None,
                        editing_pending_user_input: false,
                        panel: None,
                        panel_rows: 0,
                        popup_selection: 0,
                        popup_scroll: 0,
                        last_cursor_pos: &mut last_cursor_pos,
                        input_lines: 1,
                        selection: &selection,
                        selectable_regions: &mut selectable_regions,
                    },
                );
            })
            .expect("draw command bar");

        let lines = trimmed_buffer_lines(terminal.backend().buffer());
        assert_eq!(lines[0], "  qr");

        let mut registry = SelectionRegistry::default();
        registry.set_regions(selectable_regions);
        assert!(registry.begin(2, 0));
        assert!(registry.drag_to(4, 0));

        assert_eq!(registry.selected_text().as_deref(), Some("qr"));
    }

    #[test]
    fn command_input_selection_highlight_survives_redraw() {
        let state = crate::dashboard::DashboardState::default();
        let context = DashboardCommandContext {
            requests: &[],
            state: &state,
        };
        let input = "hello";
        let mut registry = SelectionRegistry::default();
        let mut last_cursor_pos = None;
        let mut initial_regions = Vec::new();
        let backend = TestBackend::new(20, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|f| {
                render_command_bar(
                    f,
                    f.area(),
                    CommandBarRenderState {
                        input,
                        cursor_pos: input.len(),
                        context: &context,
                        feedback: None,
                        footer_context: "",
                        pending_paste_count: 0,
                        pending_image_attachment_count: 0,
                        pending_user_inputs: &[],
                        ctrl_c_reminder: None,
                        editing_pending_user_input: false,
                        panel: None,
                        panel_rows: 0,
                        popup_selection: 0,
                        popup_scroll: 0,
                        last_cursor_pos: &mut last_cursor_pos,
                        input_lines: 1,
                        selection: &SelectionRegistry::default(),
                        selectable_regions: &mut initial_regions,
                    },
                );
            })
            .expect("initial draw command bar");
        registry.set_regions(initial_regions);
        assert!(registry.begin(2, 0));
        assert!(registry.drag_to(7, 0));
        assert!(registry.end_drag());

        let mut redraw_regions = Vec::new();
        terminal
            .draw(|f| {
                render_command_bar(
                    f,
                    f.area(),
                    CommandBarRenderState {
                        input,
                        cursor_pos: input.len(),
                        context: &context,
                        feedback: None,
                        footer_context: "",
                        pending_paste_count: 0,
                        pending_image_attachment_count: 0,
                        pending_user_inputs: &[],
                        ctrl_c_reminder: None,
                        editing_pending_user_input: false,
                        panel: None,
                        panel_rows: 0,
                        popup_selection: 0,
                        popup_scroll: 0,
                        last_cursor_pos: &mut last_cursor_pos,
                        input_lines: 1,
                        selection: &registry,
                        selectable_regions: &mut redraw_regions,
                    },
                );
            })
            .expect("redraw command bar");

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(2, 0)].style().bg, Some(Color::DarkGray));
        assert_eq!(buffer[(6, 0)].style().bg, Some(Color::DarkGray));
        assert_ne!(buffer[(0, 0)].style().bg, Some(Color::DarkGray));
    }

    #[test]
    fn command_detail_panel_registers_selectable_region() {
        let panel = CommandPanel::Detail(CommandDetailPanel {
            title: "Details".to_string(),
            text: "First line\nSecond line".to_string(),
            scroll: 0,
        });
        let region = command_panel_selectable_region(&panel, Rect::new(0, 0, 40, 6))
            .expect("panel should be selectable");
        let mut registry = SelectionRegistry::default();
        registry.set_regions(vec![region]);

        assert!(registry.begin(2, 2));
        assert!(registry.drag_to(8, 2));

        assert_eq!(registry.selected_text().as_deref(), Some("First "));
    }

    #[test]
    fn footer_line_can_show_ctrl_c_interrupt_reminder() {
        let line = render_footer_line("", "Enter send.", 0, 0, Some(CtrlCReminder::Interrupt), 120);
        assert_eq!(line_text(&line), "ctrl + c again to interrupt");
    }
}
