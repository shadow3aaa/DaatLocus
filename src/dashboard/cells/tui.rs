use ratatui::{
    buffer::Buffer,
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Clear, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthChar;

use super::markdown::render_markdown_with_width;
use super::{
    ActivityCell, LiveActivityCell,
    apps::{BrowserActivityCell, LiveBrowserActivityCell, WebSearchActivityCell},
    common::{
        AssistantActivityCell, CodingEditActivityCell, CodingOpenProjectActivityCell,
        CodingReviewActivityCell, ErrorActivityCell, ExploredActivityCell,
        ExploredCallActivityCell, FinalMessageSeparatorActivityCell, GenericAppActivityCell,
        MessageImageAttachment, TerminalWaitActivityCell, ThinkingActivityCell, UserActivityCell,
    },
    exec::{ExecResultActivityCell, LiveExecActivityCell, TerminalExecutionMeta},
    highlight::{
        DiffScopeBackgrounds, diff_scope_backgrounds, highlight_patch_lines,
        highlight_shell_command,
    },
    messages::{PatchActivityCell, ReplyActivityCell, TelegramActivityCell},
    plan::{PlanActivityCell, PlanStepDisplayStatus},
    primitive::{ActivatePrimitiveActivityCell, CreatePrimitiveSpecActivityCell},
};
use crate::dashboard::renderable::{FlexRenderable, Renderable, ViewportCulledColumn};
use crate::tool_ui::{
    ExploredCallUiAction, PatchDiffLineKind, PatchDiffLineUiData, PatchFileUiData, PlanUiKind,
    TerminalUiAction, TerminalUiOrigin, WebSearchUiAction,
};

const ACTIVITY_TITLE_GAP: &str = " ";
const ACTIVITY_BODY_INDENT: &str = "  ";
const ACTIVITY_BULLET: &str = "•";
const USER_PROMPT_PREFIX: &str = "›";
const DETAIL_INITIAL_PREFIX: &str = "  └ ";
const DETAIL_SUBSEQUENT_PREFIX: &str = "    ";
const COMMAND_CONTINUATION_PREFIX: &str = "  │ ";
const PATCH_DIFF_ADD_BACKGROUND: Color = Color::Rgb(20, 74, 42);
const PATCH_DIFF_DELETE_BACKGROUND: Color = Color::Rgb(88, 34, 38);

// ---------------------------------------------------------------------------
// Viewport-culled rendering
// ---------------------------------------------------------------------------

/// Cached pre-rendered lines per cell, keyed by index and width.
/// Avoids re-running expensive markdown rendering every frame.
pub struct CachedActivityLines {
    entries: Vec<Option<CacheEntry>>,
    #[cfg(feature = "tui-perf-cmd")]
    hits: u64,
    #[cfg(feature = "tui-perf-cmd")]
    misses: u64,
}

struct CacheEntry {
    width: u16,
    cell: ActivityCell,
    lines: Vec<Line<'static>>,
    height: u16,
}

#[cfg(feature = "tui-perf-cmd")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CachedActivityLinesStats {
    pub entries: usize,
    pub occupied_entries: usize,
    pub hits: u64,
    pub misses: u64,
}

impl CachedActivityLines {
    pub fn new() -> Self {
        Self {
            entries: vec![],
            #[cfg(feature = "tui-perf-cmd")]
            hits: 0,
            #[cfg(feature = "tui-perf-cmd")]
            misses: 0,
        }
    }

    #[cfg(feature = "tui-perf-cmd")]
    pub fn stats(&self) -> CachedActivityLinesStats {
        CachedActivityLinesStats {
            entries: self.entries.len(),
            occupied_entries: self.entries.iter().filter(|entry| entry.is_some()).count(),
            hits: self.hits,
            misses: self.misses,
        }
    }

    #[cfg(feature = "tui-perf-cmd")]
    pub fn reset_stats(&mut self) {
        self.hits = 0;
        self.misses = 0;
    }

    /// Make room for at least `count` cells.
    fn ensure_capacity(&mut self, count: usize) {
        if self.entries.len() < count {
            self.entries.resize_with(count, || None);
        } else if self.entries.len() > count {
            self.entries.truncate(count);
        }
    }

    /// Return cached lines for cell `index` at `width`, if the
    /// cached cell matches the current one.
    fn get(&mut self, index: usize, width: u16, cell: &ActivityCell) -> Option<CachedCellLines> {
        let cached = self.entries.get(index).and_then(|e| {
            e.as_ref().and_then(|entry| {
                if entry.width == width && entry.cell == *cell {
                    Some(CachedCellLines {
                        lines: entry.lines.clone(),
                        height: entry.height,
                    })
                } else {
                    None
                }
            })
        });
        #[cfg(feature = "tui-perf-cmd")]
        {
            if cached.is_some() {
                self.hits = self.hits.saturating_add(1);
            } else {
                self.misses = self.misses.saturating_add(1);
            }
        }
        cached
    }

    /// Store rendered lines for cell `index`.
    fn set(
        &mut self,
        index: usize,
        width: u16,
        cell: ActivityCell,
        lines: Vec<Line<'static>>,
    ) -> CachedCellLines {
        if index >= self.entries.len() {
            self.entries.resize_with(index + 1, || None);
        }
        let height = cached_lines_height(&lines, width);
        self.entries[index] = Some(CacheEntry {
            width,
            cell,
            lines: lines.clone(),
            height,
        });
        CachedCellLines { lines, height }
    }
}

/// Thin Renderable wrapper around pre-computed lines.
#[derive(Clone)]
struct CachedCellLines {
    lines: Vec<Line<'static>>,
    height: u16,
}

impl Renderable for CachedCellLines {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if self.lines.is_empty() {
            return;
        }
        Clear.render(area, buf);
        Paragraph::new(Text::from(self.lines.clone()))
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        self.height
    }

    fn render_skip(&self, area: Rect, skip: u16, buf: &mut Buffer) {
        if self.lines.is_empty() {
            return;
        }
        if skip == 0 {
            self.render(area, buf);
            return;
        }
        Clear.render(area, buf);
        Paragraph::new(Text::from(self.lines.clone()))
            .wrap(Wrap { trim: false })
            .scroll((skip, 0))
            .render(area, buf);
    }
}

fn cached_lines_height(lines: &[Line<'static>], width: u16) -> u16 {
    if lines.is_empty() || width == 0 {
        return 0;
    }
    Paragraph::new(Text::from(lines.to_vec()))
        .wrap(Wrap { trim: false })
        .line_count(width)
        .try_into()
        .unwrap_or(u16::MAX)
}

/// Viewport-culled activity feed renderer with per-cell cache.
///
/// Uses manual viewport culling on a flat line list.  Cells whose top rows
/// have scrolled above the viewport are rendered from the first visible row
/// downward — the title row is *not* pinned to the viewport top.
///
/// `scroll_offset` of `u16::MAX` means auto-scroll (pin to bottom).
/// Returns `max_scroll`.
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
        let placeholder =
            Paragraph::new("No activity yet").style(Style::default().fg(Color::DarkGray));
        placeholder.render(inner, buf);
        return 0;
    }

    cache.ensure_capacity(total_cells);

    let mut column = ViewportCulledColumn::new();

    let spacer_line = CachedCellLines {
        lines: vec![Line::from("")],
        height: 1,
    };

    // Committed cells: use cache to skip markdown re-render.
    for (i, cell) in cells.iter().enumerate() {
        let cached = if let Some(cached) = cache.get(i, inner.width, cell) {
            cached
        } else {
            let lines = render_activity_cell_lines(cell, inner.width);
            cache.set(i, inner.width, cell.clone(), lines)
        };
        column.push(cached);
        // Blank line spacing between adjacent cells (matches old Vec<Line> behavior).
        if !live_cells.is_empty() || i + 1 < cells.len() {
            column.push(spacer_line.clone());
        }
    }

    // Live cells are always re-rendered (they change every frame).
    for (i, lc) in live_cells.iter().enumerate() {
        let idx = cells.len() + i;
        let lines = render_activity_cell_lines(&lc.cell, inner.width);
        let cached = cache.set(idx, inner.width, lc.cell.clone(), lines);
        column.push(cached);
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

    // Render through the layout tree.
    // ViewportCulledColumn calls render_skip on each child — CachedCellLines
    // overrides render_skip with Paragraph::scroll((n,0)), so cells whose
    // top rows have scrolled above the viewport render correctly without
    // sticky-header artefacts.
    let max_scroll = column
        .desired_height(inner.width)
        .saturating_sub(inner.height.saturating_sub(1));

    let mut flex = FlexRenderable::new();
    flex.push(1, column);
    flex.render(inner, buf);

    max_scroll
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
        Clear.render(area, buf);
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let lines = render_activity_cell_lines(self, width);
        cached_lines_height(&lines, width)
    }
}

fn render_activity_cell_lines(cell: &ActivityCell, max_width: u16) -> Vec<Line<'static>> {
    match cell {
        ActivityCell::Assistant(cell) => render_assistant_cell_lines(cell, max_width),
        ActivityCell::FinalMessageSeparator(cell) => {
            render_final_message_separator_cell_lines(cell, max_width)
        }
        ActivityCell::User(cell) => render_user_cell_lines(cell, max_width),
        ActivityCell::Browser(cell) => render_browser_cell_lines(cell, max_width),
        ActivityCell::LiveBrowser(cell) => render_live_browser_cell_lines(cell, max_width),
        ActivityCell::WebSearch(cell) => render_web_search_cell_lines(cell, max_width),
        ActivityCell::CodingOpenProject(cell) => {
            render_coding_open_project_cell_lines(cell, max_width)
        }
        ActivityCell::Explored(cell) => render_explored_cell_lines(cell, max_width),
        ActivityCell::CodingEdit(cell) => render_coding_edit_cell_lines(cell, max_width),
        ActivityCell::CodingReview(cell) => render_coding_review_cell_lines(cell),
        ActivityCell::GenericApp(cell) => render_generic_app_cell_lines(cell),
        ActivityCell::PlanResult(cell) => render_plan_cell_lines(cell, max_width),
        ActivityCell::CreatePrimitiveSpecResult(cell) => {
            render_create_primitive_spec_cell_lines(cell)
        }
        ActivityCell::ActivatePrimitiveResult(cell) => render_activate_primitive_cell_lines(cell),
        ActivityCell::ExecResult(cell) => render_exec_cell_lines(cell, max_width),
        ActivityCell::LiveExec(cell) => render_live_exec_cell_lines(cell, max_width),
        ActivityCell::Patch(cell) => render_patch_cell_lines(cell, max_width),
        ActivityCell::Telegram(cell) => render_telegram_cell_lines(cell, max_width),
        ActivityCell::Reply(cell) => render_reply_cell_lines(cell, max_width),
        ActivityCell::TerminalWait(cell) => render_terminal_wait_cell_lines(cell, max_width),
        ActivityCell::Warning(cell) => render_warning_cell_lines(cell, max_width),
        ActivityCell::Error(cell) => render_error_cell_lines(cell, max_width),
        ActivityCell::Thinking(cell) => render_thinking_cell_lines(cell, max_width),
    }
}

