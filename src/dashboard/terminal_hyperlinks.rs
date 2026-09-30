use std::fmt::Write as _;
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use crossterm::{
    cursor::{Hide, MoveTo},
    queue,
    style::Print,
};
use ratatui::{buffer::Buffer, layout::Rect};
use regex::Regex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TerminalHyperlinkOverlay {
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) text: String,
    pub(super) target: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TerminalPlainTextOverlay {
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) text: String,
}

pub(super) fn collect_terminal_hyperlink_overlays(
    buffer: &Buffer,
    areas: &[Rect],
) -> Vec<TerminalHyperlinkOverlay> {
    let mut overlays = Vec::new();
    let mut pending_artifact_url: Option<String> = None;
    let mut pending_artifact_parts: Vec<String> = Vec::new();
    for area in areas {
        if area.width == 0 || area.height == 0 {
            continue;
        }
        for y in area.top()..area.bottom() {
            let row = rendered_row(buffer, *area, y);
            if row.trim().is_empty() {
                continue;
            }
            let mut occupied = Vec::new();
            if let Some(base) = pending_artifact_url.as_deref() {
                pending_artifact_parts.push(row.text.clone());
                if let Some(completed) =
                    complete_wrapped_artifact_url(base, &pending_artifact_parts)
                {
                    let displayed = artifact_signature_display(&completed);
                    let leading = row
                        .text
                        .find(displayed.as_str())
                        .or_else(|| row.text.find("?"))
                        .unwrap_or(0);
                    if let Some(overlay) = row_overlay(&row, y, leading, &displayed, &completed) {
                        overlays.push(overlay);
                    }
                    pending_artifact_url = None;
                    pending_artifact_parts.clear();
                }
                continue;
            }
            let mut saw_artifact_url = false;
            for (start, text, target) in url_spans(&row.text) {
                let end = start + text.len();
                occupied.push((start, end));
                if is_artifact_url_without_signature(&target) {
                    pending_artifact_url = Some(target.clone());
                    pending_artifact_parts.clear();
                    saw_artifact_url = true;
                }
                if let Some(overlay) = row_overlay(&row, y, start, &text, &target) {
                    overlays.push(overlay);
                }
            }
            if !saw_artifact_url {
                pending_artifact_url = None;
                pending_artifact_parts.clear();
            }
            for (start, text) in file_spans(&row.text) {
                let end = start + text.len();
                if occupied.iter().any(|(occupied_start, occupied_end)| {
                    ranges_overlap(start, end, *occupied_start, *occupied_end)
                }) {
                    continue;
                }
                if let Some(target) = file_uri_for_display_path(&text)
                    && let Some(overlay) = row_overlay(&row, y, start, &text, &target)
                {
                    overlays.push(overlay);
                }
            }
        }
    }
    overlays
}

pub(super) fn collect_removed_terminal_hyperlink_clears(
    buffer: &Buffer,
    previous: &[TerminalHyperlinkOverlay],
    current: &[TerminalHyperlinkOverlay],
) -> Vec<TerminalPlainTextOverlay> {
    previous
        .iter()
        .filter(|old| {
            !current
                .iter()
                .any(|new| new.x == old.x && new.y == old.y && new.text == old.text)
        })
        .filter_map(|old| {
            let text = buffer_text_at(buffer, old.x, old.y, old.text.chars().count())?;
            Some(TerminalPlainTextOverlay {
                x: old.x,
                y: old.y,
                text,
            })
        })
        .collect()
}

pub(super) fn write_terminal_hyperlink_overlays<W: Write>(
    writer: &mut W,
    clears: &[TerminalPlainTextOverlay],
    overlays: &[TerminalHyperlinkOverlay],
) -> io::Result<()> {
    if clears.is_empty() && overlays.is_empty() {
        return Ok(());
    }

    queue!(writer, Hide)?;
    for clear in clears {
        let text = sanitize_osc8_part(&clear.text);
        queue!(writer, MoveTo(clear.x, clear.y), Print(text))?;
    }
    for overlay in overlays {
        let target = sanitize_osc8_part(&overlay.target);
        let text = sanitize_osc8_part(&overlay.text);
        queue!(
            writer,
            MoveTo(overlay.x, overlay.y),
            Print(format!("\x1b]8;;{target}\x1b\\{text}\x1b]8;;\x1b\\"))
        )?;
    }
    writer.flush()
}

struct RenderedRow {
    text: String,
    byte_columns: Vec<(usize, u16)>,
}

