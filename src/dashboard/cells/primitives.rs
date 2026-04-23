use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

pub trait Cell {
    fn render_lines(&self) -> Vec<Line<'static>>;
}

pub fn render_text_activity_lines(
    marker: &str,
    accent: Color,
    title: &str,
    body_lines: &[String],
    limit: usize,
    prefix: Option<&str>,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            marker.to_string(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            title.to_string(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
    ])];
    for line in body_lines.iter().take(limit) {
        let mut spans = vec![Span::raw("   ")];
        if let Some(prefix) = prefix {
            spans.push(Span::styled(
                prefix.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        spans.push(Span::styled(
            line.to_string(),
            Style::default().fg(Color::Gray),
        ));
        lines.push(Line::from(spans));
    }
    lines
}

pub fn render_message_activity_lines(
    marker: &str,
    accent: Color,
    title: &str,
    detail_lines: &[String],
    message_lines: &[String],
    detail_limit: usize,
    message_limit: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            marker.to_string(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            title.to_string(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
    ])];
    for line in detail_lines.iter().take(detail_limit) {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(line.to_string(), Style::default().fg(Color::Gray)),
        ]));
    }
    for (index, line) in message_lines.iter().take(message_limit).enumerate() {
        lines.push(Line::from(vec![
            Span::styled(
                if index == 0 { "  └ " } else { "    " },
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(line.to_string(), Style::default().fg(Color::White)),
        ]));
    }
    lines
}

pub fn render_wait_activity_lines(
    title: &str,
    body_lines: &[String],
    limit: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "•",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    for line in body_lines.iter().take(limit) {
        lines.push(Line::from(vec![
            Span::styled("  └ ", Style::default().fg(Color::DarkGray)),
            Span::styled(line.to_string(), Style::default().fg(Color::Gray)),
        ]));
    }
    lines
}

pub fn render_error_lines(title: &str, body_lines: &[String], limit: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "!",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            title.to_string(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    ])];
    for line in body_lines.iter().take(limit) {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(line.to_string(), Style::default().fg(Color::LightRed)),
        ]));
    }
    lines
}
