use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Clear, Paragraph, Wrap},
};

use super::{
    cells::activity_transcript_lines,
    selection::{SelectableId, SelectableRegion, SelectionRegistry, line_plain_text},
    view_state::TranscriptOverlayState,
};

pub(super) fn render_transcript_overlay(
    f: &mut Frame,
    area: Rect,
    overlay: &mut TranscriptOverlayState,
    selection: &SelectionRegistry,
    selectable_regions: &mut Vec<SelectableRegion>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    f.render_widget(Clear, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    let total_cells = overlay.cells.len().saturating_add(overlay.live_cells.len());
    let header = Line::from(vec![
        Span::styled(
            "T R A N S C R I P T",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{total_cells} cells"),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(header), rows[0]);

    let body = rows[1];
    let lines = activity_transcript_lines(&overlay.cells, &overlay.live_cells, body.width);
    let plain_lines = lines.iter().map(line_plain_text).collect::<Vec<_>>();
    let max_scroll = lines
        .len()
        .saturating_sub(body.height as usize)
        .min(u16::MAX as usize) as u16;
    overlay.set_render_metrics(max_scroll, body.height);
    let selectable_region = SelectableRegion::new(
        SelectableId::new("transcript"),
        body,
        plain_lines,
        overlay.effective_scroll(),
    );
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((overlay.effective_scroll(), 0))
            .wrap(Wrap { trim: false }),
        body,
    );
    if let Some(range) = selection.region_selection(&selectable_region.id) {
        selectable_region.highlight(&range, f.buffer_mut());
    }
    selectable_regions.push(selectable_region);

    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            "Esc/q close   Up/Down scroll   PgUp/PgDn page   Home/End jump",
            Style::default().fg(Color::DarkGray),
        )])),
        rows[2],
    );
}