impl RenderedRow {
    fn trim(&self) -> &str {
        self.text.trim()
    }

    fn x_for_byte(&self, byte_offset: usize) -> Option<u16> {
        match self
            .byte_columns
            .binary_search_by_key(&byte_offset, |(offset, _)| *offset)
        {
            Ok(index) => Some(self.byte_columns[index].1),
            Err(0) => None,
            Err(index) => Some(self.byte_columns[index.saturating_sub(1)].1),
        }
    }
}

fn rendered_row(buffer: &Buffer, area: Rect, y: u16) -> RenderedRow {
    let mut text = String::new();
    let mut byte_columns = Vec::new();
    for x in area.left()..area.right() {
        if let Some(cell) = buffer.cell((x, y)) {
            byte_columns.push((text.len(), x));
            text.push_str(cell.symbol());
        }
    }
    byte_columns.push((text.len(), area.right()));
    RenderedRow { text, byte_columns }
}

fn buffer_text_at(buffer: &Buffer, x: u16, y: u16, char_count: usize) -> Option<String> {
    if char_count == 0 || y < buffer.area.top() || y >= buffer.area.bottom() {
        return None;
    }
    let mut text = String::new();
    for offset in 0..char_count {
        let cell_x = x.checked_add(u16::try_from(offset).ok()?)?;
        if cell_x >= buffer.area.right() {
            return None;
        }
        let cell = buffer.cell((cell_x, y))?;
        text.push_str(cell.symbol());
    }
    Some(text)
}
fn row_overlay(
    row: &RenderedRow,
    y: u16,
    byte_start: usize,
    text: &str,
    target: &str,
) -> Option<TerminalHyperlinkOverlay> {
    let x = row.x_for_byte(byte_start)?;
    Some(TerminalHyperlinkOverlay {
        x,
        y,
        text: text.to_string(),
        target: target.to_string(),
    })
}

fn trim_trailing_link_punctuation(text: &str) -> &str {
    text.trim_end_matches(['.', ',', ';', ':', ')', ']', '}'])
}

const fn ranges_overlap(
    left_start: usize,
    left_end: usize,
    right_start: usize,
    right_end: usize,
) -> bool {
    left_start < right_end && right_start < left_end
}

fn url_spans(text: &str) -> Vec<(usize, String, String)> {
    url_regex()
        .find_iter(text)
        .filter_map(|matched| {
            let raw = trim_trailing_link_punctuation(matched.as_str());
            let candidate = raw.replace('\u{00a0}', "");
            if candidate.is_empty() {
                return None;
            }
            let parsed = url::Url::parse(&candidate).ok()?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return None;
            }
            Some((matched.start(), raw.to_string(), candidate))
        })
        .collect()
}

fn url_regex() -> &'static Regex {
    static URL_RE: OnceLock<Regex> = OnceLock::new();
    URL_RE.get_or_init(|| {
        Regex::new(r#"https?://[^\s<>"')\]\u{00a0}]+(?:\u{00a0}[^\s<>"')\]]+)?"#)
            .expect("valid URL regex")
    })
}

/// True when `url` is an artifact URL whose query has no `sig` parameter.
fn is_artifact_url_without_signature(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    parsed.path().contains("/artifacts/") && !url_has_query_param(&parsed, "sig")
}

fn url_has_query_param(url: &url::Url, name: &str) -> bool {
    url.query_pairs().any(|(key, _)| key == name)
}

/// Split a URL or absolute artifact path into its path and decoded query pairs.
///
/// The `sig` parameter is identified by name. Relative artifact paths are parsed
/// with a placeholder base so callers do not scan for the substring `?sig=`.
pub(crate) fn split_url_query(uri: &str) -> Option<(String, Vec<(String, String)>)> {
    let parsed = url::Url::parse(uri)
        .or_else(|_| url::Url::parse(&format!("http://artifact.invalid{uri}")))
        .ok()?;
    if parsed.query().is_none() {
        return None;
    }
    let pairs = parsed
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    let path = if uri.starts_with('/') || parsed.host_str() == Some("artifact.invalid") {
        parsed.path().to_string()
    } else {
        let mut without_query = parsed.clone();
        without_query.set_query(None);
        without_query.set_fragment(None);
        without_query.to_string()
    };
    Some((path, pairs))
}

