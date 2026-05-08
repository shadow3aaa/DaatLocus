use std::time::Duration;

use ratatui::{
    buffer::Buffer,
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
};

use super::markdown::render_markdown;
use super::{
    ActivityCell, LiveActivityCell,
    apps::{AppAttentionActivityCell, BrowserActivityCell, LiveBrowserActivityCell},
    common::{
        AssistantActivityCell, ErrorActivityCell, GenericAppActivityCell, TerminalWaitActivityCell,
        ThinkingActivityCell, UserActivityCell,
    },
    exec::{ExecResultActivityCell, LiveExecActivityCell},
    highlight::{DiffScopeBackgrounds, diff_scope_backgrounds, highlight_patch_lines},
    messages::{PatchActivityCell, ReplyActivityCell, TelegramActivityCell},
    plan::{PlanActivityCell, PlanStepDisplayStatus},
    workflow::{ActivateWorkflowActivityCell, CreateWorkflowActivityCell, DeepRecallActivityCell},
};
use crate::tool_ui::{PatchDiffLineKind, PatchDiffLineUiData, PatchFileUiData, glyph};

use super::super::renderable::Renderable;

// ---------------------------------------------------------------------------
// Per-cell Renderable wrapper for viewport-culled rendering
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct ActivityCellRenderable<'a> {
    pub cell: &'a ActivityCell,
}

impl Renderable for ActivityCellRenderable<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        render_activity_cell_to_buf(self.cell, area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        desired_height(self.cell, width)
    }
}

/// Estimate the height (rows) of a cell for the given available width.
/// Overestimating is safe; underestimating may cause visible content to be skipped.
pub fn desired_height(cell: &ActivityCell, width: u16) -> u16 {
    match cell {
        ActivityCell::Assistant(c) => {
            1 + count_wrapped_lines(&c.body_lines, width.saturating_sub(5), 8) + 1
        }
        ActivityCell::User(c) => {
            1 + count_wrapped_lines(&c.body_lines, width.saturating_sub(5), 6) + 1
        }
        ActivityCell::AppAttention(c) => {
            1 + count_wrapped_lines(&c.body_lines, width.saturating_sub(5), 6) + 1
        }
        ActivityCell::Browser(c) => {
            let mut h: u16 = 1;
            let has_stats = c.line_count.is_some() || c.ref_count.is_some();
            if has_stats {
                h += 1;
            }
            h + 1
        }
        ActivityCell::LiveBrowser(c) => 1 + c.body_lines.len().min(1) as u16 + 1,
        ActivityCell::GenericApp(c) => {
            1 + count_wrapped_lines(&c.body_lines, width.saturating_sub(5), 6) + 1
        }
        ActivityCell::PlanResult(c) => 1 + c.steps.len().min(8) as u16 + 1,
        ActivityCell::CreateWorkflowResult(_) => 2,
        ActivityCell::ActivateWorkflowResult(_) => 2,
        ActivityCell::DeepRecallResult(_) => 2,
        ActivityCell::ExecResult(c) => {
            let n = if c.output_lines.is_empty() {
                1
            } else {
                c.output_lines.len().min(8)
            };
            1 + n as u16 + 1
        }
        ActivityCell::LiveExec(c) => {
            let n = if c.output_lines.is_empty() {
                1
            } else {
                c.output_lines.len().min(8)
            };
            1 + n as u16 + 1
        }
        ActivityCell::Patch(c) => {
            let n_files = c.files.len().min(4);
            let mut h: u16 = 1;
            if n_files > 0 {
                h += 1;
                for _ in 0..n_files {
                    h += 1;
                    h += 19;
                    h += 1;
                }
                h -= 1;
            }
            if c.files.len() > n_files {
                h += 1;
                h += 1;
            }
            h + 1
        }
        ActivityCell::Telegram(c) => {
            1 + c.detail_lines.len().min(6) as u16
                + count_wrapped_lines(&c.message_lines, width.saturating_sub(5), 6)
                + 1
        }
        ActivityCell::Reply(c) => {
            let msg_h = if c.message_lines.is_empty() {
                0
            } else {
                count_wrapped_lines(&c.message_lines, width.saturating_sub(3), 8)
            };
            1 + msg_h + 1
        }
        ActivityCell::TerminalWait(c) => {
            1 + count_wrapped_lines(&c.body_lines, width.saturating_sub(5), 6) + 1
        }
        ActivityCell::Error(c) => {
            1 + count_wrapped_lines(&c.body_lines, width.saturating_sub(5), 8) + 1
        }
        ActivityCell::Thinking(c) => {
            if c.expanded && c.full_body.is_some() {
                let full = c.full_body.as_deref().unwrap_or("");
                let cw = (width.saturating_sub(2)).max(20) as usize;
                let wrapped = textwrap::wrap(full, cw);
                1 + wrapped.len() as u16 + 1
            } else {
                1 + count_wrapped_lines(&c.body_lines, width.saturating_sub(2), 5) + 1
            }
        }
    }
}

