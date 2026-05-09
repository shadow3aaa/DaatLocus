use std::time::Duration;

use ratatui::{
    buffer::Buffer,
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
};

use super::markdown::render_markdown;
use crate::dashboard::renderable::{FlexRenderable, Renderable, ViewportCulledColumn};
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

// ---------------------------------------------------------------------------
// Viewport-culled rendering
// ---------------------------------------------------------------------------

/// Cached pre-rendered lines per cell, keyed by index and width.
/// Avoids re-running expensive markdown rendering every frame.
pub struct CachedActivityLines {
    entries: Vec<Option<CacheEntry>>,
}

struct CacheEntry {
    width: u16,
    lines: Vec<Line<'static>>,
}

impl CachedActivityLines {
    pub fn new() -> Self {
        Self { entries: vec![] }
    }

#[allow(dead_code)]
    /// Drop all cached entries (e.g. after expand/collapse toggle).
    pub fn invalidate(&mut self) {
        self.entries.clear();
    }

    /// Make room for at least `count` cells.
    fn ensure_capacity(&mut self, count: usize) {
        if self.entries.len() < count {
            self.entries.resize_with(count, || None);
        }
    }

    /// Return cached lines for cell `index` at `width`, if available.
    fn get(&self, index: usize, width: u16) -> Option<&Vec<Line<'static>>> {
        self.entries.get(index).and_then(|e| {
            e.as_ref().and_then(|entry| {
                if entry.width == width {
                    Some(&entry.lines)
                } else {
                    None
                }
            })
        })
    }

    /// Store rendered lines for cell `index`.
    fn set(&mut self, index: usize, width: u16, lines: Vec<Line<'static>>) {
        if index >= self.entries.len() {
            self.entries.resize_with(index + 1, || None);
        }
        self.entries[index] = Some(CacheEntry { width, lines });
    }
}

/// Thin Renderable wrapper around pre-computed lines.
#[derive(Clone)]
struct CachedCellLines(Vec<Line<'static>>);

impl Renderable for CachedCellLines {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if self.0.is_empty() {
            return;
        }
        Paragraph::new(Text::from(self.0.clone()))
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        self.0.len() as u16
    }
}

/// Render the activity feed through the layout tree and return `max_scroll`.
///
/// Builds a `ViewportCulledColumn` of pre-rendered `CachedCellLines` entries
/// (using `cache` to skip repeated markdown rendering), then delegates to
/// `render_with_scroll` for viewport-culled drawing.
pub fn render_activity_feed_cached(
    buf: &mut Buffer,
    area: Rect,
    cells: &[ActivityCell],
    live_cells: &[LiveActivityCell],
    scroll_offset: u16,
    cache: &mut CachedActivityLines,
    _expanded_count: usize,
) -> u16 {
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };

    let total_cells = cells.len() + live_cells.len();

    if total_cells == 0 {
        let placeholder = Paragraph::new("No activity yet")
            .style(Style::default().fg(Color::DarkGray));
        placeholder.render(inner, buf);
        return 0;
    }

    cache.ensure_capacity(total_cells);

    let mut column = ViewportCulledColumn::new();

    let spacer_line = CachedCellLines(vec![Line::from("")]);

    // Committed cells: use cache to skip markdown re-render.
    for (i, cell) in cells.iter().enumerate() {
        let lines = if let Some(cached) = cache.get(i, inner.width) {
            cached.clone()
        } else {
            let lines = render_activity_cell_lines(cell, inner.width);
            cache.set(i, inner.width, lines.clone());
            lines
        };
        column.push(CachedCellLines(lines));
        // Blank line spacing between adjacent cells (matches old Vec<Line> behavior).
        if !live_cells.is_empty() || i + 1 < cells.len() {
            column.push(spacer_line.clone());
        }
    }

    // Live cells are always re-rendered (they change every frame).
    for (i, lc) in live_cells.iter().enumerate() {
        let idx = cells.len() + i;
        let lines = render_activity_cell_lines(&lc.cell, inner.width);
        // Still cache for consistency (the next frame may hit cache if cell stabilizes).
        if let Some(cached) = cache.get(idx, inner.width) {
            if cached.len() == lines.len() {
                // Reuse cached if same structure; live cells rarely change shape.
            }
        }
        cache.set(idx, inner.width, lines.clone());
        column.push(CachedCellLines(lines));
        // Blank line spacing between adjacent cells.
        if i + 1 < live_cells.len() {
            column.push(spacer_line.clone());
        }
    }

    // Auto-scroll (u16::MAX): precompute total height and pin to bottom.
    let effective_scroll = if scroll_offset == u16::MAX {
        let total = column.desired_height(inner.width);
        total.saturating_sub(inner.height)
    } else {
        scroll_offset
    };
    column.set_scroll(effective_scroll);

    let mut flex = FlexRenderable::new();
    flex.push(1, column);
    flex.render(inner, buf);

    let total = flex.desired_height(inner.width);
    total.saturating_sub(inner.height.saturating_sub(1))
}

// ---------------------------------------------------------------------------
// Renderable impl for ActivityCell
// ---------------------------------------------------------------------------

impl Renderable for ActivityCell {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let lines = render_activity_cell_lines(self, area.width);
        if lines.is_empty() {
            return;
        }
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        // conservative estimate: overestimation is safe for viewport culling
        match self {
            ActivityCell::Assistant(c) => 3 + (c.body_lines.len() as u16).min(40),
            ActivityCell::User(c) => 3 + (c.body_lines.len() as u16).min(20),
            ActivityCell::Thinking(c) => 3 + (c.body_lines.len() as u16).min(20),
            ActivityCell::GenericApp(c) => 3 + (c.body_lines.len() as u16).min(10),
            ActivityCell::TerminalWait(c) => 3 + (c.body_lines.len() as u16).min(10),
            ActivityCell::Error(c) => 3 + (c.body_lines.len() as u16).min(10),
            ActivityCell::AppAttention(c) => 3 + (c.body_lines.len() as u16).min(10),
            ActivityCell::Browser(_) => 8,
            ActivityCell::LiveBrowser(_) => 12,
            ActivityCell::ExecResult(c) => 3 + (c.output_lines.len() as u16).min(20),
            ActivityCell::LiveExec(c) => 3 + (c.output_lines.len() as u16).min(20),
            ActivityCell::Patch(c) => {
                let file_count = c.files.len() as u16;
                3 + file_count * 6
            }
            ActivityCell::Telegram(c) => 3 + (c.message_lines.len() as u16).min(20),
            ActivityCell::Reply(c) => 3 + (c.message_lines.len() as u16).min(20),
            ActivityCell::PlanResult(c) => 3 + (c.steps.len() as u16).min(20),
            ActivityCell::CreateWorkflowResult(_) => 3,
            ActivityCell::ActivateWorkflowResult(_) => 3,
            ActivityCell::DeepRecallResult(_) => 3,
        }
    }
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
