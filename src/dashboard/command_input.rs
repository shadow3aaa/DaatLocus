use crossterm::event::KeyModifiers;
use ratatui::{
    prelude::Rect,
    style::{Color, Style},
    text::{Line, Span, Text},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::selection::{SelectableId, SelectableRegion, SelectableVisualRow};

const COMMAND_INPUT_SELECTABLE_ID: &str = "command-input";

pub(super) fn should_insert_newline_on_enter(modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::SHIFT) || modifiers.contains(KeyModifiers::ALT)
}

pub(super) fn command_input_display_height(
    input_height: u16,
    terminal_height: u16,
    popup_rows: u16,
) -> u16 {
    let reserved_rows = 2u16.saturating_add(popup_rows);
    let max_rows = terminal_height
        .saturating_sub(8)
        .saturating_sub(reserved_rows)
        .max(1);
    input_height.max(1).min(max_rows)
}

pub(super) fn wrapped_input_height(text: &str, term_width: u16) -> u16 {
    let first_row_width = command_input_first_row_content_width(term_width);
    let wrap_width = command_input_wrap_width(term_width);
    if text.is_empty() {
        return 1;
    }
    let mut total: u16 = 0;
    for line in text.split('\n') {
        total = total.saturating_add(display_line_height(
            line.width(),
            first_row_width,
            wrap_width,
        ));
    }
    total.max(1)
}

pub(super) fn command_input_required_height(text: &str, cursor_pos: usize, term_width: u16) -> u16 {
    let first_row_width = command_input_first_row_content_width(term_width);
    let wrap_width = command_input_wrap_width(term_width);
    let cursor_rows =
        cursor_display_row(text, cursor_pos, first_row_width, wrap_width).saturating_add(1);
    wrapped_input_height(text, term_width).max(cursor_rows)
}

fn command_input_first_row_content_width(term_width: u16) -> usize {
    term_width.saturating_sub(2).max(1) as usize
}

fn command_input_wrap_width(term_width: u16) -> usize {
    term_width.saturating_sub(2).max(1) as usize
}

fn display_line_height(display_width: usize, first_row_width: usize, wrap_width: usize) -> u16 {
    if display_width <= first_row_width {
        1
    } else {
        1 + (display_width - first_row_width).div_ceil(wrap_width) as u16
    }
}

/// Threshold above which pasted text gets a placeholder block instead of being inserted inline.
const LARGE_PASTE_CHAR_THRESHOLD: usize = 500;

/// Decide whether a paste should be collapsed into a placeholder block.
///
/// Rules for paste placeholders:
/// - Pastes exceeding `LARGE_PASTE_CHAR_THRESHOLD` chars -> placeholder
/// - Pastes containing newlines and > 10 chars -> placeholder
/// - Otherwise -> insert inline as normal text
pub(super) fn handle_paste_placeholder(
    text: &str,
    input: &mut String,
    pending: &mut Vec<(String, String)>,
) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let char_count = normalized.chars().count();
    let is_multi_line = normalized.contains('\n');

    if char_count > LARGE_PASTE_CHAR_THRESHOLD || (char_count > 10 && is_multi_line) {
        let base = format!("[Pasted Content {char_count} chars]");
        let prefix = format!("{base} #");
        let mut max_suffix: usize = 0;
        for (ph, _) in pending.iter() {
            if ph == &base {
                max_suffix = max_suffix.max(1);
            } else if let Some(suffix) = ph.strip_prefix(&prefix)
                && let Ok(n) = suffix.parse::<usize>()
            {
                max_suffix = max_suffix.max(n);
            }
        }
        let placeholder = if max_suffix == 0 {
            base
        } else {
            format!("{base} #{max}", max = max_suffix + 1)
        };
        input.push_str(&placeholder);
        pending.push((placeholder, normalized));
    } else {
        input.push_str(&normalized);
    }
}

