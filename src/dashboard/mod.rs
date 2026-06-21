//! Dashboard: activity feed + command console.

pub mod cells;
mod command_flow;
mod command_input;
mod command_panels;
mod command_registry;
mod command_render;
mod command_text;
mod commands;
mod frame_profiler;
pub mod frame_rate_limiter;
pub mod frame_requester;
pub mod history;
mod input_controller;
pub mod render;
pub mod renderable;
mod selection;
mod terminal_hyperlinks;
mod transcript_overlay;
mod tui_animation;
pub mod tui_event;
#[cfg(feature = "tui-perf-cmd")]
pub(crate) mod tui_perf;
mod view_state;

pub use cells::{
    ActivityCell, ActivityFeedRenderArgs, CachedActivityLines, DashboardActivityEvent,
    LiveActivityCell, LiveWebActivityItem, ReducedMotion, WebActivityActor, WebActivityItem,
    WebActivityKind, activity_cell_from_tool_ui_event, activity_cells_from_history_items,
    apply_activity_event, assistant_activity_cell, default_web_activity_version,
    final_message_separator_activity_cell, render_activity_feed_cached,
    render_activity_from_messages, sync_web_activity_state, thinking_activity_cell,
    user_activity_cell_from_event, web_activity_item_from_cell,
};
pub(crate) use command_flow::execute_control_command;
pub use commands::{
    DashboardAction, DashboardActionResult, DashboardCommandAttachment, DashboardCommandRunner,
    DashboardControlCommand, DashboardPendingUserInputMoveDirection,
};
pub use history::{
    DashboardActivityHistoryCount, DashboardActivityHistoryPage, DashboardActivityHistoryStore,
    DashboardActivityHistoryWindow, DashboardInputHistory,
};

#[cfg(test)]
use std::path::PathBuf;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use crossterm::cursor::{MoveTo, SetCursorStyle, Show};
#[cfg(test)]
use crossterm::event::KeyModifiers;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, KeyEventKind, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use futures_util::StreamExt;
use ratatui::prelude::*;
#[cfg(test)]
use ratatui::style::Color;

use crate::{
    core::TokenUsageInfo,
    openskills::{OpenSkillDashboardError, OpenSkillDashboardSummary},
    reasoning::turn_compile::load_prompt_persona_spec_sync,
    telegram_acl::PendingAccessRequest,
};
use command_flow::command_live_feedback;
use command_input::{command_input_display_height, command_input_required_height};
#[cfg(test)]
use command_input::{
    command_input_display_text, cursor_display_xy, push_command_input_display_text,
    should_insert_newline_on_enter, wrapped_input_height,
};
use command_panels::DashboardCommandContext;
#[cfg(test)]
use command_panels::{
    CommandFeedback, CommandFeedbackLevel, CommandPanel, SkillsListPanel, SkillsListPanelItem,
    SkillsTogglePanel,
};
pub(crate) use command_registry::remote_dashboard_commands;
#[cfg(test)]
use command_render::render_command_panel;
use command_render::{
    CommandBarRenderState, command_feedback_row_count, command_panel_row_count,
    command_popup_row_count, pending_user_input_preview_row_count, render_command_bar,
};
#[cfg(test)]
use command_text::{render_pending_access_requests, render_skills_list};
pub(crate) use commands::execute_dashboard_action;
use frame_profiler::{TuiFrameProfiler, TuiFrameTiming};
use serde::{Deserialize, Serialize};
use terminal_hyperlinks::{
    TerminalHyperlinkOverlay, collect_removed_terminal_hyperlink_clears,
    collect_terminal_hyperlink_overlays,
};
use transcript_overlay::render_transcript_overlay;
use tui_animation::dashboard_state_needs_animation;
use view_state::TuiViewState;

const TUI_ANIMATION_INTERVAL: Duration = Duration::from_millis(32);
const TUI_COMMAND_HISTORY_LIMIT: usize = 100;