#[cfg(test)]
pub(in crate::dashboard) fn activity_transcript_text(
    cells: &[ActivityCell],
    live_cells: &[LiveActivityCell],
) -> String {
    activity_transcript_lines(cells, live_cells, u16::MAX)
        .into_iter()
        .map(|line| rendered_line_text(&line))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(in crate::dashboard) fn activity_transcript_lines(
    cells: &[ActivityCell],
    live_cells: &[LiveActivityCell],
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for cell in cells {
        append_transcript_cell_lines(&mut lines, activity_cell_transcript_lines(cell, width));
    }
    for live_cell in live_cells {
        append_transcript_cell_lines(
            &mut lines,
            activity_cell_transcript_lines(&live_cell.cell, width),
        );
    }
    if lines.is_empty() {
        vec![Line::from(Span::styled("No activity yet.", dim_style()))]
    } else {
        lines
    }
}

fn append_transcript_cell_lines(target: &mut Vec<Line<'static>>, mut lines: Vec<Line<'static>>) {
    if lines.is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push(Line::from(""));
    }
    target.append(&mut lines);
}

fn activity_cell_transcript_lines(cell: &ActivityCell, width: u16) -> Vec<Line<'static>> {
    match cell {
        ActivityCell::Assistant(cell) => {
            let body = cell
                .full_body
                .clone()
                .unwrap_or_else(|| cell.body_lines.join("\n"));
            transcript_markdown_section("ASSISTANT", &body, Color::White, width)
        }
        ActivityCell::FinalMessageSeparator(cell) => {
            render_final_message_separator_cell_lines(cell, width)
        }
        ActivityCell::User(cell) => transcript_user_lines(cell, width),
        ActivityCell::Thinking(cell) => {
            let body = cell
                .full_body
                .clone()
                .unwrap_or_else(|| cell.body_lines.join("\n"));
            transcript_markdown_section("THINKING", &body, Color::Gray, width)
        }
        ActivityCell::PlanResult(cell) => transcript_plan_lines(cell, width),
        ActivityCell::ExecResult(cell) => exec_transcript_lines(cell, width),
        ActivityCell::LiveExec(cell) => live_exec_transcript_lines(cell, width),
        ActivityCell::Patch(cell) => {
            patch_transcript_styled_lines("PATCH", &cell.summary_line, &cell.files, width)
        }
        ActivityCell::WebSearch(cell) => render_web_search_cell_lines(cell, width),
        ActivityCell::Warning(cell) => {
            transcript_text_section("WARNING", &cell.title, &cell.body_lines, width)
        }
        ActivityCell::Error(cell) => {
            transcript_text_section("ERROR", &cell.title, &cell.body_lines, width)
        }
        _ => transcript_plain_block(activity_cell_transcript_block(cell)),
    }
}

#[cfg(test)]
fn rendered_line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn transcript_header(title: impl Into<String>) -> Line<'static> {
    Line::from(vec![Span::styled(
        title.into(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )])
}

fn transcript_plain_block(block: String) -> Vec<Line<'static>> {
    block
        .lines()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                transcript_header(line.to_string())
            } else {
                Line::from(line.to_string())
            }
        })
        .collect()
}

fn transcript_markdown_section(
    title: &str,
    body: &str,
    base_color: Color,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![transcript_header(title)];
    if !body.trim().is_empty() {
        lines.extend(prefixed_detail_lines(
            render_markdown_with_width(body, base_color, detail_markdown_width(width)),
            width,
        ));
    }
    lines
}

fn transcript_user_lines(cell: &UserActivityCell, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![transcript_header("USER")];
    let mut source_lines = user_source_lines(cell);
    source_lines.extend(
        cell.image_attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| transcript_image_attachment_line(index, attachment)),
    );
    lines.push(blank_user_message_line());
    lines.extend(user_prompt_lines(source_lines, width));
    lines.push(blank_user_message_line());
    lines
}

fn transcript_text_section(
    title: &str,
    primary: &str,
    body_lines: &[String],
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![transcript_header(title)];
    let mut body = Vec::new();
    if !primary.trim().is_empty() {
        body.push(Line::from(primary.to_string()));
    }
    body.extend(body_lines.iter().cloned().map(Line::from));
    lines.extend(prefixed_detail_lines(body, width));
    lines
}

fn transcript_plan_lines(cell: &PlanActivityCell, width: u16) -> Vec<Line<'static>> {
    let title = match cell.kind {
        PlanUiKind::Proposed => "PROPOSED PLAN",
        PlanUiKind::Updated => "PLAN",
    };
    let mut lines = vec![transcript_header(title)];
    if let Some(explanation) = cell.explanation.as_deref()
        && !explanation.trim().is_empty()
    {
        lines.extend(prefixed_detail_lines(
            render_markdown_with_width(
                &format!("note: {}", explanation.trim()),
                Color::Gray,
                detail_markdown_width(width),
            )
            .into_iter()
            .map(|mut line| {
                line.spans = line
                    .spans
                    .into_iter()
                    .map(|span| {
                        Span::styled(
                            span.content.to_string(),
                            span.style.add_modifier(Modifier::DIM | Modifier::ITALIC),
                        )
                    })
                    .collect();
                line
            })
            .collect(),
            width,
        ));
    }

    let steps = if cell.steps.is_empty() {
        vec![Line::from(Span::styled(
            "(empty plan)",
            dim_style().add_modifier(Modifier::ITALIC),
        ))]
    } else {
        cell.steps
            .iter()
            .map(|step| {
                let (marker, style) = match step.status {
                    PlanStepDisplayStatus::Completed => {
                        ("✔ ", Style::default().fg(Color::DarkGray))
                    }
                    PlanStepDisplayStatus::InProgress => (
                        "□ ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    PlanStepDisplayStatus::Pending => ("□ ", Style::default().fg(Color::Gray)),
                };
                Line::from(vec![
                    Span::styled(marker, style),
                    Span::styled(step.text.clone(), style),
                ])
            })
            .collect::<Vec<_>>()
    };
    lines.extend(prefixed_detail_lines(steps, width));
    lines
}

