use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent};
use unicode_width::UnicodeWidthChar;

use super::command_panels::{CommandFeedback, CommandPanel};
use super::selection::{SelectableRegion, SelectionRegistry};
use super::tui_event::TuiMouseSelectionKind;
use super::{
    ActivityCell, CachedActivityLines, DashboardActivityHistoryPage, DashboardCommandAttachment,
    DashboardState, LiveActivityCell, WebActivityItem, activity_cells_from_history_items,
    cells::append_runtime_status_live_cell,
};

use super::terminal_hyperlinks::TerminalHyperlinkOverlay;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CtrlCReminder {
    Interrupt,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PendingUserInputEditState {
    pub(super) event_id: String,
}

/// Editable input string with cursor tracking for in-place editing.
#[derive(Debug)]
pub(super) struct InputState {
    pub(super) text: String,
    /// Byte offset of the cursor within `text`.
    pub(super) cursor_pos: usize,
}

impl InputState {
    pub(super) fn new() -> Self {
        Self {
            text: String::new(),
            cursor_pos: 0,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub(super) fn as_str(&self) -> &str {
        &self.text
    }

    /// Insert a character at cursor and advance cursor past it.
    pub(super) fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    /// Delete the character before the cursor (Backspace).
    pub(super) fn delete_before_cursor(&mut self) {
        if self.cursor_pos > 0 {
            let mut prev = self.cursor_pos - 1;
            while prev > 0 && !self.text.is_char_boundary(prev) {
                prev -= 1;
            }
            self.text.remove(prev);
            self.cursor_pos = prev;
        }
    }

    pub(super) fn move_left(&mut self) {
        if self.cursor_pos > 0 {
            let mut pos = self.cursor_pos - 1;
            while pos > 0 && !self.text.is_char_boundary(pos) {
                pos -= 1;
            }
            self.cursor_pos = pos;
        }
    }

    pub(super) fn move_right(&mut self) {
        if self.cursor_pos < self.text.len() {
            let mut pos = self.cursor_pos + 1;
            while pos < self.text.len() && !self.text.is_char_boundary(pos) {
                pos += 1;
            }
            self.cursor_pos = pos;
        }
    }

    pub(super) fn move_up_line(&mut self) -> bool {
        self.move_to_adjacent_line(false)
    }

    pub(super) fn move_down_line(&mut self) -> bool {
        self.move_to_adjacent_line(true)
    }

    fn move_to_adjacent_line(&mut self, down: bool) -> bool {
        let (line_start, line_end) = self.current_line_bounds();
        let target_col = self.display_width(line_start, self.cursor_pos);
        let target_bounds = if down {
            self.next_line_bounds(line_end)
        } else {
            self.previous_line_bounds(line_start)
        };
        let Some((target_start, target_end)) = target_bounds else {
            return false;
        };
        self.cursor_pos = self.byte_pos_for_display_col(target_start, target_end, target_col);
        true
    }

    fn current_line_bounds(&self) -> (usize, usize) {
        let cursor_pos = self.cursor_pos.min(self.text.len());
        let line_start = self.text[..cursor_pos]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        let line_end = self.text[cursor_pos..]
            .find('\n')
            .map(|index| cursor_pos + index)
            .unwrap_or(self.text.len());
        (line_start, line_end)
    }

    fn previous_line_bounds(&self, line_start: usize) -> Option<(usize, usize)> {
        if line_start == 0 {
            return None;
        }
        let previous_line_end = line_start - 1;
        let previous_line_start = self.text[..previous_line_end]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        Some((previous_line_start, previous_line_end))
    }

    fn next_line_bounds(&self, line_end: usize) -> Option<(usize, usize)> {
        if line_end >= self.text.len() {
            return None;
        }
        let next_line_start = line_end + 1;
        let next_line_end = self.text[next_line_start..]
            .find('\n')
            .map(|index| next_line_start + index)
            .unwrap_or(self.text.len());
        Some((next_line_start, next_line_end))
    }

    fn display_width(&self, start: usize, end: usize) -> usize {
        self.text[start..end]
            .chars()
            .map(|ch| ch.width().unwrap_or(0))
            .sum()
    }

    fn byte_pos_for_display_col(
        &self,
        line_start: usize,
        line_end: usize,
        target_col: usize,
    ) -> usize {
        if target_col == 0 {
            return line_start;
        }

        let mut width = 0usize;
        for (offset, ch) in self.text[line_start..line_end].char_indices() {
            let next_width = width + ch.width().unwrap_or(0);
            if next_width >= target_col {
                if next_width == target_col {
                    return line_start + offset + ch.len_utf8();
                }
                let before_distance = target_col.saturating_sub(width);
                let after_distance = next_width.saturating_sub(target_col);
                if before_distance < after_distance {
                    return line_start + offset;
                }
                return line_start + offset + ch.len_utf8();
            }
            width = next_width;
        }

        line_end
    }

    pub(super) fn move_home(&mut self) {
        self.cursor_pos = 0;
    }

    pub(super) fn move_end(&mut self) {
        self.cursor_pos = self.text.len();
    }

    pub(super) fn clear(&mut self) {
        self.text.clear();
        self.cursor_pos = 0;
    }

    /// Replace text and move cursor to end.
    pub(super) fn set_text(&mut self, text: String) {
        self.text = text;
        self.cursor_pos = self.text.len();
    }
}

pub(super) struct TranscriptOverlayState {
    pub(super) cells: Vec<ActivityCell>,
    pub(super) live_cells: Vec<LiveActivityCell>,
    pub(super) history_prefix_len: usize,
    pub(super) scroll: u16,
    pub(super) follow_bottom: bool,
    pub(super) max_scroll: u16,
    pub(super) page_height: u16,
}

impl TranscriptOverlayState {
    pub(super) fn new(
        cells: Vec<ActivityCell>,
        live_cells: Vec<LiveActivityCell>,
        state_activity_len: usize,
    ) -> Self {
        Self {
            history_prefix_len: cells.len().saturating_sub(state_activity_len),
            cells,
            live_cells,
            scroll: 0,
            follow_bottom: true,
            max_scroll: 0,
            page_height: 20,
        }
    }

    pub(super) fn sync_state(&mut self, state: &DashboardState) {
        let mut next_cells = self
            .cells
            .iter()
            .take(self.history_prefix_len)
            .cloned()
            .collect::<Vec<_>>();
        next_cells.extend(state.activity_cells.clone());
        self.cells = next_cells;
        let mut live_cells = state.live_activity_cells.clone();
        append_runtime_status_live_cell(&mut live_cells, state);
        self.live_cells = live_cells;
        self.clamp_scroll();
    }

    pub(super) fn set_render_metrics(&mut self, max_scroll: u16, page_height: u16) {
        self.max_scroll = max_scroll;
        self.page_height = page_height.max(1);
        self.clamp_scroll();
    }

    pub(super) fn effective_scroll(&self) -> u16 {
        if self.follow_bottom {
            self.max_scroll
        } else {
            self.scroll.min(self.max_scroll)
        }
    }

    pub(super) fn handle_scroll_rows(&mut self, rows: i16) -> bool {
        match rows.cmp(&0) {
            std::cmp::Ordering::Less => {
                let rows = rows.unsigned_abs();
                self.leave_bottom_follow(rows);
                self.scroll = self.scroll.saturating_sub(rows);
                true
            }
            std::cmp::Ordering::Greater => {
                if self.follow_bottom {
                    return true;
                }
                self.scroll = self.scroll.saturating_add(rows as u16);
                if self.scroll >= self.max_scroll {
                    self.follow_bottom = true;
                    self.scroll = 0;
                }
                true
            }
            std::cmp::Ordering::Equal => false,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.handle_scroll_rows(-1),
            KeyCode::Down | KeyCode::Char('j') => self.handle_scroll_rows(1),
            KeyCode::PageUp => {
                let page_height = self.page_height.min(i16::MAX as u16) as i16;
                self.handle_scroll_rows(-page_height)
            }
            KeyCode::PageDown => {
                let page_height = self.page_height.min(i16::MAX as u16) as i16;
                self.handle_scroll_rows(page_height)
            }
            KeyCode::Home => {
                self.follow_bottom = false;
                self.scroll = 0;
                true
            }
            KeyCode::End => {
                self.follow_bottom = true;
                self.scroll = 0;
                true
            }
            _ => false,
        }
    }

    fn leave_bottom_follow(&mut self, rows_from_bottom: u16) {
        if self.follow_bottom {
            self.follow_bottom = false;
            self.scroll = self.max_scroll.saturating_sub(rows_from_bottom);
        }
    }

    fn clamp_scroll(&mut self) {
        if self.follow_bottom {
            self.scroll = 0;
        } else {
            self.scroll = self.scroll.min(self.max_scroll);
        }
    }
}

pub(super) struct TuiViewState {
    pub(super) command_input: InputState,
    pub(super) pending_pastes: Vec<(String, String)>,
    pub(super) pending_image_attachments: Vec<DashboardCommandAttachment>,
    pub(super) command_popup_selection: usize,
    pub(super) command_popup_scroll: usize,
    pub(super) command_panel: Option<CommandPanel>,
    pub(super) transcript_overlay: Option<TranscriptOverlayState>,
    pub(super) command_feedback: Option<CommandFeedback>,
    pub(super) ctrl_c_reminder: Option<CtrlCReminder>,
    pub(super) editing_pending_user_input: Option<PendingUserInputEditState>,
    command_history: Vec<String>,
    command_history_cursor: Option<usize>,
    command_history_recalled_text: Option<String>,
    pub(super) scroll_offset: u16,
    pub(super) auto_scroll: bool,
    pub(super) max_scroll: u16,
    pub(super) page_height: u16,
    pub(super) last_cursor_pos: Option<(u16, u16)>,
    pub(super) previous_hyperlink_overlays: Vec<TerminalHyperlinkOverlay>,
    pub(super) selection: SelectionRegistry,
    pub(super) extra_history_cells: Vec<ActivityCell>,
    pub(super) oldest_cursor: Option<i64>,
    pub(super) has_more_before: bool,
    pub(super) loading_history: bool,
    pub(super) load_cooldown: u8,
    pub(super) history_load_rx:
        Option<tokio::sync::oneshot::Receiver<Result<DashboardActivityHistoryPage, String>>>,
    pub(super) cached_activity_lines: CachedActivityLines,
    pub(super) expanded_thinking: HashSet<usize>,
    pub(super) visible_activity_cleared: bool,
}

impl TuiViewState {
    pub(super) fn new() -> Self {
        Self {
            command_input: InputState::new(),
            pending_pastes: Vec::new(),
            pending_image_attachments: Vec::new(),
            command_popup_selection: 0,
            command_popup_scroll: 0,
            command_panel: None,
            transcript_overlay: None,
            command_feedback: None,
            ctrl_c_reminder: None,
            editing_pending_user_input: None,
            command_history: Vec::new(),
            command_history_cursor: None,
            command_history_recalled_text: None,
            scroll_offset: 0,
            auto_scroll: true,
            max_scroll: 0,
            page_height: 20,
            last_cursor_pos: None,
            previous_hyperlink_overlays: Vec::new(),
            selection: SelectionRegistry::default(),
            extra_history_cells: Vec::new(),
            oldest_cursor: None,
            has_more_before: false,
            loading_history: false,
            load_cooldown: 0,
            history_load_rx: None,
            cached_activity_lines: CachedActivityLines::new(),
            expanded_thinking: HashSet::new(),
            visible_activity_cleared: false,
        }
    }

    pub(super) fn reset_command_popup(&mut self) {
        self.command_popup_selection = 0;
        self.command_popup_scroll = 0;
    }

    pub(super) fn open_transcript_overlay(&mut self, state: &DashboardState) {
        let (cells, live_cells) = self.visible_activity_cells(state);
        self.transcript_overlay = Some(TranscriptOverlayState::new(
            cells,
            live_cells,
            state.activity_cells.len(),
        ));
        self.command_panel = None;
        self.command_feedback = None;
        self.reset_command_popup();
        self.selection.clear_selection();
    }

    pub(super) fn sync_transcript_overlay(&mut self, state: &DashboardState) {
        if let Some(overlay) = self.transcript_overlay.as_mut() {
            overlay.sync_state(state);
        }
    }

    pub(super) fn handle_transcript_overlay_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.selection.clear_selection() {
                    return true;
                }
                self.transcript_overlay = None;
                true
            }
            _ => self
                .transcript_overlay
                .as_mut()
                .is_some_and(|overlay| overlay.handle_key(key)),
        }
    }

    pub(super) fn handle_transcript_overlay_scroll_rows(&mut self, rows: i16) -> bool {
        self.transcript_overlay
            .as_mut()
            .is_some_and(|overlay| overlay.handle_scroll_rows(rows))
    }

    pub(super) fn clear_ctrl_c_reminder(&mut self) {
        self.ctrl_c_reminder = None;
    }

    pub(super) fn begin_pending_user_input_edit(
        &mut self,
        event_id: String,
        incoming_text: String,
    ) {
        self.command_input.set_text(incoming_text);
        self.pending_pastes.clear();
        self.pending_image_attachments.clear();
        self.command_panel = None;
        self.command_feedback = None;
        self.ctrl_c_reminder = None;
        self.editing_pending_user_input = Some(PendingUserInputEditState { event_id });
        self.reset_command_history_navigation();
        self.reset_command_popup();
    }

    pub(super) fn cancel_pending_user_input_edit(&mut self) {
        self.editing_pending_user_input = None;
        self.command_input.clear();
        self.pending_pastes.clear();
        self.pending_image_attachments.clear();
        self.command_feedback = None;
        self.reset_command_history_navigation();
        self.reset_command_popup();
    }

    pub(super) fn sync_pending_user_input_edit(&mut self, state: &DashboardState) {
        let Some(editing) = self.editing_pending_user_input.as_ref() else {
            return;
        };
        if state
            .pending_user_inputs
            .iter()
            .any(|input| input.event_id == editing.event_id)
        {
            return;
        }
        self.editing_pending_user_input = None;
        self.command_input.clear();
        self.pending_pastes.clear();
        self.pending_image_attachments.clear();
        self.command_feedback = None;
        self.reset_command_history_navigation();
        self.reset_command_popup();
    }

    pub(super) fn record_command_history(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.reset_command_history_navigation();
        self.push_command_history_entry(text);
    }

    pub(super) fn replace_command_history(&mut self, entries: Vec<String>) {
        self.command_history.clear();
        self.extend_command_history(entries);
        self.reset_command_history_navigation();
    }

    pub(super) fn seed_command_history_from_state(&mut self, state: &DashboardState) {
        if !self.command_history.is_empty() {
            return;
        }
        self.extend_command_history(command_history_entries_from_state(state));
    }

    fn extend_command_history(&mut self, entries: impl IntoIterator<Item = String>) {
        for entry in entries {
            self.push_command_history_entry(&entry);
        }
    }

    fn push_command_history_entry(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self
            .command_history
            .last()
            .is_some_and(|previous| previous == text)
        {
            return;
        }
        self.command_history.push(text.to_string());
    }

    pub(super) fn reset_command_history_navigation(&mut self) {
        self.command_history_cursor = None;
        self.command_history_recalled_text = None;
    }

    pub(super) fn navigate_command_history_up(&mut self) -> bool {
        if !self.should_handle_command_history_navigation() {
            return false;
        }
        let total_entries = self.command_history.len();
        let Some(next_index) = self
            .command_history_cursor
            .map(|index| index.checked_sub(1))
            .unwrap_or_else(|| total_entries.checked_sub(1))
        else {
            return false;
        };
        self.replace_command_input_from_history(next_index)
    }

    pub(super) fn navigate_command_history_down(&mut self) -> bool {
        if !self.should_handle_command_history_navigation() {
            return false;
        }
        let Some(current_index) = self.command_history_cursor else {
            return false;
        };
        let next_index = current_index + 1;
        if next_index >= self.command_history.len() {
            self.command_history_cursor = None;
            self.command_history_recalled_text = None;
            self.command_input.clear();
            self.pending_pastes.clear();
            self.pending_image_attachments.clear();
            self.reset_command_popup();
            return true;
        }
        self.replace_command_input_from_history(next_index)
    }

    fn should_handle_command_history_navigation(&self) -> bool {
        if self.command_history.is_empty() {
            return false;
        }
        let text = self.command_input.as_str();
        if text.is_empty() {
            return true;
        }
        if self.command_input.cursor_pos != 0 {
            return false;
        }
        self.command_history_recalled_text.as_deref() == Some(text)
    }

    fn replace_command_input_from_history(&mut self, index: usize) -> bool {
        let Some(text) = self.command_history.get(index).cloned() else {
            return false;
        };
        self.command_history_cursor = Some(index);
        self.command_history_recalled_text = Some(text.clone());
        self.command_input.set_text(text);
        self.command_input.move_home();
        self.pending_pastes.clear();
        self.pending_image_attachments.clear();
        self.reset_command_popup();
        true
    }

    pub(super) fn effective_scroll(&self) -> u16 {
        if self.auto_scroll {
            self.max_scroll
        } else {
            self.scroll_offset
        }
    }

    pub(super) fn display_scroll(&self) -> u16 {
        if self.auto_scroll {
            u16::MAX
        } else {
            self.scroll_offset
        }
    }

    pub(super) fn visible_activity_cells(
        &self,
        state: &DashboardState,
    ) -> (Vec<ActivityCell>, Vec<LiveActivityCell>) {
        let mut committed_cells = if self.visible_activity_cleared {
            Vec::new()
        } else {
            let mut cells = self.extra_history_cells.clone();
            cells.extend(state.activity_cells.clone());
            cells
        };
        for (i, cell) in committed_cells.iter_mut().enumerate() {
            if let ActivityCell::Thinking(thinking) = cell {
                thinking.expanded = self.expanded_thinking.contains(&i);
            }
        }
        let mut live_cells = if self.visible_activity_cleared {
            Vec::new()
        } else {
            state.live_activity_cells.clone()
        };
        append_runtime_status_live_cell(&mut live_cells, state);
        (committed_cells, live_cells)
    }

    pub(super) fn tick_history_load_cooldown(&mut self) {
        self.load_cooldown = self.load_cooldown.saturating_sub(1);
    }

    pub(super) fn should_start_history_load(&self, has_history_loader: bool) -> bool {
        has_history_loader
            && !self.loading_history
            && self.load_cooldown == 0
            && self.has_more_before
            && self.effective_scroll() <= 3
    }

    pub(super) fn begin_history_load(
        &mut self,
        rx: tokio::sync::oneshot::Receiver<Result<DashboardActivityHistoryPage, String>>,
    ) {
        self.loading_history = true;
        self.history_load_rx = Some(rx);
    }

    pub(super) fn oldest_history_cursor(&self) -> Option<i64> {
        self.oldest_cursor
    }

    pub(super) fn take_history_load_rx(
        &mut self,
    ) -> Option<tokio::sync::oneshot::Receiver<Result<DashboardActivityHistoryPage, String>>> {
        self.history_load_rx.take()
    }

    pub(super) fn keep_history_load_rx(
        &mut self,
        rx: tokio::sync::oneshot::Receiver<Result<DashboardActivityHistoryPage, String>>,
    ) {
        self.history_load_rx = Some(rx);
    }

    pub(super) fn apply_loaded_history_page(&mut self, page: DashboardActivityHistoryPage) {
        let new_cells = activity_cells_from_history_items(&page.items);
        let mut merged = new_cells;
        merged.extend(self.extra_history_cells.clone());
        self.extra_history_cells = merged;
        self.auto_scroll = false;
        self.scroll_offset = 0;
        self.oldest_cursor = page.oldest_cursor;
        self.has_more_before = page.has_more_before;
        self.loading_history = false;
        self.load_cooldown = 10;
    }

    pub(super) fn finish_history_load_without_page(&mut self) {
        self.loading_history = false;
    }

    pub(super) fn sync_history_cursor_from_state(&mut self, state: &DashboardState) {
        if self.oldest_cursor.is_none() && !state.activity_history.items.is_empty() {
            self.oldest_cursor = state.activity_history.oldest_cursor;
            self.has_more_before = state.activity_history.has_more_before;
        }
    }

    pub(super) fn sync_visible_clear_from_state(&mut self, state: &DashboardState) {
        if self.visible_activity_cleared
            && state.activity_history.items.is_empty()
            && state.activity_cells.is_empty()
            && state.live_activity_cells.is_empty()
        {
            self.visible_activity_cleared = false;
        }
    }

    pub(super) fn clear_visible_activity(&mut self) {
        self.extra_history_cells.clear();
        self.oldest_cursor = None;
        self.has_more_before = false;
        self.loading_history = false;
        self.history_load_rx = None;
        self.cached_activity_lines = CachedActivityLines::new();
        self.pending_image_attachments.clear();
        self.ctrl_c_reminder = None;
        self.expanded_thinking.clear();
        self.auto_scroll = true;
        self.scroll_offset = 0;
        self.visible_activity_cleared = true;
        self.transcript_overlay = None;
        self.selection.clear_regions();
    }

    pub(super) fn set_selectable_regions(&mut self, regions: Vec<SelectableRegion>) {
        self.selection.set_regions(regions);
    }

    pub(super) fn handle_selection_mouse_event(
        &mut self,
        kind: TuiMouseSelectionKind,
        x: u16,
        y: u16,
    ) -> bool {
        match kind {
            TuiMouseSelectionKind::Down => self.selection.begin(x, y),
            TuiMouseSelectionKind::Drag => self.selection.drag_to(x, y),
            TuiMouseSelectionKind::Up => {
                let moved = self.selection.drag_to(x, y);
                self.selection.end_drag() || moved
            }
        }
    }

    pub(super) fn selected_text(&self) -> Option<String> {
        self.selection.selected_text()
    }

    pub(super) fn clear_selection(&mut self) -> bool {
        self.selection.clear_selection()
    }

    pub(super) fn selection_dragging(&self) -> bool {
        self.selection.is_dragging()
    }

    pub(super) fn toggle_thinking_expansion(&mut self, activity_cells: &[ActivityCell]) -> bool {
        let offset = self.extra_history_cells.len();
        let mut any_thinking = false;
        for (i, cell) in activity_cells.iter().enumerate() {
            if matches!(cell, ActivityCell::Thinking(_)) {
                let idx = offset + i;
                if self.expanded_thinking.contains(&idx) {
                    self.expanded_thinking.remove(&idx);
                } else {
                    self.expanded_thinking.insert(idx);
                }
                any_thinking = true;
            }
        }
        if any_thinking {
            self.cached_activity_lines = CachedActivityLines::new();
        }
        any_thinking
    }

    pub(super) fn handle_activity_scroll_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::PageUp => {
                let page_height = self.page_height.min(i16::MAX as u16) as i16;
                self.handle_activity_scroll_rows(-page_height)
            }
            KeyCode::PageDown => {
                let page_height = self.page_height.min(i16::MAX as u16) as i16;
                self.handle_activity_scroll_rows(page_height)
            }
            KeyCode::Home => {
                if self.max_scroll == 0 || (!self.auto_scroll && self.scroll_offset == 0) {
                    return false;
                }
                self.auto_scroll = false;
                self.scroll_offset = 0;
                true
            }
            KeyCode::End => {
                if self.auto_scroll {
                    return false;
                }
                self.auto_scroll = true;
                self.scroll_offset = 0;
                true
            }
            _ => false,
        }
    }

    pub(super) fn handle_activity_scroll_rows(&mut self, rows: i16) -> bool {
        if rows == 0 || self.max_scroll == 0 {
            return false;
        }

        match rows.cmp(&0) {
            std::cmp::Ordering::Less => {
                let rows = rows.unsigned_abs();
                if self.auto_scroll {
                    self.auto_scroll = false;
                    self.scroll_offset = self.max_scroll.saturating_sub(rows);
                    return true;
                }

                let previous_scroll = self.scroll_offset;
                self.scroll_offset = self.scroll_offset.saturating_sub(rows);
                self.scroll_offset != previous_scroll
            }
            std::cmp::Ordering::Greater => {
                if self.auto_scroll {
                    return false;
                }

                let previous_scroll = self.scroll_offset;
                let rows = rows as u16;
                self.scroll_offset = self.scroll_offset.saturating_add(rows);
                if self.scroll_offset >= self.max_scroll {
                    self.auto_scroll = true;
                }
                self.auto_scroll || self.scroll_offset != previous_scroll
            }
            std::cmp::Ordering::Equal => false,
        }
    }
}