#[derive(Clone, Serialize, Deserialize)]
pub struct DashboardPlanStep {
    pub status: String,
    pub step: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DashboardSessionTitle {
    pub title: String,
    pub generated: bool,
    pub updated_at_ms: i64,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DashboardTokenUsageSnapshot {
    pub main: Option<TokenUsageInfo>,
    #[serde(default)]
    pub main_model: Option<String>,
    pub judge: Option<TokenUsageInfo>,
    #[serde(default)]
    pub judge_model: Option<String>,
    #[serde(default)]
    pub efficient_model: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DashboardStatusCommandSnapshot {
    pub runtime_turn: String,
    pub bound_primitive: String,
    pub active_plans: usize,
    pub events: String,
    pub plan_steps: Vec<DashboardPlanStep>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DashboardPrimitiveOptimizationSnapshot {
    pub running: bool,
    pub current_trigger: Option<String>,
    pub last_result: Option<String>,
    pub last_completed_at_ms: Option<i64>,
    pub primitive_evidence_records: usize,
    pub total_primitive_evidence_run_records: usize,
    pub total_primitive_reflections: usize,
    pub total_primitive_patch_candidates: usize,
    pub total_primitive_merge_candidates: usize,
    pub total_primitive_candidate_evaluations: usize,
    pub total_primitive_frontier_entries: usize,
    pub latest_primitive_frontier_root_entries: usize,
    pub latest_primitive_frontier_branched_entries: usize,
    pub latest_primitive_frontier_max_generation: usize,
    pub total_primitive_patch_applied: usize,
    pub total_primitive_merge_applied: usize,
    pub total_primitive_update_rollbacks: usize,
    pub total_primitive_optimization_rounds: usize,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DashboardRuntimeOptimizationSnapshot {
    pub running: bool,
    pub current_trigger: Option<String>,
    pub last_result: Option<String>,
    pub last_completed_at_ms: Option<i64>,
    pub unread_runtime_error_backlog: usize,
    pub total_runtime_error_cases_consumed: usize,
    pub total_runtime_error_cases: usize,
    pub total_runtime_error_reflections: usize,
    pub total_runtime_contract_candidates: usize,
    pub total_runtime_contract_candidate_evaluations: usize,
    pub total_runtime_contract_system_additions: usize,
    pub total_runtime_contract_updates: usize,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DashboardContextCompositionSegment {
    pub name: String,
    pub label: String,
    pub source: String,
    pub tokens: usize,
    pub bytes: usize,
    pub percent: f64,
    pub hash: String,
    pub cache_role: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DashboardContextCompositionPrefixUnit {
    pub hash: String,
    pub tokens: usize,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DashboardContextCompositionSnapshot {
    pub captured_at_ms: Option<i64>,
    pub model: Option<String>,
    pub total_estimated_tokens: usize,
    pub total_bytes: usize,
    pub message_count: usize,
    pub tool_count: usize,
    pub tools_schema_tokens: usize,
    pub stable_prefix_tokens: usize,
    pub new_suffix_tokens: usize,
    pub changed_prefix_tokens: usize,
    pub previous_common_prefix_tokens: usize,
    pub previous_request_hash: Option<String>,
    pub current_request_hash: Option<String>,
    pub segments: Vec<DashboardContextCompositionSegment>,
    #[serde(default)]
    pub prefix_units: Vec<DashboardContextCompositionPrefixUnit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DashboardRuntimeActivityStatus {
    #[default]
    Idle,
    Thinking,
    Running,
    Tooling,
    Waiting,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DashboardRuntimeStatusLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardRuntimeActivity {
    pub status: DashboardRuntimeActivityStatus,
    pub label: String,
    #[serde(default)]
    pub detail: Option<String>,
    pub active_runtime_turn: bool,
    #[serde(default)]
    pub active_runtime_phase: Option<String>,
    #[serde(default)]
    pub active_runtime_started_at_ms: Option<i64>,
}

impl DashboardRuntimeActivity {
    pub fn new(
        status: DashboardRuntimeActivityStatus,
        label: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self {
            status,
            label: label.into(),
            detail,
            active_runtime_turn: false,
            active_runtime_phase: None,
            active_runtime_started_at_ms: None,
        }
    }

    pub fn with_runtime_turn(
        mut self,
        active_runtime_phase: Option<String>,
        active_runtime_started_at_ms: Option<i64>,
    ) -> Self {
        self.active_runtime_turn = true;
        self.active_runtime_phase = active_runtime_phase;
        self.active_runtime_started_at_ms = active_runtime_started_at_ms;
        self
    }
}

impl Default for DashboardRuntimeActivity {
    fn default() -> Self {
        Self::new(DashboardRuntimeActivityStatus::Idle, "Idle", None)
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DashboardPendingUserInput {
    pub event_id: String,
    pub origin: String,
    pub incoming_text: String,
    pub arrived_at_ms: i64,
    pub attachment_count: usize,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DashboardState {
    #[serde(default = "default_agent_name")]
    pub agent_name: String,
    #[serde(default)]
    pub session_title: Option<DashboardSessionTitle>,
    pub status_output: String,
    #[serde(default)]
    pub status_command: DashboardStatusCommandSnapshot,
    pub sleep_status_output: String,
    pub inspect_telegram_output: String,
    pub system_prompt_output: String,
    pub preturn_context_output: String,
    pub app_status_outputs: Vec<(String, String)>,
    #[serde(default)]
    pub skills: Vec<OpenSkillDashboardSummary>,
    #[serde(default)]
    pub skill_errors: Vec<OpenSkillDashboardError>,
    #[serde(default)]
    pub pending_access_requests: Vec<PendingAccessRequest>,
    #[serde(default)]
    pub pending_user_inputs: Vec<DashboardPendingUserInput>,
    pub activity_cells: Vec<ActivityCell>,
    pub live_activity_cells: Vec<LiveActivityCell>,
    #[serde(default = "default_web_activity_version")]
    pub web_activity_version: u8,
    #[serde(default)]
    pub web_activity_items: Vec<WebActivityItem>,
    #[serde(default)]
    pub live_web_activity_items: Vec<LiveWebActivityItem>,
    #[serde(default)]
    pub activity_history: DashboardActivityHistoryWindow,
    pub last_cycle_elapsed_ms: Option<u64>,
    pub runtime_status: Option<String>,
    #[serde(default)]
    pub runtime_status_level: Option<DashboardRuntimeStatusLevel>,
    #[serde(default)]
    pub runtime_activity: DashboardRuntimeActivity,
    #[serde(default)]
    pub current_plan_step: Option<DashboardPlanStep>,
    #[serde(default)]
    pub token_usage: DashboardTokenUsageSnapshot,
    #[serde(default)]
    pub primitive_optimization: DashboardPrimitiveOptimizationSnapshot,
    #[serde(default)]
    pub runtime_optimization: DashboardRuntimeOptimizationSnapshot,
    #[serde(default)]
    pub context_composition: Option<DashboardContextCompositionSnapshot>,
    #[serde(default)]
    pub reduced_motion: ReducedMotion,
    pub footer_context: String,
    pub footer_estimated_input_tokens: Option<usize>,
}

fn default_agent_name() -> String {
    dashboard_agent_name()
}

pub fn dashboard_agent_name() -> String {
    let name = load_prompt_persona_spec_sync().name.trim().to_string();
    if name.is_empty() {
        "Agent".to_string()
    } else {
        name
    }
}

#[async_trait]
pub trait DashboardHistoryLoader: Send + Sync {
    async fn load_history_before(
        &self,
        before: Option<i64>,
        limit: usize,
    ) -> Result<DashboardActivityHistoryPage, String>;

    async fn load_recent_user_inputs(&self, limit: usize) -> Result<DashboardInputHistory, String>;
}

#[derive(Clone)]
pub struct DashboardIncomingAttachment {
    pub media_type: String,
    pub local_path: String,
    pub description: Option<String>,
}

pub async fn run_tui_dashboard(
    rx: &mut tokio::sync::watch::Receiver<DashboardState>,
    command_runner: &dyn DashboardCommandRunner,
    history_loader: Option<Arc<dyn DashboardHistoryLoader>>,
) -> Result<(), std::io::Error> {
    let initial_command_history = if let Some(loader) = history_loader.as_ref() {
        match loader
            .load_recent_user_inputs(TUI_COMMAND_HISTORY_LIMIT)
            .await
        {
            Ok(history) => history.entries,
            Err(err) => {
                tracing::warn!("TUI command history load failed: {err}");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen,)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    crossterm::execute!(terminal.backend_mut(), SetCursorStyle::SteadyBar,)?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::event::EnableBracketedPaste
    )?;
    crossterm::execute!(terminal.backend_mut(), EnableMouseCapture)?;
    let keyboard_enhancement_enabled = crossterm::execute!(
        terminal.backend_mut(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
    )
    .is_ok();
    let mut view = TuiViewState::new();
    view.replace_command_history(initial_command_history);
    view.seed_command_history_from_state(&rx.borrow());

    // Async event loop: terminal input, dashboard state updates, and scheduled draw requests.
    let mut event_stream = crossterm::event::EventStream::new();
    let (draw_tx, mut draw_rx) = tokio::sync::broadcast::channel::<()>(16);
    // FrameRequester spawns FrameScheduler; keep alive so the task isn't cancelled.
    let frame_requester = frame_requester::FrameRequester::new(draw_tx);
    frame_requester.schedule_frame();

    let mut frame_profiler = TuiFrameProfiler::new();

    loop {
        tokio::select! {
                    event = event_stream.next() => {
                        let event = match event {
                            Some(Ok(e)) => e,
                            _ => continue,
                        };
                        let Some(tui_event) = tui_event::TuiEvent::from_crossterm(event) else {
                            continue;
                        };
                        match tui_event {
                            tui_event::TuiEvent::Key(key) => {
                                if key.kind != KeyEventKind::Press {
                                    continue;
                                }
                                let state = rx.borrow_and_update().clone();
                                let outcome = input_controller::handle_key_event(
                                    key,
                                    &mut view,
                                    &state,
                                );
                                if let input_controller::TuiInputOutcome::CopySelection { text } = outcome {
                                    selection::write_osc52_clipboard(terminal.backend_mut(), &text)?;
                                    frame_requester.schedule_frame();
                                    continue;
                                }
                                let should_exit = input_controller::execute_input_outcome(
                                    outcome,
                                    &mut view,
                                    &state,
                                    command_runner,
                                )
                                .await;
                                frame_requester.schedule_frame();
                                if should_exit {
                                    break;
                                }
                                continue;
                    }
                    tui_event::TuiEvent::MouseWheel { rows } => {
                        if view.selection_dragging() {
                            continue;
                        }
                        if view.transcript_overlay.is_some() {
                            if view.handle_transcript_overlay_scroll_rows(rows) {
                                frame_requester.schedule_frame();
                            }
                        } else if view.command_panel.is_none() && view.handle_activity_scroll_rows(rows) {
                            frame_requester.schedule_frame();
                        }
                    }
                    tui_event::TuiEvent::MouseSelection { kind, x, y, .. } => {
                        if view.handle_selection_mouse_event(kind, x, y) {
                            frame_requester.schedule_frame();
                        }
                    }
                    tui_event::TuiEvent::Resize => {
                        frame_requester.schedule_frame();
                    }
                    tui_event::TuiEvent::Paste(text) => {
                        input_controller::handle_paste_event(&text, &mut view);
                        frame_requester.schedule_frame();
                    }
                }
                continue;
            }
            result = rx.changed() => {
                if result.is_ok() {
                    frame_requester.schedule_frame();
                }
                continue;
            }
            result = draw_rx.recv() => {
                if result.is_err() {
                    continue;
                }
            }
        }

        let state = rx.borrow_and_update();
        view.sync_visible_clear_from_state(&state);
        view.seed_command_history_from_state(&state);
        view.sync_transcript_overlay(&state);
        view.sync_pending_user_input_edit(&state);
        if let Some(panel) = view.command_panel.as_mut() {
            panel.sync_state(&state);
        }
        view.tick_history_load_cooldown();

        // Lazy-load more history when scrolled near the top
        if view.should_start_history_load(history_loader.is_some())
            && let Some(loader) = history_loader.clone()
        {
            let cursor = view.oldest_history_cursor();
            let (load_tx, load_rx) = tokio::sync::oneshot::channel();
            view.begin_history_load(load_rx);
            let frame_requester = frame_requester.clone();
            tokio::spawn(async move {
                let result = loader.load_history_before(cursor, 40).await;
                let _ = load_tx.send(result);
                frame_requester.schedule_frame();
            });
        }

        // Check if a lazy load completed and merge results
        if let Some(mut rx) = view.take_history_load_rx() {
            match rx.try_recv() {
                Ok(Ok(page)) => {
                    view.apply_loaded_history_page(page);
                }
                Ok(Err(err)) => {
                    tracing::warn!("TUI history lazy load failed: {err}");
                    view.finish_history_load_without_page();
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    // Still loading
                    view.keep_history_load_rx(rx);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    view.finish_history_load_without_page();
                }
            }
        }

        // On first iteration, sync cursor from state
        view.sync_history_cursor_from_state(&state);

        terminal.hide_cursor()?;
        let frame_render = render_tui_dashboard_frame(&mut terminal, &mut view, &state)?;
        terminal_hyperlinks::write_terminal_hyperlink_overlays(
            terminal.backend_mut(),
            &frame_render.hyperlink_clears,
            &frame_render.hyperlink_overlays,
        )?;
        restore_terminal_cursor(&mut terminal, view.last_cursor_pos)?;
        frame_profiler.record(frame_render.timing);
        if dashboard_state_needs_animation(&state) {
            frame_requester.schedule_frame_in(TUI_ANIMATION_INTERVAL);
        }
    }

    crossterm::terminal::disable_raw_mode()?;
    if keyboard_enhancement_enabled {
        let _ = crossterm::execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags,);
    }
    crossterm::execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        SetCursorStyle::DefaultUserShape,
        crossterm::terminal::LeaveAlternateScreen,
    )?;
    Ok(())
}

fn restore_terminal_cursor<W: std::io::Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    cursor_pos: Option<(u16, u16)>,
) -> std::io::Result<()> {
    if let Some((x, y)) = cursor_pos {
        crossterm::execute!(terminal.backend_mut(), MoveTo(x, y), Show)?;
    } else {
        terminal.hide_cursor()?;
    }
    Ok(())
}

struct TuiFrameRender {
    timing: TuiFrameTiming,
    hyperlink_clears: Vec<terminal_hyperlinks::TerminalPlainTextOverlay>,
    hyperlink_overlays: Vec<TerminalHyperlinkOverlay>,
}

fn render_tui_dashboard_frame<B: Backend>(
    terminal: &mut Terminal<B>,
    view: &mut TuiViewState,
    state: &DashboardState,
) -> Result<TuiFrameRender, B::Error> {
    let frame_start = Instant::now();
    let prep_start = Instant::now();
    let pending_requests = state.pending_access_requests.clone();
    let overlay_open = view.transcript_overlay.is_some();
    let panel_open =
        view.command_panel.is_some() || view.editing_pending_user_input.is_some() || overlay_open;
    let live_command_feedback = if !panel_open {
        let command_context = DashboardCommandContext {
            requests: &pending_requests,
            state,
        };
        command_live_feedback(view.command_input.as_str(), &command_context)
    } else {
        None
    };
    let active_command_feedback = if view.editing_pending_user_input.is_some() {
        view.command_feedback.as_ref()
    } else if !panel_open {
        live_command_feedback
            .as_ref()
            .or(view.command_feedback.as_ref())
    } else {
        None
    };
    let feedback_rows = command_feedback_row_count(active_command_feedback);
    let popup_rows = if !panel_open {
        let command_context = DashboardCommandContext {
            requests: &pending_requests,
            state,
        };
        command_popup_row_count(view.command_input.as_str(), &command_context)
    } else {
        0
    };
    let term_size = terminal.size().ok();
    let term_width = term_size.map(|size| size.width).unwrap_or(80);
    let term_height = term_size.map(|size| size.height).unwrap_or(24);
    let pending_user_input_rows =
        if view.command_panel.is_none() && view.editing_pending_user_input.is_none() {
            pending_user_input_preview_row_count(&state.pending_user_inputs)
        } else {
            0
        };
    let input_lines = command_input_display_height(
        command_input_required_height(
            view.command_input.as_str(),
            view.command_input.cursor_pos,
            term_width,
        ),
        term_height,
        popup_rows
            .saturating_add(feedback_rows)
            .saturating_add(pending_user_input_rows),
    );
    let panel_rows = command_panel_row_count(
        view.command_panel.as_ref(),
        term_height,
        input_lines,
        popup_rows,
        feedback_rows,
    );

    let (combined_cells, live_activity_cells) = view.visible_activity_cells(state);
    let display_scroll = view.display_scroll();

    let committed_cell_count = combined_cells.len();
    let live_cell_count = live_activity_cells.len();
    let prep_elapsed = prep_start.elapsed();
    let mut activity_elapsed = Duration::ZERO;
    let mut command_elapsed = Duration::ZERO;
    let mut hyperlink_overlays = Vec::new();
    let mut hyperlink_clears = Vec::new();
    let mut user_hyperlink_areas = Vec::new();
    let mut selectable_regions = Vec::new();
    let selection = view.selection.clone();
    let draw_start = Instant::now();
    if overlay_open {
        terminal.draw(|f| {
            view.last_cursor_pos = None;
            let activity_start = Instant::now();
            if let Some(overlay) = view.transcript_overlay.as_mut() {
                render_transcript_overlay(
                    f,
                    f.area(),
                    overlay,
                    &selection,
                    &mut selectable_regions,
                );
                hyperlink_clears = collect_removed_terminal_hyperlink_clears(
                    f.buffer_mut(),
                    &view.previous_hyperlink_overlays,
                    &[],
                );
                view.previous_hyperlink_overlays.clear();
            }
            activity_elapsed = activity_start.elapsed();
        })?;
        view.set_selectable_regions(selectable_regions);
        let draw_elapsed = draw_start.elapsed();
        return Ok(TuiFrameRender {
            timing: TuiFrameTiming {
                committed_cells: committed_cell_count,
                live_cells: live_cell_count,
                frame: frame_start.elapsed(),
                prep: prep_elapsed,
                draw: draw_elapsed,
                activity: activity_elapsed,
                command: command_elapsed,
            },
            hyperlink_clears,
            hyperlink_overlays,
        });
    }
    terminal.draw(|f| {
        let root = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(6),
                Constraint::Length(
                    1 + input_lines
                        + panel_rows
                        + popup_rows
                        + feedback_rows
                        + pending_user_input_rows,
                ),
            ])
            .split(f.area());
        // max_scroll now returned directly from render (no double traversal)
        let activity_start = Instant::now();
        view.max_scroll = render_activity_feed_cached(
            f.buffer_mut(),
            root[0],
            ActivityFeedRenderArgs {
                cells: &combined_cells,
                live_cells: &live_activity_cells,
                scroll_offset: display_scroll,
                cache: &mut view.cached_activity_lines,
                user_hyperlink_areas: &mut user_hyperlink_areas,
                selection: &selection,
                selectable_regions: &mut selectable_regions,
            },
        );
        activity_elapsed = activity_start.elapsed();
        // Update page height for PageUp/PageDown
        view.page_height = root[0].height.saturating_sub(1);

        let command_start = Instant::now();
        render_command_bar(
            f,
            root[1],
            CommandBarRenderState {
                input: view.command_input.as_str(),
                cursor_pos: view.command_input.cursor_pos,
                context: &DashboardCommandContext {
                    requests: &pending_requests,
                    state,
                },
                footer_context: &state.footer_context,
                pending_paste_count: view.pending_pastes.len(),
                pending_image_attachment_count: view.pending_image_attachments.len(),
                pending_user_inputs: &state.pending_user_inputs,
                ctrl_c_reminder: view.ctrl_c_reminder,
                editing_pending_user_input: view.editing_pending_user_input.is_some(),
                panel: view.command_panel.as_ref(),
                panel_rows,
                popup_selection: view.command_popup_selection,
                popup_scroll: view.command_popup_scroll,
                last_cursor_pos: &mut view.last_cursor_pos,
                input_lines,
                feedback: active_command_feedback,
                selection: &selection,
                selectable_regions: &mut selectable_regions,
            },
        );
        command_elapsed = command_start.elapsed();
        if selection.has_selection() {
            hyperlink_clears = collect_removed_terminal_hyperlink_clears(
                f.buffer_mut(),
                &view.previous_hyperlink_overlays,
                &[],
            );
            view.previous_hyperlink_overlays.clear();
        } else {
            hyperlink_overlays =
                collect_terminal_hyperlink_overlays(f.buffer_mut(), &user_hyperlink_areas);
            hyperlink_clears = collect_removed_terminal_hyperlink_clears(
                f.buffer_mut(),
                &view.previous_hyperlink_overlays,
                &hyperlink_overlays,
            );
            view.previous_hyperlink_overlays = hyperlink_overlays.clone();
        }
    })?;
    view.set_selectable_regions(selectable_regions);
    let draw_elapsed = draw_start.elapsed();
    Ok(TuiFrameRender {
        timing: TuiFrameTiming {
            committed_cells: committed_cell_count,
            live_cells: live_cell_count,
            frame: frame_start.elapsed(),
            prep: prep_elapsed,
            draw: draw_elapsed,
            activity: activity_elapsed,
            command: command_elapsed,
        },
        hyperlink_clears,
        hyperlink_overlays,
    })
}

#[cfg(test)]
mod tests {
    use super::command_flow::{
        command_blocks_submission, command_live_feedback, command_panel_for_input,
        dashboard_action_for_input, dashboard_command_body, is_clear_command_input,
        matching_commands, telegram_access_picker_for_input,
    };
    use super::selection::{SelectableId, SelectableRegion};
    use super::*;
    use crate::tool_ui::{
        PatchDiffLineKind, PatchDiffLineUiData, PatchFileOperation, PatchFileUiData, PatchUiData,
        TerminalUiAction, ToolUiEvent,
    };
    use ratatui::{Terminal, backend::TestBackend};

    fn pending_request(chat_id: i64) -> PendingAccessRequest {
        PendingAccessRequest {
            chat_id,
            title: format!("chat-{chat_id}"),
            sender: format!("sender-{chat_id}"),
            last_message_preview: format!("message-{chat_id}"),
            first_seen_at_ms: chat_id,
            last_seen_at_ms: chat_id,
        }
    }

    fn test_command_context<'a>() -> DashboardCommandContext<'a> {
        DashboardCommandContext {
            requests: &[],
            state: Box::leak(Box::new(DashboardState::default())),
        }
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                if let Some(cell) = buffer.cell((x, y)) {
                    out.push_str(cell.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    fn trimmed_buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer_text(buffer)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn transcript_overlay_visual_snapshot() {
        let state = DashboardState {
            activity_cells: vec![
            assistant_activity_cell(
                "## Result\nSee [docs](https://example.test/docs).\n\n```rust\nfn main() {}\n```",
            )
            .expect("assistant cell"),
            activity_cell_from_tool_ui_event(ToolUiEvent::terminal(
                TerminalUiAction::Execute,
                "cargo check",
                vec![
                    "main  exited  exit=0  cwd=C:/repo".to_string(),
                    "Finished dev profile".to_string(),
                ],
            ))
            .expect("terminal cell"),
        ],
            ..DashboardState::default()
        };
        let mut view = TuiViewState::new();
        view.open_transcript_overlay(&state);
        let backend = TestBackend::new(72, 14);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|f| {
                let overlay = view
                    .transcript_overlay
                    .as_mut()
                    .expect("transcript overlay");
                let selection = selection::SelectionRegistry::default();
                let mut selectable_regions = Vec::new();
                render_transcript_overlay(
                    f,
                    f.area(),
                    overlay,
                    &selection,
                    &mut selectable_regions,
                );
            })
            .expect("draw transcript overlay");

        let output = trimmed_buffer_text(terminal.backend().buffer());
        assert_eq!(
            output,
            [
                "T R A N S C R I P T  2 cells",
                "ASSISTANT",
                "  └ Result",
                "    See docs (https://example.test/docs).",
                "    fn main() {}",
                "",
                "COMMAND",
                "  └ $ cargo check",
                "  └ Finished dev profile",
                "  └ ✓ exit=0",
                "  └ main  exited  exit=0  cwd=C:/repo",
                "",
                "",
                "Esc/q close   Up/Down scroll   PgUp/PgDn page   Home/End jump",
            ]
            .join("\n")
        );
    }

    #[test]
    fn dashboard_full_frame_visual_snapshot() {
        let mut state = DashboardState {
            activity_cells: vec![
                assistant_activity_cell(
                    "I updated the renderer.\n\n- transcript\n- markdown\n- composer",
                )
                .expect("assistant cell"),
                activity_cell_from_tool_ui_event(ToolUiEvent::Patch(PatchUiData {
                    summary_line: "updated dashboard renderer".to_string(),
                    files: vec![PatchFileUiData {
                        path: "src/dashboard/cells/tui.rs".to_string(),
                        operation: PatchFileOperation::Update,
                        added_lines: 2,
                        removed_lines: 1,
                        diff_lines: vec![
                            PatchDiffLineUiData {
                                kind: PatchDiffLineKind::Context,
                                old_lineno: Some(10),
                                new_lineno: Some(10),
                                text: "fn render() {".to_string(),
                            },
                            PatchDiffLineUiData {
                                kind: PatchDiffLineKind::Delete,
                                old_lineno: Some(11),
                                new_lineno: None,
                                text: "old();".to_string(),
                            },
                            PatchDiffLineUiData {
                                kind: PatchDiffLineKind::Add,
                                old_lineno: None,
                                new_lineno: Some(11),
                                text: "new();".to_string(),
                            },
                        ],
                    }],
                }))
                .expect("patch cell"),
            ],
            footer_context: "gpt-5.5 · 10k/100k used".to_string(),
            ..DashboardState::default()
        };
        sync_web_activity_state(&mut state);
        let mut view = TuiViewState::new();
        view.command_input.set_text("review this".to_string());
        view.pending_image_attachments
            .push(DashboardCommandAttachment {
                placeholder: "[Image: dashboard.png]".to_string(),
                name: "dashboard.png".to_string(),
                path: PathBuf::from("C:/tmp/dashboard.png"),
                media_type: "image/png".to_string(),
            });
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        render_tui_dashboard_frame(&mut terminal, &mut view, &state)
            .expect("render dashboard frame");

        let output = trimmed_buffer_text(terminal.backend().buffer());
        assert_eq!(
            output,
            [
                " • I updated the renderer.",
                "   - transcript",
                "   - markdown",
                "   - composer",
                "",
                " • Edited src/dashboard/cells/tui.rs (+2 -1)",
                "   └ 10 10   fn render() {",
                "     11    - old();",
                "        11 + new();",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "› review this",
                "gpt-5.5 · 10k/100k used  ·  1 image attachment queued  ·  Enter send. Shift+E...",
            ]
            .join("\n")
        );
    }

    #[test]
    fn dashboard_hyperlinks_only_cover_user_messages() {
        let mut activity_cells = vec![
            assistant_activity_cell(
                "Assistant path src/dashboard/command_render.rs and https://assistant.test",
            )
            .expect("assistant cell"),
        ];
        activity_cells.extend(render_activity_from_messages(vec![
            crate::reasoning::runtime::HistoryMessage::user(
                "See src/dashboard/mod.rs:42 and https://example.com/docs",
            ),
        ]));
        let state = DashboardState {
            activity_cells,
            footer_context: "gpt-5.5 · 126.5k/258.4k used".to_string(),
            ..DashboardState::default()
        };
        let mut view = TuiViewState::new();
        let backend = TestBackend::new(100, 18);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        let frame_render =
            render_tui_dashboard_frame(&mut terminal, &mut view, &state).expect("render frame");

        assert!(
            frame_render
                .hyperlink_overlays
                .iter()
                .any(|overlay| overlay.text == "src/dashboard/mod.rs:42"),
            "user file path should be linked: {:?}",
            frame_render.hyperlink_overlays
        );
        assert!(
            frame_render
                .hyperlink_overlays
                .iter()
                .any(|overlay| overlay.target == "https://example.com/docs"),
            "user URL should be linked: {:?}",
            frame_render.hyperlink_overlays
        );
        assert!(
            frame_render.hyperlink_overlays.iter().all(|overlay| {
                !overlay.text.contains("command_render")
                    && !overlay.target.contains("assistant.test")
                    && !overlay.text.contains("126.5k/258.4k")
            }),
            "assistant, tool/status, and footer rows must not be linked: {:?}",
            frame_render.hyperlink_overlays
        );
    }

    #[test]
    fn dashboard_hyperlinks_are_suppressed_while_selection_is_active() {
        let state = DashboardState {
            activity_cells: render_activity_from_messages(vec![
                crate::reasoning::runtime::HistoryMessage::user(
                    "See src/dashboard/mod.rs:42 and https://example.com/docs",
                ),
            ]),
            ..DashboardState::default()
        };
        let mut view = TuiViewState::new();
        view.set_selectable_regions(vec![SelectableRegion::new(
            SelectableId::new("preselect"),
            Rect::new(0, 0, 20, 1),
            vec!["selected text".to_string()],
            0,
        )]);
        assert!(view.handle_selection_mouse_event(tui_event::TuiMouseSelectionKind::Down, 0, 0));
        assert!(view.handle_selection_mouse_event(tui_event::TuiMouseSelectionKind::Drag, 8, 0));
        assert!(view.handle_selection_mouse_event(tui_event::TuiMouseSelectionKind::Up, 8, 0));

        let backend = TestBackend::new(100, 12);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let frame_render =
            render_tui_dashboard_frame(&mut terminal, &mut view, &state).expect("render frame");

        assert!(
            frame_render.hyperlink_overlays.is_empty(),
            "hyperlink overlay writes must not overwrite active selection highlight"
        );
    }

    #[test]
    fn dashboard_command_body_requires_slash_prefix() {
        assert_eq!(dashboard_command_body("status"), None);
        assert_eq!(dashboard_command_body("/status"), Some("status"));
        assert_eq!(dashboard_command_body("  /status  "), Some("status"));
    }

    #[test]
    fn matching_commands_supports_slash_and_skill_prefixes() {
        let state = DashboardState {
            skills: vec![OpenSkillDashboardSummary {
                name: "writer".to_string(),
                description: "Write release notes".to_string(),
                path: "/tmp/skills/writer/SKILL.md".to_string(),
                scope: "user".to_string(),
                allow_implicit_invocation: true,
                user_disabled: false,
                auto_use_enabled: true,
            }],
            ..DashboardState::default()
        };
        let context = DashboardCommandContext {
            requests: &[],
            state: &state,
        };

        assert!(matching_commands("status", &context).is_empty());
        let slash_matches = matching_commands("/sta", &context);
        assert!(!slash_matches.is_empty());
        assert!(
            slash_matches
                .iter()
                .all(|suggestion| suggestion.completion.starts_with('/'))
        );

        let skill_matches = matching_commands("please use $wri", &context);
        assert_eq!(skill_matches.len(), 1);
        assert_eq!(skill_matches[0].display, "$writer");
        assert_eq!(skill_matches[0].completion, "please use $writer");
        assert!(skill_matches[0].description.contains("Write release notes"));
    }

    #[test]
    fn exact_entry_command_does_not_complete_to_subcommands() {
        let context = test_command_context();
        let matches = matching_commands("/sleep", &context);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].completion, "/sleep");
        assert!(command_live_feedback("/sleep", &context).is_none());
    }

    #[test]
    fn completions_stay_at_top_level_entries() {
        let requests = vec![pending_request(42)];
        let state = DashboardState {
            app_status_outputs: vec![("browser".to_string(), "state".to_string())],
            skills: vec![OpenSkillDashboardSummary {
                name: "writer".to_string(),
                description: "Write release notes".to_string(),
                path: "/tmp/skills/writer/SKILL.md".to_string(),
                scope: "user".to_string(),
                allow_implicit_invocation: true,
                user_disabled: false,
                auto_use_enabled: true,
            }],
            ..DashboardState::default()
        };
        let context = DashboardCommandContext {
            requests: &requests,
            state: &state,
        };

        let app_match = matching_commands("/app", &context)
            .into_iter()
            .next()
            .expect("app completion");
        assert_eq!(app_match.completion, "/app-status");

        assert!(matching_commands("/app-status ", &context).is_empty());
        assert!(matching_commands("/telegram approve ", &context).is_empty());
        assert!(matching_commands("/skills disable ", &context).is_empty());
        assert_eq!(
            matching_commands("$wri", &context)
                .into_iter()
                .map(|suggestion| suggestion.completion)
                .collect::<Vec<_>>(),
            vec!["$writer".to_string()]
        );
    }

    #[test]
    fn skills_command_lists_without_requiring_subcommand() {
        let state = DashboardState {
            skills: vec![OpenSkillDashboardSummary {
                name: "writer".to_string(),
                description: "Write release notes".to_string(),
                path: "/tmp/skills/writer/SKILL.md".to_string(),
                scope: "user".to_string(),
                allow_implicit_invocation: true,
                user_disabled: false,
                auto_use_enabled: true,
            }],
            ..DashboardState::default()
        };
        let context = DashboardCommandContext {
            requests: &[],
            state: &state,
        };

        assert!(command_blocks_submission("/skills", &context).is_none());
        let text = render_skills_list(&state);
        assert!(text.contains("OpenSkills loaded: 1"));
        assert!(text.contains("writer"));
    }

    #[test]
    fn skills_list_panel_keeps_each_skill_on_one_row() {
        let panel = CommandPanel::SkillsList(SkillsListPanel {
            items: vec![
                SkillsListPanelItem {
                    name: "gpt-taste".to_string(),
                    description: "Elite UX/UI & Advanced GSAP Motion Engineer. Enforces Python-driven true randomization for layout variance, strict AIDA page structure, wide editorial typography.".to_string(),
                    path: "/tmp/skills/gpt-taste/SKILL.md".to_string(),
                    scope: "user".to_string(),
                    status: "auto-use enabled".to_string(),
                },
                SkillsListPanelItem {
                    name: "shadcn".to_string(),
                    description: "Manages shadcn components and projects".to_string(),
                    path: "/tmp/skills/shadcn/SKILL.md".to_string(),
                    scope: "user".to_string(),
                    status: "auto-use enabled".to_string(),
                },
            ],
            errors: Vec::new(),
            selected: 0,
            scroll: 0,
            search: String::new(),
        });
        let backend = TestBackend::new(72, panel.desired_height());
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|f| render_command_panel(f, f.area(), &panel))
            .expect("draw skills panel");

        let output = buffer_text(terminal.backend().buffer());
        assert!(output.contains("gpt-taste"));
        assert!(output.contains("shadcn"));
    }

    #[test]
    fn root_commands_open_interactive_bottom_panels() {
        let requests = vec![pending_request(42)];
        let state = DashboardState {
            app_status_outputs: vec![("browser".to_string(), "ready".to_string())],
            skills: vec![OpenSkillDashboardSummary {
                name: "writer".to_string(),
                description: "Write release notes".to_string(),
                path: "/tmp/skills/writer/SKILL.md".to_string(),
                scope: "user".to_string(),
                allow_implicit_invocation: true,
                user_disabled: false,
                auto_use_enabled: true,
            }],
            ..DashboardState::default()
        };
        let context = DashboardCommandContext {
            requests: &requests,
            state: &state,
        };

        match command_panel_for_input("/debug", &context).expect("debug panel") {
            CommandPanel::Selection(panel) => {
                assert!(panel.items.iter().any(|item| item.name.contains("persona")));
            }
            _ => panic!("debug should open a selection panel"),
        }
        match command_panel_for_input("/app-status", &context).expect("app panel") {
            CommandPanel::Selection(panel) => {
                assert_eq!(panel.items[0].name, "browser");
            }
            _ => panic!("app-status should open an app selection panel"),
        }
        match command_panel_for_input("/skills", &context).expect("skills panel") {
            CommandPanel::Selection(panel) => {
                assert_eq!(
                    panel
                        .items
                        .iter()
                        .map(|item| item.name.as_str())
                        .collect::<Vec<_>>(),
                    vec!["List skills", "Enable/Disable Skills"]
                );
            }
            _ => panic!("skills should open a management panel"),
        }
        match command_panel_for_input("/skills list", &context).expect("skills list panel") {
            CommandPanel::SkillsList(panel) => {
                assert_eq!(panel.items[0].name, "writer");
            }
            _ => panic!("skills list should open a skill list panel"),
        }
        match command_panel_for_input("/telegram approve", &context).expect("telegram panel") {
            CommandPanel::TelegramAccess(panel) => {
                assert_eq!(panel.requests.len(), 1);
            }
            _ => panic!("telegram approve should open an access picker"),
        }
    }

    #[test]
    fn illegal_extra_arguments_block_submission() {
        let context = test_command_context();
        let feedback =
            command_blocks_submission("/sleep run now", &context).expect("extra arg feedback");

        assert!(matches!(feedback.level, CommandFeedbackLevel::Error));
        assert!(feedback.message.contains("does not take extra arguments"));
    }

    #[test]
    fn hidden_action_commands_parse_to_dashboard_actions() {
        let requests = vec![pending_request(42)];
        let state = DashboardState {
            skills: vec![OpenSkillDashboardSummary {
                name: "writer".to_string(),
                description: "Write release notes".to_string(),
                path: "/tmp/skills/writer/SKILL.md".to_string(),
                scope: "user".to_string(),
                allow_implicit_invocation: true,
                user_disabled: false,
                auto_use_enabled: true,
            }],
            ..DashboardState::default()
        };
        let context = DashboardCommandContext {
            requests: &requests,
            state: &state,
        };

        let invocation = dashboard_action_for_input("/skills disable writer", &context)
            .expect("valid action")
            .expect("skills action");
        assert_eq!(
            invocation.action,
            DashboardAction::SetSkillAutoUse {
                path: PathBuf::from("/tmp/skills/writer/SKILL.md"),
                enabled: false,
            }
        );

        let invocation = dashboard_action_for_input("/telegram approve 42", &context)
            .expect("valid action")
            .expect("telegram action");
        assert_eq!(
            invocation.action,
            DashboardAction::ApproveTelegramAccess { chat_id: 42 }
        );
    }

    #[test]
    fn skills_toggle_panel_shows_only_error_feedback() {
        let mut panel = CommandPanel::SkillsToggle(SkillsTogglePanel {
            items: Vec::new(),
            selected: 0,
            scroll: 0,
            search: String::new(),
            feedback: None,
        });

        panel.set_error_feedback(CommandFeedback {
            title: "SKILLS".to_string(),
            message: "queued skills auto-use enable".to_string(),
            detail: None,
            level: CommandFeedbackLevel::Info,
        });
        match &panel {
            CommandPanel::SkillsToggle(panel) => assert!(panel.feedback.is_none()),
            _ => panic!("expected skills toggle panel"),
        }

        panel.set_error_feedback(CommandFeedback {
            title: "SKILLS".to_string(),
            message: "failed to queue skills auto-use".to_string(),
            detail: None,
            level: CommandFeedbackLevel::Error,
        });
        match &panel {
            CommandPanel::SkillsToggle(panel) => {
                assert_eq!(
                    panel
                        .feedback
                        .as_ref()
                        .map(|feedback| feedback.message.as_str()),
                    Some("failed to queue skills auto-use")
                );
            }
            _ => panic!("expected skills toggle panel"),
        }
    }

    #[test]
    fn clear_command_maps_to_quiet_dashboard_action() {
        let context = test_command_context();
        let invocation = dashboard_action_for_input("/clear", &context)
            .expect("valid action")
            .expect("clear action");

        assert!(is_clear_command_input("/clear"));
        assert!(invocation.quiet_success);
        assert_eq!(invocation.action, DashboardAction::ClearConversation);
    }

    #[test]
    fn shift_or_alt_enter_insert_newline() {
        assert!(should_insert_newline_on_enter(KeyModifiers::SHIFT));
        assert!(should_insert_newline_on_enter(KeyModifiers::ALT));
        assert!(should_insert_newline_on_enter(
            KeyModifiers::SHIFT | KeyModifiers::ALT
        ));
        assert!(!should_insert_newline_on_enter(KeyModifiers::NONE));
        assert!(!should_insert_newline_on_enter(KeyModifiers::CONTROL));
    }

    #[test]
    fn wrapped_input_height_counts_trailing_newline() {
        assert_eq!(wrapped_input_height("hello", 80), 1);
        assert_eq!(wrapped_input_height("hello\n", 80), 2);
        assert_eq!(wrapped_input_height("hello\nworld", 80), 2);
        assert_eq!(wrapped_input_height("abcdefghi", 10), 2);
        assert_eq!(wrapped_input_height("abcdefghijklmnopqr", 10), 3);
    }

    #[test]
    fn command_input_display_height_expands_past_ten_lines() {
        assert_eq!(command_input_display_height(18, 40, 0), 18);
    }

    #[test]
    fn command_input_required_height_includes_natural_wrap_cursor_row() {
        assert_eq!(command_input_required_height("abcdefgh", 8, 10), 2);
    }

    #[test]
    fn cursor_display_xy_moves_after_trailing_newline() {
        let area = Rect::new(0, 0, 80, 10);
        assert_eq!(
            cursor_display_xy("hello\n", "hello\n".len(), 78, 80, 2, area, 0),
            (2, 1)
        );
    }

    #[test]
    fn cursor_display_xy_places_natural_wrap_cursor_on_visual_line() {
        let area = Rect::new(0, 0, 10, 4);
        assert_eq!(cursor_display_xy("abcdefgh", 8, 8, 8, 2, area, 0), (2, 1));
        assert_eq!(cursor_display_xy("abcdefghi", 9, 8, 8, 2, area, 0), (3, 1));
    }

    #[test]
    fn command_input_display_text_indents_multiline_continuations() {
        let mut display = String::from("› ");
        push_command_input_display_text(&mut display, "hello\nworld\n");

        assert_eq!(display, "› hello\n  world\n  ");
    }

    #[test]
    fn command_input_display_text_renders_completion_suffix_dim() {
        let text = command_input_display_text("/app", Some("/app-status"));

        assert_eq!(text.lines.len(), 1);
        assert_eq!(text.lines[0].spans.len(), 2);
        assert_eq!(text.lines[0].spans[0].content.as_ref(), "› /app");
        assert_eq!(text.lines[0].spans[0].style.fg, Some(Color::White));
        assert_eq!(text.lines[0].spans[1].content.as_ref(), "-status");
        assert_eq!(text.lines[0].spans[1].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn remote_dashboard_commands_are_derived_from_command_registry() {
        let commands = remote_dashboard_commands()
            .into_iter()
            .map(|command| command.command)
            .collect::<Vec<_>>();

        assert!(commands.contains(&"debug"));
        assert!(commands.contains(&"app_status"));
        assert!(commands.contains(&"restart"));
        assert!(commands.contains(&"skills"));
        assert!(!commands.contains(&"pending"));
        assert!(!commands.contains(&"snapshot"));
        assert!(!commands.contains(&"system_prompt"));
        assert!(!commands.contains(&"quit"));
    }

    #[test]
    fn telegram_access_without_chat_id_lists_pending_requests() {
        let requests = vec![pending_request(42)];
        let state = DashboardState::default();
        let context = DashboardCommandContext {
            requests: &requests,
            state: &state,
        };

        let output = render_pending_access_requests("approve", context.requests);

        assert!(output.contains("pending requests"));
        assert!(output.contains("/telegram approve <chat_id>"));
        assert!(output.contains("42"));
    }

    #[test]
    fn local_telegram_access_picker_matches_no_arg_commands() {
        let requests = vec![pending_request(42), pending_request(7)];

        let approve = telegram_access_picker_for_input("/telegram approve", &requests)
            .expect("approve command should open picker");
        assert_eq!(approve.action.verb(), "approve");
        assert_eq!(approve.requests.len(), 2);

        let reject = telegram_access_picker_for_input("/telegram reject ", &requests)
            .expect("reject command should open picker");
        assert_eq!(reject.action.verb(), "reject");

        assert!(telegram_access_picker_for_input("/telegram approve 42", &requests).is_none());
        assert!(telegram_access_picker_for_input("/status", &requests).is_none());
    }

    #[test]
    fn dashboard_state_millisecond_fields_json_round_trip() {
        let state = DashboardState {
            last_cycle_elapsed_ms: Some(42),
            ..DashboardState::default()
        };

        let encoded = serde_json::to_string(&state).expect("serialize dashboard state");
        let decoded: DashboardState =
            serde_json::from_str(&encoded).expect("deserialize dashboard state");

        assert_eq!(decoded.last_cycle_elapsed_ms, Some(42));
    }
}
