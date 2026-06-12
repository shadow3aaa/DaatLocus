use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent};

use super::command_panels::{CommandFeedback, CommandPanel};
use super::{
    ActivityCell, CachedActivityLines, DashboardActivityHistoryPage, DashboardState,
    LiveActivityCell, activity_cells_from_history_items,
};

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

pub(super) struct TuiViewState {
    pub(super) command_input: InputState,
    pub(super) pending_pastes: Vec<(String, String)>,
    pub(super) command_popup_selection: usize,
    pub(super) command_popup_scroll: usize,
    pub(super) command_panel: Option<CommandPanel>,
    pub(super) command_feedback: Option<CommandFeedback>,
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
            command_popup_selection: 0,
            command_popup_scroll: 0,
            command_panel: None,
            command_feedback: None,
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
        let live_cells = if self.visible_activity_cleared {
            Vec::new()
        } else {
            state.live_activity_cells.clone()
        };
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
        self.expanded_thinking.clear();
        self.auto_scroll = true;
        self.scroll_offset = 0;
        self.visible_activity_cleared = true;
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
            KeyCode::Up => {
                if self.auto_scroll {
                    self.auto_scroll = false;
                    self.scroll_offset = self.max_scroll.saturating_sub(1);
                } else {
                    self.scroll_offset = self.scroll_offset.saturating_sub(1);
                }
                true
            }
            KeyCode::Down => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
                if self.scroll_offset >= self.max_scroll {
                    self.auto_scroll = true;
                }
                true
            }
            KeyCode::PageUp => {
                if self.auto_scroll {
                    self.auto_scroll = false;
                    self.scroll_offset = self.max_scroll.saturating_sub(self.page_height);
                } else {
                    self.scroll_offset = self.scroll_offset.saturating_sub(self.page_height);
                }
                true
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_add(self.page_height);
                if self.scroll_offset >= self.max_scroll {
                    self.auto_scroll = true;
                }
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
}
