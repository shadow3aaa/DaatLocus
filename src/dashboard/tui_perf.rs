use std::{str::FromStr, time::Duration};

use miette::{IntoDiagnostic, Result, miette};
use ratatui::{Terminal, backend::TestBackend};
use serde::Serialize;

use super::{
    ActivityCell, DashboardActivityEvent, DashboardState, LiveActivityCell,
    activity_cell_from_tool_ui_event, apply_activity_event, assistant_activity_cell,
    command_panels::{CommandPanel, SkillsListPanel, SkillsTogglePanel},
    render_tui_dashboard_frame, thinking_activity_cell,
    view_state::TuiViewState,
};
use crate::{
    openskills::OpenSkillDashboardSummary,
    telegram_acl::PendingAccessRequest,
    tool_ui::{
        BrowserUiAction, BrowserUiData, ExploredCallUiAction, ExploredCallUiData, ExploredUiData,
        PatchDiffLineKind, PatchDiffLineUiData, PatchFileOperation, PatchFileUiData,
        ReplyDisposition, ReplySubject, ReplyUiData, TerminalUiAction, TerminalUiData, ToolUiEvent,
    },
};

#[derive(Clone, Debug)]
pub(crate) struct TuiPerfCommand {
    pub(crate) scenario: String,
    pub(crate) frames: usize,
    pub(crate) warmup: usize,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) json: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TuiPerfScenario {
    Mixed,
    LongHistory,
    Scrolling,
    LiveActivity,
    CommandPanels,
}

impl TuiPerfScenario {
    fn as_str(self) -> &'static str {
        match self {
            Self::Mixed => "mixed",
            Self::LongHistory => "long-history",
            Self::Scrolling => "scrolling",
            Self::LiveActivity => "live-activity",
            Self::CommandPanels => "command-panels",
        }
    }

    fn valid_values() -> &'static str {
        "mixed, long-history, scrolling, live-activity, command-panels"
    }
}

impl FromStr for TuiPerfScenario {
    type Err = miette::Report;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim() {
            "mixed" => Ok(Self::Mixed),
            "long-history" => Ok(Self::LongHistory),
            "scrolling" => Ok(Self::Scrolling),
            "live-activity" => Ok(Self::LiveActivity),
            "command-panels" => Ok(Self::CommandPanels),
            other => Err(miette!(
                "unknown TUI perf scenario `{other}`; expected one of {}",
                Self::valid_values()
            )),
        }
    }
}

#[derive(Debug, Serialize)]
struct TuiPerfReport {
    scenario: String,
    frames: usize,
    warmup_frames: usize,
    width: u16,
    height: u16,
    committed_cells: usize,
    live_cells: usize,
    scroll_steps: usize,
    final_scroll_offset: u16,
    final_auto_scroll: bool,
    nonblank_cells: usize,
    frame_ms: TuiPerfTimingSummary,
    prep_ms: TuiPerfTimingSummary,
    draw_ms: TuiPerfTimingSummary,
    activity_ms: TuiPerfTimingSummary,
    command_ms: TuiPerfTimingSummary,
    cache: TuiPerfCacheReport,
}

#[derive(Debug, Default, Serialize)]
struct TuiPerfTimingSummary {
    avg: f64,
    max: f64,
}

#[derive(Debug, Serialize)]
struct TuiPerfCacheReport {
    entries: usize,
    occupied_entries: usize,
    hits: u64,
    misses: u64,
    hit_rate: f64,
}

#[derive(Default)]
struct TuiPerfTimingAccumulator {
    frame: Duration,
    max_frame: Duration,
    prep: Duration,
    max_prep: Duration,
    draw: Duration,
    max_draw: Duration,
    activity: Duration,
    max_activity: Duration,
    command: Duration,
    max_command: Duration,
    committed_cells: usize,
    live_cells: usize,
}

pub(crate) fn run_tui_perf_command(command: TuiPerfCommand) -> Result<()> {
    let report = run_tui_perf(command.clone())?;
    if command.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|err| miette!("serialize TUI perf report failed: {err}"))?
        );
    } else {
        print_text_report(&report);
    }
    Ok(())
}