fn count_wrapped_lines(lines: &[String], wrap_width: u16, limit: usize) -> u16 {
    if lines.is_empty() {
        return 0;
    }
    let w = wrap_width.max(3) as usize;
    let mut total: usize = 0;
    for line in lines.iter().take(limit) {
        let wrapped = textwrap::wrap(line, w);
        total += wrapped.len().max(1);
    }
    total as u16
}

/// Render a single cell into a buffer at the given area.
pub fn render_activity_cell_to_buf(cell: &ActivityCell, area: Rect, buf: &mut Buffer) {
    let lines = render_activity_cell_lines(cell, area.width);
    if lines.is_empty() {
        return;
    }
    let text = Text::from(lines);
    Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .render(area, buf);
}

/// Cached rendered lines to avoid rebuilding every frame.
/// Cells are replaced entirely (not mutated in-place), so comparing cell count
/// is sufficient to detect changes.
///
/// Currently retained for future per-cell markdown-line caching;
/// the viewport-culled render path uses `desired_height` for culling
/// and calls `render_activity_cell_lines` only for visible cells.
#[allow(dead_code)]
pub struct CachedActivityLines {
    pub lines: Vec<Line<'static>>,
    width: u16,
    total_cells: usize,
    expanded_count: usize,
}

#[allow(dead_code)]
impl CachedActivityLines {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            width: 0,
            total_cells: 0,
            expanded_count: 0,
        }
    }

    fn needs_rebuild(&self, inner_width: u16, total_cells: usize, expanded_count: usize) -> bool {
        self.width != inner_width
            || self.total_cells != total_cells
            || self.expanded_count != expanded_count
    }
}

/// Render the activity feed and return max_scroll for key-handling / auto-scroll.
/// Uses `cache` to avoid rebuilding all lines every frame.
pub fn render_activity_feed_cached(
    buf: &mut Buffer,
    area: Rect,
    cells: &[ActivityCell],
    live_cells: &[LiveActivityCell],
    scroll_offset: u16,
    _cache: &mut CachedActivityLines,
    _expanded_count: usize,
) -> u16 {
    let inner_width = area.width.saturating_sub(2);
    let total_cells = cells.len() + live_cells.len();

    // Compute total content height for max_scroll
    let total_height: u16 = if total_cells == 0 {
        0
    } else {
        cells
            .iter()
            .map(|c| desired_height(c, inner_width))
            .sum::<u16>()
            + live_cells
                .iter()
                .map(|c| desired_height(&c.cell, inner_width))
                .sum::<u16>()
    };

    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };

    let max_scroll = total_height.saturating_sub(inner.height.saturating_sub(1));
    let scroll = scroll_offset.min(max_scroll);

    if total_cells == 0 {
        // Render placeholder
        let placeholder =
            Paragraph::new("No activity yet").style(Style::default().fg(Color::DarkGray));
        placeholder.render(inner, buf);
    } else {
        // Viewport-culled rendering: only render cells that overlap with the visible area
        let viewport_top = scroll;
        let viewport_bottom = scroll + inner.height;
        let mut y: u16 = 0;

        // Combine committed and live cells in order
        let all_cells: Vec<&ActivityCell> = cells
            .iter()
            .chain(live_cells.iter().map(|lc| &lc.cell))
            .collect();

        for cell in &all_cells {
            let cell_h = desired_height(cell, inner_width);
            let cell_bottom = y + cell_h;

            if cell_bottom > viewport_top && y < viewport_bottom {
                // Cell overlaps with viewport – render the full cell at its screen position
                let screen_y = inner.y + y.saturating_sub(viewport_top);
                let cell_area = Rect::new(inner.x, screen_y, inner.width, cell_h);
                let clipped = cell_area.intersection(inner);
                if !clipped.is_empty() {
                    render_activity_cell_to_buf(cell, cell_area, buf);
                }
            }

            y += cell_h;
        }
    }

    max_scroll
}

