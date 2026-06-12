use std::time::{Duration, Instant};

const TUI_PROFILE_SUMMARY_INTERVAL: Duration = Duration::from_secs(2);
const TUI_SLOW_FRAME_WARN_THRESHOLD: Duration = Duration::from_millis(32);
const TUI_HIGH_FPS_WARN_THRESHOLD: f64 = 30.0;
const TUI_PROFILE_ENV: &str = "DAAT_LOCUS_TUI_PROFILE";

pub(super) struct TuiFrameProfiler {
    enabled: bool,
    window_started_at: Instant,
    frames: u64,
    slow_frames: u64,
    total_frame: Duration,
    total_prep: Duration,
    total_draw: Duration,
    total_activity: Duration,
    total_command: Duration,
    max_frame: Duration,
    max_activity: Duration,
    last_committed_cells: usize,
    last_live_cells: usize,
}

pub(super) struct TuiFrameTiming {
    pub(super) committed_cells: usize,
    pub(super) live_cells: usize,
    pub(super) frame: Duration,
    pub(super) prep: Duration,
    pub(super) draw: Duration,
    pub(super) activity: Duration,
    pub(super) command: Duration,
}

impl TuiFrameProfiler {
    pub(super) fn new() -> Self {
        let enabled = std::env::var(TUI_PROFILE_ENV).is_ok_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        });
        Self {
            enabled,
            window_started_at: Instant::now(),
            frames: 0,
            slow_frames: 0,
            total_frame: Duration::ZERO,
            total_prep: Duration::ZERO,
            total_draw: Duration::ZERO,
            total_activity: Duration::ZERO,
            total_command: Duration::ZERO,
            max_frame: Duration::ZERO,
            max_activity: Duration::ZERO,
            last_committed_cells: 0,
            last_live_cells: 0,
        }
    }

    pub(super) fn record(&mut self, timing: TuiFrameTiming) {
        self.frames = self.frames.saturating_add(1);
        if timing.frame >= TUI_SLOW_FRAME_WARN_THRESHOLD {
            self.slow_frames = self.slow_frames.saturating_add(1);
        }
        self.total_frame += timing.frame;
        self.total_prep += timing.prep;
        self.total_draw += timing.draw;
        self.total_activity += timing.activity;
        self.total_command += timing.command;
        self.max_frame = self.max_frame.max(timing.frame);
        self.max_activity = self.max_activity.max(timing.activity);
        self.last_committed_cells = timing.committed_cells;
        self.last_live_cells = timing.live_cells;

        let elapsed = self.window_started_at.elapsed();
        if elapsed < TUI_PROFILE_SUMMARY_INTERVAL {
            return;
        }
        let fps = self.frames as f64 / elapsed.as_secs_f64().max(0.001);
        let should_warn = fps >= TUI_HIGH_FPS_WARN_THRESHOLD || self.slow_frames > 0;
        if self.enabled || should_warn {
            let frame_count = self.frames.max(1) as f64;
            let message = format!(
                "tui frame profile fps={fps:.1} frames={} slow_frames={} avg_frame_ms={:.2} max_frame_ms={:.2} avg_prep_ms={:.2} avg_draw_ms={:.2} avg_activity_ms={:.2} max_activity_ms={:.2} avg_command_ms={:.2} committed_cells={} live_cells={}",
                self.frames,
                self.slow_frames,
                duration_ms(self.total_frame) / frame_count,
                duration_ms(self.max_frame),
                duration_ms(self.total_prep) / frame_count,
                duration_ms(self.total_draw) / frame_count,
                duration_ms(self.total_activity) / frame_count,
                duration_ms(self.max_activity),
                duration_ms(self.total_command) / frame_count,
                self.last_committed_cells,
                self.last_live_cells,
            );
            if should_warn {
                tracing::warn!("{message}");
            } else {
                tracing::info!("{message}");
            }
        }
        self.reset_window();
    }

    fn reset_window(&mut self) {
        self.window_started_at = Instant::now();
        self.frames = 0;
        self.slow_frames = 0;
        self.total_frame = Duration::ZERO;
        self.total_prep = Duration::ZERO;
        self.total_draw = Duration::ZERO;
        self.total_activity = Duration::ZERO;
        self.total_command = Duration::ZERO;
        self.max_frame = Duration::ZERO;
        self.max_activity = Duration::ZERO;
    }
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