fn complete_wrapped_artifact_url(base: &str, continuation_rows: &[String]) -> Option<String> {
    let joined = continuation_rows
        .iter()
        .map(|row| row.trim().trim_start_matches('\u{00a0}'))
        .filter(|row| !row.is_empty())
        .collect::<String>();
    let candidate = format!("{base}{joined}");
    let (path, pairs) = split_url_query(&candidate)?;
    if !path.contains("/artifacts/") || !pairs.iter().any(|(key, _)| key == "sig") {
        return None;
    }
    Some(candidate)
}

fn artifact_signature_display(url: &str) -> String {
    let Some((_, pairs)) = split_url_query(url) else {
        return String::new();
    };
    let mut query = String::new();
    for (key, value) in pairs {
        if !query.is_empty() {
            query.push('&');
        }
        query.push_str(&key);
        query.push('=');
        query.push_str(&value);
    }
    format!("?{query}")
}

fn file_spans(text: &str) -> Vec<(usize, String)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut spans = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if !path_boundary_before(text, chars[index].0) {
            index += 1;
            continue;
        }
        let start = chars[index].0;
        let rest = &text[start..];
        let Some(raw_len) = path_candidate_len(rest) else {
            index += 1;
            continue;
        };
        let raw = &rest[..raw_len];
        let trimmed = trim_trailing_link_punctuation(raw);
        if let Some(reference) = DisplayPath::parse(trimmed) {
            spans.push((start, reference.display));
        }
        let consumed = raw.chars().count().max(1);
        index += consumed;
    }
    spans
}

fn path_boundary_before(text: &str, byte_offset: usize) -> bool {
    text[..byte_offset]
        .chars()
        .next_back()
        .is_none_or(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '{' | '<' | '"' | '\''))
}

fn path_candidate_len(text: &str) -> Option<usize> {
    let mut len = 0usize;
    let mut saw_token = false;
    for ch in text.chars() {
        if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'' | '|' | ')' | ']' | '}') {
            break;
        }
        saw_token = true;
        len += ch.len_utf8();
    }
    saw_token.then_some(len)
}

struct DisplayPath {
    display: String,
    path: String,
    line: Option<u64>,
    column: Option<u64>,
}

impl DisplayPath {
    fn parse(text: &str) -> Option<Self> {
        let (path_text, line, column) = split_position_suffix(text)?;
        if !is_display_path(path_text) {
            return None;
        }
        Some(Self {
            display: text.to_string(),
            path: path_text.to_string(),
            line,
            column,
        })
    }
}

fn split_position_suffix(text: &str) -> Option<(&str, Option<u64>, Option<u64>)> {
    let drive_len = windows_drive_prefix_len(text).unwrap_or(0);
    let last_separator = text.rfind(['/', '\\']).unwrap_or(0);
    let suffix_start = drive_len.max(last_separator);
    let mut colon_positions = text
        .match_indices(':')
        .map(|(index, _)| index)
        .filter(|index| *index >= suffix_start)
        .collect::<Vec<_>>();
    if colon_positions.len() >= 2 {
        let column_at = colon_positions.pop()?;
        let line_at = colon_positions.pop()?;
        if let (Some(column), Some(line)) = (
            numeric_suffix(&text[column_at + 1..]),
            numeric_suffix(&text[line_at + 1..column_at]),
        ) {
            let path = &text[..line_at];
            if !path.is_empty() && is_display_path(path) {
                return Some((path, Some(line), Some(column)));
            }
        }
    }
    if let Some(line_at) = colon_positions.pop()
        && let Some(line) = numeric_suffix(&text[line_at + 1..])
    {
        let path = &text[..line_at];
        if !path.is_empty() && is_display_path(path) {
            return Some((path, Some(line), None));
        }
    }
    Some((text, None, None))
}