fn command_history_entries_from_state(state: &DashboardState) -> Vec<String> {
    let items = if state.activity_history.items.is_empty() {
        state.web_activity_items.as_slice()
    } else {
        state.activity_history.items.as_slice()
    };
    items
        .iter()
        .filter_map(command_history_text_from_activity_item)
        .collect()
}

fn command_history_text_from_activity_item(item: &WebActivityItem) -> Option<String> {
    let Some(ActivityCell::User(cell)) = item.cell.as_ref() else {
        return None;
    };
    let text = cell
        .full_body
        .clone()
        .unwrap_or_else(|| {
            std::iter::once(cell.title.as_str())
                .chain(cell.body_lines.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .trim()
        .to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::selection::{SelectableId, SelectableRegion};
    use crate::dashboard::{
        DashboardActivityHistoryWindow, DashboardRuntimeActivity, assistant_activity_cell,
        render_activity_from_messages, web_activity_item_from_cell,
    };
    use crate::reasoning::runtime::HistoryMessage;
    use ratatui::layout::Rect;

    fn user_history_item(id: &str, text: &str) -> WebActivityItem {
        let cell = render_activity_from_messages(vec![HistoryMessage::user(text.to_string())])
            .into_iter()
            .next()
            .expect("user activity cell");
        web_activity_item_from_cell(&cell, id, false)
    }

    #[test]
    fn visible_activity_cells_adds_runtime_status_live_cell() {
        let view = TuiViewState::new();
        let state = DashboardState {
            runtime_activity: DashboardRuntimeActivity::default()
                .with_runtime_turn(Some("model request".to_string()), Some(1_000)),
            ..DashboardState::default()
        };

        let (_, live_cells) = view.visible_activity_cells(&state);

        assert!(state.live_activity_cells.is_empty());
        assert_eq!(live_cells.len(), 1);
        assert_eq!(live_cells[0].key, "runtime-status");
        let ActivityCell::RuntimeStatus(cell) = &live_cells[0].cell else {
            panic!("expected runtime status live cell");
        };
        assert_eq!(cell.label, "Working");
        assert_eq!(cell.active_runtime_started_at_ms, Some(1_000));
    }

    #[test]
    fn command_history_seeds_from_activity_history() {
        let mut view = TuiViewState::new();
        let state = DashboardState {
            activity_history: DashboardActivityHistoryWindow {
                items: vec![
                    user_history_item("history-1", "first command"),
                    user_history_item("history-2", "second command"),
                ],
                ..DashboardActivityHistoryWindow::default()
            },
            ..DashboardState::default()
        };

        view.seed_command_history_from_state(&state);

        assert!(view.navigate_command_history_up());
        assert_eq!(view.command_input.as_str(), "second command");
        assert!(view.navigate_command_history_up());
        assert_eq!(view.command_input.as_str(), "first command");
        assert!(view.navigate_command_history_down());
        assert_eq!(view.command_input.as_str(), "second command");
    }
    #[test]
    fn scroll_rows_moves_up_from_auto_scroll_without_key_event() {
        let mut view = TuiViewState::new();
        view.max_scroll = 100;
        view.auto_scroll = true;

        assert!(view.handle_activity_scroll_rows(-3));

        assert!(!view.auto_scroll);
        assert_eq!(view.scroll_offset, 97);
    }

    #[test]
    fn selection_dragging_tracks_mouse_gesture_lifetime() {
        let mut view = TuiViewState::new();
        view.set_selectable_regions(vec![SelectableRegion::new(
            SelectableId::new("drag"),
            Rect::new(0, 0, 20, 1),
            vec!["drag selection".to_string()],
            0,
        )]);

        assert!(view.handle_selection_mouse_event(TuiMouseSelectionKind::Down, 0, 0));
        assert!(view.selection_dragging());
        assert!(view.handle_selection_mouse_event(TuiMouseSelectionKind::Up, 4, 0));
        assert!(!view.selection_dragging());
        assert_eq!(view.selected_text().as_deref(), Some("drag"));
    }

    #[test]
    fn scroll_rows_reenters_auto_scroll_at_bottom() {
        let mut view = TuiViewState::new();
        view.max_scroll = 100;
        view.auto_scroll = false;
        view.scroll_offset = 98;

        assert!(view.handle_activity_scroll_rows(3));

        assert!(view.auto_scroll);
    }

    #[test]
    fn zero_scroll_rows_are_ignored() {
        let mut view = TuiViewState::new();

        assert!(!view.handle_activity_scroll_rows(0));
        assert!(view.auto_scroll);
        assert_eq!(view.scroll_offset, 0);
    }

    #[test]
    fn up_down_keys_do_not_scroll_activity_feed() {
        let mut view = TuiViewState::new();
        view.max_scroll = 100;
        view.auto_scroll = true;

        assert!(!view.handle_activity_scroll_key(KeyEvent::new(
            KeyCode::Up,
            crossterm::event::KeyModifiers::NONE
        )));
        assert!(!view.handle_activity_scroll_key(KeyEvent::new(
            KeyCode::Down,
            crossterm::event::KeyModifiers::NONE
        )));

        assert!(view.auto_scroll);
        assert_eq!(view.scroll_offset, 0);
    }

    #[test]
    fn page_keys_still_scroll_activity_feed() {
        let mut view = TuiViewState::new();
        view.max_scroll = 100;
        view.page_height = 20;
        view.auto_scroll = true;

        assert!(view.handle_activity_scroll_key(KeyEvent::new(
            KeyCode::PageUp,
            crossterm::event::KeyModifiers::NONE
        )));

        assert!(!view.auto_scroll);
        assert_eq!(view.scroll_offset, 80);
    }

    fn assistant_cell(text: &str) -> ActivityCell {
        assistant_activity_cell(text).expect("assistant cell")
    }

    #[test]
    fn transcript_overlay_syncs_state_after_history_prefix() {
        let history = assistant_cell("older history");
        let first = assistant_cell("first state cell");
        let second = assistant_cell("second state cell");
        let mut overlay =
            TranscriptOverlayState::new(vec![history.clone(), first.clone()], Vec::new(), 1);
        let state = DashboardState {
            activity_cells: vec![first, second.clone()],
            ..DashboardState::default()
        };

        overlay.sync_state(&state);

        assert!(overlay.follow_bottom);
        assert_eq!(
            overlay.cells,
            vec![history, state.activity_cells[0].clone(), second]
        );
    }

    #[test]
    fn transcript_overlay_syncs_live_activity_cells() {
        let first_live = LiveActivityCell {
            key: "first".to_string(),
            cell: assistant_cell("first live cell"),
        };
        let second_live = LiveActivityCell {
            key: "second".to_string(),
            cell: assistant_cell("second live cell"),
        };
        let mut overlay = TranscriptOverlayState::new(Vec::new(), vec![first_live], 0);
        let state = DashboardState {
            live_activity_cells: vec![second_live.clone()],
            ..DashboardState::default()
        };

        overlay.sync_state(&state);

        assert_eq!(overlay.live_cells, vec![second_live]);
    }

    #[test]
    fn transcript_overlay_manual_scroll_leaves_bottom_follow() {
        let cells = (0..30)
            .map(|index| assistant_cell(&format!("cell {index}")))
            .collect::<Vec<_>>();
        let mut overlay = TranscriptOverlayState::new(cells, Vec::new(), 30);
        overlay.set_render_metrics(100, 20);

        assert!(overlay.follow_bottom);
        assert!(overlay.handle_key(KeyEvent::new(
            KeyCode::Up,
            crossterm::event::KeyModifiers::NONE
        )));

        assert!(!overlay.follow_bottom);

        assert!(overlay.handle_key(KeyEvent::new(
            KeyCode::End,
            crossterm::event::KeyModifiers::NONE
        )));

        assert!(overlay.follow_bottom);
    }
}
