use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent};

use super::command_panels::{CommandFeedback, CommandPanel};
use super::{
    ActivityCell, CachedActivityLines, DashboardActivityHistoryPage, DashboardCommandAttachment,
    DashboardState, LiveActivityCell, activity_cells_from_history_items,
    cells::append_runtime_status_live_cell,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CtrlCReminder {
    Interrupt,
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
    command_history: Vec<String>,
    command_history_cursor: Option<usize>,
    command_history_recalled_text: Option<String>,
    pub(super) scroll_offset: u16,
    pub(super) auto_scroll: bool,
    pub(super) max_scroll: u16,
    pub(super) page_height: u16,
    pub(super) last_cursor_pos: Option<(u16, u16)>,
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
            command_history: Vec::new(),
            command_history_cursor: None,
            command_history_recalled_text: None,
            scroll_offset: 0,
            auto_scroll: true,
            max_scroll: 0,
            page_height: 20,
            last_cursor_pos: None,
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
    }

    pub(super) fn sync_transcript_overlay(&mut self, state: &DashboardState) {
        if let Some(overlay) = self.transcript_overlay.as_mut() {
            overlay.sync_state(state);
        }
    }

    pub(super) fn handle_transcript_overlay_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
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

    pub(super) fn record_command_history(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.reset_command_history_navigation();
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

    pub(super) fn expanded_thinking_count(&self) -> usize {
        self.expanded_thinking.len()
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
                self.handle_activity_scroll_rows(-page_height);
                true
            }
            KeyCode::PageDown => {
                let page_height = self.page_height.min(i16::MAX as u16) as i16;
                self.handle_activity_scroll_rows(page_height);
                true
            }
            KeyCode::Home => {
                self.auto_scroll = false;
                self.scroll_offset = 0;
                true
            }
            KeyCode::End => {
                self.auto_scroll = true;
                self.scroll_offset = 0;
                true
            }
            _ => false,
        }
    }

    pub(super) fn handle_activity_scroll_rows(&mut self, rows: i16) -> bool {
        match rows.cmp(&0) {
            std::cmp::Ordering::Less => {
                let rows = rows.unsigned_abs();
                if self.auto_scroll {
                    self.auto_scroll = false;
                    self.scroll_offset = self.max_scroll.saturating_sub(rows);
                } else {
                    self.scroll_offset = self.scroll_offset.saturating_sub(rows);
                }
                true
            }
            std::cmp::Ordering::Greater => {
                let rows = rows as u16;
                self.scroll_offset = self.scroll_offset.saturating_add(rows);
                if self.scroll_offset >= self.max_scroll {
                    self.auto_scroll = true;
                }
                true
            }
            std::cmp::Ordering::Equal => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::{DashboardRuntimeActivity, assistant_activity_cell};

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
    fn scroll_rows_moves_up_from_auto_scroll_without_key_event() {
        let mut view = TuiViewState::new();
        view.max_scroll = 100;
        view.auto_scroll = true;

        assert!(view.handle_activity_scroll_rows(-3));

        assert!(!view.auto_scroll);
        assert_eq!(view.scroll_offset, 97);
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