fn numeric_suffix(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn is_display_path(text: &str) -> bool {
    if text.is_empty() || text.contains("::") {
        return false;
    }
    let normalized = text.replace('\\', "/");
    let (drive, remainder) = split_windows_drive(&normalized);
    if drive.is_none() && remainder.matches(':').count() > position_colon_allowance(remainder) {
        return false;
    }
    if !remainder.contains('/') {
        return false;
    }
    let components = path_components(remainder);
    if components.is_empty()
        || components
            .iter()
            .any(|component| !valid_path_component(component))
    {
        return false;
    }
    if drive.is_some() {
        return true;
    }
    if remainder.starts_with('/') {
        // `/command` is a slash command; absolute paths need at least two components.
        return components.len() >= 2 && !is_conceptual_slash_label(&components);
    }
    if remainder.starts_with("./") || remainder.starts_with("../") {
        return true;
    }
    components.len() >= 2 && !is_conceptual_slash_label(&components)
}

fn position_colon_allowance(remainder: &str) -> usize {
    let Some(separator) = remainder.rfind('/') else {
        return 0;
    };
    let file_name = &remainder[separator + 1..];
    let colon = file_name.find(':');
    let Some(colon) = colon else {
        return 0;
    };
    let suffix = &file_name[colon + 1..];
    if numeric_suffix(suffix).is_some() {
        return 1;
    }
    if let Some((line, column)) = suffix.split_once(':')
        && numeric_suffix(line).is_some()
        && numeric_suffix(column).is_some()
    {
        return 2;
    }
    0
}

fn is_conceptual_slash_label(components: &[&str]) -> bool {
    components.len() >= 3
        && components
            .iter()
            .all(|component| component.chars().all(|ch| ch.is_ascii_alphabetic()))
        && components.iter().all(|component| !component.contains('.'))
}

fn split_windows_drive(text: &str) -> (Option<&str>, &str) {
    match windows_drive_prefix_len(text) {
        Some(len) => (Some(&text[..len]), &text[len..]),
        None => (None, text),
    }
}

fn windows_drive_prefix_len(text: &str) -> Option<usize> {
    let mut chars = text.chars();
    let drive = chars.next()?;
    let colon = chars.next()?;
    let separator = chars.next()?;
    if drive.is_ascii_alphabetic() && colon == ':' && (separator == '/' || separator == '\\') {
        Some(drive.len_utf8() + colon.len_utf8() + separator.len_utf8())
    } else {
        None
    }
}

fn path_components(text: &str) -> Vec<&str> {
    text.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect()
}

fn valid_path_component(component: &str) -> bool {
    if component == ".." {
        return true;
    }
    !component.is_empty()
        && component
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '@' | ':'))
}