fn exec_transcript_lines(cell: &ExecResultActivityCell, width: u16) -> Vec<Line<'static>> {
    let output = cell.command_output();
    let mut lines = vec![transcript_header("COMMAND")];
    let mut command = highlighted_shell_command_line(&output.command);
    command.spans.insert(
        0,
        Span::styled(
            "$ ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    );
    lines.extend(prefixed_detail_lines(vec![command], width));

    let output_lines = if output.output_lines.is_empty() {
        vec![Line::from(Span::styled("(no output)", dim_style()))]
    } else {
        output
            .output_lines
            .iter()
            .map(|line| Line::from(Span::styled(line.clone(), Style::default().fg(Color::Gray))))
            .collect()
    };
    lines.extend(prefixed_detail_lines(output_lines, width));

    if let Some(exit_code) = output.meta.exit_code {
        lines.extend(prefixed_detail_lines(
            vec![exit_marker_line(exit_code)],
            width,
        ));
    }
    if let Some(meta) = cell.meta.as_deref()
        && !meta.trim().is_empty()
    {
        lines.extend(prefixed_detail_lines(
            vec![Line::from(Span::styled(meta.to_string(), dim_style()))],
            width,
        ));
    }
    lines
}

fn live_exec_transcript_lines(cell: &LiveExecActivityCell, width: u16) -> Vec<Line<'static>> {
    let output = cell.command_output();
    let mut lines = vec![transcript_header("COMMAND")];
    let mut command = highlighted_shell_command_line(&output.command);
    command.spans.insert(
        0,
        Span::styled(
            "$ ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    );
    lines.extend(prefixed_detail_lines(vec![command], width));
    let mut output_lines = output
        .output_lines
        .iter()
        .map(|line| Line::from(Span::styled(line.clone(), Style::default().fg(Color::Gray))))
        .collect::<Vec<_>>();
    output_lines.push(Line::from(Span::styled("running...", dim_style())));
    lines.extend(prefixed_detail_lines(output_lines, width));
    lines
}

fn patch_transcript_styled_lines(
    title: &str,
    summary: &str,
    files: &[PatchFileUiData],
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![transcript_header(title)];
    if !summary.trim().is_empty() {
        lines.extend(prefixed_detail_lines(
            vec![Line::from(summary.to_string())],
            width,
        ));
    }
    let diff_backgrounds = diff_scope_backgrounds();
    let old_lineno_width = files
        .iter()
        .flat_map(|file| file.diff_lines.iter().filter_map(|line| line.old_lineno))
        .max()
        .unwrap_or(0)
        .to_string()
        .len()
        .max(1);
    let new_lineno_width = files
        .iter()
        .flat_map(|file| file.diff_lines.iter().filter_map(|line| line.new_lineno))
        .max()
        .unwrap_or(0)
        .to_string()
        .len()
        .max(1);
    for file in files {
        let mut file_lines = vec![render_patch_file_header(file)];
        file_lines.extend(render_patch_file_diff_lines(
            file,
            diff_backgrounds,
            old_lineno_width,
            new_lineno_width,
            usize::MAX,
        ));
        lines.extend(prefixed_detail_lines(file_lines, width));
    }
    lines
}

fn activity_cell_transcript_block(cell: &ActivityCell) -> String {
    match cell {
        ActivityCell::Assistant(cell) => transcript_section(
            "ASSISTANT",
            cell.full_body
                .clone()
                .unwrap_or_else(|| primary_transcript_text(&cell.title, &cell.body_lines)),
        ),
        ActivityCell::FinalMessageSeparator(cell) => cell
            .elapsed_seconds
            .map(|seconds| format!("Worked for {}", format_elapsed_seconds_compact(seconds)))
            .unwrap_or_else(|| "Worked".to_string()),
        ActivityCell::User(cell) => transcript_section("USER", user_transcript_text(cell)),
        ActivityCell::Thinking(cell) => transcript_section(
            "THINKING",
            cell.full_body
                .clone()
                .unwrap_or_else(|| primary_transcript_text(&cell.title, &cell.body_lines)),
        ),
        ActivityCell::Browser(cell) => {
            let mut lines = vec![format!(
                "Captured URL: {}",
                cell.url.as_deref().unwrap_or("unknown")
            )];
            if let Some(line_count) = cell.line_count {
                lines.push(format!("lines: {line_count}"));
            }
            if let Some(ref_count) = cell.ref_count {
                lines.push(format!("refs: {ref_count}"));
            }
            transcript_section("BROWSER", lines.join("\n"))
        }
        ActivityCell::LiveBrowser(cell) => {
            let mut lines = vec![format!(
                "Opening URL: {}",
                cell.url.as_deref().unwrap_or(cell.title.as_str())
            )];
            lines.extend(cell.body_lines.clone());
            transcript_section("BROWSER", lines.join("\n"))
        }
        ActivityCell::WebSearch(cell) => {
            let mut lines = vec![format!("query: {}", cell.query)];
            if let Some(url) = cell.url.as_deref() {
                lines.push(format!("url: {url}"));
            }
            lines.extend(cell.body_lines.clone());
            let title = match cell.action {
                WebSearchUiAction::Searching => "SEARCHING THE WEB",
                WebSearchUiAction::Searched => "SEARCHED THE WEB",
            };
            transcript_section(title, lines.join("\n"))
        }
        ActivityCell::CodingOpenProject(cell) => transcript_section(
            "OPENED PROJECT",
            primary_transcript_text(&cell.project_root, &cell.detail_lines),
        ),
        ActivityCell::Explored(cell) => {
            let lines = cell
                .calls
                .iter()
                .flat_map(|call| {
                    let mut lines = vec![format!("{} {}", call.tool_name, call.summary)];
                    lines.extend(call.detail_lines.iter().map(|line| format!("  {line}")));
                    lines
                })
                .collect::<Vec<_>>();
            transcript_section(&cell.title, lines.join("\n"))
        }
        ActivityCell::CodingEdit(cell) => {
            let mut lines = vec![
                cell.selector.clone(),
                format!("+{} -{}", cell.added_lines, cell.removed_lines),
            ];
            if let Some(file) = cell.file.as_deref() {
                lines.push(format!("file: {file}"));
            }
            lines.extend(
                cell.impact_lines
                    .iter()
                    .map(|line| format!("impact: {line}")),
            );
            lines.extend(patch_files_transcript_lines(&cell.diff_files));
            transcript_section(&cell.title, lines.join("\n"))
        }
        ActivityCell::CodingReview(cell) => {
            let mut lines = Vec::new();
            if !cell.title.trim().is_empty() {
                lines.push(cell.title.clone());
            }
            if !cell.summary.trim().is_empty() {
                lines.push(cell.summary.clone());
            }
            if cell.review_pending {
                lines.push("review pending".to_string());
            }
            transcript_section("REVIEW", lines.join("\n"))
        }
        ActivityCell::GenericApp(cell) => {
            transcript_section(&cell.title, cell.body_lines.join("\n"))
        }
        ActivityCell::PlanResult(cell) => {
            let mut lines = Vec::new();
            if let Some(explanation) = cell.explanation.as_deref()
                && !explanation.trim().is_empty()
            {
                lines.push(format!("note: {}", explanation.trim()));
            }
            if cell.steps.is_empty() {
                lines.push("(empty plan)".to_string());
            } else {
                lines.extend(cell.steps.iter().map(|step| {
                    let marker = match step.status {
                        PlanStepDisplayStatus::Pending => "[ ]",
                        PlanStepDisplayStatus::InProgress => "[~]",
                        PlanStepDisplayStatus::Completed => "[x]",
                    };
                    format!("{marker} {}", step.text)
                }));
            }
            let title = match cell.kind {
                PlanUiKind::Proposed => "PROPOSED PLAN",
                PlanUiKind::Updated => "PLAN",
            };
            transcript_section(title, lines.join("\n"))
        }
        ActivityCell::CreatePrimitiveSpecResult(cell) => {
            transcript_section("CREATED PRIMITIVE SPEC", cell.primitive_id.clone())
        }
        ActivityCell::ActivatePrimitiveResult(cell) => {
            transcript_section("ACTIVATED PRIMITIVE", cell.primitive_id.clone())
        }
        ActivityCell::ExecResult(cell) => exec_transcript_block(
            "COMMAND",
            &cell.title,
            cell.meta.as_deref(),
            &cell.output_lines,
        ),
        ActivityCell::LiveExec(cell) => live_exec_transcript_block(cell),
        ActivityCell::Patch(cell) => {
            let mut lines = vec![cell.summary_line.clone()];
            lines.extend(patch_files_transcript_lines(&cell.files));
            transcript_section("PATCH", lines.join("\n"))
        }
        ActivityCell::Telegram(cell) => {
            let mut lines = cell.detail_lines.clone();
            lines.extend(cell.message_lines.clone());
            transcript_section(&cell.title, lines.join("\n"))
        }
        ActivityCell::Reply(cell) => {
            let title = match cell.disposition {
                crate::tool_ui::ReplyDisposition::Resolved => "REPLY RESOLVED",
                crate::tool_ui::ReplyDisposition::Dismissed => "REPLY DISMISSED",
                crate::tool_ui::ReplyDisposition::Failed => "REPLY FAILED",
            };
            transcript_section(title, cell.message_lines.join("\n"))
        }
        ActivityCell::TerminalWait(cell) => {
            transcript_section(&cell.title, cell.body_lines.join("\n"))
        }
        ActivityCell::Warning(cell) => transcript_section(
            "WARNING",
            primary_transcript_text(&cell.title, &cell.body_lines),
        ),
        ActivityCell::Error(cell) => transcript_section(
            "ERROR",
            primary_transcript_text(&cell.title, &cell.body_lines),
        ),
    }
}

fn transcript_section(title: &str, body: String) -> String {
    let body = body.trim_end();
    if body.is_empty() {
        title.to_string()
    } else {
        format!("{title}\n{body}")
    }
}

fn primary_transcript_text(title: &str, body_lines: &[String]) -> String {
    let mut lines = vec![title.to_string()];
    lines.extend(body_lines.iter().cloned());
    lines.join("\n")
}

fn user_transcript_text(cell: &UserActivityCell) -> String {
    let mut lines = if let Some(full) = cell.full_body.as_deref() {
        full.trim_end_matches(['\r', '\n'])
            .lines()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    } else {
        let mut lines = vec![cell.title.clone()];
        lines.extend(cell.body_lines.iter().cloned());
        lines
    };
    lines.extend(
        cell.image_attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| {
                format!(
                    "[{}: {}] {} {}",
                    index + 1,
                    image_attachment_kind(&attachment.uri),
                    attachment.label,
                    attachment.uri
                )
            }),
    );
    lines.join("\n")
}

fn exec_transcript_block(
    title: &str,
    command: &str,
    meta: Option<&str>,
    output_lines: &[String],
) -> String {
    let mut lines = vec![format!("$ {command}")];
    if let Some(meta) = meta
        && !meta.trim().is_empty()
    {
        lines.push(meta.to_string());
    }
    lines.extend(output_lines.iter().cloned());
    let parsed_meta = TerminalExecutionMeta::parse(meta, output_lines);
    if let Some(exit_code) = parsed_meta.exit_code {
        let marker = if exit_code == 0 { "✓" } else { "✗" };
        lines.push(format!("{marker} exit={exit_code}"));
    }
    transcript_section(title, lines.join("\n"))
}

fn live_exec_transcript_block(cell: &LiveExecActivityCell) -> String {
    let mut lines = vec![format!("$ {}", cell.title)];
    if let Some(started_at_ms) = cell.started_at_ms
        && let Some(elapsed_ms) = elapsed_since_ms(started_at_ms)
    {
        lines.push(format!("running for {}", format_elapsed_ms(elapsed_ms)));
    }
    lines.extend(cell.call_lines.iter().map(|line| format!("call: {line}")));
    lines.extend(cell.output_lines.iter().cloned());
    lines.push("… running".to_string());
    transcript_section("COMMAND", lines.join("\n"))
}

fn elapsed_since_ms(started_at_ms: i64) -> Option<u64> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    let now_ms = i64::try_from(now_ms).ok()?;
    let elapsed = now_ms.saturating_sub(started_at_ms);
    u64::try_from(elapsed).ok()
}

fn format_elapsed_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1_000.0)
    }
}

fn patch_files_transcript_lines(files: &[PatchFileUiData]) -> Vec<String> {
    files
        .iter()
        .flat_map(|file| {
            let mut lines = vec![format!(
                "{} (+{} -{})",
                file.path, file.added_lines, file.removed_lines
            )];
            lines.extend(file.diff_lines.iter().map(patch_diff_line_transcript_text));
            lines
        })
        .collect()
}

fn patch_diff_line_transcript_text(line: &PatchDiffLineUiData) -> String {
    let gutter = match line.kind {
        PatchDiffLineKind::Context => " ",
        PatchDiffLineKind::Delete => "-",
        PatchDiffLineKind::Add => "+",
        PatchDiffLineKind::HunkBreak => "⋮",
    };
    format!(
        "{} {} {} {}",
        line.old_lineno
            .map(|lineno| lineno.to_string())
            .unwrap_or_default(),
        line.new_lineno
            .map(|lineno| lineno.to_string())
            .unwrap_or_default(),
        gutter,
        line.text
    )
}

fn dim_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn bold_style() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn user_message_style() -> Style {
    Style::default()
}

fn codex_header(title: impl Into<String>) -> Line<'static> {
    codex_header_with_styles(title, dim_style(), bold_style())
}

fn codex_header_with_styles(
    title: impl Into<String>,
    bullet_style: Style,
    title_style: Style,
) -> Line<'static> {
    codex_header_with_marker(
        Span::styled(ACTIVITY_BULLET, bullet_style),
        title,
        title_style,
    )
}

fn codex_header_with_marker(
    marker: Span<'static>,
    title: impl Into<String>,
    title_style: Style,
) -> Line<'static> {
    Line::from(vec![
        marker,
        Span::raw(ACTIVITY_TITLE_GAP),
        Span::styled(title.into(), title_style),
    ])
}

fn command_header_lines_from_line(
    title: &str,
    command: Line<'static>,
    bullet_style: Style,
    max_width: u16,
) -> Vec<Line<'static>> {
    command_header_lines_with_marker(
        Span::styled(ACTIVITY_BULLET, bullet_style),
        title,
        command,
        max_width,
    )
}

fn command_header_lines_with_marker(
    marker: Span<'static>,
    title: &str,
    command: Line<'static>,
    max_width: u16,
) -> Vec<Line<'static>> {
    prefixed_wrapped_line(
        command,
        vec![
            marker,
            Span::raw(ACTIVITY_TITLE_GAP),
            Span::styled(title.to_string(), bold_style()),
            Span::raw(ACTIVITY_TITLE_GAP),
        ],
        vec![Span::styled(COMMAND_CONTINUATION_PREFIX, dim_style())],
        max_width,
    )
}

fn activity_marker(started_at_ms: Option<i64>) -> Span<'static> {
    let glyphs = ["•", "◦", "▪", "◦"];
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let seed = started_at_ms.unwrap_or(0);
    let frame = ((now_ms.saturating_sub(seed) / 180).rem_euclid(glyphs.len() as i64)) as usize;
    Span::styled(
        glyphs[frame],
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )
}

fn user_prompt_lines(lines: Vec<Line<'static>>, max_width: u16) -> Vec<Line<'static>> {
    let mut lines = prefix_wrapped_lines(
        lines,
        vec![
            Span::styled(USER_PROMPT_PREFIX, dim_style().add_modifier(Modifier::BOLD)),
            Span::raw(ACTIVITY_TITLE_GAP),
        ],
        vec![Span::raw(ACTIVITY_BODY_INDENT)],
        max_width,
    );
    for line in &mut lines {
        line.style = line.style.patch(user_message_style());
    }
    lines
}

fn blank_user_message_line() -> Line<'static> {
    let mut line = Line::from("");
    line.style = user_message_style();
    line
}