fn render_activity_cell_lines(cell: &ActivityCell, max_width: u16) -> Vec<Line<'static>> {
    match cell {
        ActivityCell::Assistant(cell) => render_assistant_cell_lines(cell),
        ActivityCell::User(cell) => render_user_cell_lines(cell),
        ActivityCell::AppAttention(cell) => render_app_attention_cell_lines(cell),
        ActivityCell::Browser(cell) => render_browser_cell_lines(cell),
        ActivityCell::LiveBrowser(cell) => render_live_browser_cell_lines(cell),
        ActivityCell::GenericApp(cell) => render_generic_app_cell_lines(cell),
        ActivityCell::PlanResult(cell) => render_plan_cell_lines(cell),
        ActivityCell::CreateWorkflowResult(cell) => render_create_workflow_cell_lines(cell),
        ActivityCell::ActivateWorkflowResult(cell) => render_activate_workflow_cell_lines(cell),
        ActivityCell::DeepRecallResult(cell) => render_deep_recall_cell_lines(cell),
        ActivityCell::ExecResult(cell) => render_exec_cell_lines(cell),
        ActivityCell::LiveExec(cell) => render_live_exec_cell_lines(cell),
        ActivityCell::Patch(cell) => render_patch_cell_lines(cell),
        ActivityCell::Telegram(cell) => render_telegram_cell_lines(cell),
        ActivityCell::Reply(cell) => render_reply_cell_lines(cell),
        ActivityCell::TerminalWait(cell) => render_terminal_wait_cell_lines(cell),
        ActivityCell::Error(cell) => render_error_cell_lines(cell),
        ActivityCell::Thinking(cell) => render_thinking_cell_lines(cell, max_width),
    }
}

fn render_assistant_cell_lines(cell: &AssistantActivityCell) -> Vec<Line<'static>> {
    render_text_activity_lines(
        "›",
        Color::Cyan,
        &cell.title,
        &cell.body_lines,
        8,
        None,
        true,
    )
}