fn run_tui_perf(command: TuiPerfCommand) -> Result<TuiPerfReport> {
    let scenario = TuiPerfScenario::from_str(&command.scenario)?;
    if command.frames == 0 {
        return Err(miette!("--frames must be greater than zero"));
    }
    if command.width < 40 || command.height < 12 {
        return Err(miette!("--width must be >= 40 and --height must be >= 12"));
    }

    let (state, mut view) = mock_dashboard_state(scenario);
    let backend = TestBackend::new(command.width, command.height);
    let mut terminal = Terminal::new(backend).into_diagnostic()?;

    for frame in 0..command.warmup {
        prepare_view_for_frame(&state, &mut view);
        apply_perf_scroll_step(scenario, frame, &mut view);
        render_tui_dashboard_frame(&mut terminal, &mut view, &state).into_diagnostic()?;
    }

    view.cached_activity_lines.reset_stats();
    let mut timings = TuiPerfTimingAccumulator::default();
    let mut scroll_steps = 0;
    for frame in 0..command.frames {
        prepare_view_for_frame(&state, &mut view);
        if apply_perf_scroll_step(scenario, command.warmup + frame, &mut view) {
            scroll_steps += 1;
        }
        let frame_render =
            render_tui_dashboard_frame(&mut terminal, &mut view, &state).into_diagnostic()?;
        timings.record(frame_render.timing);
    }
    let cache_stats = view.cached_activity_lines.stats();
    let last_buffer = terminal.backend().buffer();
    let nonblank_cells = last_buffer
        .content()
        .iter()
        .filter(|cell| cell.symbol() != " ")
        .count();
    std::hint::black_box(nonblank_cells);

    Ok(TuiPerfReport {
        scenario: scenario.as_str().to_string(),
        frames: command.frames,
        warmup_frames: command.warmup,
        width: command.width,
        height: command.height,
        committed_cells: timings.committed_cells,
        live_cells: timings.live_cells,
        scroll_steps,
        final_scroll_offset: view.scroll_offset,
        final_auto_scroll: view.auto_scroll,
        nonblank_cells,
        frame_ms: timings.summary(timings.frame, timings.max_frame, command.frames),
        prep_ms: timings.summary(timings.prep, timings.max_prep, command.frames),
        draw_ms: timings.summary(timings.draw, timings.max_draw, command.frames),
        activity_ms: timings.summary(timings.activity, timings.max_activity, command.frames),
        command_ms: timings.summary(timings.command, timings.max_command, command.frames),
        cache: TuiPerfCacheReport {
            entries: cache_stats.entries,
            occupied_entries: cache_stats.occupied_entries,
            hits: cache_stats.hits,
            misses: cache_stats.misses,
            hit_rate: hit_rate(cache_stats.hits, cache_stats.misses),
        },
    })
}

fn print_text_report(report: &TuiPerfReport) {
    println!(
        "tui-perf scenario={} frames={} warmup={} size={}x{}",
        report.scenario, report.frames, report.warmup_frames, report.width, report.height
    );
    println!(
        "cells committed={} live={} nonblank={}",
        report.committed_cells, report.live_cells, report.nonblank_cells
    );
    println!(
        "scroll steps={} final_offset={} auto_scroll={}",
        report.scroll_steps, report.final_scroll_offset, report.final_auto_scroll
    );
    println!(
        "frame_ms avg={:.3} max={:.3}  prep avg={:.3} max={:.3}  draw avg={:.3} max={:.3}",
        report.frame_ms.avg,
        report.frame_ms.max,
        report.prep_ms.avg,
        report.prep_ms.max,
        report.draw_ms.avg,
        report.draw_ms.max
    );
    println!(
        "activity_ms avg={:.3} max={:.3}  command_ms avg={:.3} max={:.3}",
        report.activity_ms.avg,
        report.activity_ms.max,
        report.command_ms.avg,
        report.command_ms.max
    );
    println!(
        "cache entries={} occupied={} hits={} misses={} hit_rate={:.3}",
        report.cache.entries,
        report.cache.occupied_entries,
        report.cache.hits,
        report.cache.misses,
        report.cache.hit_rate
    );
}

impl TuiPerfTimingAccumulator {
    fn record(&mut self, timing: super::frame_profiler::TuiFrameTiming) {
        self.committed_cells = timing.committed_cells;
        self.live_cells = timing.live_cells;
        self.frame += timing.frame;
        self.max_frame = self.max_frame.max(timing.frame);
        self.prep += timing.prep;
        self.max_prep = self.max_prep.max(timing.prep);
        self.draw += timing.draw;
        self.max_draw = self.max_draw.max(timing.draw);
        self.activity += timing.activity;
        self.max_activity = self.max_activity.max(timing.activity);
        self.command += timing.command;
        self.max_command = self.max_command.max(timing.command);
    }