fn prefixed_body_lines(lines: Vec<Line<'static>>, max_width: u16) -> Vec<Line<'static>> {
    prefix_wrapped_lines(
        lines,
        vec![Span::raw(ACTIVITY_BODY_INDENT)],
        vec![Span::raw(ACTIVITY_BODY_INDENT)],
        max_width,
    )
}

fn body_markdown_width(max_width: u16) -> Option<u16> {
    Some(
        max_width
            .saturating_sub(ACTIVITY_BODY_INDENT.len() as u16)
            .max(1),
    )
}

fn detail_markdown_width(max_width: u16) -> Option<u16> {
    Some(
        max_width
            .saturating_sub(DETAIL_SUBSEQUENT_PREFIX.len() as u16)
            .max(1),
    )
}

fn prefixed_detail_lines(lines: Vec<Line<'static>>, max_width: u16) -> Vec<Line<'static>> {
    prefix_wrapped_lines(
        lines,
        vec![Span::styled(DETAIL_INITIAL_PREFIX, dim_style())],
        vec![Span::raw(DETAIL_SUBSEQUENT_PREFIX)],
        max_width,
    )
}

fn prefix_wrapped_lines(
    lines: Vec<Line<'static>>,
    initial_prefix: Vec<Span<'static>>,
    subsequent_prefix: Vec<Span<'static>>,
    max_width: u16,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for (index, line) in lines.into_iter().enumerate() {
        let prefix = if index == 0 {
            initial_prefix.clone()
        } else {
            subsequent_prefix.clone()
        };
        out.extend(prefixed_wrapped_line(
            line,
            prefix,
            subsequent_prefix.clone(),
            max_width,
        ));
    }
    out
}

fn prefixed_wrapped_line(
    content: Line<'static>,
    initial_prefix: Vec<Span<'static>>,
    subsequent_prefix: Vec<Span<'static>>,
    max_width: u16,
) -> Vec<Line<'static>> {
    let line_style = content.style;
    let mut out = Vec::new();
    let mut current_prefix = initial_prefix;
    let mut current_spans = current_prefix.clone();
    let mut current_width = 0usize;
    let mut has_content = false;

    for span in content.spans {
        let style = span.style;
        let mut chunk = String::new();
        let mut chunk_width = 0usize;

        for ch in span.content.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            let content_width = max_width
                .saturating_sub(spans_display_width(&current_prefix) as u16)
                .max(1) as usize;
            if current_width + chunk_width > 0
                && current_width + chunk_width + ch_width > content_width
            {
                if !chunk.is_empty() {
                    current_spans.push(Span::styled(std::mem::take(&mut chunk), style));
                    chunk_width = 0;
                }
                let mut line = Line::from(std::mem::replace(
                    &mut current_spans,
                    subsequent_prefix.clone(),
                ));
                line.style = line_style;
                out.push(line);
                current_prefix = subsequent_prefix.clone();
                current_width = 0;
            }

            chunk.push(ch);
            chunk_width += ch_width;
            has_content = true;
        }

        if !chunk.is_empty() {
            current_width += chunk_width;
            current_spans.push(Span::styled(chunk, style));
        }
    }

    if !has_content || current_spans.len() > current_prefix.len() {
        let mut line = Line::from(current_spans);
        line.style = line_style;
        out.push(line);
    }

    out
}

fn spans_display_width(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .flat_map(|span| span.content.chars())
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

fn render_final_message_separator_cell_lines(
    cell: &FinalMessageSeparatorActivityCell,
    max_width: u16,
) -> Vec<Line<'static>> {
    let width = usize::from(max_width);
    if width == 0 {
        return Vec::new();
    }

    let label = cell
        .elapsed_seconds
        .map(|seconds| format!("─ Worked for {} ─", format_elapsed_seconds_compact(seconds)))
        .unwrap_or_else(|| "─".to_string());
    let label_width = display_width(&label);
    let text = if label_width >= width {
        truncate_display_width(&label, width)
    } else {
        format!("{label}{}", "─".repeat(width.saturating_sub(label_width)))
    };
    vec![Line::from(Span::styled(text, dim_style()))]
}

fn format_elapsed_seconds_compact(elapsed_seconds: u64) -> String {
    if elapsed_seconds < 60 {
        return format!("{elapsed_seconds}s");
    }
    if elapsed_seconds < 3600 {
        let minutes = elapsed_seconds / 60;
        let seconds = elapsed_seconds % 60;
        return format!("{minutes}m {seconds:02}s");
    }
    let hours = elapsed_seconds / 3600;
    let minutes = (elapsed_seconds % 3600) / 60;
    let seconds = elapsed_seconds % 60;
    format!("{hours}h {minutes:02}m {seconds:02}s")
}

fn display_width(text: &str) -> usize {
    text.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

fn truncate_display_width(text: &str, max_width: usize) -> String {
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out
}

fn render_assistant_cell_lines(cell: &AssistantActivityCell, max_width: u16) -> Vec<Line<'static>> {
    // When rich (markdown) mode is on and full_body is available,
    // render markdown from the complete text.  Using truncated
    // body_lines breaks multi-line constructs (fenced code blocks,
    // tables) because the closing fences / separators are cut off,
    // causing the markdown parser to treat following text as part of
    // the truncated construct.
    if cell.rich_mode
        && let Some(ref full) = cell.full_body
    {
        let body_text = full
            .lines()
            .skip(1) // first line is the title, already rendered above
            .collect::<Vec<_>>()
            .join("\n");
        let mut lines = vec![codex_header(cell.title.clone())];
        let md_lines =
            render_markdown_with_width(&body_text, Color::White, body_markdown_width(max_width));
        lines.extend(prefixed_body_lines(md_lines, max_width));
        return lines;
    }
    render_text_activity_lines(&cell.title, &cell.body_lines, 8, true, max_width)
}

fn render_thinking_cell_lines(cell: &ThinkingActivityCell, max_width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![codex_header_with_styles(
        cell.title.clone(),
        dim_style(),
        Style::default().add_modifier(Modifier::BOLD | Modifier::ITALIC),
    )];

    let body_text = if cell.expanded {
        cell.full_body
            .clone()
            .unwrap_or_else(|| cell.body_lines.join("\n"))
    } else {
        cell.body_lines
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    };
    let md_lines =
        render_markdown_with_width(&body_text, Color::Gray, body_markdown_width(max_width))
            .into_iter()
            .map(|mut line| {
                line.spans = line
                    .spans
                    .into_iter()
                    .map(|span| {
                        Span::styled(
                            span.content.to_string(),
                            span.style.add_modifier(Modifier::DIM | Modifier::ITALIC),
                        )
                    })
                    .collect();
                line
            })
            .collect::<Vec<_>>();
    lines.extend(prefixed_body_lines(md_lines, max_width));
    lines
}

fn render_user_cell_lines(cell: &UserActivityCell, max_width: u16) -> Vec<Line<'static>> {
    let mut source_lines = user_source_lines(cell);
    source_lines.extend(
        cell.image_attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| image_attachment_line(index, attachment)),
    );

    let mut rendered = Vec::new();
    rendered.push(blank_user_message_line());
    rendered.extend(user_prompt_lines(source_lines, max_width));
    rendered.push(blank_user_message_line());
    rendered
}

fn user_source_lines(cell: &UserActivityCell) -> Vec<Line<'static>> {
    let mut source_lines = Vec::new();
    if let Some(ref full) = cell.full_body {
        source_lines.extend(
            full.trim_end_matches(['\r', '\n'])
                .lines()
                .map(|line| Line::from(line.to_string())),
        );
    } else {
        source_lines.push(Line::from(cell.title.clone()));
        source_lines.extend(cell.body_lines.iter().cloned().map(Line::from));
    }
    source_lines
}

fn image_attachment_line(index: usize, attachment: &MessageImageAttachment) -> Line<'static> {
    let kind = image_attachment_kind(&attachment.uri);
    let label = attachment.label.trim();
    let display_label = if label.is_empty() { "image" } else { label };
    let mut spans = vec![
        Span::styled(
            format!("[{}: {}]", index + 1, kind),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(" "),
        Span::styled(display_label.to_string(), Style::default().fg(Color::Gray)),
    ];
    if !attachment.mime_type.trim().is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("({})", attachment.mime_type),
            dim_style(),
        ));
    }
    Line::from(spans)
}

fn transcript_image_attachment_line(
    index: usize,
    attachment: &MessageImageAttachment,
) -> Line<'static> {
    let kind = image_attachment_kind(&attachment.uri);
    let label = attachment.label.trim();
    let display_label = if label.is_empty() { "image" } else { label };
    let mut spans = vec![
        Span::styled(
            format!("[{}: {}]", index + 1, kind),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(" "),
        Span::styled(display_label.to_string(), Style::default().fg(Color::Gray)),
    ];
    if !attachment.uri.trim().is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            attachment.uri.trim().to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::UNDERLINED),
        ));
    }
    if !attachment.mime_type.trim().is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("({})", attachment.mime_type),
            dim_style(),
        ));
    }
    Line::from(spans)
}

fn image_attachment_kind(uri: &str) -> &'static str {
    if uri.starts_with("http://") || uri.starts_with("https://") || uri.starts_with("data:") {
        "remote image"
    } else {
        "local image"
    }
}

fn render_generic_app_cell_lines(cell: &GenericAppActivityCell) -> Vec<Line<'static>> {
    vec![codex_header(format!("App: {}", cell.title))]
}

fn render_coding_open_project_cell_lines(
    cell: &CodingOpenProjectActivityCell,
    max_width: u16,
) -> Vec<Line<'static>> {
    let title = format!("Opened Project: {}", cell.project_root);
    let mut lines = vec![codex_header(title)];
    let detail = cell
        .detail_lines
        .iter()
        .take(6)
        .cloned()
        .map(Line::from)
        .collect::<Vec<_>>();
    lines.extend(prefixed_detail_lines(detail, max_width));
    lines
}

fn render_explored_cell_lines(cell: &ExploredActivityCell, max_width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![codex_header(cell.title.clone())];
    let mut detail = Vec::new();
    let mut index = 0;
    while index < cell.calls.len().min(12) {
        let call = &cell.calls[index];
        if matches!(explored_call_action(call), Some(ExploredCallUiAction::Read)) {
            let mut names = vec![explored_read_target(call)];
            index += 1;
            while index < cell.calls.len().min(12)
                && matches!(
                    explored_call_action(&cell.calls[index]),
                    Some(ExploredCallUiAction::Read)
                )
            {
                names.push(explored_read_target(&cell.calls[index]));
                index += 1;
            }
            names.dedup();
            detail.push(coding_action_line("Read", names.join(", ")));
        } else {
            detail.push(explored_call_line(call));
            index += 1;
        }
    }

    if cell.calls.len() > 12 {
        detail.push(Line::from(Span::styled(
            format!("… +{} more calls", cell.calls.len() - 12),
            dim_style(),
        )));
    }

    lines.extend(prefixed_detail_lines(detail, max_width));
    lines
}

fn explored_call_line(call: &ExploredCallActivityCell) -> Line<'static> {
    let tool_name = call.tool_name.trim();
    let summary = call.summary.trim();
    match explored_call_action(call) {
        Some(ExploredCallUiAction::Read) => coding_action_line("Read", explored_read_target(call)),
        Some(ExploredCallUiAction::Search) => {
            coding_action_line("Search", explored_search_target(call))
        }
        Some(ExploredCallUiAction::List) => coding_action_line(
            "List",
            call.target
                .as_deref()
                .map(compact_coding_summary_path)
                .unwrap_or_else(|| summary.to_string()),
        ),
        Some(ExploredCallUiAction::Run) => coding_action_line(
            "Run",
            call.target
                .as_deref()
                .map(ToString::to_string)
                .unwrap_or_else(|| summary.to_string()),
        ),
        None if tool_name == "Read" => {
            coding_action_line("Read", compact_coding_summary_path(summary))
        }
        None if tool_name == "Search" => {
            coding_action_line("Search", format_coding_search_summary(summary))
        }
        _ if summary.is_empty() => coding_action_line(tool_name, String::new()),
        _ => coding_action_line(tool_name, summary.to_string()),
    }
}

