use crossterm::event::KeyModifiers;
use ratatui::{
    prelude::Rect,
    style::{Color, Style},
    text::{Line, Span, Text},
};
use unicode_width::UnicodeWidthStr;

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
    let available = term_width.saturating_sub(2).max(1) as usize;
    if text.is_empty() {
        return 1;
    }
    let mut total: u16 = 0;
    for line in text.split('\n') {
        if line.is_empty() {
            total += 1;
            continue;
        }
        let display_width = line.width();
        let lines = display_width.div_ceil(available).max(1);
        total += lines as u16;
    }
    total.max(1)
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

/// Compute (x, y) display position for a byte cursor position within the input text.
/// Accounts for multi-line input and terminal wrapping at `available_width`.
/// `prompt_width` is the display width of the leading prompt (e.g. "› " = 2).
pub(super) fn cursor_display_row(text: &str, byte_pos: usize, available_width: usize) -> u16 {
    let byte_pos = byte_pos.min(text.len());
    let before = &text[..byte_pos];
    let mut total_rows: u16 = 0;
    let mut lines = before.split('\n').peekable();
    while let Some(line) = lines.next() {
        let dw = line.width();
        if lines.peek().is_some() {
            if dw == 0 {
                total_rows += 1;
            } else {
                total_rows += dw.div_ceil(available_width) as u16;
            }
        } else {
            total_rows += (dw / available_width) as u16;
        }
    }
    total_rows
}

pub(super) fn cursor_display_xy(
    text: &str,
    byte_pos: usize,
    available_width: usize,
    prompt_width: u16,
    area: Rect,
    scroll: u16,
) -> (u16, u16) {
    let byte_pos = byte_pos.min(text.len());
    let before = &text[..byte_pos];
    let mut total_rows: u16 = 0;
    let mut col: u16 = 0;
    let mut lines = before.split('\n').peekable();
    while let Some(line) = lines.next() {
        let dw = line.width();
        if lines.peek().is_some() {
            // completed logical line
            if dw == 0 {
                total_rows += 1;
            } else {
                total_rows += dw.div_ceil(available_width) as u16;
            }
        } else {
            // current (last) line
            total_rows += (dw / available_width) as u16;
            col = (dw % available_width) as u16;
        }
    }
    let x = area.x + prompt_width + col;
    let y = area.y + total_rows.saturating_sub(scroll);
    (x, y)
}

pub(super) fn push_command_input_display_text(output: &mut String, input: &str) {
    for (line_index, line) in input.split('\n').enumerate() {
        if line_index > 0 {
            output.push('\n');
            output.push_str("  ");
        }
        output.push_str(line);
    }
}

pub(super) fn command_input_display_text(input: &str, completion: Option<&str>) -> Text<'static> {
    if input.is_empty() {
        return Text::from(Line::from(vec![Span::styled(
            "› type a message, or /command",
            Style::default().fg(Color::DarkGray),
        )]));
    }

    let mut display = String::with_capacity(input.len() + 2 + input.matches('\n').count() * 2);
    display.push_str("› ");
    push_command_input_display_text(&mut display, input);
    let completion_suffix = completion
        .filter(|completion| *completion != input)
        .and_then(|completion| completion.strip_prefix(input))
        .unwrap_or_default();
    let raw_lines = display.split('\n').collect::<Vec<_>>();
    let last_index = raw_lines.len().saturating_sub(1);
    let lines = raw_lines
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            let mut spans = vec![Span::styled(
                line.to_string(),
                Style::default().fg(Color::White),
            )];
            if idx == last_index && !completion_suffix.is_empty() {
                spans.push(Span::styled(
                    completion_suffix.to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    Text::from(lines)
}
