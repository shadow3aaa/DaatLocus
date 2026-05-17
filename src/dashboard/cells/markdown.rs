//! Markdown rendering for the TUI dashboard.
//!
//! Thin wrapper around [`tui-markdown`], converting markdown text into styled
//! ratatui [`Line`]s with a configurable base colour. Delegates all parsing
//! and layout to the upstream crate so that tables, task-lists, and other
//! Markdown extensions are handled correctly without hand-rolled code.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use tui_markdown::{self, Options, StyleSheet, from_str_with_options};

// ── Custom StyleSheet ─────────────────────────────────────────────────────

/// A [`StyleSheet`] that applies a configurable base colour to headings /
/// plain text while keeping standard styling for code, links, blockquotes,
/// and metadata.
#[derive(Clone, Debug)]
struct DashboardStyleSheet {
    base_color: Color,
}

impl StyleSheet for DashboardStyleSheet {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::default()
                .fg(self.base_color)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            2 => Style::default()
                .fg(self.base_color)
                .add_modifier(Modifier::BOLD),
            3 => Style::default()
                .fg(self.base_color)
                .add_modifier(Modifier::BOLD),
            4 => Style::default()
                .fg(self.base_color)
                .add_modifier(Modifier::ITALIC),
            5 => Style::default()
                .fg(self.base_color)
                .add_modifier(Modifier::ITALIC),
            _ => Style::default()
                .fg(self.base_color)
                .add_modifier(Modifier::ITALIC),
        }
    }

    fn code(&self) -> Style {
        Style::default().fg(Color::Yellow)
    }

    fn link(&self) -> Style {
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::UNDERLINED)
    }

    fn blockquote(&self) -> Style {
        Style::default().fg(Color::Green)
    }

    fn heading_meta(&self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    fn metadata_block(&self) -> Style {
        Style::default().fg(Color::Yellow)
    }
}

// ── Syntax-marker normalisation ─────────────────────────────────────────

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn is_code_fence_line(line: &Line<'_>) -> bool {
    line_text(line).trim_start().starts_with("```")
}

fn heading_marker_prefix_len(text: &str) -> Option<usize> {
    let mut hash_count = 0usize;
    let mut after_hashes = 0usize;

    for (index, ch) in text.char_indices() {
        if ch != '#' || hash_count >= 6 {
            break;
        }
        hash_count += 1;
        after_hashes = index + ch.len_utf8();
    }

    if hash_count == 0 {
        return None;
    }

    let rest = &text[after_hashes..];
    if rest.is_empty() {
        return Some(after_hashes);
    }

    let mut whitespace_len = 0usize;
    for (index, ch) in rest.char_indices() {
        if !ch.is_whitespace() {
            break;
        }
        whitespace_len = index + ch.len_utf8();
    }

    if whitespace_len == 0 {
        None
    } else {
        Some(after_hashes + whitespace_len)
    }
}

fn strip_prefix_from_spans(spans: Vec<Span<'static>>, mut prefix_len: usize) -> Vec<Span<'static>> {
    spans
        .into_iter()
        .filter_map(|span| {
            let content = span.content.into_owned();
            if prefix_len == 0 {
                return Some(Span::styled(content, span.style));
            }

            if prefix_len >= content.len() {
                prefix_len -= content.len();
                return None;
            }

            let split_at = if content.is_char_boundary(prefix_len) {
                prefix_len
            } else {
                content
                    .char_indices()
                    .map(|(index, _)| index)
                    .find(|index| *index > prefix_len)
                    .unwrap_or(content.len())
            };
            prefix_len = 0;
            let stripped = content[split_at..].to_string();
            if stripped.is_empty() {
                None
            } else {
                Some(Span::styled(stripped, span.style))
            }
        })
        .collect()
}

fn strip_heading_marker(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let text: String = spans.iter().map(|span| span.content.as_ref()).collect();
    let Some(prefix_len) = heading_marker_prefix_len(&text) else {
        return spans;
    };
    strip_prefix_from_spans(spans, prefix_len)
}

fn is_heading_line_style(style: Style) -> bool {
    let modifiers = style.add_modifier;
    modifiers.contains(Modifier::BOLD)
        || modifiers.contains(Modifier::UNDERLINED)
        || modifiers.contains(Modifier::ITALIC)
}

// ── Public API ────────────────────────────────────────────────────────────

/// Render a full markdown text into styled ratatui [`Line`]s.
///
/// Supports:
/// - Paragraphs, headings, blockquotes, lists, code blocks
/// - Horizontal rules (`---`, `***`)
/// - Inline: **bold**, *italic*, `code`, ~~strikethrough~~, links
/// - Tables, task-lists, and other extensions (via tui-markdown)
pub fn render_markdown(input: &str, base_color: Color) -> Vec<Line<'static>> {
    if input.is_empty() {
        return Vec::new();
    }

    let stylesheet = DashboardStyleSheet { base_color };
    let options = Options::new(stylesheet);
    let text = from_str_with_options(input, &options);

    // Convert to 'static lines. For spans that have no explicit fg/bg (e.g.
    // plain paragraph text, emphasis, strong, strikethrough), apply the base
    // colour. tui-markdown keeps some block-level syntax markers as visible
    // spans; hide those here so the dashboard reads like a Markdown preview.
    text.lines
        .into_iter()
        .filter_map(|line| {
            if is_code_fence_line(&line) {
                return None;
            }

            let line_style = line.style;
            let spans: Vec<Span<'static>> = line
                .spans
                .into_iter()
                .map(|s| {
                    // A span is "uncoloured" only when neither the span
                    // nor the line style provides a colour attribute.
                    let has_span_color = s.style.fg.is_some()
                        || s.style.bg.is_some()
                        || s.style.underline_color.is_some();
                    let has_line_color = line_style.fg.is_some()
                        || line_style.bg.is_some()
                        || line_style.underline_color.is_some();
                    let style = if !has_span_color && !has_line_color {
                        s.style.fg(base_color)
                    } else {
                        s.style
                    };
                    Span::styled(s.content.into_owned(), style)
                })
                .collect();
            let spans = if is_heading_line_style(line_style) {
                strip_heading_marker(spans)
            } else {
                spans
            };
            let mut new_line = Line::from(spans);
            // Preserve line-level style so that constructs whose
            // colour lives on the line (e.g. fenced code blocks via
            // tui-markdown's line_styles stack) are not lost.
            new_line.style = line_style;
            Some(new_line)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};

    use super::{line_text, render_markdown};

    fn rendered_text(input: &str) -> Vec<String> {
        render_markdown(input, Color::White)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn headings_drop_atx_markers() {
        let lines = rendered_text("# Markdown 渲染测试\n\n## 基础样式");

        assert_eq!(lines.first().map(String::as_str), Some("Markdown 渲染测试"));
        assert!(lines.iter().any(|line| line == "基础样式"));
        assert!(!lines.iter().any(|line| line.starts_with('#')));
    }

    #[test]
    fn headings_keep_heading_style_after_marker_removal() {
        let line = render_markdown("# Markdown 渲染测试", Color::White)
            .into_iter()
            .next()
            .expect("expected heading line");

        assert_eq!(line_text(&line), "Markdown 渲染测试");
        assert!(line.style.add_modifier.contains(Modifier::BOLD));
        assert!(line.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn fenced_code_blocks_drop_delimiter_lines() {
        let lines = rendered_text("```rust\nfn main() {}\n```");
        let joined = lines.join("\n");

        assert!(joined.contains("fn main() {}"));
        assert!(!joined.contains("```"));
    }
}