fn render_thinking_cell_lines(cell: &ThinkingActivityCell, max_width: u16) -> Vec<Line<'static>> {
    let bar = Span::styled("│", Style::default().fg(Color::DarkGray));
    let mut lines = Vec::new();

    // First line: │ Thinking [Ctrl+T]
    let mut title_spans = vec![
        bar.clone(),
        Span::raw(" "),
        Span::styled(
            cell.title.clone(),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if cell.full_body.is_some() {
        title_spans.push(Span::raw("  "));
        title_spans.push(Span::styled(
            "[Ctrl+T]",
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.push(Line::from(title_spans));

    // Body lines: │ content
    let content_width = (max_width.saturating_sub(2)).max(20) as usize;
    if cell.expanded {
        // Expanded: show full reasoning content
        if let Some(ref full) = cell.full_body {
            for body_line in full.lines() {
                let wrapped = textwrap::wrap(body_line, content_width);
                for sub_line in &wrapped {
                    lines.push(Line::from(vec![
                        bar.clone(),
                        Span::raw(" "),
                        Span::styled(sub_line.to_string(), Style::default().fg(Color::Gray)),
                    ]));
                }
            }
        }
    } else {
        // Collapsed: show truncated preview (first 5 lines)
        for body_line in cell.body_lines.iter().take(5) {
            let wrapped = textwrap::wrap(body_line, content_width);
            for sub_line in &wrapped {
                lines.push(Line::from(vec![
                    bar.clone(),
                    Span::raw(" "),
                    Span::styled(sub_line.to_string(), Style::default().fg(Color::Gray)),
                ]));
            }
        }
    }
    lines
}

fn render_user_cell_lines(cell: &UserActivityCell) -> Vec<Line<'static>> {
    render_text_activity_lines(
        glyph::EXEC,
        Color::Green,
        &cell.title,
        &cell.body_lines,
        6,
        None,
        true,
    )
}

fn render_generic_app_cell_lines(cell: &GenericAppActivityCell) -> Vec<Line<'static>> {
    render_text_activity_lines(
        glyph::EXEC,
        Color::LightGreen,
        &format!("App: {}", cell.title),
        &[],
        0,
        None,
        false,
    )
}

fn render_terminal_wait_cell_lines(cell: &TerminalWaitActivityCell) -> Vec<Line<'static>> {
    render_wait_activity_lines(&cell.title, &cell.body_lines, 6)
}

fn render_error_cell_lines(cell: &ErrorActivityCell) -> Vec<Line<'static>> {
    render_error_lines(&cell.title, &cell.body_lines, 12)
}

fn render_app_attention_cell_lines(cell: &AppAttentionActivityCell) -> Vec<Line<'static>> {
    render_text_activity_lines(
        glyph::APP_ATTENTION,
        Color::LightBlue,
        &cell.title,
        &cell.body_lines,
        6,
        None,
        false,
    )
}

fn render_browser_cell_lines(cell: &BrowserActivityCell) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            glyph::BROWSER,
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!(
                "Captured URL: {}",
                cell.url
                    .as_deref()
                    .map(compact_browser_url)
                    .unwrap_or_else(|| "unknown".to_string())
            ),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    let mut stats = Vec::new();
    if let Some(line_count) = cell.line_count {
        stats.push(format!("{line_count} lines"));
    }
    if let Some(ref_count) = cell.ref_count {
        stats.push(format!("{ref_count} refs"));
    }
    if !stats.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(stats.join(" · "), Style::default().fg(Color::Gray)),
        ]));
    }
    lines
}

fn render_live_browser_cell_lines(cell: &LiveBrowserActivityCell) -> Vec<Line<'static>> {
    let title = cell
        .url
        .as_deref()
        .map(|url| format!("Opening URL: {}", compact_browser_url(url)))
        .unwrap_or_else(|| cell.title.clone());
    let mut lines = vec![Line::from(vec![
        Span::styled(
            glyph::BROWSER,
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            title,
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    for line in cell.body_lines.iter().take(1) {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(line.clone(), Style::default().fg(Color::Gray)),
        ]));
    }
    lines
}

fn render_exec_cell_lines(cell: &ExecResultActivityCell) -> Vec<Line<'static>> {
    let exit_code = cell.meta.as_deref().and_then(parse_exit_code_from_meta);
    let indicator_style = if exit_code == Some(0) {
        Style::default()
            .fg(Color::LightGreen)
            .add_modifier(Modifier::BOLD)
    } else if exit_code.is_some() {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(glyph::EXEC, indicator_style),
        Span::raw("  "),
        Span::styled(
            "Ran",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(cell.title.clone(), Style::default().fg(Color::White)),
    ])];
    let rendered_output = if cell.output_lines.is_empty() {
        vec!["(no output)".to_string()]
    } else {
        truncate_lines_middle(&cell.output_lines, 4, 4)
    };
    for line in rendered_output {
        let style = if line.starts_with("… +") {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(vec![
            Span::styled("  ", Style::default().fg(Color::DarkGray)),
            Span::styled(line, style),
        ]));
    }
    lines
}