/// Replace all `[Pasted Content N chars]` placeholders in `text` with their stored full text.
pub(super) fn expand_paste_placeholders(text: &str, pending: &[(String, String)]) -> String {
    let mut result = text.to_string();
    for (placeholder, full_text) in pending {
        result = result.replace(placeholder, full_text);
    }
    result
}

/// Compute the visual row for a byte cursor position within the input text.
/// Accounts for prompt/continuation prefixes and terminal wrapping.
pub(super) fn cursor_display_row(
    text: &str,
    byte_pos: usize,
    first_row_width: usize,
    wrap_width: usize,
) -> u16 {
    let byte_pos = byte_pos.min(text.len());
    let before = &text[..byte_pos];
    let mut total_rows: u16 = 0;
    let mut lines = before.split('\n').peekable();
    while let Some(line) = lines.next() {
        let dw = line.width();
        if lines.peek().is_some() {
            total_rows =
                total_rows.saturating_add(display_line_height(dw, first_row_width, wrap_width));
        } else {
            total_rows =
                total_rows.saturating_add(cursor_line_row(dw, first_row_width, wrap_width));
        }
    }
    total_rows
}

pub(super) fn cursor_display_xy(
    text: &str,
    byte_pos: usize,
    first_row_width: usize,
    wrap_width: usize,
    prompt_width: u16,
    area: Rect,
    scroll: u16,
) -> (u16, u16) {
    let byte_pos = byte_pos.min(text.len());
    let before = &text[..byte_pos];
    let mut total_rows: u16 = 0;
    let mut col: u16 = prompt_width;
    let mut lines = before.split('\n').peekable();
    while let Some(line) = lines.next() {
        let dw = line.width();
        if lines.peek().is_some() {
            total_rows =
                total_rows.saturating_add(display_line_height(dw, first_row_width, wrap_width));
        } else {
            let (line_row, line_col) =
                cursor_line_row_col(dw, first_row_width, wrap_width, prompt_width);
            total_rows = total_rows.saturating_add(line_row);
            col = line_col;
        }
    }
    let x = area.x + col;
    let y = area.y + total_rows.saturating_sub(scroll);
    (x, y)
}

fn cursor_line_row(display_width: usize, first_row_width: usize, wrap_width: usize) -> u16 {
    cursor_line_row_col(display_width, first_row_width, wrap_width, 0).0
}

fn cursor_line_row_col(
    display_width: usize,
    first_row_width: usize,
    wrap_width: usize,
    prefix_width: u16,
) -> (u16, u16) {
    if display_width < first_row_width {
        return (0, prefix_width.saturating_add(display_width as u16));
    }
    let remaining = display_width - first_row_width;
    let row = 1 + (remaining / wrap_width) as u16;
    let col = prefix_width.saturating_add((remaining % wrap_width) as u16);
    (row, col)
}

#[cfg(test)]
pub(super) fn push_command_input_display_text(output: &mut String, input: &str) {
    for (line_index, line) in input.split('\n').enumerate() {
        if line_index > 0 {
            output.push('\n');
            output.push_str("  ");
        }
        output.push_str(line);
    }
}

#[cfg(test)]
pub(super) fn command_input_display_text(input: &str, completion: Option<&str>) -> Text<'static> {
    command_input_display_text_for_width(input, completion, u16::MAX)
}

