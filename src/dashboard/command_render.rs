use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use super::command_flow::{
    command_completion_body, dashboard_command_parts, dashboard_parts_open_panel,
    dashboard_parts_run_action, is_dashboard_command_input, matching_commands,
    selected_command_completion,
};
use super::command_input::{command_input_display_text, cursor_display_row, cursor_display_xy};
use super::command_panels::{
    CommandDetailPanel, CommandFeedback, CommandFeedbackLevel, CommandPanel, CommandSelectionPanel,
    DashboardCommandContext, SkillsListPanel, SkillsTogglePanel,
    TELEGRAM_ACCESS_PICKER_VISIBLE_ROWS, TelegramAccessPicker,
};
use super::command_registry::dashboard_command_is_known;
use super::command_text::{truncate_command_text, truncate_display_width};

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
        }
    }
}

pub(super) struct CommandBarRenderState<'a> {
    pub(super) input: &'a str,
    pub(super) cursor_pos: usize,
    pub(super) context: &'a DashboardCommandContext<'a>,
    pub(super) feedback: Option<&'a CommandFeedback>,
    pub(super) runtime_status: Option<&'a str>,
    pub(super) footer_context: &'a str,
    pub(super) panel: Option<&'a CommandPanel>,
    pub(super) panel_rows: u16,
    pub(super) popup_selection: usize,
    pub(super) popup_scroll: usize,
    pub(super) last_cursor_pos: &'a mut Option<(u16, u16)>,
    pub(super) input_lines: u16,
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
    let base_rows = 2u16
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
    }
}

pub(super) fn render_command_bar(f: &mut Frame, area: Rect, state: CommandBarRenderState<'_>) {
    let CommandBarRenderState {
        input,
        cursor_pos,
        input_lines,
        context,
        feedback,
        runtime_status,
        footer_context,
        panel,
        panel_rows,
        popup_selection,
        popup_scroll,
        last_cursor_pos,
    } = state;

    let completion = if panel.is_none() {
        selected_command_completion(input, 0, context)
    } else {
        None
    };
    let hint = command_hint(input, context);
    let popup_rows = if panel.is_some() {
        0
    } else {
        command_popup_row_count(input, context)
    };
    let feedback_rows = if panel.is_some() {
        0
    } else {
        command_feedback_row_count(feedback)
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints({
            let mut c = Vec::new();
            if panel_rows > 0 {
                c.push(Constraint::Length(panel_rows));
            }
            c.push(Constraint::Length(1));
            if feedback_rows > 0 {
                c.push(Constraint::Length(feedback_rows));
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
        render_command_panel(f, rows[row_index], panel);
        row_index += 1;
    }
    let status_line = match runtime_status {
        Some("Working") => render_working_status_line(),
        Some(status) if !status.trim().is_empty() => Line::from(vec![Span::styled(
            status.to_string(),
            Style::default().fg(Color::DarkGray),
        )]),
        _ => Line::from(""),
    };
    f.render_widget(Paragraph::new(status_line), rows[row_index]);
    row_index += 1;
    if let Some(feedback) = feedback
        && feedback_rows > 0
    {
        render_command_feedback(f, rows[row_index], feedback);
        row_index += 1;
    }
    let input_row_index = row_index;
    let available_width = area.width.saturating_sub(2).max(1) as usize;
    let cursor_total_row = cursor_display_row(input, cursor_pos, available_width);
    let input_scroll =
        cursor_total_row.saturating_sub(rows[input_row_index].height.saturating_sub(1));
    let input_para = Paragraph::new(command_input_display_text(input, completion.as_deref()))
        .wrap(Wrap { trim: false })
        .scroll((input_scroll, 0));
    f.render_widget(input_para, rows[input_row_index]);

    let (cursor_x, cursor_y) = cursor_display_xy(
        input,
        cursor_pos,
        available_width,
        2,
        rows[input_row_index],
        input_scroll,
    );
    f.set_cursor_position(Position {
        x: cursor_x,
        y: cursor_y,
    });
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
    let footer_line = if let Some(panel) = panel {
        Line::from(vec![
            Span::styled("panel", Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(panel.footer_hint(), Style::default().fg(Color::DarkGray)),
        ])
    } else if !footer_context.trim().is_empty() {
        Line::from(vec![Span::styled(
            footer_context.to_string(),
            Style::default().fg(Color::DarkGray),
        )])
    } else {
        Line::from(vec![
            Span::styled("hint", Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(hint, Style::default().fg(Color::DarkGray)),
        ])
    };
    f.render_widget(Paragraph::new(footer_line), footer_row);
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

fn render_working_status_line() -> Line<'static> {
    let frame = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        / 200) as usize
        % 4;
    let glyph = ["•", "◦", "▪", "◦"][frame];
    Line::from(vec![
        Span::styled(
            glyph,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            "Working",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn command_hint(input: &str, context: &DashboardCommandContext<'_>) -> String {
    if !is_dashboard_command_input(input) {
        if input.trim().is_empty() {
            return "Enter send. Shift+Enter newline. Prefix / for commands. Esc clear."
                .to_string();
        }
        return "Enter send. Shift+Enter newline. Prefix / for commands.".to_string();
    }
    let matches = matching_commands(input, context);
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
