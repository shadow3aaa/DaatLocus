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
            for matched in url_regex().find_iter(&row.text) {
                let text = trim_trailing_link_punctuation(matched.as_str());
                if text.is_empty() {
                    continue;
                }
                let start = matched.start();
                let end = start + text.len();
                occupied.push((start, end));
                if let Some(overlay) = row_overlay(&row, y, start, text, text) {
                    overlays.push(overlay);
                }
            }
            for matched in file_regex().find_iter(&row.text) {
                let matched_text = matched.as_str();
                let leading_whitespace = matched_text.len() - matched_text.trim_start().len();
                let text = trim_trailing_link_punctuation(matched_text.trim_start());
                if text.is_empty() {
                    continue;
                }
                let start = matched.start() + leading_whitespace;
                let end = start + text.len();
                if occupied.iter().any(|(occupied_start, occupied_end)| {
                    ranges_overlap(start, end, *occupied_start, *occupied_end)
                }) {
                    continue;
                }
                if let Some(target) = file_uri_for_display_path(text)
                    && let Some(overlay) = row_overlay(&row, y, start, text, &target)
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

fn url_regex() -> &'static Regex {
    static URL_RE: OnceLock<Regex> = OnceLock::new();
    URL_RE.get_or_init(|| Regex::new(r#"https?://[^\s<>"')\]]+"#).expect("valid URL regex"))
}

fn file_regex() -> &'static Regex {
    static FILE_RE: OnceLock<Regex> = OnceLock::new();
    FILE_RE.get_or_init(|| {
        Regex::new(
            r#"(?x)
            (?:
                [A-Za-z]:[\\/][^\s<>"'|]+
                |
                (?:^|\s)/[A-Za-z0-9_.@-]+(?:/[A-Za-z0-9_.@-]+)+
                |
                (?:\.{1,2}[\\/])?[A-Za-z0-9_.@-]+(?:[\\/][A-Za-z0-9_.@-]+)+
            )
            (?::\d+)?
            "#,
        )
        .expect("valid file regex")
    })
}

fn trim_trailing_link_punctuation(text: &str) -> &str {
    text.trim_end_matches(['.', ',', ';', ':', ')', ']', '}'])
}

fn ranges_overlap(
    left_start: usize,
    left_end: usize,
    right_start: usize,
    right_end: usize,
) -> bool {
    left_start < right_end && right_start < left_end
}

fn file_uri_for_display_path(text: &str) -> Option<String> {
    let (without_line, line) = split_line_suffix(text);
    if !is_probable_file_reference(without_line) {
        return None;
    }
    let path = Path::new(without_line);
    let absolute = if path.is_absolute() || path.has_root() {
        PathBuf::from(path)
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut target = format!("file://{}", uri_path(&absolute));
    if let Some(line) = line {
        target.push_str(&format!("#L{line}"));
    }
    Some(target)
}

fn is_probable_file_reference(text: &str) -> bool {
    let normalized = text.replace('\\', "/");
    if normalized.starts_with("./")
        || normalized.starts_with("../")
        || is_windows_absolute_path(&normalized)
        || is_unix_absolute_path(&normalized)
    {
        return true;
    }

    let path = Path::new(text);
    if path.exists() {
        return true;
    }

    normalized
        .rsplit('/')
        .next()
        .is_some_and(|file_name| file_name.contains('.'))
}

fn is_unix_absolute_path(text: &str) -> bool {
    if !text.starts_with('/') {
        return false;
    }
    let mut parts = text.split('/').filter(|part| !part.is_empty());
    parts.next().is_some() && parts.next().is_some()
}

fn is_windows_absolute_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

fn split_line_suffix(text: &str) -> (&str, Option<u64>) {
    let Some((path, line)) = text.rsplit_once(':') else {
        return (text, None);
    };
    if !line.is_empty()
        && line.chars().all(|ch| ch.is_ascii_digit())
        && let Ok(line) = line.parse::<u64>()
    {
        (path, Some(line))
    } else {
        (text, None)
    }
}

fn uri_path(path: &Path) -> String {
    let mut value = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) && !value.starts_with('/') {
        value.insert(0, '/');
    }
    value
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace('#', "%23")
}

fn sanitize_osc8_part(text: &str) -> String {
    text.replace(['\x1b', '\n', '\r'], "")
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
}