    fn summary(&self, total: Duration, max: Duration, frames: usize) -> TuiPerfTimingSummary {
        let frame_count = frames.max(1) as f64;
        TuiPerfTimingSummary {
            avg: duration_ms(total) / frame_count,
            max: duration_ms(max),
        }
    }
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn hit_rate(hits: u64, misses: u64) -> f64 {
    let total = hits.saturating_add(misses);
    if total == 0 {
        0.0
    } else {
        hits as f64 / total as f64
    }
}

fn prepare_view_for_frame(state: &DashboardState, view: &mut TuiViewState) {
    view.sync_visible_clear_from_state(state);
    view.sync_transcript_overlay(state);
    if let Some(panel) = view.command_panel.as_mut() {
        panel.sync_state(state);
    }
    view.tick_history_load_cooldown();
    view.sync_history_cursor_from_state(state);
}

fn apply_perf_scroll_step(
    scenario: TuiPerfScenario,
    frame_index: usize,
    view: &mut TuiViewState,
) -> bool {
    if scenario != TuiPerfScenario::Scrolling {
        return false;
    }

    let rows = if (frame_index / 18) % 2 == 0 { 4 } else { -4 };
    view.handle_activity_scroll_rows(rows)
}

fn mock_dashboard_state(scenario: TuiPerfScenario) -> (DashboardState, TuiViewState) {
    let mut state = DashboardState {
        activity_cells: mock_activity_cells(match scenario {
            TuiPerfScenario::LongHistory | TuiPerfScenario::Scrolling => 260,
            TuiPerfScenario::LiveActivity => 45,
            TuiPerfScenario::CommandPanels => 55,
            TuiPerfScenario::Mixed => 140,
        }),
        skills: mock_skills(),
        pending_access_requests: mock_pending_requests(),
        status_output: "runtime: active\nsessions: 4\ntransport: local".to_string(),
        sleep_status_output: "sleep: idle\nlast run: 12m ago".to_string(),
        inspect_telegram_output: "telegram: polling\npending access: 2".to_string(),
        app_status_outputs: vec![
            (
                "coding".to_string(),
                "project_root=/Users/example/project\nlsp=ready\npending_reviews=0".to_string(),
            ),
            (
                "terminal".to_string(),
                "cwd=/Users/example/project\nsessions=2\nactive=main".to_string(),
            ),
        ],
        runtime_status: Some(if scenario == TuiPerfScenario::LiveActivity {
            "Working".to_string()
        } else {
            "Idle".to_string()
        }),
        footer_context: "gpt-5.5 · 126.5k/258.4k used".to_string(),
        footer_estimated_input_tokens: Some(126_500),
        ..DashboardState::default()
    };
    if matches!(
        scenario,
        TuiPerfScenario::Mixed | TuiPerfScenario::LiveActivity
    ) {
        add_live_cells(&mut state, 5);
    }

    let mut view = TuiViewState::new();
    match scenario {
        TuiPerfScenario::LongHistory => {
            view.auto_scroll = false;
            view.scroll_offset = 80;
        }
        TuiPerfScenario::Scrolling => {
            view.auto_scroll = false;
            view.scroll_offset = 80;
            view.max_scroll = u16::MAX;
        }
        TuiPerfScenario::LiveActivity => {
            view.command_input.set_text("/telegram approve".to_string());
        }
        TuiPerfScenario::CommandPanels => {
            view.command_panel = Some(CommandPanel::SkillsList(SkillsListPanel::from_state(
                &state,
            )));
            view.command_input.set_text("/skills".to_string());
        }
        TuiPerfScenario::Mixed => {
            view.command_panel = Some(CommandPanel::SkillsToggle(SkillsTogglePanel::from_state(
                &state,
            )));
            view.command_input.set_text("/skills".to_string());
        }
    }
    (state, view)
}

fn mock_activity_cells(count: usize) -> Vec<ActivityCell> {
    (0..count)
        .filter_map(|idx| match idx % 7 {
            0 => assistant_activity_cell(&mock_markdown(idx)),
            1 => thinking_activity_cell(&mock_reasoning(idx)),
            2 => activity_cell_from_tool_ui_event(ToolUiEvent::Explored(mock_explored(idx))),
            3 => activity_cell_from_tool_ui_event(ToolUiEvent::Terminal(mock_terminal(idx))),
            4 => activity_cell_from_tool_ui_event(ToolUiEvent::Browser(mock_browser(idx))),
            5 => activity_cell_from_tool_ui_event(ToolUiEvent::Patch(mock_patch(idx))),
            _ => activity_cell_from_tool_ui_event(ToolUiEvent::Reply(mock_reply(idx))),
        })
        .collect()
}

fn add_live_cells(state: &mut DashboardState, count: usize) {
    for idx in 0..count {
        let key = format!("live-exec-{idx}");
        apply_activity_event(
            state,
            DashboardActivityEvent::ExecBegin {
                key: key.clone(),
                title: format!("Running command {idx}"),
                call_lines: vec![
                    "cargo test dashboard::".to_string(),
                    "collecting deterministic render timings".to_string(),
                ],
            },
        );
        apply_activity_event(
            state,
            DashboardActivityEvent::ExecUpdate {
                key,
                meta: Some(format!("{}ms", 120 + idx)),
                output_lines: vec![
                    "Compiling daat-locus".to_string(),
                    "rendering test frame".to_string(),
                    "waiting for next event".to_string(),
                ],
            },
        );
    }
    state.live_activity_cells.push(LiveActivityCell {
        key: "live-browser".to_string(),
        cell: ActivityCell::LiveBrowser(
            BrowserUiData {
                action: BrowserUiAction::Snapshot,
                title: "Capturing dashboard docs".to_string(),
                body_lines: vec!["https://example.local/docs/dashboard".to_string()],
                url: Some("https://example.local/docs/dashboard".to_string()),
                line_count: Some(180),
                ref_count: Some(24),
            }
            .into(),
        ),
    });
}

fn mock_markdown(idx: usize) -> String {
    format!(
        "Assistant update {idx}\n\n- inspected dashboard frame path\n- kept render state local\n- measured cache reuse\n\n```rust\nfn frame_{idx}() {{\n    render_activity_feed_cached();\n}}\n```\n\n{}",
        "This paragraph is intentionally long enough to wrap across several terminal rows and exercise markdown rendering, syntax spans, and width-aware cached cell height computation."
    )
}

fn mock_reasoning(idx: usize) -> String {
    format!(
        "Frame {idx} reasoning\nThe dashboard should render from current state only.\nAnimation scheduling should happen after draw.\nCache invalidation should follow source cell identity."
    )
}

fn mock_explored(idx: usize) -> ExploredUiData {
    ExploredUiData {
        stable_id: format!("explored-{idx}"),
        title: "Explored".to_string(),
        calls: vec![
            ExploredCallUiData {
                tool_name: "Read".to_string(),
                action: Some(ExploredCallUiAction::Read),
                target: Some("src/dashboard/mod.rs".to_string()),
                secondary_target: None,
                summary: "Read mod.rs".to_string(),
                detail_lines: vec!["src/dashboard/mod.rs#L350-L590".to_string()],
            },
            ExploredCallUiData {
                tool_name: "Search".to_string(),
                action: Some(ExploredCallUiAction::Search),
                target: Some("schedule_frame|render_tui_dashboard_frame".to_string()),
                secondary_target: Some("dashboard".to_string()),
                summary: "Search schedule_frame|render_tui_dashboard_frame in dashboard"
                    .to_string(),
                detail_lines: vec!["src/dashboard/mod.rs".to_string()],
            },
        ],
    }
}

fn mock_terminal(idx: usize) -> TerminalUiData {
    TerminalUiData {
        action: TerminalUiAction::Execute,
        origin: None,
        title: format!("cargo check #{idx}"),
        body_lines: vec![
            "0.42s".to_string(),
            "Checking daat-locus".to_string(),
            "Finished dev profile".to_string(),
            "No stale redraw loop found".to_string(),
        ],
    }
}

fn mock_browser(idx: usize) -> BrowserUiData {
    BrowserUiData {
        action: BrowserUiAction::Snapshot,
        title: format!("Browser snapshot {idx}"),
        body_lines: vec![
            "Dashboard Architecture".to_string(),
            "TUI Event Loop".to_string(),
            "FrameRequester".to_string(),
            "Render Caches".to_string(),
        ],
        url: Some("https://example.local/architecture".to_string()),
        line_count: Some(240 + idx),
        ref_count: Some(18),
    }
}

fn mock_patch(idx: usize) -> crate::tool_ui::PatchUiData {
    crate::tool_ui::PatchUiData {
        summary_line: format!("Edited dashboard perf harness {idx}"),
        files: vec![PatchFileUiData {
            path: "src/dashboard/tui_perf.rs".to_string(),
            operation: PatchFileOperation::Update,
            added_lines: 12,
            removed_lines: 3,
            diff_lines: vec![
                PatchDiffLineUiData {
                    kind: PatchDiffLineKind::Context,
                    old_lineno: Some(10),
                    new_lineno: Some(10),
                    text: "fn render_frame() {".to_string(),
                },
                PatchDiffLineUiData {
                    kind: PatchDiffLineKind::Delete,
                    old_lineno: Some(11),
                    new_lineno: None,
                    text: "    old_render_loop();".to_string(),
                },
                PatchDiffLineUiData {
                    kind: PatchDiffLineKind::Add,
                    old_lineno: None,
                    new_lineno: Some(11),
                    text: "    render_tui_dashboard_frame();".to_string(),
                },
            ],
        }],
    }
}

fn mock_reply(idx: usize) -> ReplyUiData {
    ReplyUiData {
        disposition: ReplyDisposition::Resolved,
        subject: if idx % 2 == 0 {
            ReplySubject::Message
        } else {
            ReplySubject::Notice
        },
        message_lines: vec![
            format!("Resolved dashboard update {idx}."),
            "The render path is deterministic for this scenario.".to_string(),
        ],
    }
}

fn mock_skills() -> Vec<OpenSkillDashboardSummary> {
    vec![
        OpenSkillDashboardSummary {
            name: "gpt-taste".to_string(),
            description: "Elite UX/UI and advanced motion review for frontend interface polish."
                .to_string(),
            path: "/Users/example/.agents/skills/gpt-taste/SKILL.md".to_string(),
            scope: "user".to_string(),
            allow_implicit_invocation: true,
            user_disabled: false,
            auto_use_enabled: true,
        },
        OpenSkillDashboardSummary {
            name: "shadcn".to_string(),
            description: "Use shadcn/ui components and preserve design-system conventions."
                .to_string(),
            path: "/Users/example/.agents/skills/shadcn/SKILL.md".to_string(),
            scope: "user".to_string(),
            allow_implicit_invocation: true,
            user_disabled: false,
            auto_use_enabled: true,
        },
        OpenSkillDashboardSummary {
            name: "release-notes".to_string(),
            description: "Manual-only release note drafting helper.".to_string(),
            path: "/Users/example/project/.agents/skills/release-notes/SKILL.md".to_string(),
            scope: "project".to_string(),
            allow_implicit_invocation: false,
            user_disabled: false,
            auto_use_enabled: false,
        },
    ]
}

fn mock_pending_requests() -> Vec<PendingAccessRequest> {
    vec![
        PendingAccessRequest {
            chat_id: 42,
            title: "Build Channel".to_string(),
            sender: "shadow3".to_string(),
            last_message_preview: "/status".to_string(),
            first_seen_at_ms: 1_700_000_000_000,
            last_seen_at_ms: 1_700_000_010_000,
        },
        PendingAccessRequest {
            chat_id: 7,
            title: "Ops".to_string(),
            sender: "tester".to_string(),
            last_message_preview: "/skills".to_string(),
            first_seen_at_ms: 1_700_000_020_000,
            last_seen_at_ms: 1_700_000_030_000,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_perf_mixed_scenario_renders_measured_frames() {
        let report = run_tui_perf(TuiPerfCommand {
            scenario: "mixed".to_string(),
            frames: 3,
            warmup: 1,
            width: 100,
            height: 32,
            json: false,
        })
        .expect("perf report");

        assert_eq!(report.scenario, "mixed");
        assert_eq!(report.frames, 3);
        assert!(report.committed_cells > 0);
        assert!(report.nonblank_cells > 0);
        assert!(report.cache.occupied_entries > 0);
        assert!(report.cache.hits > 0);
    }

    #[test]
    fn tui_perf_scrolling_scenario_exercises_scroll_frames() {
        let report = run_tui_perf(TuiPerfCommand {
            scenario: "scrolling".to_string(),
            frames: 8,
            warmup: 1,
            width: 100,
            height: 32,
            json: false,
        })
        .expect("perf report");

        assert_eq!(report.scenario, "scrolling");
        assert_eq!(report.scroll_steps, 8);
        assert!(!report.final_auto_scroll);
        assert_ne!(report.final_scroll_offset, 80);
        assert!(report.cache.hits > 0);
    }

    #[test]
    fn tui_perf_rejects_unknown_scenario() {
        let err = run_tui_perf(TuiPerfCommand {
            scenario: "unknown".to_string(),
            frames: 1,
            warmup: 0,
            width: 80,
            height: 24,
            json: false,
        })
        .expect_err("unknown scenario should fail");

        assert!(err.to_string().contains("unknown TUI perf scenario"));
    }
}