pub(super) fn command_input_display_text_for_width(
    input: &str,
    completion: Option<&str>,
    term_width: u16,
) -> Text<'static> {
    if input.is_empty() {
        return Text::from(Line::from(vec![Span::styled(
            "› type a message, or /command",
            Style::default().fg(Color::DarkGray),
        )]));
    }

    let completion_suffix = completion
        .filter(|completion| *completion != input)
        .and_then(|completion| completion.strip_prefix(input))
        .unwrap_or_default();
    let content_width = command_input_first_row_content_width(term_width);
    let mut lines = Vec::new();
    for (line_index, line) in input.split('\n').enumerate() {
        push_wrapped_input_display_lines(&mut lines, line, line_index == 0, content_width);
    }
    if let Some(last_line) = lines.last_mut()
        && !completion_suffix.is_empty()
    {
        last_line.spans.push(Span::styled(
            completion_suffix.to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Text::from(lines)
}

pub(super) fn command_input_selectable_region(
    input: &str,
    area: Rect,
    scroll: u16,
) -> Option<SelectableRegion> {
    if input.is_empty() || area.width <= 2 || area.height == 0 {
        return None;
    }

    let content_width = command_input_first_row_content_width(area.width);
    let lines = input
        .split('\n')
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let viewport_top = scroll;
    let viewport_bottom = scroll.saturating_add(area.height);
    let mut visual_rows = Vec::new();
    let mut visual_row = 0u16;
    let mut push_row = |visual_row: u16,
                        logical_line: usize,
                        byte_start: usize,
                        byte_end: usize,
                        display_width: u16| {
        if visual_row < viewport_top || visual_row >= viewport_bottom {
            return;
        }
        visual_rows.push(SelectableVisualRow {
            logical_line,
            byte_start,
            byte_end,
            screen_x: area.x.saturating_add(2),
            screen_y: area
                .y
                .saturating_add(visual_row.saturating_sub(viewport_top)),
            display_width,
        });
    };

    for (line_index, line) in lines.iter().enumerate() {
        let mut remaining = line.as_str();
        let mut byte_start = 0usize;
        if remaining.is_empty() {
            push_row(visual_row, line_index, 0, 0, 0);
            visual_row = visual_row.saturating_add(1);
            continue;
        }

        while !remaining.is_empty() {
            let (segment, rest) = split_display_prefix(remaining, content_width);
            let byte_end = byte_start + segment.len();
            push_row(
                visual_row,
                line_index,
                byte_start,
                byte_end,
                segment.width().try_into().unwrap_or(u16::MAX),
            );
            remaining = rest;
            byte_start = byte_end;
            visual_row = visual_row.saturating_add(1);
        }
    }

    Some(SelectableRegion::from_visual_rows(
        SelectableId::new(COMMAND_INPUT_SELECTABLE_ID),
        area,
        lines,
        visual_rows,
    ))
}

fn push_wrapped_input_display_lines(
    out: &mut Vec<Line<'static>>,
    input: &str,
    first_input_line: bool,
    content_width: usize,
) {
    let mut remaining = input;
    let mut first_segment = true;
    if remaining.is_empty() {
        out.push(input_display_line(
            prefix_for_segment(first_input_line, true),
            "",
        ));
        return;
    }
    while !remaining.is_empty() {
        let (segment, rest) = split_display_prefix(remaining, content_width);
        out.push(input_display_line(
            prefix_for_segment(first_input_line, first_segment),
            segment,
        ));
        remaining = rest;
        first_segment = false;
    }
}

fn prefix_for_segment(first_input_line: bool, first_segment: bool) -> &'static str {
    if first_input_line && first_segment {
        "› "
    } else {
        "  "
    }
}

fn input_display_line(prefix: &str, content: &str) -> Line<'static> {
    Line::from(vec![Span::styled(
        format!("{prefix}{content}"),
        Style::default().fg(Color::White),
    )])
}

fn split_display_prefix(text: &str, max_width: usize) -> (&str, &str) {
    let mut width = 0usize;
    let mut end = 0usize;
    for (index, ch) in text.char_indices() {
        let ch_width = ch.width().unwrap_or(0);
        if end > 0 && width.saturating_add(ch_width) > max_width {
            return (&text[..end], &text[end..]);
        }
        let next_end = index + ch.len_utf8();
        width = width.saturating_add(ch_width);
        end = next_end;
        if width >= max_width {
            return (&text[..end], &text[end..]);
        }
    }
    (text, "")
}