fn render_live_exec_cell_lines(cell: &LiveExecActivityCell) -> Vec<Line<'static>> {
    let elapsed = cell.started_at_ms.and_then(|started_at_ms| {
        let now_ms = current_time_ms();
        if now_ms >= started_at_ms {
            Some(Duration::from_millis((now_ms - started_at_ms) as u64))
        } else {
            None
        }
    });
    let mut lines = vec![Line::from(vec![
        Span::styled(
            exec_spinner(elapsed),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            "Running",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(cell.title.clone(), Style::default().fg(Color::White)),
    ])];
    if cell.output_lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  ", Style::default().fg(Color::DarkGray)),
            Span::styled("running...", Style::default().fg(Color::DarkGray)),
        ]));
    }
    let rendered_output = truncate_lines_middle(&cell.output_lines, 4, 4);
    for line in rendered_output {
        let style = if line.starts_with("… +") {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(vec![
            Span::styled("  ", Style::default().fg(Color::DarkGray)),
            Span::styled(line, style),
        ]));
    }
    lines
}

fn render_patch_cell_lines(cell: &PatchActivityCell) -> Vec<Line<'static>> {
    let visible_files = limit_patch_files(&cell.files, 4);
    let diff_backgrounds = diff_scope_backgrounds();
    let old_lineno_width = visible_files
        .iter()
        .flat_map(|file| file.diff_lines.iter().filter_map(|line| line.old_lineno))
        .max()
        .unwrap_or(0)
        .to_string()
        .len()
        .max(1);
    let new_lineno_width = visible_files
        .iter()
        .flat_map(|file| file.diff_lines.iter().filter_map(|line| line.new_lineno))
        .max()
        .unwrap_or(0)
        .to_string()
        .len()
        .max(1);
    let file_noun = if cell.files.len() == 1 {
        "File"
    } else {
        "Files"
    };

    let mut lines = vec![Line::from(vec![
        Span::styled(
            glyph::PATCH,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("Edited {} {}", cell.files.len(), file_noun),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    if !visible_files.is_empty() {
        lines.push(Line::from(""));
    }
    for (index, file) in visible_files.iter().enumerate() {
        if index > 0 {
            lines.push(Line::from(""));
        }
        lines.push(render_patch_file_header(file));
        lines.extend(render_patch_file_diff_lines(
            file,
            diff_backgrounds,
            old_lineno_width,
            new_lineno_width,
            18,
        ));
    }
    if cell.files.len() > visible_files.len() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            format!("… {} more files", cell.files.len() - visible_files.len()),
            Style::default().fg(Color::DarkGray),
        )]));
    }
    lines
}

fn render_telegram_cell_lines(cell: &TelegramActivityCell) -> Vec<Line<'static>> {
    render_message_activity_lines(
        glyph::TELEGRAM,
        Color::Cyan,
        &cell.title,
        &cell.detail_lines,
        &cell.message_lines,
        6,
        6,
        false,
    )
}

fn render_reply_cell_lines(cell: &ReplyActivityCell) -> Vec<Line<'static>> {
    let (title, color) = match cell.disposition {
        crate::tool_ui::ReplyDisposition::Resolved => {
            (resolved_reply_title(cell), Color::LightGreen)
        }
        crate::tool_ui::ReplyDisposition::Dismissed => ("Dismissed", Color::DarkGray),
        crate::tool_ui::ReplyDisposition::Failed => ("Failed", Color::Red),
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            glyph::REPLY,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            title,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ])];
    if !cell.message_lines.is_empty() {
        let joined = cell
            .message_lines
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let md_lines = render_markdown(&joined, Color::White);
        lines.extend(md_lines);
    }
    lines
}

fn resolved_reply_title(cell: &ReplyActivityCell) -> &'static str {
    match cell.subject {
        crate::tool_ui::ReplySubject::Message => "Resolved Message",
        crate::tool_ui::ReplySubject::Notice => "Resolved Notice",
    }
}

