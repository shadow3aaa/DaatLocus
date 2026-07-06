//! Markdown rendering for dashboard activity cells.
//!
//! Uses the forked `daat-locus-md` crate for parsing and rendering.

use daat_locus_md::markdown::MarkdownRenderer;
use daat_locus_md::theme::ThemeConfig;
use ratatui::{style::Color, text::Line};

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
    let renderer = MarkdownRenderer::new(width);
    let blocks = renderer.parse(input);
    let theme = ThemeConfig::default().with_text_color(base_color);
    renderer.render(&blocks, &theme)
}
