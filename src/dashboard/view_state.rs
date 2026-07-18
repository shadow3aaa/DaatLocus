use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent};
use unicode_width::UnicodeWidthChar;

use super::command_panels::{CommandFeedback, CommandPanel};
use super::selection::{SelectableRegion, SelectionRegistry};
use super::tui_event::TuiMouseSelectionKind;
use super::{
    CachedActivityLines, DashboardActivityHistoryItem, DashboardActivityHistoryPage,
    DashboardCommandAttachment, DashboardState, LiveActivityEvent, SessionActivityEvent,
    WorkflowWorkerActivityPage, activity_events_from_history_items,
    cells::sync_runtime_status_live_cell,
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
    pub(super) const fn new() -> Self {
        Self {
            text: String::new(),
            cursor_pos: 0,
        }
    }

    pub(super) const fn is_empty(&self) -> bool {
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
            .map_or(0, |index| index + 1);
        let line_end = self.text[cursor_pos..]
            .find('\n')
            .map_or(self.text.len(), |index| cursor_pos + index);
        (line_start, line_end)
    }

    fn previous_line_bounds(&self, line_start: usize) -> Option<(usize, usize)> {
        if line_start == 0 {
            return None;
        }
        let previous_line_end = line_start - 1;
        let previous_line_start = self.text[..previous_line_end]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        Some((previous_line_start, previous_line_end))
    }

    fn next_line_bounds(&self, line_end: usize) -> Option<(usize, usize)> {
        if line_end >= self.text.len() {
            return None;
        }
        let next_line_start = line_end + 1;
        let next_line_end = self.text[next_line_start..]
            .find('\n')
            .map_or(self.text.len(), |index| next_line_start + index);
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

    pub(super) const fn move_home(&mut self) {
        self.cursor_pos = 0;
    }

    pub(super) const fn move_end(&mut self) {
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

pub(super) struct ActivityScrollState {
    pub(super) scroll: u16,
    pub(super) follow_bottom: bool,
    pub(super) max_scroll: u16,
    pub(super) page_height: u16,
}

impl ActivityScrollState {
    pub(super) const fn new(page_height: u16) -> Self {
        Self {
            scroll: 0,
            follow_bottom: true,
            max_scroll: 0,
            page_height,
        }
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

    pub(super) fn display_scroll(&self) -> u16 {
        if self.follow_bottom {
            u16::MAX
        } else {
            self.scroll.min(self.max_scroll)
        }
    }

    pub(super) fn handle_scroll_rows(&mut self, rows: i16) -> bool {
        if rows == 0 || self.max_scroll == 0 {
            return false;
        }
        match rows.cmp(&0) {
            std::cmp::Ordering::Less => {
                let rows = rows.unsigned_abs();
                if self.follow_bottom {
                    self.follow_bottom = false;
                    self.scroll = self.max_scroll;
                }
                let previous_scroll = self.scroll;
                self.scroll = self.scroll.saturating_sub(rows);
                self.scroll != previous_scroll || !self.follow_bottom
            }
            std::cmp::Ordering::Greater => {
                if self.follow_bottom {
                    return false;
                }
                let previous_scroll = self.scroll;
                self.scroll = self.scroll.saturating_add(rows.cast_unsigned());
                if self.scroll >= self.max_scroll {
                    self.reset_to_bottom();
                    return true;
                }
                self.scroll != previous_scroll
            }
            std::cmp::Ordering::Equal => false,
        }
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.handle_scroll_rows(-1),
            KeyCode::Down | KeyCode::Char('j') => self.handle_scroll_rows(1),
            KeyCode::PageUp => {
                let page_height = self.page_height.min(i16::MAX as u16).cast_signed();
                self.handle_scroll_rows(-page_height)
            }
            KeyCode::PageDown => {
                let page_height = self.page_height.min(i16::MAX as u16).cast_signed();
                self.handle_scroll_rows(page_height)
            }
            KeyCode::Home => {
                if self.max_scroll == 0 || (!self.follow_bottom && self.scroll == 0) {
                    return false;
                }
                self.follow_bottom = false;
                self.scroll = 0;
                true
            }
            KeyCode::End => {
                if self.follow_bottom {
                    return false;
                }
                self.reset_to_bottom();
                true
            }
            _ => false,
        }
    }

    pub(super) const fn reset_to_bottom(&mut self) {
        self.follow_bottom = true;
        self.scroll = 0;
    }

    fn clamp_scroll(&mut self) {
        if self.follow_bottom {
            self.scroll = 0;
        } else {
            self.scroll = self.scroll.min(self.max_scroll);
        }
    }
}

pub(super) struct TranscriptOverlayState {
    pub(super) cells: Vec<SessionActivityEvent>,
    pub(super) live_cells: Vec<LiveActivityEvent>,
    pub(super) history_prefix_len: usize,
    pub(super) activity_scroll: ActivityScrollState,
}

impl TranscriptOverlayState {
    pub(super) const fn new(
        cells: Vec<SessionActivityEvent>,
        live_cells: Vec<LiveActivityEvent>,
        state_activity_len: usize,
    ) -> Self {
        Self {
            history_prefix_len: cells.len().saturating_sub(state_activity_len),
            cells,
            live_cells,
            activity_scroll: ActivityScrollState::new(20),
        }
    }

    pub(super) fn sync_state(&mut self, state: &DashboardState) {
        let mut next_cells = self
            .cells
            .iter()
            .take(self.history_prefix_len)
            .cloned()
            .collect::<Vec<_>>();
        next_cells.extend(state.activity_events.clone());
        self.cells = next_cells;
        let mut live_cells = state.live_activity_events.clone();
        sync_runtime_status_live_cell(&mut live_cells, state);
        self.live_cells = live_cells;
    }

    pub(super) fn set_render_metrics(&mut self, max_scroll: u16, page_height: u16) {
        self.activity_scroll
            .set_render_metrics(max_scroll, page_height);
    }

    pub(super) fn effective_scroll(&self) -> u16 {
        self.activity_scroll.effective_scroll()
    }

    pub(super) fn handle_scroll_rows(&mut self, rows: i16) -> bool {
        self.activity_scroll.handle_scroll_rows(rows)
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        self.activity_scroll.handle_key(key)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkflowInspectorPage {
    Outline,
    Activity,
}

#[derive(Clone, Debug)]
pub(super) struct WorkflowInspectorAgent {
    pub(super) role: String,
    pub(super) model: String,
    pub(super) status: crate::workflow::WorkflowNodeStatus,
    pub(super) agent_run_time_ms: u64,
    pub(super) attempt_count: usize,
    pub(super) latest_worker_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WorkflowInspectorRoleTransition {
    pub(super) source_role: String,
    pub(super) target_role: String,
    pub(super) kind: crate::workflow::WorkflowTransitionKind,
    pub(super) count: usize,
}

pub(super) struct WorkflowInspectorState {
    pub(super) snapshot: crate::workflow::WorkflowRunSnapshot,
    pub(super) selected_worker: usize,
    pub(super) page: WorkflowInspectorPage,
    pub(super) activity_scroll: ActivityScrollState,
    pub(super) activity_cache: CachedActivityLines,
    pub(super) expanded_thinking: HashSet<usize>,
    pub(super) worker_activity: Vec<SessionActivityEvent>,
    pub(super) worker_activity_oldest_cursor: Option<i64>,
    pub(super) worker_activity_has_more_before: bool,
    pub(super) worker_activity_loading: bool,
    pub(super) worker_activity_loaded_revision: Option<i64>,
    pub(super) worker_activity_error: Option<String>,
    worker_activity_load_rx:
        Option<tokio::sync::oneshot::Receiver<Result<WorkflowWorkerActivityPage, String>>>,
}

impl WorkflowInspectorState {
    pub(super) fn agents(&self) -> Vec<WorkflowInspectorAgent> {
        let mut agents = Vec::<WorkflowInspectorAgent>::new();
        for (worker_index, worker) in self.snapshot.workers.iter().enumerate() {
            let role = workflow_worker_role(worker);
            if let Some(agent) = agents.iter_mut().find(|agent| agent.role == role) {
                agent.attempt_count += 1;
                agent.agent_run_time_ms = agent
                    .agent_run_time_ms
                    .saturating_add(worker.agent_run_time_ms);
                if workflow_status_priority(worker.status) > workflow_status_priority(agent.status)
                {
                    agent.status = worker.status;
                }
                if worker.started_at_ms
                    >= self.snapshot.workers[agent.latest_worker_index].started_at_ms
                {
                    agent.model = worker.model.clone();
                    agent.latest_worker_index = worker_index;
                }
            } else {
                agents.push(WorkflowInspectorAgent {
                    role,
                    model: worker.model.clone(),
                    status: worker.status,
                    agent_run_time_ms: worker.agent_run_time_ms,
                    attempt_count: 1,
                    latest_worker_index: worker_index,
                });
            }
        }
        agents
    }

    pub(super) fn role_transitions(&self) -> Vec<WorkflowInspectorRoleTransition> {
        let mut transitions = Vec::<WorkflowInspectorRoleTransition>::new();
        for transition in self.transitions() {
            let Some(source_worker) = self
                .snapshot
                .workers
                .iter()
                .find(|worker| worker.worker_id == transition.source_worker_id)
            else {
                continue;
            };
            let Some(target_worker) = self
                .snapshot
                .workers
                .iter()
                .find(|worker| worker.worker_id == transition.target_worker_id)
            else {
                continue;
            };
            let source_role = workflow_worker_role(source_worker);
            let target_role = workflow_worker_role(target_worker);
            if let Some(existing) = transitions.iter_mut().find(|existing| {
                existing.source_role == source_role
                    && existing.target_role == target_role
                    && existing.kind == transition.kind
            }) {
                existing.count += 1;
            } else {
                transitions.push(WorkflowInspectorRoleTransition {
                    source_role,
                    target_role,
                    kind: transition.kind,
                    count: 1,
                });
            }
        }
        transitions
    }

    pub(super) fn total_agent_run_time_ms(&self) -> u64 {
        self.snapshot
            .workers
            .iter()
            .map(|worker| worker.agent_run_time_ms)
            .sum()
    }

    fn transitions(&self) -> Vec<crate::workflow::WorkflowTransitionSnapshot> {
        let direct_transitions = self
            .snapshot
            .transitions
            .iter()
            .filter(|transition| {
                transition.source_worker_id != transition.target_worker_id
                    && self
                        .snapshot
                        .workers
                        .iter()
                        .any(|worker| worker.worker_id == transition.source_worker_id)
                    && self
                        .snapshot
                        .workers
                        .iter()
                        .any(|worker| worker.worker_id == transition.target_worker_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        if !direct_transitions.is_empty() {
            return direct_transitions;
        }

        let mut groups = self.snapshot.await_groups.iter().collect::<Vec<_>>();
        groups.sort_by_key(|group| group.sequence);
        let mut transitions = Vec::new();
        let mut previous_worker_ids = None::<Vec<String>>;
        for group in groups {
            let worker_ids = group
                .worker_ids
                .iter()
                .filter(|worker_id| {
                    self.snapshot
                        .workers
                        .iter()
                        .any(|worker| worker.worker_id == **worker_id)
                })
                .cloned()
                .collect::<Vec<_>>();
            if let Some(previous_worker_ids) = &previous_worker_ids {
                for source_worker_id in previous_worker_ids {
                    for target_worker_id in &worker_ids {
                        if source_worker_id != target_worker_id {
                            transitions.push(crate::workflow::WorkflowTransitionSnapshot {
                                source_worker_id: source_worker_id.clone(),
                                target_worker_id: target_worker_id.clone(),
                                kind: crate::workflow::WorkflowTransitionKind::Await,
                            });
                        }
                    }
                }
            }
            if !worker_ids.is_empty() {
                previous_worker_ids = Some(worker_ids);
            }
        }
        transitions
    }

    fn new(snapshot: crate::workflow::WorkflowRunSnapshot) -> Self {
        let selected_worker = snapshot.workers.len().saturating_sub(1);
        Self {
            snapshot,
            selected_worker,
            page: WorkflowInspectorPage::Outline,
            activity_scroll: ActivityScrollState::new(20),
            activity_cache: CachedActivityLines::new(),
            expanded_thinking: HashSet::new(),
            worker_activity: Vec::new(),
            worker_activity_oldest_cursor: None,
            worker_activity_has_more_before: false,
            worker_activity_loading: false,
            worker_activity_loaded_revision: None,
            worker_activity_error: None,
            worker_activity_load_rx: None,
        }
    }

    fn sync_snapshot(&mut self, snapshot: &crate::workflow::WorkflowRunSnapshot) {
        let previous_worker_id = self
            .selected_worker()
            .map(|worker| worker.worker_id.clone());
        let previous_revision = self
            .selected_worker()
            .map(|worker| worker.activity_revision);
        self.snapshot = snapshot.clone();
        self.selected_worker = self
            .selected_worker
            .min(self.snapshot.workers.len().saturating_sub(1));
        let selected_worker_id = self
            .selected_worker()
            .map(|worker| worker.worker_id.clone());
        let selected_revision = self
            .selected_worker()
            .map(|worker| worker.activity_revision);
        if previous_worker_id != selected_worker_id {
            self.reset_worker_activity();
        } else if previous_revision != selected_revision
            && self.worker_activity_loaded_revision.is_some()
        {
            self.worker_activity_loaded_revision = None;
            self.worker_activity_oldest_cursor = None;
            self.worker_activity_has_more_before = false;
            self.worker_activity_error = None;
        }
        self.activity_cache = CachedActivityLines::new();
    }

    pub(super) fn selected_worker(&self) -> Option<&crate::workflow::WorkflowWorkerSnapshot> {
        self.snapshot.workers.get(self.selected_worker)
    }

    pub(super) fn displayed_worker_activity(&self) -> &[SessionActivityEvent] {
        if self.worker_activity_loaded_revision.is_some() {
            &self.worker_activity
        } else {
            self.selected_worker()
                .map_or(&[], |worker| worker.activity.as_slice())
        }
    }

    pub(super) fn should_start_worker_activity_load(
        &self,
        has_history_loader: bool,
        activity_pane_visible: bool,
    ) -> bool {
        let Some(worker) = self.selected_worker() else {
            return false;
        };
        has_history_loader
            && (activity_pane_visible || self.page == WorkflowInspectorPage::Activity)
            && !self.worker_activity_loading
            && self.worker_activity_error.is_none()
            && self.worker_activity_loaded_revision != Some(worker.activity_revision)
    }

    pub(super) fn should_start_older_worker_activity_load(&self, has_history_loader: bool) -> bool {
        has_history_loader
            && self.page == WorkflowInspectorPage::Activity
            && !self.worker_activity_loading
            && self.worker_activity_error.is_none()
            && self.worker_activity_loaded_revision.is_some()
            && self.worker_activity_has_more_before
            && self.activity_scroll.effective_scroll() <= 3
    }

    pub(super) fn worker_activity_request(&self) -> Option<(String, String, Option<i64>)> {
        let worker = self.selected_worker()?;
        Some((
            self.snapshot.run_id.clone(),
            worker.worker_id.clone(),
            self.worker_activity_oldest_cursor,
        ))
    }

    pub(super) fn begin_worker_activity_load(
        &mut self,
        rx: tokio::sync::oneshot::Receiver<Result<WorkflowWorkerActivityPage, String>>,
    ) {
        self.worker_activity_loading = true;
        self.worker_activity_error = None;
        self.worker_activity_load_rx = Some(rx);
    }

    pub(super) const fn take_worker_activity_load_rx(
        &mut self,
    ) -> Option<tokio::sync::oneshot::Receiver<Result<WorkflowWorkerActivityPage, String>>> {
        self.worker_activity_load_rx.take()
    }

    pub(super) fn keep_worker_activity_load_rx(
        &mut self,
        rx: tokio::sync::oneshot::Receiver<Result<WorkflowWorkerActivityPage, String>>,
    ) {
        self.worker_activity_load_rx = Some(rx);
    }

    pub(super) fn apply_worker_activity_page(&mut self, page: &WorkflowWorkerActivityPage) {
        let Some(worker) = self.selected_worker() else {
            self.finish_worker_activity_load_without_page(None);
            return;
        };
        if page.run_id != self.snapshot.run_id || page.worker_id != worker.worker_id {
            self.finish_worker_activity_load_without_page(Some(
                "workflow worker activity response did not match the selected worker".to_string(),
            ));
            return;
        }

        let loading_older = self.worker_activity_loaded_revision.is_some()
            && self.worker_activity_oldest_cursor.is_some();
        let page_events = page
            .items
            .iter()
            .map(|item| item.event.clone())
            .collect::<Vec<_>>();
        if loading_older {
            let mut merged = page_events;
            merged.extend(std::mem::take(&mut self.worker_activity));
            self.worker_activity = merged;
            self.activity_scroll.follow_bottom = false;
            self.activity_scroll.scroll = 0;
        } else {
            self.worker_activity = page_events;
            self.activity_scroll.reset_to_bottom();
        }
        self.worker_activity_oldest_cursor = page.oldest_cursor;
        self.worker_activity_has_more_before = page.has_more_before;
        self.worker_activity_loaded_revision = Some(page.revision);
        self.worker_activity_loading = false;
        self.worker_activity_error = None;
        self.activity_cache = CachedActivityLines::new();
    }

    pub(super) fn finish_worker_activity_load_without_page(&mut self, error: Option<String>) {
        self.worker_activity_loading = false;
        self.worker_activity_error = error;
        self.worker_activity_load_rx = None;
    }

    fn reset_worker_activity(&mut self) {
        self.worker_activity.clear();
        self.worker_activity_oldest_cursor = None;
        self.worker_activity_has_more_before = false;
        self.worker_activity_loading = false;
        self.worker_activity_loaded_revision = None;
        self.worker_activity_error = None;
        self.worker_activity_load_rx = None;
        self.activity_scroll.reset_to_bottom();
        self.activity_cache = CachedActivityLines::new();
        self.expanded_thinking.clear();
    }

    fn select_worker(&mut self, worker_index: usize) {
        if worker_index != self.selected_worker {
            self.selected_worker = worker_index;
            self.reset_worker_activity();
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if self.page == WorkflowInspectorPage::Outline => {
                self.select_worker(self.selected_worker.saturating_sub(1));
                true
            }
            KeyCode::Down | KeyCode::Char('j') if self.page == WorkflowInspectorPage::Outline => {
                self.select_worker(
                    (self.selected_worker + 1).min(self.snapshot.workers.len().saturating_sub(1)),
                );
                true
            }
            KeyCode::Enter | KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.page = WorkflowInspectorPage::Activity;
                true
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.page = WorkflowInspectorPage::Outline;
                true
            }
            _ if self.page == WorkflowInspectorPage::Activity => {
                self.activity_scroll.handle_key(key)
            }
            _ => false,
        }
    }
}

fn workflow_worker_role(worker: &crate::workflow::WorkflowWorkerSnapshot) -> String {
    if worker.role.trim().is_empty() {
        "agent".to_string()
    } else {
        worker.role.clone()
    }
}

const fn workflow_status_priority(status: crate::workflow::WorkflowNodeStatus) -> u8 {
    match status {
        crate::workflow::WorkflowNodeStatus::Running => 4,
        crate::workflow::WorkflowNodeStatus::Failed => 3,
        crate::workflow::WorkflowNodeStatus::Interrupted => 2,
        crate::workflow::WorkflowNodeStatus::Pending => 1,
        crate::workflow::WorkflowNodeStatus::Completed => 0,
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
    pub(super) workflow_inspector: Option<WorkflowInspectorState>,
    pub(super) command_feedback: Option<CommandFeedback>,
    pub(super) ctrl_c_reminder: Option<CtrlCReminder>,
    pub(super) editing_pending_user_input: Option<PendingUserInputEditState>,
    command_history: Vec<String>,
    command_history_cursor: Option<usize>,
    command_history_recalled_text: Option<String>,
    pub(super) activity_scroll: ActivityScrollState,
    pub(super) last_cursor_pos: Option<(u16, u16)>,
    pub(super) previous_hyperlink_overlays: Vec<TerminalHyperlinkOverlay>,
    pub(super) selection: SelectionRegistry,
    pub(super) extra_history_cells: Vec<SessionActivityEvent>,
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
            workflow_inspector: None,
            command_feedback: None,
            ctrl_c_reminder: None,
            editing_pending_user_input: None,
            command_history: Vec::new(),
            command_history_cursor: None,
            command_history_recalled_text: None,
            activity_scroll: ActivityScrollState::new(20),
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

    pub(super) const fn reset_command_popup(&mut self) {
        self.command_popup_selection = 0;
        self.command_popup_scroll = 0;
    }

    pub(super) fn open_transcript_overlay(&mut self, state: &DashboardState) {
        let (cells, live_cells) = self.visible_activity_cells(state);
        self.transcript_overlay = Some(TranscriptOverlayState::new(
            cells,
            live_cells,
            state.activity_events.len(),
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
    pub(super) fn open_latest_workflow_inspector(&mut self, state: &DashboardState) -> bool {
        let snapshot = state.active_workflow_runs.last().cloned().or_else(|| {
            state
                .activity_events
                .iter()
                .rev()
                .find_map(|event| match event {
                    SessionActivityEvent::Workflow(workflow) => workflow.snapshot.clone(),
                    _ => None,
                })
        });
        let Some(snapshot) = snapshot else {
            return false;
        };
        self.workflow_inspector = Some(WorkflowInspectorState::new(snapshot));
        self.transcript_overlay = None;
        self.command_panel = None;
        self.command_feedback = None;
        self.reset_command_popup();
        self.selection.clear_selection();
        true
    }

    pub(super) fn sync_workflow_inspector(&mut self, state: &DashboardState) {
        let Some(inspector) = self.workflow_inspector.as_mut() else {
            return;
        };
        if let Some(snapshot) = state
            .active_workflow_runs
            .iter()
            .find(|run| run.run_id == inspector.snapshot.run_id)
            .or_else(|| {
                state
                    .activity_events
                    .iter()
                    .rev()
                    .find_map(|event| match event {
                        SessionActivityEvent::Workflow(workflow) => workflow
                            .snapshot
                            .as_ref()
                            .filter(|snapshot| snapshot.run_id == inspector.snapshot.run_id),
                        _ => None,
                    })
            })
        {
            inspector.sync_snapshot(snapshot);
        }
    }

    pub(super) fn handle_workflow_inspector_key(&mut self, key: KeyEvent) -> bool {
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            if self
                .workflow_inspector
                .as_ref()
                .is_some_and(|inspector| inspector.page == WorkflowInspectorPage::Activity)
            {
                if let Some(inspector) = self.workflow_inspector.as_mut() {
                    inspector.page = WorkflowInspectorPage::Outline;
                    inspector.activity_scroll.reset_to_bottom();
                }
            } else {
                self.workflow_inspector = None;
            }
            return true;
        }
        self.workflow_inspector
            .as_mut()
            .is_some_and(|inspector| inspector.handle_key(key))
    }

    pub(super) fn handle_workflow_inspector_scroll_rows(&mut self, rows: i16) -> bool {
        self.workflow_inspector.as_mut().is_some_and(|inspector| {
            inspector.page == WorkflowInspectorPage::Activity
                && inspector.activity_scroll.handle_scroll_rows(rows)
        })
    }

    pub(super) fn handle_transcript_overlay_scroll_rows(&mut self, rows: i16) -> bool {
        self.transcript_overlay
            .as_mut()
            .is_some_and(|overlay| overlay.handle_scroll_rows(rows))
    }

    pub(super) const fn clear_ctrl_c_reminder(&mut self) {
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
        let Some(next_index) = self.command_history_cursor.map_or_else(
            || total_entries.checked_sub(1),
            |index| index.checked_sub(1),
        ) else {
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
        self.activity_scroll.effective_scroll()
    }

    pub(super) fn display_scroll(&self) -> u16 {
        self.activity_scroll.display_scroll()
    }

    pub(super) fn visible_activity_cells(
        &self,
        state: &DashboardState,
    ) -> (Vec<SessionActivityEvent>, Vec<LiveActivityEvent>) {
        let committed_cells = if self.visible_activity_cleared {
            Vec::new()
        } else {
            let mut cells = self.extra_history_cells.clone();
            cells.extend(state.activity_events.clone());
            cells
        };
        let mut live_cells = if self.visible_activity_cleared {
            Vec::new()
        } else {
            state.live_activity_events.clone()
        };
        sync_runtime_status_live_cell(&mut live_cells, state);
        (committed_cells, live_cells)
    }

    pub(super) const fn tick_history_load_cooldown(&mut self) {
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

    pub(super) const fn oldest_history_cursor(&self) -> Option<i64> {
        self.oldest_cursor
    }

    pub(super) const fn take_history_load_rx(
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

    pub(super) fn apply_loaded_history_page(&mut self, page: &DashboardActivityHistoryPage) {
        let new_cells = activity_events_from_history_items(&page.items);
        let mut merged = new_cells;
        merged.extend(self.extra_history_cells.clone());
        self.extra_history_cells = merged;
        self.activity_scroll.follow_bottom = false;
        self.activity_scroll.scroll = 0;
        self.oldest_cursor = page.oldest_cursor;
        self.has_more_before = page.has_more_before;
        self.loading_history = false;
        self.load_cooldown = 10;
    }

    pub(super) const fn finish_history_load_without_page(&mut self) {
        self.loading_history = false;
    }

    pub(super) const fn sync_history_cursor_from_state(&mut self, state: &DashboardState) {
        if self.oldest_cursor.is_none() && !state.activity_history.items.is_empty() {
            self.oldest_cursor = state.activity_history.oldest_cursor;
            self.has_more_before = state.activity_history.has_more_before;
        }
    }

    pub(super) const fn sync_visible_clear_from_state(&mut self, state: &DashboardState) {
        if self.visible_activity_cleared
            && state.activity_history.items.is_empty()
            && state.activity_events.is_empty()
            && state.live_activity_events.is_empty()
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
        self.activity_scroll.reset_to_bottom();
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

    pub(super) fn toggle_thinking_expansion(
        &mut self,
        activity_events: &[SessionActivityEvent],
    ) -> bool {
        let offset = self.extra_history_cells.len();
        let mut any_thinking = false;
        for (i, cell) in activity_events.iter().enumerate() {
            if matches!(cell, SessionActivityEvent::Thinking(_)) {
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
            KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End => {
                self.activity_scroll.handle_key(key)
            }
            _ => false,
        }
    }

    pub(super) fn handle_activity_scroll_rows(&mut self, rows: i16) -> bool {
        self.activity_scroll.handle_scroll_rows(rows)
    }
}

fn command_history_entries_from_state(state: &DashboardState) -> Vec<String> {
    if state.activity_history.items.is_empty() {
        return state
            .activity_events
            .iter()
            .filter_map(command_history_text_from_activity_cell)
            .collect();
    }
    state
        .activity_history
        .items
        .iter()
        .filter_map(command_history_text_from_history_item)
        .collect()
}

fn command_history_text_from_history_item(item: &DashboardActivityHistoryItem) -> Option<String> {
    let SessionActivityEvent::User(cell) = &item.event else {
        return None;
    };
    let text = cell.content.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn command_history_text_from_activity_cell(item: &SessionActivityEvent) -> Option<String> {
    let SessionActivityEvent::User(cell) = item else {
        return None;
    };
    let text = cell.content.trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::selection::{SelectableId, SelectableRegion};
    use crate::dashboard::{
        DashboardActivityHistoryWindow, DashboardRuntimeActivity, WorkflowWorkerActivityItem,
        assistant_activity_cell, render_activity_from_messages,
        sync_dashboard_runtime_status_live_cell,
    };
    use crate::reasoning::runtime::HistoryMessage;
    use ratatui::layout::Rect;

    fn user_history_item(id: &str, text: &str) -> DashboardActivityHistoryItem {
        let cell = render_activity_from_messages(vec![HistoryMessage::user(text.to_string())])
            .into_iter()
            .next()
            .expect("user activity cell");
        DashboardActivityHistoryItem::from_event_with_id(&cell, id)
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

        assert!(state.live_activity_events.is_empty());
        assert_eq!(live_cells.len(), 1);
        assert_eq!(live_cells[0].key, "runtime-status");
        let SessionActivityEvent::RuntimeStatus(cell) = &live_cells[0].event else {
            panic!("expected runtime status live cell");
        };
        assert_eq!(cell.label, "Working");
        assert_eq!(cell.active_runtime_started_at_ms, Some(1_000));
    }

    #[test]
    fn shared_dashboard_state_syncs_runtime_status_live_cell_once() {
        let view = TuiViewState::new();
        let mut state = DashboardState {
            runtime_activity: DashboardRuntimeActivity::default()
                .with_runtime_turn(Some("model request".to_string()), Some(1_000)),
            ..DashboardState::default()
        };

        sync_dashboard_runtime_status_live_cell(&mut state);
        sync_dashboard_runtime_status_live_cell(&mut state);
        let (_, live_cells) = view.visible_activity_cells(&state);

        assert_eq!(state.live_activity_events.len(), 1);
        assert_eq!(live_cells.len(), 1);
        assert_eq!(live_cells[0].key, "runtime-status");

        state.runtime_activity = DashboardRuntimeActivity::default();
        sync_dashboard_runtime_status_live_cell(&mut state);

        assert!(state.live_activity_events.is_empty());
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
    fn scroll_rows_moves_up_from_follow_bottom_without_key_event() {
        let mut view = TuiViewState::new();
        view.activity_scroll.set_render_metrics(100, 20);

        assert!(view.handle_activity_scroll_rows(-3));

        assert!(!view.activity_scroll.follow_bottom);
        assert_eq!(view.activity_scroll.scroll, 97);
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
    fn scroll_rows_rejoins_follow_bottom_at_end() {
        let mut view = TuiViewState::new();
        view.activity_scroll.set_render_metrics(100, 20);
        view.activity_scroll.follow_bottom = false;
        view.activity_scroll.scroll = 98;

        assert!(view.handle_activity_scroll_rows(3));

        assert!(view.activity_scroll.follow_bottom);
    }

    #[test]
    fn zero_scroll_rows_are_ignored() {
        let mut view = TuiViewState::new();

        assert!(!view.handle_activity_scroll_rows(0));
        assert!(view.activity_scroll.follow_bottom);
        assert_eq!(view.activity_scroll.scroll, 0);
    }

    #[test]
    fn up_down_keys_do_not_scroll_activity_feed() {
        let mut view = TuiViewState::new();
        view.activity_scroll.set_render_metrics(100, 20);

        assert!(!view.handle_activity_scroll_key(KeyEvent::new(
            KeyCode::Up,
            crossterm::event::KeyModifiers::NONE
        )));
        assert!(!view.handle_activity_scroll_key(KeyEvent::new(
            KeyCode::Down,
            crossterm::event::KeyModifiers::NONE
        )));

        assert!(view.activity_scroll.follow_bottom);
        assert_eq!(view.activity_scroll.scroll, 0);
    }

    #[test]
    fn page_keys_still_scroll_activity_feed() {
        let mut view = TuiViewState::new();
        view.activity_scroll.set_render_metrics(100, 20);

        assert!(view.handle_activity_scroll_key(KeyEvent::new(
            KeyCode::PageUp,
            crossterm::event::KeyModifiers::NONE
        )));

        assert!(!view.activity_scroll.follow_bottom);
        assert_eq!(view.activity_scroll.scroll, 80);
    }

    fn assistant_cell(text: &str) -> SessionActivityEvent {
        assistant_activity_cell(text).expect("assistant cell")
    }

    #[test]
    fn activity_scroll_state_preserves_manual_position_and_rejoins_bottom() {
        let mut scroll = ActivityScrollState::new(20);
        scroll.set_render_metrics(100, 20);

        assert!(scroll.follow_bottom);
        assert!(scroll.handle_scroll_rows(-3));
        assert!(!scroll.follow_bottom);
        assert_eq!(scroll.scroll, 97);

        scroll.set_render_metrics(120, 20);
        assert_eq!(scroll.scroll, 97);
        assert_eq!(scroll.display_scroll(), 97);

        assert!(scroll.handle_key(KeyEvent::new(
            KeyCode::End,
            crossterm::event::KeyModifiers::NONE
        )));
        assert!(scroll.follow_bottom);
        assert_eq!(scroll.display_scroll(), u16::MAX);
    }

    #[test]
    fn activity_scroll_state_page_keys_follow_the_shared_policy() {
        let mut scroll = ActivityScrollState::new(20);
        scroll.set_render_metrics(100, 20);

        assert!(scroll.handle_key(KeyEvent::new(
            KeyCode::PageUp,
            crossterm::event::KeyModifiers::NONE
        )));
        assert!(!scroll.follow_bottom);
        assert_eq!(scroll.scroll, 80);
    }

    #[test]
    fn workflow_inspector_groups_role_attempts_and_transitions() {
        let snapshot = crate::workflow::WorkflowRunSnapshot {
            run_id: "run-roles".to_string(),
            workflow_id: "workflow".to_string(),
            status: crate::workflow::WorkflowNodeStatus::Running,
            started_at_ms: 1,
            completed_at_ms: None,
            input: serde_json::json!({}),
            output: None,
            error: None,
            await_groups: Vec::new(),
            transitions: vec![
                crate::workflow::WorkflowTransitionSnapshot {
                    source_worker_id: "worker-1".to_string(),
                    target_worker_id: "worker-3".to_string(),
                    kind: crate::workflow::WorkflowTransitionKind::Await,
                },
                crate::workflow::WorkflowTransitionSnapshot {
                    source_worker_id: "worker-2".to_string(),
                    target_worker_id: "worker-3".to_string(),
                    kind: crate::workflow::WorkflowTransitionKind::Await,
                },
            ],
            workers: vec![
                crate::workflow::WorkflowWorkerSnapshot {
                    worker_id: "worker-1".to_string(),
                    await_group_id: "await-1".to_string(),
                    role: "researcher".to_string(),
                    model: "main".to_string(),
                    status: crate::workflow::WorkflowNodeStatus::Completed,
                    started_at_ms: 1,
                    completed_at_ms: Some(2),
                    agent_run_time_ms: 12,
                    input: serde_json::json!({}),
                    output: None,
                    error: None,
                    activity_count: 0,
                    activity_revision: 0,
                    activity: Vec::new(),
                },
                crate::workflow::WorkflowWorkerSnapshot {
                    worker_id: "worker-2".to_string(),
                    await_group_id: "await-1".to_string(),
                    role: "researcher".to_string(),
                    model: "efficient".to_string(),
                    status: crate::workflow::WorkflowNodeStatus::Failed,
                    started_at_ms: 3,
                    completed_at_ms: Some(4),
                    agent_run_time_ms: 18,
                    input: serde_json::json!({}),
                    output: None,
                    error: Some("retry".to_string()),
                    activity_count: 0,
                    activity_revision: 0,
                    activity: Vec::new(),
                },
                crate::workflow::WorkflowWorkerSnapshot {
                    worker_id: "worker-3".to_string(),
                    await_group_id: "await-2".to_string(),
                    role: "reviewer".to_string(),
                    model: "main".to_string(),
                    status: crate::workflow::WorkflowNodeStatus::Running,
                    started_at_ms: 5,
                    completed_at_ms: None,
                    agent_run_time_ms: 24,
                    input: serde_json::json!({}),
                    output: None,
                    error: None,
                    activity_count: 0,
                    activity_revision: 0,
                    activity: Vec::new(),
                },
            ],
        };

        let inspector = WorkflowInspectorState::new(snapshot);
        let agents = inspector.agents();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].role, "researcher");
        assert_eq!(agents[0].model, "efficient");
        assert_eq!(
            agents[0].status,
            crate::workflow::WorkflowNodeStatus::Failed
        );
        assert_eq!(agents[0].attempt_count, 2);
        assert_eq!(agents[0].agent_run_time_ms, 30);
        assert_eq!(inspector.total_agent_run_time_ms(), 54);
        assert_eq!(
            inspector.role_transitions(),
            vec![WorkflowInspectorRoleTransition {
                source_role: "researcher".to_string(),
                target_role: "reviewer".to_string(),
                kind: crate::workflow::WorkflowTransitionKind::Await,
                count: 2,
            }]
        );
    }

    #[test]
    fn workflow_inspector_wide_pane_starts_activity_lazy_load_before_activity_page() {
        let snapshot = crate::workflow::WorkflowRunSnapshot {
            run_id: "run-wide".to_string(),
            workflow_id: "workflow".to_string(),
            status: crate::workflow::WorkflowNodeStatus::Running,
            started_at_ms: 1,
            completed_at_ms: None,
            input: serde_json::json!({}),
            output: None,
            error: None,
            await_groups: Vec::new(),
            transitions: Vec::new(),
            workers: vec![crate::workflow::WorkflowWorkerSnapshot {
                worker_id: "worker-1".to_string(),
                await_group_id: "await-1".to_string(),
                role: "researcher".to_string(),
                model: "main".to_string(),
                status: crate::workflow::WorkflowNodeStatus::Running,
                started_at_ms: 1,
                completed_at_ms: None,
                agent_run_time_ms: 0,
                input: serde_json::json!({}),
                output: None,
                error: None,
                activity_count: 2,
                activity_revision: 2,
                activity: Vec::new(),
            }],
        };
        let inspector = WorkflowInspectorState::new(snapshot);
        assert!(inspector.should_start_worker_activity_load(true, true));
        assert!(!inspector.should_start_worker_activity_load(true, false));
    }

    #[test]
    fn workflow_inspector_worker_activity_pages_prepend_without_duplicates() {
        let snapshot = crate::workflow::WorkflowRunSnapshot {
            run_id: "run-1".to_string(),
            workflow_id: "workflow".to_string(),
            status: crate::workflow::WorkflowNodeStatus::Running,
            started_at_ms: 1,
            completed_at_ms: None,
            input: serde_json::json!({}),
            output: None,
            error: None,
            await_groups: Vec::new(),
            transitions: Vec::new(),
            workers: vec![crate::workflow::WorkflowWorkerSnapshot {
                worker_id: "worker-1".to_string(),
                await_group_id: "await-1".to_string(),
                role: "researcher".to_string(),
                model: "main".to_string(),
                status: crate::workflow::WorkflowNodeStatus::Running,
                started_at_ms: 1,
                completed_at_ms: None,
                agent_run_time_ms: 0,
                input: serde_json::json!({}),
                output: None,
                error: None,
                activity_count: 3,
                activity_revision: 3,
                activity: Vec::new(),
            }],
        };
        let event = |text: &str| assistant_activity_cell(text).expect("assistant event");
        let mut inspector = WorkflowInspectorState::new(snapshot);
        inspector.page = WorkflowInspectorPage::Activity;

        inspector.apply_worker_activity_page(&WorkflowWorkerActivityPage {
            run_id: "run-1".to_string(),
            worker_id: "worker-1".to_string(),
            items: vec![
                WorkflowWorkerActivityItem {
                    cursor: 2,
                    event: event("second"),
                },
                WorkflowWorkerActivityItem {
                    cursor: 3,
                    event: event("third"),
                },
            ],
            oldest_cursor: Some(2),
            newest_cursor: Some(3),
            has_more_before: true,
            has_more_after: false,
            activity_count: 3,
            revision: 3,
        });
        assert_eq!(inspector.worker_activity.len(), 2);
        assert!(inspector.should_start_older_worker_activity_load(true));

        inspector.apply_worker_activity_page(&WorkflowWorkerActivityPage {
            run_id: "run-1".to_string(),
            worker_id: "worker-1".to_string(),
            items: vec![WorkflowWorkerActivityItem {
                cursor: 1,
                event: event("first"),
            }],
            oldest_cursor: Some(1),
            newest_cursor: Some(1),
            has_more_before: false,
            has_more_after: true,
            activity_count: 3,
            revision: 3,
        });

        assert_eq!(inspector.worker_activity.len(), 3);
        let rendered = inspector
            .worker_activity
            .iter()
            .map(|event| match event {
                SessionActivityEvent::Assistant(message) => message.content.as_str(),
                _ => "unexpected",
            })
            .collect::<Vec<_>>();
        assert_eq!(rendered, vec!["first", "second", "third"]);
        assert!(!inspector.worker_activity_has_more_before);
    }

    #[test]
    fn workflow_inspector_activity_scroll_is_follow_bottom_by_default() {
        let snapshot = crate::workflow::WorkflowRunSnapshot {
            run_id: "run-1".to_string(),
            workflow_id: "workflow".to_string(),
            status: crate::workflow::WorkflowNodeStatus::Running,
            started_at_ms: 1,
            completed_at_ms: None,
            input: serde_json::json!({}),
            output: None,
            error: None,
            await_groups: Vec::new(),
            transitions: Vec::new(),
            workers: Vec::new(),
        };
        let mut inspector = WorkflowInspectorState::new(snapshot);
        assert!(inspector.handle_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE
        )));
        assert_eq!(inspector.page, WorkflowInspectorPage::Activity);
        assert!(inspector.activity_scroll.follow_bottom);
        assert_eq!(inspector.activity_scroll.display_scroll(), u16::MAX);
    }

    #[test]
    fn transcript_overlay_syncs_state_after_history_prefix() {
        let history = assistant_cell("older history");
        let first = assistant_cell("first state cell");
        let second = assistant_cell("second state cell");
        let mut overlay =
            TranscriptOverlayState::new(vec![history.clone(), first.clone()], Vec::new(), 1);
        let state = DashboardState {
            activity_events: vec![first, second.clone()],
            ..DashboardState::default()
        };

        overlay.sync_state(&state);

        assert!(overlay.activity_scroll.follow_bottom);
        assert_eq!(
            overlay.cells,
            vec![history, state.activity_events[0].clone(), second]
        );
    }

    #[test]
    fn workflow_inspector_scroll_rows_only_routes_on_activity_page() {
        let snapshot = crate::workflow::WorkflowRunSnapshot {
            run_id: "run-1".to_string(),
            workflow_id: "workflow".to_string(),
            status: crate::workflow::WorkflowNodeStatus::Running,
            started_at_ms: 1,
            completed_at_ms: None,
            input: serde_json::json!({}),
            output: None,
            error: None,
            await_groups: Vec::new(),
            transitions: Vec::new(),
            workers: Vec::new(),
        };
        let state = DashboardState {
            active_workflow_runs: vec![snapshot],
            ..DashboardState::default()
        };
        let mut view = TuiViewState::new();
        assert!(view.open_latest_workflow_inspector(&state));
        assert!(!view.handle_workflow_inspector_scroll_rows(-1));
        assert!(view.handle_workflow_inspector_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE
        )));
        view.workflow_inspector
            .as_mut()
            .expect("workflow inspector")
            .activity_scroll
            .set_render_metrics(100, 20);

        assert!(view.handle_workflow_inspector_scroll_rows(-1));
        assert!(
            view.workflow_inspector
                .as_ref()
                .is_some_and(|inspector| !inspector.activity_scroll.follow_bottom)
        );
    }

    #[test]
    fn transcript_overlay_syncs_live_activity_cells() {
        let first_live = LiveActivityEvent {
            key: "first".to_string(),
            event: assistant_cell("first live cell"),
        };
        let second_live = LiveActivityEvent {
            key: "second".to_string(),
            event: assistant_cell("second live cell"),
        };
        let mut overlay = TranscriptOverlayState::new(Vec::new(), vec![first_live], 0);
        let state = DashboardState {
            live_activity_events: vec![second_live.clone()],
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

        assert!(overlay.activity_scroll.follow_bottom);
        assert!(overlay.handle_key(KeyEvent::new(
            KeyCode::Up,
            crossterm::event::KeyModifiers::NONE
        )));

        assert!(!overlay.activity_scroll.follow_bottom);

        assert!(overlay.handle_key(KeyEvent::new(
            KeyCode::End,
            crossterm::event::KeyModifiers::NONE
        )));

        assert!(overlay.activity_scroll.follow_bottom);
    }
}