fn render_plan_cell_lines(cell: &PlanActivityCell) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            glyph::PLAN,
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            "Plan",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    for step in cell.steps.iter().take(8) {
        let (marker, marker_style, text_style) = match step.status {
            PlanStepDisplayStatus::InProgress => (
                "●",
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            PlanStepDisplayStatus::Pending => (
                "○",
                Style::default().fg(Color::DarkGray),
                Style::default().fg(Color::Gray),
            ),
            PlanStepDisplayStatus::Completed => (
                "●",
                Style::default()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(Color::LightGreen),
            ),
        };
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(marker, marker_style),
            Span::raw(" "),
            Span::styled(step.text.clone(), text_style),
        ]));
    }
    lines
}

fn render_create_workflow_cell_lines(cell: &CreateWorkflowActivityCell) -> Vec<Line<'static>> {
    render_workflow_line(
        format!("Created Workflow: {}", cell.workflow_id),
        glyph::WORKFLOW,
    )
}

fn render_activate_workflow_cell_lines(cell: &ActivateWorkflowActivityCell) -> Vec<Line<'static>> {
    render_workflow_line(
        format!("Activated Workflow: {}", cell.workflow_id),
        glyph::WORKFLOW,
    )
}

fn render_deep_recall_cell_lines(cell: &DeepRecallActivityCell) -> Vec<Line<'static>> {
    render_workflow_line(
        format!("Recalled {} Memories", cell.memory_count),
        glyph::MEMORY,
    )
}

fn render_workflow_line(title: String, marker: &str) -> Vec<Line<'static>> {
    vec![Line::from(vec![
        Span::styled(
            marker.to_string(),
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            title,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])]
}

fn render_text_activity_lines(
    marker: &str,
    accent: Color,
    title: &str,
    body_lines: &[String],
    limit: usize,
    extra_prefix: Option<&str>,
    markdown: bool,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            marker.to_string(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            title.to_string(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
    ])];

    let ep = extra_prefix.unwrap_or("");

    if markdown && !body_lines.is_empty() {
        let joined = body_lines
            .iter()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let md_lines = render_markdown(&joined, Color::Gray);
        for md_line in md_lines {
            let mut spans: Vec<Span<'static>> = vec![Span::raw("   ")];
            if !ep.is_empty() {
                spans.push(Span::styled(
                    ep.to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            spans.extend(md_line.spans);
            lines.push(Line::from(spans));
        }
    } else {
        for line in body_lines.iter().take(limit) {
            let mut spans: Vec<Span<'static>> = vec![Span::raw("   ")];
            if !ep.is_empty() {
                spans.push(Span::styled(
                    ep.to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            spans.push(Span::styled(
                line.to_string(),
                Style::default().fg(Color::Gray),
            ));
            lines.push(Line::from(spans));
        }
    }
    lines
}

#[allow(clippy::too_many_arguments)]
fn render_message_activity_lines(
    marker: &str,
    accent: Color,
    title: &str,
    detail_lines: &[String],
    message_lines: &[String],
    detail_limit: usize,
    message_limit: usize,
    markdown: bool,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            marker.to_string(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            title.to_string(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
    ])];
    for line in detail_lines.iter().take(detail_limit) {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(line.to_string(), Style::default().fg(Color::Gray)),
        ]));
    }

    if markdown && !message_lines.is_empty() {
        let joined = message_lines
            .iter()
            .take(message_limit)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let md_lines = render_markdown(&joined, Color::White);
        for (index, md_line) in md_lines.into_iter().enumerate() {
            let mut msg_spans = vec![Span::styled(
                if index == 0 { "  └ " } else { "    " },
                Style::default().fg(Color::DarkGray),
            )];
            msg_spans.extend(md_line.spans);
            lines.push(Line::from(msg_spans));
        }
    } else {
        for (index, line) in message_lines.iter().take(message_limit).enumerate() {
            let mut msg_spans = vec![Span::styled(
                if index == 0 { "  └ " } else { "    " },
                Style::default().fg(Color::DarkGray),
            )];
            msg_spans.push(Span::styled(
                line.to_string(),
                Style::default().fg(Color::White),
            ));
            lines.push(Line::from(msg_spans));
        }
    }
    lines
}

fn render_wait_activity_lines(
    title: &str,
    body_lines: &[String],
    limit: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            glyph::EXEC,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    for line in body_lines.iter().take(limit) {
        lines.push(Line::from(vec![
            Span::styled("  └ ", Style::default().fg(Color::DarkGray)),
            Span::styled(line.to_string(), Style::default().fg(Color::Gray)),
        ]));
    }
    lines
}

fn render_error_lines(title: &str, body_lines: &[String], limit: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            glyph::ERROR,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            title.to_string(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    ])];
    for line in body_lines.iter().take(limit) {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(line.to_string(), Style::default().fg(Color::LightRed)),
        ]));
    }
    lines
}

