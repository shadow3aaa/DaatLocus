use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};
use std::io::{self, Write};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SelectableId(String);

impl SelectableId {
    pub(super) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextPosition {
    pub(super) line: usize,
    pub(super) byte: usize,
}

impl TextPosition {
    const fn new(line: usize, byte: usize) -> Self {
        Self { line, byte }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionRange {
    pub(super) start: TextPosition,
    pub(super) end: TextPosition,
}

impl SelectionRange {
    fn normalized(&self) -> Self {
        if position_le(self.start, self.end) {
            self.clone()
        } else {
            Self {
                start: self.end,
                end: self.start,
            }
        }
    }

    fn intersects_row(&self, row: &VisualRow) -> Option<(usize, usize)> {
        let range = self.normalized();
        if row.logical_line < range.start.line || row.logical_line > range.end.line {
            return None;
        }

        let mut start = row.byte_start;
        let mut end = row.byte_end;
        if row.logical_line == range.start.line {
            start = start.max(range.start.byte);
        }
        if row.logical_line == range.end.line {
            end = end.min(range.end.byte);
        }

        if start < end {
            Some((start, end))
        } else if row.byte_start == row.byte_end
            && row.logical_line >= range.start.line
            && row.logical_line <= range.end.line
            && row.logical_line != range.end.line
        {
            Some((row.byte_start, row.byte_end))
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveSelection {
    group: SelectableId,
    anchor: SelectionEndpoint,
    focus: SelectionEndpoint,
    dragging: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SelectionEndpoint {
    id: SelectableId,
    position: TextPosition,
}

impl SelectionEndpoint {
    const fn new(id: SelectableId, position: TextPosition) -> Self {
        Self { id, position }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveSelectionSpan {
    region_indices: Vec<usize>,
    start_ordinal: usize,
    end_ordinal: usize,
    start: TextPosition,
    end: TextPosition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectableRegion {
    pub(super) id: SelectableId,
    pub(super) area: Rect,
    pub(super) group: SelectableId,
    lines: Vec<String>,
    visual_rows: Vec<VisualRow>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SelectableVisualRow {
    pub(super) logical_line: usize,
    pub(super) byte_start: usize,
    pub(super) byte_end: usize,
    pub(super) screen_x: u16,
    pub(super) screen_y: u16,
    pub(super) display_width: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VisualRow {
    logical_line: usize,
    byte_start: usize,
    byte_end: usize,
    screen_x: u16,
    screen_y: u16,
    display_width: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectionRegistry {
    regions: Vec<SelectableRegion>,
    active: Option<ActiveSelection>,
}

impl SelectionRegistry {
    pub(super) fn set_regions(&mut self, regions: Vec<SelectableRegion>) {
        if let Some(active) = self.active.as_ref() {
            let endpoint_visible = |endpoint: &SelectionEndpoint| {
                regions
                    .iter()
                    .any(|region| region.group == active.group && region.id == endpoint.id)
            };
            if !endpoint_visible(&active.anchor) || !endpoint_visible(&active.focus) {
                self.active = None;
            }
        }
        self.regions = regions;
    }

    pub(super) fn clear_regions(&mut self) {
        self.regions.clear();
        self.active = None;
    }

    #[cfg(test)]
    pub(super) const fn active(&self) -> Option<&ActiveSelection> {
        self.active.as_ref()
    }

    pub(super) fn clear_selection(&mut self) -> bool {
        let had_selection = self.active.is_some();
        self.active = None;
        had_selection
    }

    pub(super) const fn has_selection(&self) -> bool {
        self.active.is_some()
    }

    pub(super) fn is_dragging(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|selection| selection.dragging)
    }

    pub(super) fn begin(&mut self, x: u16, y: u16) -> bool {
        let Some((group, endpoint)) = self.position_at(x, y) else {
            return self.clear_selection();
        };
        self.active = Some(ActiveSelection {
            group,
            anchor: endpoint.clone(),
            focus: endpoint,
            dragging: true,
        });
        true
    }

    pub(super) fn drag_to(&mut self, x: u16, y: u16) -> bool {
        let Some(active) = self.active.as_ref() else {
            return false;
        };
        if !active.dragging {
            return false;
        }
        let group = active.group.clone();
        let Some(endpoint) = self.position_near_group(x, y, &group) else {
            return false;
        };
        if let Some(active) = self.active.as_mut() {
            active.focus = endpoint;
        }
        true
    }

    pub(super) const fn end_drag(&mut self) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        active.dragging = false;
        true
    }

    pub(super) fn selected_text(&self) -> Option<String> {
        let active = self.active.as_ref()?;
        let span = self.active_span(active)?;
        let text = (span.start_ordinal..=span.end_ordinal)
            .filter_map(|ordinal| {
                let region = &self.regions[span.region_indices[ordinal]];
                let range = self.range_for_span_ordinal(&span, ordinal)?;
                let text = region.copy_range(&range);
                (ordinal == span.start_ordinal || ordinal == span.end_ordinal || !text.is_empty())
                    .then_some(text)
            })
            .collect::<Vec<_>>()
            .join("\n");
        (!text.is_empty()).then_some(text)
    }

    pub(super) fn region_selection(&self, id: &SelectableId) -> Option<SelectionRange> {
        let active = self.active.as_ref()?;
        let span = self.active_span(active)?;
        let ordinal = span
            .region_indices
            .iter()
            .position(|index| &self.regions[*index].id == id)?;
        if ordinal < span.start_ordinal || ordinal > span.end_ordinal {
            return None;
        }
        self.range_for_span_ordinal(&span, ordinal)
    }

    fn active_span(&self, active: &ActiveSelection) -> Option<ActiveSelectionSpan> {
        let region_indices = self.group_region_indices(&active.group);
        let anchor_ordinal = self.endpoint_ordinal(&region_indices, &active.anchor)?;
        let focus_ordinal = self.endpoint_ordinal(&region_indices, &active.focus)?;
        let anchor_first = anchor_ordinal < focus_ordinal
            || (anchor_ordinal == focus_ordinal
                && position_le(active.anchor.position, active.focus.position));
        let (start_ordinal, end_ordinal, start, end) = if anchor_first {
            (
                anchor_ordinal,
                focus_ordinal,
                active.anchor.position,
                active.focus.position,
            )
        } else {
            (
                focus_ordinal,
                anchor_ordinal,
                active.focus.position,
                active.anchor.position,
            )
        };
        Some(ActiveSelectionSpan {
            region_indices,
            start_ordinal,
            end_ordinal,
            start,
            end,
        })
    }

    fn range_for_span_ordinal(
        &self,
        span: &ActiveSelectionSpan,
        ordinal: usize,
    ) -> Option<SelectionRange> {
        let region = &self.regions[*span.region_indices.get(ordinal)?];
        let start = if ordinal == span.start_ordinal {
            span.start
        } else {
            SelectableRegion::start_position()
        };
        let end = if ordinal == span.end_ordinal {
            span.end
        } else {
            region.end_position()
        };
        Some(SelectionRange { start, end })
    }

    fn group_region_indices(&self, group: &SelectableId) -> Vec<usize> {
        let mut indices = self
            .regions
            .iter()
            .enumerate()
            .filter_map(|(index, region)| (&region.group == group).then_some(index))
            .collect::<Vec<_>>();
        indices.sort_by_key(|index| {
            let area = self.regions[*index].area;
            (area.y, area.x)
        });
        indices
    }

    fn endpoint_ordinal(
        &self,
        region_indices: &[usize],
        endpoint: &SelectionEndpoint,
    ) -> Option<usize> {
        region_indices
            .iter()
            .position(|index| self.regions[*index].id == endpoint.id)
    }

    fn position_at(&self, x: u16, y: u16) -> Option<(SelectableId, SelectionEndpoint)> {
        self.regions.iter().rev().find_map(|region| {
            region
                .area
                .contains((x, y).into())
                .then(|| region.endpoint_for_screen_clamped(x, y))
                .flatten()
                .map(|endpoint| (region.group.clone(), endpoint))
        })
    }

    fn position_at_in_group(
        &self,
        x: u16,
        y: u16,
        group: &SelectableId,
    ) -> Option<SelectionEndpoint> {
        self.regions
            .iter()
            .rev()
            .filter(|region| &region.group == group)
            .find_map(|region| {
                region
                    .area
                    .contains((x, y).into())
                    .then(|| region.endpoint_for_screen_clamped(x, y))
                    .flatten()
            })
    }

    fn position_near_group(
        &self,
        x: u16,
        y: u16,
        group: &SelectableId,
    ) -> Option<SelectionEndpoint> {
        if let Some(endpoint) = self.position_at_in_group(x, y, group) {
            return Some(endpoint);
        }

        let mut first = None;
        let mut previous = None;
        for region in self
            .regions
            .iter()
            .filter(|region| &region.group == group && !region.visual_rows.is_empty())
        {
            if first.is_none() {
                first = Some(region);
            }
            if y < region.area.y {
                return previous
                    .or(first)
                    .and_then(|region| region.endpoint_for_screen_clamped(x, y));
            }
            previous = Some(region);
        }

        previous
            .or(first)
            .and_then(|region| region.endpoint_for_screen_clamped(x, y))
    }
}

impl SelectableRegion {
    pub(super) fn new(id: SelectableId, area: Rect, lines: Vec<String>, scroll: u16) -> Self {
        let visual_rows = wrap_lines(&lines, area, scroll);
        Self {
            group: id.clone(),
            id,
            area,
            lines,
            visual_rows,
        }
    }

    pub(super) fn new_with_group(
        id: SelectableId,
        group: SelectableId,
        area: Rect,
        lines: Vec<String>,
        scroll: u16,
    ) -> Self {
        let visual_rows = wrap_lines(&lines, area, scroll);
        Self {
            id,
            area,
            group,
            lines,
            visual_rows,
        }
    }

    pub(super) fn from_visual_rows(
        id: SelectableId,
        area: Rect,
        lines: Vec<String>,
        rows: Vec<SelectableVisualRow>,
    ) -> Self {
        Self::from_visual_rows_with_group(id.clone(), id, area, lines, rows)
    }

    pub(super) fn from_visual_rows_with_group(
        id: SelectableId,
        group: SelectableId,
        area: Rect,
        lines: Vec<String>,
        rows: Vec<SelectableVisualRow>,
    ) -> Self {
        let visual_rows = rows
            .into_iter()
            .map(|row| VisualRow {
                logical_line: row.logical_line,
                byte_start: row.byte_start,
                byte_end: row.byte_end,
                screen_x: row.screen_x,
                screen_y: row.screen_y,
                display_width: row.display_width,
            })
            .collect();
        Self {
            id,
            area,
            group,
            lines,
            visual_rows,
        }
    }

    pub(super) fn highlight(&self, range: &SelectionRange, buf: &mut Buffer) {
        let selection_style = Style::default().bg(Color::DarkGray);
        for row in &self.visual_rows {
            let Some((start, end)) = range.intersects_row(row) else {
                continue;
            };
            let (start_col, end_col) =
                row.byte_range_to_visual_cols(&self.lines[row.logical_line], start, end);
            let screen_start = row.screen_x.saturating_add(start_col);
            let screen_end = if start == end && row.byte_start == row.byte_end {
                screen_start.saturating_add(1)
            } else {
                row.screen_x.saturating_add(end_col)
            };
            for x in screen_start..screen_end.min(self.area.right()) {
                if let Some(cell) = buf.cell_mut((x, row.screen_y)) {
                    cell.set_style(cell.style().patch(selection_style));
                }
            }
        }
    }

    fn endpoint_for_screen_clamped(&self, x: u16, y: u16) -> Option<SelectionEndpoint> {
        self.position_for_screen_clamped(x, y)
            .map(|position| SelectionEndpoint::new(self.id.clone(), position))
    }

    fn position_for_screen_clamped(&self, x: u16, y: u16) -> Option<TextPosition> {
        let first = self.visual_rows.first()?;
        let last = self.visual_rows.last()?;
        let row = if y <= first.screen_y {
            first
        } else if y >= last.screen_y {
            last
        } else {
            self.visual_rows
                .iter()
                .find(|row| row.screen_y == y)
                .unwrap_or(last)
        };
        let line = &self.lines[row.logical_line];
        let col = x.saturating_sub(row.screen_x).min(row.display_width);
        Some(TextPosition::new(
            row.logical_line,
            row.visual_col_to_byte(line, col),
        ))
    }

    const fn start_position() -> TextPosition {
        TextPosition::new(0, 0)
    }

    fn end_position(&self) -> TextPosition {
        let Some((line_index, line)) = self.lines.iter().enumerate().next_back() else {
            return TextPosition::new(0, 0);
        };
        TextPosition::new(line_index, line.len())
    }

    fn copy_range(&self, range: &SelectionRange) -> String {
        let range = range.normalized();
        if range.start.line >= self.lines.len() || range.end.line >= self.lines.len() {
            return String::new();
        }

        if range.start.line == range.end.line {
            return safe_slice(
                &self.lines[range.start.line],
                range.start.byte,
                range.end.byte,
            );
        }

        let mut out = String::new();
        out.push_str(&safe_slice(
            &self.lines[range.start.line],
            range.start.byte,
            self.lines[range.start.line].len(),
        ));
        for line in range.start.line + 1..range.end.line {
            out.push('\n');
            out.push_str(&self.lines[line]);
        }
        out.push('\n');
        out.push_str(&safe_slice(&self.lines[range.end.line], 0, range.end.byte));
        out
    }
}

impl VisualRow {
    fn visual_col_to_byte(&self, line: &str, col: u16) -> usize {
        if col == 0 {
            return self.byte_start;
        }

        let mut width = 0u16;
        for (offset, ch) in line[self.byte_start..self.byte_end].char_indices() {
            let byte = self.byte_start + offset;
            let ch_width = char_width(ch);
            let next = width.saturating_add(ch_width);
            if next >= col {
                if next == col {
                    return byte + ch.len_utf8();
                }
                let before = col.saturating_sub(width);
                let after = next.saturating_sub(col);
                return if before < after {
                    byte
                } else {
                    byte + ch.len_utf8()
                };
            }
            width = next;
        }

        self.byte_end
    }

    fn byte_range_to_visual_cols(&self, line: &str, start: usize, end: usize) -> (u16, u16) {
        let start = start.clamp(self.byte_start, self.byte_end);
        let end = end.clamp(self.byte_start, self.byte_end);
        (
            display_width_bytes(line, self.byte_start, start),
            display_width_bytes(line, self.byte_start, end),
        )
    }
}

fn wrap_lines(lines: &[String], area: Rect, scroll: u16) -> Vec<VisualRow> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }

    let mut rows = Vec::new();
    let mut logical_visual_row = 0u16;
    let viewport_top = scroll;
    let viewport_bottom = scroll.saturating_add(area.height);

    for (line_index, line) in lines.iter().enumerate() {
        let segments = wrap_line_segments(line, area.width);
        for (byte_start, byte_end, display_width) in segments {
            if logical_visual_row >= viewport_top && logical_visual_row < viewport_bottom {
                rows.push(VisualRow {
                    logical_line: line_index,
                    byte_start,
                    byte_end,
                    screen_x: area.x,
                    screen_y: area
                        .y
                        .saturating_add(logical_visual_row.saturating_sub(viewport_top)),
                    display_width,
                });
            }
            logical_visual_row = logical_visual_row.saturating_add(1);
        }
    }

    rows
}

fn wrap_line_segments(line: &str, width: u16) -> Vec<(usize, usize, u16)> {
    if width == 0 {
        return Vec::new();
    }
    if line.is_empty() {
        return vec![(0, 0, 0)];
    }

    let mut rows = Vec::new();
    let mut row_start = 0usize;
    let mut row_width = 0u16;

    for (byte, ch) in line.char_indices() {
        let ch_width = char_width(ch);
        if row_width > 0 && row_width.saturating_add(ch_width) > width {
            rows.push((row_start, byte, row_width));
            row_start = byte;
            row_width = 0;
        }
        row_width = row_width.saturating_add(ch_width);
        if row_width >= width {
            let end = byte + ch.len_utf8();
            rows.push((row_start, end, row_width.min(width)));
            row_start = end;
            row_width = 0;
        }
    }

    if row_start < line.len() {
        rows.push((row_start, line.len(), row_width));
    }
    rows
}

fn display_width_bytes(line: &str, start: usize, end: usize) -> u16 {
    line[start..end].chars().map(char_width).sum::<u16>()
}

fn char_width(ch: char) -> u16 {
    ch.width().unwrap_or(0).try_into().unwrap_or(u16::MAX)
}

fn safe_slice(line: &str, start: usize, end: usize) -> String {
    let start = previous_char_boundary(line, start.min(line.len()));
    let end = previous_char_boundary(line, end.min(line.len()));
    if start >= end {
        String::new()
    } else {
        line[start..end].to_string()
    }
}

const fn previous_char_boundary(line: &str, mut index: usize) -> usize {
    while index > 0 && !line.is_char_boundary(index) {
        index -= 1;
    }
    index
}

const fn position_le(left: TextPosition, right: TextPosition) -> bool {
    left.line < right.line || (left.line == right.line && left.byte <= right.byte)
}

pub(super) fn line_plain_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

pub(super) fn write_osc52_clipboard<W: Write>(writer: &mut W, text: &str) -> io::Result<()> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let encoded = STANDARD.encode(text.as_bytes());
    write!(writer, "\x1b]52;c;{encoded}\x07")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(lines: &[&str], width: u16, height: u16, scroll: u16) -> SelectableRegion {
        SelectableRegion::new(
            SelectableId::new("test"),
            Rect::new(0, 0, width, height),
            lines.iter().map(std::string::ToString::to_string).collect(),
            scroll,
        )
    }

    #[test]
    fn natural_wrap_selection_copies_without_fake_newline() {
        let region = region(&["abcdefghijkl"], 4, 3, 0);
        let start = region.position_for_screen_clamped(1, 0).unwrap();
        let end = region.position_for_screen_clamped(2, 2).unwrap();

        assert_eq!(
            region.copy_range(&SelectionRange { start, end }),
            "bcdefghij"
        );
    }

    #[test]
    fn manual_newline_selection_preserves_real_newline() {
        let region = region(&["hello", "world"], 10, 2, 0);
        let start = region.position_for_screen_clamped(2, 0).unwrap();
        let end = region.position_for_screen_clamped(3, 1).unwrap();

        assert_eq!(
            region.copy_range(&SelectionRange { start, end }),
            "llo\nwor"
        );
    }

    #[test]
    fn mixed_wrap_and_manual_newlines_copy_logical_text() {
        let region = region(&["abcdef", "ghijkl"], 3, 4, 0);
        let start = region.position_for_screen_clamped(1, 0).unwrap();
        let end = region.position_for_screen_clamped(2, 3).unwrap();

        assert_eq!(
            region.copy_range(&SelectionRange { start, end }),
            "bcdef\nghijk"
        );
    }

    #[test]
    fn wide_unicode_columns_map_to_char_boundaries() {
        let region = region(&["a你b"], 10, 1, 0);

        let before_wide = region.position_for_screen_clamped(1, 0).unwrap();
        let after_wide = region.position_for_screen_clamped(3, 0).unwrap();

        assert_eq!(
            region.copy_range(&SelectionRange {
                start: before_wide,
                end: after_wide
            }),
            "你"
        );
    }

    #[test]
    fn combining_marks_do_not_panic_or_shift_later_text() {
        let region = region(&["e\u{301}cho"], 10, 1, 0);
        let start = region.position_for_screen_clamped(0, 0).unwrap();
        let end = region.position_for_screen_clamped(4, 0).unwrap();

        assert_eq!(
            region.copy_range(&SelectionRange { start, end }),
            "e\u{301}cho"
        );
    }

    #[test]
    fn empty_lines_can_be_selected() {
        let region = region(&["alpha", "", "omega"], 10, 3, 0);
        let start = TextPosition::new(0, 5);
        let end = TextPosition::new(2, 0);

        assert_eq!(region.copy_range(&SelectionRange { start, end }), "\n\n");
    }

    #[test]
    fn reverse_selection_normalizes_to_same_copy_text() {
        let region = region(&["abcdef"], 10, 1, 0);
        let start = TextPosition::new(0, 5);
        let end = TextPosition::new(0, 1);

        assert_eq!(region.copy_range(&SelectionRange { start, end }), "bcde");
    }

    #[test]
    fn partially_scrolled_region_keeps_logical_selection() {
        let region = region(&["one", "two", "three"], 10, 2, 1);
        let range = SelectionRange {
            start: TextPosition::new(0, 1),
            end: TextPosition::new(2, 2),
        };

        assert_eq!(region.copy_range(&range), "ne\ntwo\nth");
        assert_eq!(region.visual_rows[0].logical_line, 1);
    }

    #[test]
    fn bottom_clipped_region_keeps_logical_selection_and_clamps_highlight() {
        let region = region(&["one", "two", "three"], 10, 2, 0);
        let range = SelectionRange {
            start: TextPosition::new(0, 1),
            end: TextPosition::new(2, 2),
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 2));

        region.highlight(&range, &mut buf);

        assert_eq!(region.copy_range(&range), "ne\ntwo\nth");
        assert_eq!(region.visual_rows.len(), 2);
        assert_eq!(region.visual_rows[0].logical_line, 0);
        assert_eq!(region.visual_rows[1].logical_line, 1);
        assert_eq!(buf[(1, 0)].style().bg, Some(Color::DarkGray));
        assert_eq!(buf[(0, 1)].style().bg, Some(Color::DarkGray));
    }

    #[test]
    fn registry_clears_selection_when_component_disappears() {
        let mut registry = SelectionRegistry::default();
        registry.set_regions(vec![region(&["visible"], 20, 1, 0)]);
        assert!(registry.begin(1, 0));
        assert!(registry.drag_to(4, 0));

        registry.set_regions(Vec::new());

        assert!(registry.active().is_none());
    }

    #[test]
    fn dragging_outside_component_clamps_to_component_boundary() {
        let mut registry = SelectionRegistry::default();
        registry.set_regions(vec![region(&["abc", "def"], 3, 2, 0)]);

        assert!(registry.begin(1, 0));
        assert!(registry.drag_to(99, 99));

        assert_eq!(registry.selected_text().as_deref(), Some("bc\ndef"));
    }

    #[test]
    fn dragging_across_grouped_regions_selects_all_intervening_text() {
        let mut registry = SelectionRegistry::default();
        let group = SelectableId::new("messages");
        registry.set_regions(vec![
            SelectableRegion::new_with_group(
                SelectableId::new("message-1"),
                group.clone(),
                Rect::new(0, 0, 20, 1),
                vec!["first message".to_string()],
                0,
            ),
            SelectableRegion::new_with_group(
                SelectableId::new("message-2"),
                group,
                Rect::new(0, 2, 20, 1),
                vec!["second message".to_string()],
                0,
            ),
        ]);

        assert!(registry.begin(6, 0));
        assert!(registry.drag_to(6, 2));

        assert_eq!(registry.selected_text().as_deref(), Some("message\nsecond"));
    }

    #[test]
    fn starting_new_selection_replaces_previous_selection() {
        let mut registry = SelectionRegistry::default();
        registry.set_regions(vec![region(&["first", "second"], 20, 2, 0)]);

        assert!(registry.begin(0, 0));
        assert!(registry.drag_to(5, 0));
        assert_eq!(registry.selected_text().as_deref(), Some("first"));

        assert!(registry.begin(0, 1));
        assert!(registry.drag_to(6, 1));

        assert_eq!(registry.selected_text().as_deref(), Some("second"));
    }

    #[test]
    fn osc52_writer_emits_base64_clipboard_sequence() {
        let mut output = Vec::new();

        write_osc52_clipboard(&mut output, "copy me").unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "\x1b]52;c;Y29weSBtZQ==\x07"
        );
    }
}
