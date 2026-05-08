use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};

/// Convert a single line of inline markdown to styled spans.
/// Handles: **bold**, *italic*, `code`, [links](url), ~~strikethrough~~
pub fn markdown_to_spans(text: &str, base_color: Color) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let parser = Parser::new(text);

    let mut bold = 0u8;
    let mut italic = 0u8;
    let mut link_url: Option<String> = None;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Emphasis => italic += 1,
                Tag::Strong => bold += 1,
                Tag::Strikethrough => {} // ignore for terminal
                Tag::Link { dest_url, .. } => {
                    link_url = Some(dest_url.to_string());
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Emphasis => italic = italic.saturating_sub(1),
                TagEnd::Strong => bold = bold.saturating_sub(1),
                TagEnd::Strikethrough => {}
                TagEnd::Link => {
                    if let Some(url) = link_url.take() {
                        spans.push(Span::styled(
                            format!(" ({})", url),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                }
                _ => {}
            },
            Event::Text(t) => build_styled_span(&mut spans, &t, base_color, bold > 0, italic > 0, false),
            Event::Code(t) => build_styled_span(&mut spans, &t, base_color, bold > 0, italic > 0, true),
            Event::SoftBreak | Event::HardBreak => {
                spans.push(Span::raw(" "));
            }
            _ => {}
        }
    }

    if spans.is_empty() && !text.is_empty() {
        spans.push(Span::styled(text.to_string(), Style::default().fg(base_color)));
    }
    spans
}

fn build_styled_span(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    base_color: Color,
    bold: bool,
    italic: bool,
    code: bool,
) {
    if text.is_empty() {
        return;
    }
    let mut style = if code {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(base_color)
    };
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    spans.push(Span::styled(text.to_string(), style));
}