fn explored_call_action(call: &ExploredCallActivityCell) -> Option<ExploredCallUiAction> {
    call.action.clone().or_else(|| match call.tool_name.trim() {
        "Read" => Some(ExploredCallUiAction::Read),
        "Search" => Some(ExploredCallUiAction::Search),
        "List" => Some(ExploredCallUiAction::List),
        "Run" => Some(ExploredCallUiAction::Run),
        _ => None,
    })
}

fn explored_read_target(call: &ExploredCallActivityCell) -> String {
    call.target
        .as_deref()
        .map(compact_coding_summary_path)
        .unwrap_or_else(|| compact_coding_summary_path(&call.summary))
}

fn explored_search_target(call: &ExploredCallActivityCell) -> String {
    if let Some(query) = call.target.as_deref() {
        return match call.secondary_target.as_deref() {
            Some(path) if !path.trim().is_empty() => {
                format!("{} in {}", query.trim(), compact_coding_summary_path(path))
            }
            _ => query.trim().to_string(),
        };
    }
    format_coding_search_summary(&call.summary)
}

fn coding_action_line(title: &str, detail: String) -> Line<'static> {
    let mut spans = vec![Span::styled(
        title.to_string(),
        Style::default().fg(Color::Cyan),
    )];
    if !detail.is_empty() {
        spans.push(Span::raw(ACTIVITY_TITLE_GAP));
        spans.push(Span::raw(detail));
    }
    Line::from(spans)
}

fn format_coding_search_summary(summary: &str) -> String {
    let (query_part, path) = summary
        .rsplit_once(" in ")
        .map(|(query, path)| (query, Some(path)))
        .unwrap_or((summary, None));
    let query = strip_coding_result_count(query_part);
    match path {
        Some(path) => format!("{} in {}", query, compact_coding_summary_path(path)),
        None => query.to_string(),
    }
}

fn strip_coding_result_count(summary: &str) -> &str {
    summary
        .rsplit_once(" — ")
        .map(|(query, _)| query.trim())
        .unwrap_or_else(|| summary.trim())
}

fn compact_coding_summary_path(summary: &str) -> String {
    let target = summary
        .split_once(" -> ")
        .map(|(target, _)| target)
        .unwrap_or(summary)
        .trim();
    let path = target
        .split_once(":L")
        .map(|(path, _)| path)
        .unwrap_or(target);
    let path = path
        .split_once('#')
        .map(|(path, _)| path)
        .unwrap_or(path)
        .trim();
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn render_coding_edit_cell_lines(
    cell: &CodingEditActivityCell,
    max_width: u16,
) -> Vec<Line<'static>> {
    let visible_files = limit_patch_files(&cell.diff_files, 3);
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

    let mut lines = vec![codex_header(coding_edit_title(cell))];

    for (index, file) in visible_files.iter().enumerate() {
        if index > 0 {
            lines.push(Line::from(""));
        }
        let mut file_lines = if cell.diff_files.len() == 1 {
            Vec::new()
        } else {
            vec![render_patch_file_header(file)]
        };
        file_lines.extend(render_patch_file_diff_lines(
            file,
            diff_backgrounds,
            old_lineno_width,
            new_lineno_width,
            18,
        ));
        lines.extend(prefixed_detail_lines(file_lines, max_width));
    }
    if cell.diff_files.len() > visible_files.len() {
        lines.extend(prefixed_detail_lines(
            vec![Line::from(Span::styled(
                format!(
                    "… {} more files",
                    cell.diff_files.len() - visible_files.len()
                ),
                dim_style(),
            ))],
            max_width,
        ));
    }

    lines
}

fn coding_edit_title(cell: &CodingEditActivityCell) -> String {
    if let [file] = cell.diff_files.as_slice() {
        return patch_single_file_title(file);
    }

    if !cell.diff_files.is_empty() {
        let file_noun = if cell.diff_files.len() == 1 {
            "File"
        } else {
            "Files"
        };
        return format!("Edited {} {}", cell.diff_files.len(), file_noun);
    }

    if let Some(file) = cell.file.as_deref().filter(|file| !file.trim().is_empty()) {
        return format!(
            "Edited {} (+{} -{})",
            file, cell.added_lines, cell.removed_lines
        );
    }

    format!(
        "Edited Code (+{} -{})",
        cell.added_lines, cell.removed_lines
    )
}

fn render_terminal_wait_cell_lines(
    cell: &TerminalWaitActivityCell,
    max_width: u16,
) -> Vec<Line<'static>> {
    render_wait_activity_lines(&cell.title, &cell.body_lines, 6, max_width)
}

fn render_error_cell_lines(cell: &ErrorActivityCell, max_width: u16) -> Vec<Line<'static>> {
    render_error_lines(&cell.title, &cell.body_lines, 12, max_width)
}

fn render_warning_cell_lines(cell: &ErrorActivityCell, max_width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "⚠",
            Style::default()
                .fg(Color::LightYellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(ACTIVITY_TITLE_GAP),
        Span::styled(
            cell.title.clone(),
            Style::default()
                .fg(Color::LightYellow)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    lines.extend(prefixed_body_lines(
        cell.body_lines
            .iter()
            .take(12)
            .map(|line| {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::Gray),
                ))
            })
            .collect(),
        max_width,
    ));
    lines
}

fn render_browser_cell_lines(cell: &BrowserActivityCell, max_width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![codex_header(format!(
        "Captured URL: {}",
        cell.url
            .as_deref()
            .map(compact_browser_url)
            .unwrap_or_else(|| "unknown".to_string())
    ))];
    let mut stats = Vec::new();
    if let Some(line_count) = cell.line_count {
        stats.push(format!("{line_count} lines"));
    }
    if let Some(ref_count) = cell.ref_count {
        stats.push(format!("{ref_count} refs"));
    }
    if !stats.is_empty() {
        lines.extend(prefixed_detail_lines(
            vec![Line::from(Span::styled(
                stats.join(" · "),
                Style::default().fg(Color::Gray),
            ))],
            max_width,
        ));
    }
    lines
}

fn render_live_browser_cell_lines(
    cell: &LiveBrowserActivityCell,
    max_width: u16,
) -> Vec<Line<'static>> {
    let title = cell
        .url
        .as_deref()
        .map(|url| format!("Opening URL: {}", compact_browser_url(url)))
        .unwrap_or_else(|| cell.title.clone());
    let mut lines = vec![codex_header_with_marker(
        activity_marker(None),
        title,
        bold_style(),
    )];
    lines.extend(prefixed_detail_lines(
        cell.body_lines
            .iter()
            .take(1)
            .cloned()
            .map(|line| Line::from(Span::styled(line, Style::default().fg(Color::Gray))))
            .collect(),
        max_width,
    ));
    lines
}

fn render_web_search_cell_lines(
    cell: &WebSearchActivityCell,
    max_width: u16,
) -> Vec<Line<'static>> {
    let title = match cell.action {
        WebSearchUiAction::Searching => format!("Searching the web: {}", cell.query),
        WebSearchUiAction::Searched => format!("Searched the web: {}", cell.query),
    };
    let marker = match cell.action {
        WebSearchUiAction::Searching => activity_marker(None),
        WebSearchUiAction::Searched => Span::styled(ACTIVITY_BULLET, dim_style()),
    };
    let mut lines = vec![codex_header_with_marker(marker, title, bold_style())];
    let mut detail = Vec::new();
    if let Some(url) = cell.url.as_deref() {
        detail.push(Line::from(Span::styled(
            compact_browser_url(url),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::UNDERLINED),
        )));
    }
    detail.extend(cell.body_lines.iter().take(4).map(|line| {
        Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(Color::Gray),
        ))
    }));
    lines.extend(prefixed_detail_lines(detail, max_width));
    lines
}

fn render_exec_cell_lines(cell: &ExecResultActivityCell, max_width: u16) -> Vec<Line<'static>> {
    let output = cell.command_output();
    let exit_code = output.meta.exit_code;
    let running = output.meta.is_running();
    let indicator_style = if running {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else if exit_code == Some(0) {
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
    let mut lines = command_header_lines_from_line(
        exec_header_title(cell, &output.meta),
        highlighted_shell_command_line(&output.command),
        indicator_style,
        max_width,
    );
    lines.extend(exec_output_detail_lines(
        &cell.output_lines,
        "(no output)",
        max_width,
    ));
    lines
}

fn exec_header_title(cell: &ExecResultActivityCell, meta: &TerminalExecutionMeta) -> &'static str {
    if meta.is_running() {
        return "Running";
    }
    match cell.terminal_action {
        Some(TerminalUiAction::Execute) if cell.terminal_origin == Some(TerminalUiOrigin::User) => {
            "You ran"
        }
        Some(TerminalUiAction::Continue) => "Continued",
        Some(TerminalUiAction::Poll) => "Checked",
        Some(TerminalUiAction::Terminate) => "Terminated",
        Some(TerminalUiAction::Execute) | None => "Ran",
    }
}

fn exit_marker_line(exit_code: i32) -> Line<'static> {
    let (marker, style) = if exit_code == 0 {
        (
            "✓",
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            "✗",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    };
    Line::from(vec![
        Span::styled(marker, style),
        Span::raw(format!(" exit={exit_code}")),
    ])
}

fn render_live_exec_cell_lines(cell: &LiveExecActivityCell, max_width: u16) -> Vec<Line<'static>> {
    let mut lines = command_header_lines_with_marker(
        activity_marker(cell.started_at_ms),
        "Running",
        highlighted_shell_command_line(&cell.title),
        max_width,
    );
    lines.extend(exec_output_detail_lines(
        &cell.output_lines,
        "running...",
        max_width,
    ));
    lines
}

fn highlighted_shell_command_line(command: &str) -> Line<'static> {
    highlight_shell_command(command)
        .map(Line::from)
        .unwrap_or_else(|| Line::from(Span::raw(command.to_string())))
}

fn render_patch_cell_lines(cell: &PatchActivityCell, max_width: u16) -> Vec<Line<'static>> {
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
    let mut lines = if let [file] = cell.files.as_slice() {
        vec![codex_header(patch_single_file_title(file))]
    } else {
        let file_noun = if cell.files.len() == 1 {
            "File"
        } else {
            "Files"
        };
        vec![codex_header(format!(
            "Edited {} {}",
            cell.files.len(),
            file_noun
        ))]
    };
    for (index, file) in visible_files.iter().enumerate() {
        if index > 0 {
            lines.push(Line::from(""));
        }
        let mut file_lines = if cell.files.len() == 1 {
            Vec::new()
        } else {
            vec![render_patch_file_header(file)]
        };
        file_lines.extend(render_patch_file_diff_lines(
            file,
            diff_backgrounds,
            old_lineno_width,
            new_lineno_width,
            18,
        ));
        lines.extend(prefixed_detail_lines(file_lines, max_width));
    }
    if cell.files.len() > visible_files.len() {
        lines.extend(prefixed_detail_lines(
            vec![Line::from(Span::styled(
                format!("… {} more files", cell.files.len() - visible_files.len()),
                dim_style(),
            ))],
            max_width,
        ));
    }
    lines
}