fn file_uri_for_display_path(text: &str) -> Option<String> {
    let reference = DisplayPath::parse(text)?;
    let path = Path::new(&reference.path);
    let absolute = if path.is_absolute() || path.has_root() {
        PathBuf::from(path)
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut target = file_url_for_absolute_path(&absolute)?;
    if let Some(line) = reference.line {
        let _ = write!(target, "#L{line}");
        if let Some(column) = reference.column {
            let _ = write!(target, ":{column}");
        }
    }
    Some(target)
}

fn file_url_for_absolute_path(path: &Path) -> Option<String> {
    if let Ok(url) = url::Url::from_file_path(path) {
        return Some(url.to_string());
    }
    let text = path.to_string_lossy().replace('\\', "/");
    let mut url = url::Url::parse("file://localhost/").ok()?;
    url.set_path(&text);
    Some(url.to_string())
}

fn sanitize_osc8_part(text: &str) -> String {
    text.chars()
        .filter(|ch| !is_osc8_forbidden_control(*ch))
        .collect()
}

fn is_osc8_forbidden_control(ch: char) -> bool {
    ch.is_control() || ch == '\u{7f}' || ch == '\u{9c}'
}

#[cfg(test)]
mod tests {
    use ratatui::{buffer::Buffer, layout::Rect, style::Style};

    use super::*;

    #[test]
    fn collects_url_and_file_overlays_from_rendered_buffer() {
        let area = Rect::new(0, 0, 120, 2);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(
            0,
            0,
            "See https://example.com/docs and src/dashboard/mod.rs:42",
            Style::default(),
        );

        let overlays = collect_terminal_hyperlink_overlays(&buffer, &[area]);

        assert!(
            overlays
                .iter()
                .any(|overlay| overlay.target == "https://example.com/docs")
        );
        assert!(
            overlays
                .iter()
                .any(|overlay| overlay.text == "src/dashboard/mod.rs:42"
                    && overlay.target.starts_with("file://")
                    && overlay.target.ends_with("#L42"))
        );
    }

    #[test]
    fn file_overlay_column_uses_buffer_cells_after_wide_text() {
        let area = Rect::new(0, 0, 120, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "ＷＩＤＥ src/dashboard/mod.rs:42", Style::default());

        let overlays = collect_terminal_hyperlink_overlays(&buffer, &[area]);
        let overlay = overlays
            .iter()
            .find(|overlay| overlay.text == "src/dashboard/mod.rs:42")
            .expect("file path should be linked");

        assert_eq!(
            overlay.x, 9,
            "wide CJK cells before a link must not shift OSC8 overlay placement"
        );
    }

    #[test]
    fn absolute_slash_paths_are_file_links() {
        let area = Rect::new(0, 0, 120, 2);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "/var/log/daat-locus.log:12", Style::default());
        buffer.set_string(0, 1, "Open /tmp/daat-locus/session", Style::default());

        let overlays = collect_terminal_hyperlink_overlays(&buffer, &[area]);

        assert!(
            overlays
                .iter()
                .any(|overlay| overlay.text == "/var/log/daat-locus.log:12"
                    && overlay.target.ends_with("/var/log/daat-locus.log#L12")),
            "absolute slash path with line should be linked: {overlays:?}"
        );
        assert!(
            overlays
                .iter()
                .any(|overlay| overlay.text == "/tmp/daat-locus/session"),
            "absolute slash path after whitespace should be linked without its prefix: {overlays:?}"
        );
    }

    #[test]
    fn conceptual_slash_terms_are_not_file_links() {
        let area = Rect::new(0, 0, 120, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(
            0,
            0,
            "Keep model constraints in App/Event/Workflow/PendingWork concepts",
            Style::default(),
        );

        let overlays = collect_terminal_hyperlink_overlays(&buffer, &[area]);

        assert!(
            overlays.is_empty(),
            "conceptual slash-separated labels should not become file links: {overlays:?}"
        );
    }

    #[test]
    fn slash_commands_are_not_file_links() {
        let area = Rect::new(0, 0, 120, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(
            0,
            0,
            "Use /command or /status in the dashboard",
            Style::default(),
        );

        let overlays = collect_terminal_hyperlink_overlays(&buffer, &[area]);

        assert!(
            overlays.is_empty(),
            "slash commands should not become file links: {overlays:?}"
        );
    }

    #[test]
    fn ignores_links_outside_allowed_rows() {
        let area = Rect::new(0, 0, 120, 3);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(
            0,
            0,
            "Thinking about src/dashboard/mod.rs and https://assistant.test",
            Style::default(),
        );
        buffer.set_string(
            0,
            1,
            "User mentioned src/dashboard/mod.rs:42 and https://example.com/docs",
            Style::default(),
        );
        buffer.set_string(0, 2, "gpt-5.5 · 126.5k/258.4k used", Style::default());

        let overlays = collect_terminal_hyperlink_overlays(&buffer, &[Rect::new(0, 1, 120, 1)]);

        assert_eq!(
            overlays.len(),
            2,
            "only user-message row links should be emitted"
        );
        assert!(
            overlays
                .iter()
                .any(|overlay| overlay.text == "src/dashboard/mod.rs:42"),
            "user file path should still be linked: {overlays:?}"
        );
        assert!(
            overlays
                .iter()
                .any(|overlay| overlay.target == "https://example.com/docs"),
            "user URL should still be linked: {overlays:?}"
        );
        assert!(
            overlays
                .iter()
                .all(|overlay| !overlay.text.contains("assistant.test")
                    && !overlay.text.contains("126.5k/258.4k")),
            "non-user rows must not be linked: {overlays:?}"
        );
    }

    #[test]
    fn removed_overlays_are_repainted_from_current_buffer() {
        let area = Rect::new(0, 0, 120, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(
            0,
            0,
            "Assistant path src/dashboard/mod.rs",
            Style::default(),
        );
        let previous = vec![TerminalHyperlinkOverlay {
            x: 15,
            y: 0,
            text: "src/dashboard/mod.rs".to_string(),
            target: "file:///workspace/src/dashboard/mod.rs".to_string(),
        }];
        let current = Vec::new();

        let clears = collect_removed_terminal_hyperlink_clears(&buffer, &previous, &current);

        assert_eq!(
            clears,
            vec![TerminalPlainTextOverlay {
                x: 15,
                y: 0,
                text: "src/dashboard/mod.rs".to_string(),
            }],
            "removed OSC8 overlays must be repainted as plain text so stale links disappear"
        );
    }

    #[test]
    fn unchanged_overlays_do_not_repaint_plain_text() {
        let area = Rect::new(0, 0, 120, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "User path src/dashboard/mod.rs", Style::default());
        let overlay = TerminalHyperlinkOverlay {
            x: 10,
            y: 0,
            text: "src/dashboard/mod.rs".to_string(),
            target: "file:///workspace/src/dashboard/mod.rs".to_string(),
        };

        let clears = collect_removed_terminal_hyperlink_clears(
            &buffer,
            std::slice::from_ref(&overlay),
            std::slice::from_ref(&overlay),
        );

        assert!(
            clears.is_empty(),
            "unchanged OSC8 overlays should stay linked instead of being repainted"
        );
    }

    #[test]
    fn hyperlink_overlay_writer_does_not_show_cursor() {
        let mut output = Vec::new();
        let overlay = TerminalHyperlinkOverlay {
            x: 4,
            y: 2,
            text: "https://example.com".to_string(),
            target: "https://example.com".to_string(),
        };

        write_terminal_hyperlink_overlays(&mut output, &[], &[overlay])
            .expect("overlay write should succeed");

        let output = String::from_utf8_lossy(&output);
        assert!(
            output.contains("\x1b[?25l"),
            "overlay writes should hide the cursor before moving around the frame"
        );
        assert!(
            !output.contains("\x1b[?25h"),
            "the TUI loop restores the cursor only after all overlay writes finish"
        );
    }

    #[test]
    fn rejects_url_candidates_that_url_parse_rejects() {
        let area = Rect::new(0, 0, 80, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "see http:// not a link", Style::default());

        let overlays = collect_terminal_hyperlink_overlays(&buffer, &[area]);

        assert!(
            overlays.is_empty(),
            "regex candidates must still be accepted by url::Url::parse: {overlays:?}"
        );
    }

    #[test]
    fn wrapped_artifact_signature_matches_named_query_parameter() {
        let area = Rect::new(0, 0, 36, 2);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(
            0,
            0,
            "http://127.0.0.1:9/artifacts/a.png",
            Style::default(),
        );
        buffer.set_string(0, 1, "?token=no&sig=abc", Style::default());

        let overlays = collect_terminal_hyperlink_overlays(&buffer, &[area]);
        let signature = overlays
            .iter()
            .find(|overlay| overlay.y == 1)
            .expect("wrapped signature row should complete the artifact link");

        assert_eq!(signature.text, "?token=no&sig=abc");
        assert!(
            signature.target.ends_with("/artifacts/a.png?token=no&sig=abc"),
            "sig must be matched by parameter name: {}",
            signature.target
        );
        let (path, pairs) = split_url_query(&signature.target).expect("completed url");
        assert!(path.contains("/artifacts/a.png"));
        assert!(pairs.iter().any(|(key, value)| key == "sig" && value == "abc"));
    }

    #[test]
    fn prose_with_a_dot_is_not_a_file_path() {
        let area = Rect::new(0, 0, 80, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(
            0,
            0,
            "version 1.2 and note.txt stay plain, but src/lib.rs:12:4 links",
            Style::default(),
        );

        let overlays = collect_terminal_hyperlink_overlays(&buffer, &[area]);

        assert_eq!(overlays.len(), 1, "only the path should link: {overlays:?}");
        assert_eq!(overlays[0].text, "src/lib.rs:12:4");
        assert!(
            overlays[0].target.ends_with("/src/lib.rs#L12:4"),
            "line and column suffixes should survive file URI assembly: {}",
            overlays[0].target
        );
        assert!(overlays[0].target.starts_with("file://"));
    }

    #[test]
    fn windows_drive_paths_use_file_uris() {
        let area = Rect::new(0, 0, 40, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "C:/repo/src/main.rs:8", Style::default());

        let overlays = collect_terminal_hyperlink_overlays(&buffer, &[area]);

        assert_eq!(overlays.len(), 1, "{overlays:?}");
        assert!(overlays[0].target.starts_with("file:///"));
        let target = overlays[0].target.to_ascii_lowercase();
        assert!(
            target.contains("/c:/repo/src/main.rs"),
            "windows drive path should become a file URI: {}",
            overlays[0].target
        );
        assert!(overlays[0].target.ends_with("#L8"));
    }

    #[test]
    fn osc8_emitter_strips_controls_bel_and_st() {
        let mut output = Vec::new();
        let overlay = TerminalHyperlinkOverlay {
            x: 0,
            y: 0,
            text: "label\u{0001}\u{0007}text\u{009c}".to_string(),
            target: "https://example.com/a\nb\u{0007}".to_string(),
        };

        write_terminal_hyperlink_overlays(&mut output, &[], &[overlay]).expect("write");

        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("labeltext"));
        assert!(output.contains("https://example.com/ab"));
        assert!(!output.contains('\u{0001}'));
        assert!(!output.contains('\u{0007}'));
        assert!(!output.contains('\u{009c}'));
        assert!(!output.contains('\n'));
    }
}
