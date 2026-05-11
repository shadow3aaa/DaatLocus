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
use tui_markdown::{self, from_str_with_options, Options, StyleSheet};

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

    // Convert to 'static lines.
    // For spans that have no explicit fg/bg (e.g. plain paragraph text,
    // emphasis, strong, strikethrough), apply the base colour.
    text.lines
        .into_iter()
        .map(|line| {
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
                    let style = if !has_span_color && !has_line_color
                    {
                        s.style.fg(base_color)
                    } else {
                        s.style
                    };
                    Span::styled(s.content.into_owned(), style)
                })
                .collect();
            let mut new_line = Line::from(spans);
            // Preserve line-level style so that constructs whose
            // colour lives on the line (e.g. fenced code blocks via
            // tui-markdown's line_styles stack) are not lost.
            new_line.style = line_style;
            new_line
        })
        .collect()
}