fn render_telegram_cell_lines(cell: &TelegramActivityCell, max_width: u16) -> Vec<Line<'static>> {
    render_message_activity_lines(
        &cell.title,
        &cell.detail_lines,
        &cell.message_lines,
        6,
        6,
        false,
        max_width,
    )
}

fn render_reply_cell_lines(cell: &ReplyActivityCell, max_width: u16) -> Vec<Line<'static>> {
    if cell.disposition == crate::tool_ui::ReplyDisposition::Resolved
        && cell.subject == crate::tool_ui::ReplySubject::Message
    {
        return render_agent_message_reply_lines(&cell.message_lines, max_width);
    }

    let (title, color) = match cell.disposition {
        crate::tool_ui::ReplyDisposition::Resolved => {
            (resolved_reply_title(cell), Color::LightGreen)
        }
        crate::tool_ui::ReplyDisposition::Dismissed => ("Dismissed", Color::DarkGray),
        crate::tool_ui::ReplyDisposition::Failed => ("Failed", Color::Red),
    };
    let mut lines = vec![codex_header_with_styles(
        title,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )];
    if !cell.message_lines.is_empty() {
        let joined = cell.message_lines.join("\n");
        let md_lines =
            render_markdown_with_width(&joined, Color::White, detail_markdown_width(max_width));
        lines.extend(prefixed_detail_lines(md_lines, max_width));
    }
    lines
}

fn render_agent_message_reply_lines(
    message_lines: &[String],
    max_width: u16,
) -> Vec<Line<'static>> {
    if message_lines.is_empty() {
        return Vec::new();
    }

    let joined = message_lines.join("\n");
    let mut lines_iter = joined.trim_end_matches(['\r', '\n']).lines();
    let title = lines_iter.next().unwrap_or_default().to_string();
    let body = lines_iter.collect::<Vec<_>>().join("\n");
    let mut lines = vec![codex_header(title)];
    if !body.trim().is_empty() {
        let md_lines =
            render_markdown_with_width(&body, Color::White, body_markdown_width(max_width));
        lines.extend(prefixed_body_lines(md_lines, max_width));
    }
    lines
}

fn resolved_reply_title(cell: &ReplyActivityCell) -> &'static str {
    match cell.subject {
        crate::tool_ui::ReplySubject::Message => "Resolved Message",
        crate::tool_ui::ReplySubject::Notice => "Resolved Notice",
    }
}

fn render_plan_cell_lines(cell: &PlanActivityCell, max_width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![plan_header_line(cell.kind)];
    if let Some(explanation) = cell.explanation.as_deref()
        && !explanation.trim().is_empty()
    {
        let note_lines = render_markdown_with_width(
            explanation.trim(),
            Color::Gray,
            detail_markdown_width(max_width),
        )
        .into_iter()
        .map(|mut line| {
            line.spans = line
                .spans
                .into_iter()
                .map(|span| Span::styled(span.content.to_string(), span.style.fg(Color::Gray)))
                .collect();
            line
        })
        .collect::<Vec<_>>();
        lines.extend(prefixed_detail_lines(note_lines, max_width));
    }
    let mut steps = Vec::new();
    if cell.steps.is_empty() {
        steps.push(Line::from(Span::styled(
            "No active plan.",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for step in cell.steps.iter().take(8) {
            let (marker, marker_style, text_style) = match step.status {
                PlanStepDisplayStatus::InProgress => (
                    "□ ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                PlanStepDisplayStatus::Pending => (
                    "□ ",
                    Style::default().fg(Color::DarkGray),
                    Style::default().fg(Color::Gray),
                ),
                PlanStepDisplayStatus::Completed => (
                    "✔ ",
                    Style::default().fg(Color::DarkGray),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::CROSSED_OUT),
                ),
            };
            steps.push(Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(step.text.clone(), text_style),
            ]));
        }
    }
    lines.extend(prefixed_detail_lines(steps, max_width));
    lines
}

fn plan_header_line(kind: PlanUiKind) -> Line<'static> {
    match kind {
        PlanUiKind::Proposed => codex_header_with_styles(
            "Proposed Plan",
            dim_style().bg(Color::Rgb(42, 47, 55)),
            bold_style().bg(Color::Rgb(42, 47, 55)),
        ),
        PlanUiKind::Updated => codex_header("Updated Plan"),
    }
}

fn render_create_primitive_spec_cell_lines(
    cell: &CreatePrimitiveSpecActivityCell,
) -> Vec<Line<'static>> {
    render_primitive_line(format!("Created Primitive Spec: {}", cell.primitive_id))
}

fn render_activate_primitive_cell_lines(
    cell: &ActivatePrimitiveActivityCell,
) -> Vec<Line<'static>> {
    render_primitive_line(format!("Activated Primitive: {}", cell.primitive_id))
}

fn render_primitive_line(title: String) -> Vec<Line<'static>> {
    vec![codex_header(title)]
}

fn render_text_activity_lines(
    title: &str,
    body_lines: &[String],
    limit: usize,
    markdown: bool,
    max_width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![codex_header(title.to_string())];

    if markdown && !body_lines.is_empty() {
        let joined = body_lines
            .iter()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let md_lines =
            render_markdown_with_width(&joined, Color::Gray, body_markdown_width(max_width));
        lines.extend(prefixed_body_lines(md_lines, max_width));
    } else {
        lines.extend(prefixed_body_lines(
            body_lines
                .iter()
                .take(limit)
                .map(|line| {
                    Line::from(Span::styled(
                        line.to_string(),
                        Style::default().fg(Color::Gray),
                    ))
                })
                .collect(),
            max_width,
        ));
    }
    lines
}

#[allow(clippy::too_many_arguments)]
fn render_message_activity_lines(
    title: &str,
    detail_lines: &[String],
    message_lines: &[String],
    detail_limit: usize,
    message_limit: usize,
    markdown: bool,
    max_width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![codex_header(title.to_string())];
    lines.extend(prefixed_body_lines(
        detail_lines
            .iter()
            .take(detail_limit)
            .map(|line| {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::Gray),
                ))
            })
            .collect(),
        max_width,
    ));

    if markdown && !message_lines.is_empty() {
        let joined = message_lines
            .iter()
            .take(message_limit)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let md_lines =
            render_markdown_with_width(&joined, Color::White, detail_markdown_width(max_width));
        lines.extend(prefixed_detail_lines(md_lines, max_width));
    } else {
        lines.extend(prefixed_detail_lines(
            message_lines
                .iter()
                .take(message_limit)
                .map(|line| {
                    Line::from(Span::styled(
                        line.to_string(),
                        Style::default().fg(Color::White),
                    ))
                })
                .collect(),
            max_width,
        ));
    }
    lines
}

fn render_wait_activity_lines(
    title: &str,
    body_lines: &[String],
    limit: usize,
    max_width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![codex_header(title.to_string())];
    lines.extend(prefixed_detail_lines(
        body_lines
            .iter()
            .take(limit)
            .map(|line| {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::Gray),
                ))
            })
            .collect(),
        max_width,
    ));
    lines
}

fn render_error_lines(
    title: &str,
    body_lines: &[String],
    limit: usize,
    max_width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "■",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(ACTIVITY_TITLE_GAP),
        Span::styled(
            title.to_string(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    ])];
    lines.extend(prefixed_body_lines(
        body_lines
            .iter()
            .take(limit)
            .map(|line| {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::LightRed),
                ))
            })
            .collect(),
        max_width,
    ));
    lines
}

fn limit_patch_files(files: &[PatchFileUiData], limit: usize) -> Vec<PatchFileUiData> {
    if files.len() <= limit {
        return files.to_vec();
    }
    files.iter().take(limit).cloned().collect()
}

fn render_coding_review_cell_lines(cell: &CodingReviewActivityCell) -> Vec<Line<'static>> {
    let title = if cell.title.trim().is_empty() {
        "Review".to_string()
    } else {
        cell.title.clone()
    };
    vec![codex_header(title)]
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

fn patch_single_file_title(file: &PatchFileUiData) -> String {
    format!(
        "Edited {} (+{} -{})",
        file.path, file.added_lines, file.removed_lines
    )
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
    _diff_backgrounds: DiffScopeBackgrounds,
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
            Some(PATCH_DIFF_DELETE_BACKGROUND),
        ),
        PatchDiffLineKind::Add => (
            "+",
            Style::default().fg(Color::LightGreen),
            Some(PATCH_DIFF_ADD_BACKGROUND),
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
            patch_diff_style(Style::default().fg(Color::DarkGray), background),
        ),
        Span::styled(" ", patch_diff_style(Style::default(), background)),
        Span::styled(
            format!("{new_lineno:>new_width$}", new_width = new_lineno_width),
            patch_diff_style(Style::default().fg(Color::DarkGray), background),
        ),
        Span::styled(" ", patch_diff_style(Style::default(), background)),
        Span::styled(
            gutter,
            patch_diff_style(text_style.add_modifier(Modifier::BOLD), background),
        ),
        Span::styled(" ", patch_diff_style(Style::default(), background)),
    ];
    if let Some(highlighted_spans) = highlighted_spans {
        for span in highlighted_spans {
            let style = patch_diff_style(span.style, background);
            spans.push(Span::styled(span.content.to_string(), style));
        }
    } else {
        let style = patch_diff_style(text_style, background);
        spans.push(Span::styled(line.text.clone(), style));
    }
    let mut line = Line::from(spans);
    line.style = patch_diff_style(Style::default(), background);
    line
}

fn patch_diff_style(style: Style, background: Option<Color>) -> Style {
    match background {
        Some(color) => style.bg(color),
        None => style,
    }
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

fn exec_output_detail_lines(
    output_lines: &[String],
    empty_text: &str,
    max_width: u16,
) -> Vec<Line<'static>> {
    let logical_lines = if output_lines.is_empty() {
        vec![Line::from(Span::styled(
            empty_text.to_string(),
            dim_style(),
        ))]
    } else {
        output_lines
            .iter()
            .map(|line| {
                let style = if line.starts_with("… +") {
                    dim_style()
                } else {
                    Style::default().fg(Color::Gray)
                };
                Line::from(Span::styled(line.to_string(), style))
            })
            .collect()
    };
    let prefixed = prefixed_detail_lines(logical_lines, max_width);
    truncate_prefixed_lines_middle(prefixed, 4, 4)
}