fn limit_patch_files(files: &[PatchFileUiData], limit: usize) -> Vec<PatchFileUiData> {
    if files.len() <= limit {
        return files.to_vec();
    }
    files.iter().take(limit).cloned().collect()
}

fn render_patch_file_header(file: &PatchFileUiData) -> Line<'static> {
    let mut spans = Vec::new();
    spans.push(Span::styled(
        file.path.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled("(", Style::default().fg(Color::DarkGray)));
    spans.push(Span::styled(
        format!("+{}", file.added_lines),
        Style::default().fg(Color::LightGreen),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("-{}", file.removed_lines),
        Style::default().fg(Color::LightRed),
    ));
    spans.push(Span::styled(")", Style::default().fg(Color::DarkGray)));
    Line::from(spans)
}

fn render_patch_file_diff_lines(
    file: &PatchFileUiData,
    diff_backgrounds: DiffScopeBackgrounds,
    old_lineno_width: usize,
    new_lineno_width: usize,
    line_limit: usize,
) -> Vec<Line<'static>> {
    let highlighted = highlight_patch_lines(&file.path, &file.diff_lines);
    let visible_lines = file.diff_lines.iter().take(line_limit).collect::<Vec<_>>();
    let mut lines = visible_lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            render_patch_diff_line(
                line,
                highlighted
                    .get(index)
                    .and_then(|spans| spans.as_ref())
                    .cloned(),
                diff_backgrounds,
                old_lineno_width,
                new_lineno_width,
            )
        })
        .collect::<Vec<_>>();
    if file.diff_lines.len() > visible_lines.len() {
        lines.push(Line::from(vec![
            Span::styled("…", Style::default().fg(Color::DarkGray)),
            Span::raw(" "),
            Span::styled(
                format!(
                    "{} more line(s)",
                    file.diff_lines.len() - visible_lines.len()
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    lines
}

fn render_patch_diff_line(
    line: &PatchDiffLineUiData,
    highlighted_spans: Option<Vec<Span<'static>>>,
    diff_backgrounds: DiffScopeBackgrounds,
    old_lineno_width: usize,
    new_lineno_width: usize,
) -> Line<'static> {
    if matches!(line.kind, PatchDiffLineKind::HunkBreak) {
        return Line::from(vec![Span::styled(
            format!(
                "{:>old_width$} {:>new_width$} ⋮",
                "",
                "",
                old_width = old_lineno_width,
                new_width = new_lineno_width
            ),
            Style::default().fg(Color::DarkGray),
        )]);
    }

    let (gutter, text_style, background) = match line.kind {
        PatchDiffLineKind::Context => (" ", Style::default().fg(Color::Gray), None),
        PatchDiffLineKind::Delete => (
            "-",
            Style::default().fg(Color::LightRed),
            diff_backgrounds.deleted.or(Some(Color::Rgb(58, 24, 24))),
        ),
        PatchDiffLineKind::Add => (
            "+",
            Style::default().fg(Color::LightGreen),
            diff_backgrounds.inserted.or(Some(Color::Rgb(22, 44, 30))),
        ),
        PatchDiffLineKind::HunkBreak => unreachable!(),
    };
    let old_lineno = line
        .old_lineno
        .map(|lineno| lineno.to_string())
        .unwrap_or_default();
    let new_lineno = line
        .new_lineno
        .map(|lineno| lineno.to_string())
        .unwrap_or_default();

    let mut spans = vec![
        Span::styled(
            format!("{old_lineno:>old_width$}", old_width = old_lineno_width),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{new_lineno:>new_width$}", new_width = new_lineno_width),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(gutter, text_style.add_modifier(Modifier::BOLD)),
        Span::raw(" "),
    ];
    if let Some(highlighted_spans) = highlighted_spans {
        for span in highlighted_spans {
            let style = match background {
                Some(color) => span.style.bg(color),
                None => span.style,
            };
            spans.push(Span::styled(span.content.to_string(), style));
        }
    } else {
        let style = match background {
            Some(color) => text_style.bg(color),
            None => text_style,
        };
        spans.push(Span::styled(line.text.clone(), style));
    }
    Line::from(spans)
}

fn compact_browser_url(url: &str) -> String {
    const MAX_CHARS: usize = 88;
    let compact = url.trim().replace('\n', "");
    let mut chars = compact.chars();
    let head = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{head}...")
    } else {
        head
    }
}

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn exec_spinner(elapsed: Option<Duration>) -> String {
    const FRAMES: &[&str] = &[glyph::EXEC, "◦", "▪", "◦"];
    let index = elapsed
        .map(|duration| ((duration.as_millis() / 200) as usize) % FRAMES.len())
        .unwrap_or(0);
    FRAMES[index].to_string()
}

fn parse_exit_code_from_meta(meta: &str) -> Option<i32> {
    let exit = meta
        .split_whitespace()
        .find_map(|part| part.strip_prefix("exit="))?;
    exit.parse::<i32>().ok()
}

fn truncate_lines_middle(lines: &[String], head: usize, tail: usize) -> Vec<String> {
    if lines.len() <= head + tail + 1 {
        return lines.to_vec();
    }
    let omitted = lines.len().saturating_sub(head + tail);
    let mut result = Vec::new();
    result.extend(lines.iter().take(head).cloned());
    result.push(format!("… +{omitted} more line(s)"));
    result.extend(lines.iter().skip(lines.len().saturating_sub(tail)).cloned());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_ui::{PatchDiffLineKind, PatchFileOperation, ReplyDisposition, ReplySubject};

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn patch_activity_cell_renders_diff_lines() {
        let cell = PatchActivityCell {
            summary_line: "1 file(s) changed (+1 -1)".to_string(),
            files: vec![PatchFileUiData {
                path: "src/app.rs".to_string(),
                operation: PatchFileOperation::Update,
                added_lines: 1,
                removed_lines: 1,
                diff_lines: vec![
                    PatchDiffLineUiData {
                        kind: PatchDiffLineKind::Context,
                        old_lineno: Some(1),
                        new_lineno: Some(1),
                        text: "fn main() {".to_string(),
                    },
                    PatchDiffLineUiData {
                        kind: PatchDiffLineKind::Delete,
                        old_lineno: Some(2),
                        new_lineno: None,
                        text: "    println!(\"old\");".to_string(),
                    },
                    PatchDiffLineUiData {
                        kind: PatchDiffLineKind::Add,
                        old_lineno: None,
                        new_lineno: Some(2),
                        text: "    println!(\"new\");".to_string(),
                    },
                ],
            }],
        };

        let lines = render_patch_cell_lines(&cell);
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>();

        assert!(
            rendered
                .iter()
                .any(|line| line.contains("∂  Edited 1 File"))
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("src/app.rs (+1 -1)"))
        );
        assert!(rendered.iter().any(|line| line.contains("1 1   fn main()")));
        assert!(rendered.iter().any(|line| line.contains("2   -")));
        assert!(rendered.iter().any(|line| line.contains(" 2 +")));
    }

    #[test]
    fn reply_activity_cell_labels_notice_subjects() {
        let notice = render_reply_cell_lines(&ReplyActivityCell {
            disposition: ReplyDisposition::Resolved,
            subject: ReplySubject::Notice,
            message_lines: Vec::new(),
        })
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();
        assert!(notice.iter().any(|line| line.contains("Resolved Notice")));
    }
}
