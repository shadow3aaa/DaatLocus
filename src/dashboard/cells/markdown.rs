//! Markdown rendering for dashboard activity cells.
//!
//! Uses the forked `daat-locus-md` crate for parsing and rendering.

use daat_locus_md::constants::list_prefix::{HLINE, ROUNDED_BL, ROUNDED_TL, VLINE};
use daat_locus_md::markdown::{MarkdownRenderer, RenderHooks};
use daat_locus_md::theme::ThemeConfig;
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use super::highlight::highlight_block;

struct SyntaxHighlightHooks;

impl RenderHooks for SyntaxHighlightHooks {
    fn render_code_block(&self, lang: &str, content: &str) -> Option<Vec<Line<'static>>> {
        let content = content.trim_end();
        if content.is_empty() {
            return None;
        }
        let lang_for_highlight = if lang.is_empty() { "code" } else { lang };
        let highlighted_lines = highlight_block(content, lang_for_highlight)?;

        let border_style = Style::default().fg(Color::DarkGray);
        let display_lang = if lang.is_empty() { "code" } else { lang };
        let prefix = format!("{VLINE} ");

        let mut lines = Vec::new();
        lines.push(Line::from(Span::styled(
            format!("{ROUNDED_TL}{HLINE} {display_lang}"),
            border_style,
        )));

        for spans in highlighted_lines {
            let mut line_spans = vec![Span::styled(prefix.clone(), border_style)];
            line_spans.extend(spans);
            lines.push(Line::from(line_spans));
        }

        lines.push(Line::from(Span::styled(
            format!("{ROUNDED_BL}{HLINE}"),
            border_style,
        )));

        Some(lines)
    }
}

/// Render markdown without width constraint.
pub fn render_markdown(input: &str, base_color: Color) -> Vec<Line<'static>> {
    render_markdown_with_width(input, base_color, None)
}

pub fn render_markdown_with_width(
    input: &str,
    base_color: Color,
    wrap_width: Option<u16>,
) -> Vec<Line<'static>> {
    let width = wrap_width.map(|w| w as usize).unwrap_or(usize::MAX);
    let renderer = MarkdownRenderer::new(width).with_render_hooks(Box::new(SyntaxHighlightHooks));
    let blocks = renderer.parse(input);
    let theme = ThemeConfig::default().with_text_color(base_color);
    renderer.render(&blocks, &theme)
}