fn truncate_prefixed_lines_middle(
    lines: Vec<Line<'static>>,
    head: usize,
    tail: usize,
) -> Vec<Line<'static>> {
    if lines.len() <= head + tail + 1 {
        return lines;
    }
    let omitted = lines.len().saturating_sub(head + tail);
    let mut result = Vec::new();
    result.extend(lines.iter().take(head).cloned());
    result.push(Line::from(vec![
        Span::raw(DETAIL_SUBSEQUENT_PREFIX),
        Span::styled(
            format!("… +{omitted} lines (ctrl + t to view transcript)"),
            dim_style(),
        ),
    ]));
    result.extend(lines.iter().skip(lines.len().saturating_sub(tail)).cloned());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_ui::{PatchDiffLineKind, PatchFileOperation, ReplyDisposition, ReplySubject};

    /// Verify that fenced code blocks inside an assistant cell hide their
    /// delimiters while preserving syntax-highlighted code spans.
    #[test]
    fn assistant_cell_renders_code_block_with_code_style() {
        let body = "\
Here is some code:

```rust
fn main() {
    println!(\"hello\");
}
```

That's it.";
        let cell = AssistantActivityCell {
            title: "Here is some code:".to_string(),
            body_lines: body
                .lines()
                .skip(1)
                .take(8)
                .map(|s| s.to_string())
                .collect(),
            full_body: Some(body.to_string()),
            rich_mode: true,
        };
        let lines = render_assistant_cell_lines(&cell, 80);

        // ── Fence line(s) are hidden ───────────────────────────
        let fence_lines: Vec<_> = lines
            .iter()
            .filter(|line| line_text(line).trim_start().starts_with("```"))
            .collect();
        assert!(
            fence_lines.is_empty(),
            "expected fence delimiter lines to be hidden, got {:?}",
            fence_lines
        );

        // ── Code content is syntax‑highlighted ─────────────────
        let code_line = lines
            .iter()
            .find(|line| line_text(line).contains("fn main()"));
        assert!(
            code_line.is_some(),
            "expected to find code line 'fn main()' in rendered output"
        );
        let code_line = code_line.unwrap();

        // Skip "   " indentation span; syntect‑coloured spans follow.
        let code_span = code_line
            .spans
            .iter()
            .find(|s| s.style.fg.is_some())
            .expect("at least one span in the code line should have syntect fg");
        assert!(
            code_span.style.fg.is_some(),
            "code span '{}' should have syntect fg, got None",
            code_span.content
        );

        // At least 2 distinct colours → syntax highlighting active.
        let unique_fgs: std::collections::HashSet<Color> =
            code_line.spans.iter().filter_map(|s| s.style.fg).collect();
        assert!(
            unique_fgs.len() >= 2,
            "code block should have >= 2 distinct colours, got {:?}",
            unique_fgs
        );

        // ── Plain text is White (base_color) ────────────────────
        let plain_text = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.contains("That's it"))
            .and_then(|span| span.style.fg);
        assert!(
            plain_text.is_some(),
            "expected to find plain text line in rendered output"
        );
        assert_eq!(
            plain_text,
            Some(Color::White),
            "plain text should have White fg (base_color)"
        );
    }

    #[test]
    fn thinking_cell_renders_body_as_markdown() {
        let collapsed = ThinkingActivityCell {
            title: "Thinking".to_string(),
            body_lines: vec!["Some `code` and **bold** text".to_string()],
            full_body: Some("Full body with `details`".to_string()),
            expanded: false,
        };
        let collapsed_lines = render_thinking_cell_lines(&collapsed, 80);
        let code_span = collapsed_lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.as_ref() == "code")
            .expect("inline code should be rendered as a separate markdown span");
        assert_eq!(code_span.style.fg, Some(Color::Yellow));
        assert!(collapsed_lines.iter().skip(1).all(|line| {
            line.spans
                .first()
                .is_some_and(|span| span.content.as_ref() == ACTIVITY_BODY_INDENT)
        }));

        let expanded = ThinkingActivityCell {
            title: "Thinking".to_string(),
            body_lines: vec!["Preview only".to_string()],
            full_body: Some("Expanded body with `details`".to_string()),
            expanded: true,
        };
        let expanded_rendered = render_thinking_cell_lines(&expanded, 80)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();
        assert!(
            expanded_rendered
                .iter()
                .any(|line| line.contains("Expanded body with details"))
        );
        assert!(
            !expanded_rendered
                .iter()
                .any(|line| line.contains("Preview only"))
        );
    }

    #[test]
    fn thinking_cell_wraps_multiline_body_with_bar_prefix() {
        let cell = ThinkingActivityCell {
            title: "Thinking".to_string(),
            body_lines: vec![
                "Planning code reviews".to_string(),
                String::new(),
                "I need to keep going and review the events before finalizing everything. I should probably run git status, and then commit and push.".to_string(),
            ],
            full_body: None,
            expanded: false,
        };

        let rendered = render_thinking_cell_lines(&cell, 34)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();

        assert!(
            rendered.len() > 4,
            "long thinking body should be pre-wrapped into multiple prefixed lines: {rendered:?}"
        );
        assert!(
            rendered
                .iter()
                .skip(1)
                .all(|line| line.starts_with(ACTIVITY_BODY_INDENT)),
            "every body and continuation line should keep the Codex body prefix: {rendered:?}"
        );
        assert!(
            rendered
                .iter()
                .skip(1)
                .all(|line| line_display_width(line) <= 34),
            "pre-wrapped thinking lines should fit the requested width: {rendered:?}"
        );
    }

    #[test]
    fn thinking_cell_wraps_wide_unicode_with_aligned_prefix() {
        let cell = ThinkingActivityCell {
            title: "Thinking".to_string(),
            body_lines: vec![
                "处理中文对齐问题需要按终端显示宽度换行，否则左侧边线会错位".to_string(),
                "English text followed by 中文字符 should still align".to_string(),
            ],
            full_body: None,
            expanded: false,
        };

        let rendered = render_thinking_cell_lines(&cell, 34)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();

        assert!(
            rendered.len() > 3,
            "wide unicode thinking body should be wrapped: {rendered:?}"
        );
        assert!(
            rendered
                .iter()
                .skip(1)
                .all(|line| line.starts_with(ACTIVITY_BODY_INDENT)),
            "every wide-unicode continuation line should keep the Codex body prefix: {rendered:?}"
        );
        assert!(
            rendered
                .iter()
                .skip(1)
                .all(|line| line_display_width(line) <= 34),
            "wide-unicode thinking lines should fit the requested terminal width: {rendered:?}"
        );
    }

    #[test]
    fn explored_renders_codex_style_exploration_lines() {
        let cell = ExploredActivityCell {
            stable_id: "explored".to_string(),
            title: "Explored".to_string(),
            calls: vec![
                ExploredCallActivityCell {
                    tool_name: "Read".to_string(),
                    action: Some(ExploredCallUiAction::Read),
                    target: Some("src/dashboard/mod.rs".to_string()),
                    secondary_target: None,
                    summary: "src/dashboard/mod.rs:L1268+83".to_string(),
                    detail_lines: vec!["83 lines".to_string()],
                    detail_title: None,
                },
                ExploredCallActivityCell {
                    tool_name: "Read".to_string(),
                    action: Some(ExploredCallUiAction::Read),
                    target: Some("src/dashboard/mod.rs".to_string()),
                    secondary_target: None,
                    summary: "src/dashboard/mod.rs:L286+25".to_string(),
                    detail_lines: vec!["25 lines".to_string()],
                    detail_title: None,
                },
                ExploredCallActivityCell {
                    tool_name: "Search".to_string(),
                    action: Some(ExploredCallUiAction::Search),
                    target: Some(
                        "push_command_input_display_text|command_input_display_text|display_text"
                            .to_string(),
                    ),
                    secondary_target: Some("src/dashboard/mod.rs".to_string()),
                    summary: "push_command_input_display_text|command_input_display_text|display_text — 3 targets in src/dashboard/mod.rs".to_string(),
                    detail_lines: Vec::new(),
                    detail_title: None,
                },
                ExploredCallActivityCell {
                    tool_name: "Read".to_string(),
                    action: Some(ExploredCallUiAction::Read),
                    target: Some("src/dashboard/mod.rs".to_string()),
                    secondary_target: None,
                    summary: "src/dashboard/mod.rs".to_string(),
                    detail_lines: Vec::new(),
                    detail_title: None,
                },
            ],
        };

        let rendered = render_explored_cell_lines(&cell, 80)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();

        assert_eq!(
            rendered,
            vec![
                "• Explored",
                "  └ Read mod.rs",
                "    Search push_command_input_display_text|command_input_display_text|display_te",
                "    xt in mod.rs",
                "    Read mod.rs",
            ]
        );
        assert!(
            rendered.iter().all(|line| !line.contains("Read×")),
            "exploration should not render an aggregate action count: {rendered:?}"
        );
        assert!(
            rendered.iter().all(|line| !line.contains("lines")),
            "exploration should not render read line-count details: {rendered:?}"
        );
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    fn line_display_width(line: &str) -> usize {
        line.chars()
            .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
            .sum()
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

        let lines = render_patch_cell_lines(&cell, 80);
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>();

        assert!(
            rendered
                .iter()
                .any(|line| line.contains("• Edited src/app.rs (+1 -1)"))
        );
        assert!(
            rendered
                .iter()
                .all(|line| !line.contains("  └ src/app.rs (+1 -1)")),
            "single-file patches should omit the repeated file header: {rendered:?}"
        );
        assert!(rendered.iter().any(|line| line.contains("1 1   fn main()")));
        assert!(rendered.iter().any(|line| line.contains("2   -")));
        assert!(rendered.iter().any(|line| line.contains(" 2 +")));
    }

    #[test]
    fn coding_edit_cell_renders_compact_diff_without_internal_details() {
        let cell = CodingEditActivityCell {
            stable_id: "edit-1".to_string(),
            title: "Code Edit".to_string(),
            selector: "hash-anchored edit".to_string(),
            file: Some("src/app.rs".to_string()),
            added_lines: 1,
            removed_lines: 1,
            propagation_count: 7,
            impact_lines: vec!["src/app.rs::run - propagation review".to_string()],
            diff_files: vec![PatchFileUiData {
                path: "src/app.rs".to_string(),
                operation: PatchFileOperation::Update,
                added_lines: 1,
                removed_lines: 1,
                diff_lines: vec![
                    PatchDiffLineUiData {
                        kind: PatchDiffLineKind::Delete,
                        old_lineno: Some(1),
                        new_lineno: None,
                        text: "old();".to_string(),
                    },
                    PatchDiffLineUiData {
                        kind: PatchDiffLineKind::Add,
                        old_lineno: None,
                        new_lineno: Some(1),
                        text: "new();".to_string(),
                    },
                ],
            }],
        };

        let lines = render_coding_edit_cell_lines(&cell, 80);
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>();

        assert!(
            rendered
                .iter()
                .any(|line| line.contains("• Edited src/app.rs (+1 -1)"))
        );
        assert!(rendered.iter().any(|line| line.contains("1   - old();")));
        assert!(rendered.iter().any(|line| line.contains(" 1 + new();")));
        assert!(
            rendered.iter().all(|line| !line.contains("hash-anchored")),
            "selector should not be visible in compact edit cell: {rendered:?}"
        );
        assert!(
            rendered.iter().all(|line| !line.contains("propagation")),
            "review impact should not be visible in compact edit cell: {rendered:?}"
        );
        assert!(
            rendered.iter().all(|line| !line.contains("Impact")),
            "impact heading should not be visible in compact edit cell: {rendered:?}"
        );
        for pattern in ["1   - old();", " 1 + new();"] {
            let line = lines
                .iter()
                .find(|line| line_text(line).contains(pattern))
                .expect("changed line should be rendered");
            assert!(
                line.style.bg.is_some() || line.spans.iter().any(|span| span.style.bg.is_some()),
                "changed line should keep diff background: {:?}",
                line
            );
        }

        let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 12));
        let mut cache = CachedActivityLines::new();
        render_activity_feed_cached(
            &mut buffer,
            Rect::new(0, 0, 80, 12),
            &[ActivityCell::CodingEdit(cell)],
            &[],
            0,
            &mut cache,
            0,
        );

        for (pattern, expected_bg) in [
            ("old();", PATCH_DIFF_DELETE_BACKGROUND),
            ("new();", PATCH_DIFF_ADD_BACKGROUND),
        ] {
            let y = (0..buffer.area.height)
                .find(|y| buffer_row_text(&buffer, *y).contains(pattern))
                .expect("changed row should be rendered into buffer");
            assert!(
                (0..buffer.area.width).any(|x| buffer
                    .cell((x, y))
                    .is_some_and(|cell| cell.bg == expected_bg)),
                "changed buffer row should keep diff background: {}",
                buffer_row_text(&buffer, y)
            );
        }
    }

    fn buffer_row_text(buffer: &Buffer, y: u16) -> String {
        let mut out = String::new();
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
        out
    }

    #[test]
    fn exec_output_truncation_points_to_transcript() {
        let output_lines = (1..=20)
            .map(|index| format!("output line {index}"))
            .collect::<Vec<_>>();
        let rendered = exec_output_detail_lines(&output_lines, "(no output)", 80)
            .into_iter()
            .map(|line| line_text(&line))
            .collect::<Vec<_>>();

        assert!(
            rendered
                .iter()
                .any(|line| line.contains("ctrl + t to view transcript")),
            "truncated exec output should point to transcript mode: {rendered:?}"
        );
    }

    #[test]
    fn transcript_includes_exec_command_output_and_exit_marker() {
        let transcript = activity_transcript_text(
            &[ActivityCell::ExecResult(ExecResultActivityCell {
                title: "cargo check".to_string(),
                terminal_action: Some(TerminalUiAction::Execute),
                terminal_origin: None,
                meta: Some("exit=0 cwd=C:/repo".to_string()),
                output_lines: vec!["Finished dev profile".to_string()],
            })],
            &[],
        );

        assert!(transcript.contains("$ cargo check"));
        assert!(transcript.contains("Finished dev profile"));
        assert!(transcript.contains("✓ exit=0"));
    }

    #[test]
    fn transcript_lines_preserve_styled_exec_content() {
        let lines = activity_transcript_lines(
            &[ActivityCell::ExecResult(ExecResultActivityCell {
                title: "cargo check".to_string(),
                terminal_action: Some(TerminalUiAction::Execute),
                terminal_origin: None,
                meta: Some("exit=0 cwd=C:/repo".to_string()),
                output_lines: vec!["Finished dev profile".to_string()],
            })],
            &[],
            80,
        );

        let command_line = lines
            .iter()
            .find(|line| line_text(line).contains("$ cargo check"))
            .expect("transcript should include shell command");
        assert!(
            command_line
                .spans
                .iter()
                .any(|span| span.style.fg.is_some()),
            "shell command transcript should preserve styled spans: {command_line:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line_text(line).contains("✓ exit=0")),
            "transcript should include styled exit marker"
        );
    }

    #[test]
    fn exec_header_reflects_terminal_action_and_running_status() {
        let continued = render_exec_cell_lines(
            &ExecResultActivityCell {
                title: "stdin".to_string(),
                terminal_action: Some(TerminalUiAction::Continue),
                terminal_origin: None,
                meta: Some("main  exited  exit=0  cwd=C:/repo".to_string()),
                output_lines: Vec::new(),
            },
            80,
        )
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();
        assert_eq!(
            continued.first().map(String::as_str),
            Some("• Continued stdin")
        );

        let running = render_exec_cell_lines(
            &ExecResultActivityCell {
                title: "cargo test".to_string(),
                terminal_action: Some(TerminalUiAction::Execute),
                terminal_origin: None,
                meta: Some("main  running  exit=-  cwd=C:/repo".to_string()),
                output_lines: Vec::new(),
            },
            80,
        )
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();
        assert_eq!(
            running.first().map(String::as_str),
            Some("• Running cargo test")
        );

        let user_command = render_exec_cell_lines(
            &ExecResultActivityCell {
                title: "ls -la".to_string(),
                terminal_action: Some(TerminalUiAction::Execute),
                terminal_origin: Some(TerminalUiOrigin::User),
                meta: Some("main  exited  exit=0  cwd=C:/repo".to_string()),
                output_lines: Vec::new(),
            },
            80,
        )
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();
        assert_eq!(
            user_command.first().map(String::as_str),
            Some("• You ran ls -la")
        );
    }

    #[test]
    fn error_activity_cell_uses_codex_error_marker() {
        let rendered = render_error_cell_lines(
            &ErrorActivityCell {
                title: "Command failed".to_string(),
                body_lines: vec!["exit=1".to_string()],
            },
            80,
        )
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();

        assert_eq!(
            rendered.first().map(String::as_str),
            Some("■ Command failed")
        );
    }

    #[test]
    fn warning_activity_cell_uses_warning_marker() {
        let rendered = render_warning_cell_lines(
            &ErrorActivityCell {
                title: "Config drift detected".to_string(),
                body_lines: vec!["using fallback".to_string()],
            },
            80,
        )
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();

        assert_eq!(
            rendered.first().map(String::as_str),
            Some("⚠ Config drift detected")
        );
    }

    #[test]
    fn web_search_activity_cell_renders_searching_and_searched_states() {
        let searching = render_web_search_cell_lines(
            &WebSearchActivityCell {
                action: WebSearchUiAction::Searching,
                query: "ratatui hyperlinks".to_string(),
                url: None,
                body_lines: Vec::new(),
            },
            80,
        )
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();
        assert!(
            searching
                .first()
                .is_some_and(|line| line.contains("Searching the web: ratatui hyperlinks")),
            "searching web cell should include the query: {searching:?}"
        );

        let searched = render_web_search_cell_lines(
            &WebSearchActivityCell {
                action: WebSearchUiAction::Searched,
                query: "ratatui hyperlinks".to_string(),
                url: Some("https://docs.rs/ratatui".to_string()),
                body_lines: vec!["found docs".to_string()],
            },
            80,
        )
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();
        assert!(
            searched
                .iter()
                .any(|line| line.contains("https://docs.rs/ratatui")),
            "searched web cell should include the result url: {searched:?}"
        );
    }

    #[test]
    fn final_message_separator_renders_worked_duration() {
        let rendered = render_activity_cell_lines(
            &ActivityCell::FinalMessageSeparator(FinalMessageSeparatorActivityCell {
                elapsed_seconds: Some(17 * 60 + 44),
            }),
            80,
        )
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();

        assert_eq!(rendered.len(), 1);
        assert!(rendered[0].starts_with("─ Worked for 17m 44s ─"));
        assert_eq!(display_width(&rendered[0]), 80);
    }

    #[test]
    fn plan_activity_cell_renders_explanation_and_empty_state() {
        let rendered = render_plan_cell_lines(
            &PlanActivityCell {
                kind: PlanUiKind::Updated,
                explanation: Some("Need to finish the setup first.".to_string()),
                steps: Vec::new(),
            },
            80,
        )
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();

        assert!(rendered.iter().any(|line| line.contains("Need to finish")));
        assert!(rendered.iter().any(|line| line.contains("No active plan.")));

        let transcript = activity_transcript_text(
            &[ActivityCell::PlanResult(PlanActivityCell {
                kind: PlanUiKind::Updated,
                explanation: Some("Need to finish the setup first.".to_string()),
                steps: Vec::new(),
            })],
            &[],
        );
        assert!(transcript.contains("note: Need to finish the setup first."));
        assert!(transcript.contains("(empty plan)"));

        let proposed = render_plan_cell_lines(
            &PlanActivityCell {
                kind: PlanUiKind::Proposed,
                explanation: None,
                steps: Vec::new(),
            },
            80,
        )
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();
        assert_eq!(
            proposed.first().map(String::as_str),
            Some("• Proposed Plan")
        );
    }

    #[test]
    fn reply_activity_cell_labels_notice_subjects() {
        let notice = render_reply_cell_lines(
            &ReplyActivityCell {
                disposition: ReplyDisposition::Resolved,
                subject: ReplySubject::Notice,
                message_lines: Vec::new(),
            },
            80,
        )
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();
        assert_eq!(
            notice.first().map(String::as_str),
            Some("• Resolved Notice")
        );
        assert!(notice.iter().any(|line| line.contains("Resolved Notice")));
    }

    #[test]
    fn user_activity_cell_renders_full_message() {
        let body = (1..=12)
            .map(|index| format!("[定位段 {index:03}] marker-{index:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let full_body = format!("Title\n{body}");
        let cell = UserActivityCell {
            title: "Title".to_string(),
            body_lines: body.lines().take(6).map(ToString::to_string).collect(),
            full_body: Some(full_body),
            image_attachments: Vec::new(),
        };

        let rendered = render_user_cell_lines(&cell, 80)
            .into_iter()
            .map(|line| line_text(&line))
            .collect::<Vec<_>>();

        assert_eq!(rendered.first().map(String::as_str), Some(""));
        assert_eq!(rendered.last().map(String::as_str), Some(""));
        assert!(rendered.iter().any(|line| line.contains("marker-012")));
    }

    #[test]
    fn user_activity_cell_renders_image_labels() {
        let cell = UserActivityCell {
            title: "inspect this".to_string(),
            body_lines: Vec::new(),
            full_body: None,
            image_attachments: vec![MessageImageAttachment {
                label: "dashboard screenshot".to_string(),
                uri: "C:/tmp/dashboard.png".to_string(),
                mime_type: "image/png".to_string(),
                description: None,
            }],
        };

        let rendered = render_user_cell_lines(&cell, 80)
            .into_iter()
            .map(|line| line_text(&line))
            .collect::<Vec<_>>();

        assert!(
            rendered
                .iter()
                .any(|line| line.contains("[1: local image] dashboard screenshot (image/png)")),
            "user image attachments should be visible in TUI user cells: {rendered:?}"
        );

        let transcript = activity_transcript_text(&[ActivityCell::User(cell)], &[]);
        assert!(transcript.contains("[1: local image] dashboard screenshot C:/tmp/dashboard.png"));
    }

    #[test]
    fn reply_activity_cell_renders_agent_message_with_activity_marker() {
        let message_lines = (1..=12)
            .map(|index| format!("[定位段 {index:03}] marker-{index:03}"))
            .collect::<Vec<_>>();

        let rendered = render_reply_cell_lines(
            &ReplyActivityCell {
                disposition: ReplyDisposition::Resolved,
                subject: ReplySubject::Message,
                message_lines,
            },
            80,
        )
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();

        assert!(
            rendered
                .first()
                .is_some_and(|line| line.starts_with("• [定位段 001] marker-001")),
            "resolved message reply should start with an agent activity marker, not a prompt marker: {rendered:?}"
        );
        let first_body_line = rendered
            .iter()
            .find(|line| line.contains("marker-002"))
            .expect("reply body should render subsequent message lines");
        assert!(
            first_body_line.starts_with(ACTIVITY_BODY_INDENT),
            "resolved message body should use the agent body indent: {rendered:?}"
        );
        assert!(
            rendered
                .iter()
                .all(|line| !line.starts_with(USER_PROMPT_PREFIX)),
            "resolved message reply should not render like a user prompt: {rendered:?}"
        );
        assert!(
            !first_body_line.starts_with("   "),
            "reply body should not keep the old three-space indent: {rendered:?}"
        );
        assert!(rendered.iter().any(|line| line.contains("marker-012")));
    }
}
