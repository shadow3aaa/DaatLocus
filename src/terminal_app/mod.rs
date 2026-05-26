pub mod process;
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

use crate::terminal_app::process::{
    DEFAULT_OUTPUT_BUFFER_CAPACITY_BYTES, TerminalOutputChunk, TerminalOutputStats, TerminalProcess,
};
use async_trait::async_trait;
use miette::{Result, bail, miette};
use schemars::schema_for;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    app::{
        App, AppHowToUse, AppId, AppStateRender, AppToolExecutionContext, AppToolExecutionResult,
        AppToolScope, AppToolSpec, AppUsage,
    },
    core::{TerminalExecArgs, TerminalTerminateArgs, TerminalWaitMode, TerminalWriteStdinArgs},
    dashboard::{DashboardActivityEvent, apply_activity_event},
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    sandbox::RuntimeSandboxPolicy,
    tool_ui::{TerminalUiAction, ToolCallUiEvent, ToolUiEvent, compact_body_lines},
};

const TERMINAL_USAGE_PURPOSE: &str =
    "Terminal is the local command execution and long-running process interaction surface.";
const TERMINAL_WHEN_TO_FOCUS: &[&str] = &[
    "When local commands or scripts need to run.",
    "When command output, errors, or filesystem inspection results are needed.",
    "When an already-running process needs continued stdin interaction, output waiting, or termination.",
];

#[cfg(windows)]
const TERMINAL_HOW_TO_USE_LINES: &[&str] = &[
    "Operate Terminal only through terminal tools; do not assume that plain assistant text is terminal input.",
    "Use only the currently exposed Terminal tool names for terminal operations; app scope mangling exposes them as `terminal__terminal_exec / terminal__terminal_write_stdin / terminal__terminal_terminate`.",
    "`terminal_exec` creates a new session when `session_id` is omitted and reuses an existing session only when `session_id` is explicitly provided.",
    "If a command is still running, continue with `terminal_write_stdin` and explicitly provide the target `session_id`. Send empty text when you only want to wait for more output.",
    "For `terminal_write_stdin`, omit `wait_mode` or use `any_output` to return after the next output update; use `timeout` to wait the full yield window or process exit without streaming intermediate progress updates.",
    "Never use interactive full-screen terminal programs such as vim, vi, nano, less, or top. Use non-interactive commands such as `cat`, `grep`, `head`, `tail`, or `python -c` to inspect files; prefer `apply_patch` for edits instead of shell string assembly.",
    "Never proactively start commands that require human accounts, passwords, browser authorization, device-code authorization, or interactive login wizards, such as `gh auth login`, `docker login`, or `npm login`. Prefer public webpages, HTTP APIs, `git clone`, `curl`, or unauthenticated lookup paths.",
    "If the terminal is already stuck in an authentication or login prompt you should not enter, do not continue answering wizard questions; interrupt it and switch to a non-interactive approach.",
];

#[cfg(not(windows))]
const TERMINAL_HOW_TO_USE_LINES: &[&str] = &[
    "Operate Terminal only through terminal tools; do not assume that plain assistant text is terminal input.",
    "Use only the currently exposed Terminal tool names for terminal operations; app scope mangling exposes them as `terminal__terminal_exec / terminal__terminal_write_stdin / terminal__terminal_terminate`.",
    "`terminal_exec` creates a new session when `session_id` is omitted and reuses an existing session only when `session_id` is explicitly provided.",
    "If a command is still running, continue with `terminal_write_stdin` and explicitly provide the target `session_id`. Send empty text when you only want to wait for more output.",
    "For `terminal_write_stdin`, omit `wait_mode` or use `any_output` to return after the next output update; use `timeout` to wait the full yield window or process exit without streaming intermediate progress updates.",
    "Never use interactive full-screen terminal programs such as vim, vi, nano, less, or top. Use non-interactive commands such as `cat`, `grep`, `head`, `tail`, or `python -c` to inspect files; prefer `apply_patch` for edits instead of shell string assembly.",
    "Never proactively start commands that require human accounts, passwords, browser authorization, device-code authorization, or interactive login wizards, such as `gh auth login`, `docker login`, or `npm login`. Prefer public webpages, HTTP APIs, `git clone`, `curl`, or unauthenticated lookup paths.",
    "If the terminal is already stuck in an authentication or login prompt you should not enter, do not continue answering wizard questions; interrupt it and switch to a non-interactive approach.",
];

pub struct TerminalApp {
    sessions: BTreeMap<String, TerminalSession>,
    next_session_index: usize,
    output_buffer_capacity: usize,
}

pub struct TerminalExecCommandRequest<'a> {
    pub command: String,
    pub session_id: Option<String>,
    pub workdir: Option<String>,
    pub sandbox_policy: &'a RuntimeSandboxPolicy,
    pub yield_time_ms: Option<u64>,
    pub max_chars: Option<usize>,
}

struct TerminalSession {
    process: Option<TerminalProcess>,
    output_offset: usize,
    last_activity: Instant,
    state: TerminalSessionState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalToolResult {
    pub session: TerminalSessionState,
    pub output: String,
    pub output_missed_bytes: usize,
    pub output_dropped_bytes: usize,
    pub output_retained_bytes: usize,
    pub output_buffer_capacity: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSessionState {
    pub session_id: String,
    pub process_id: Option<u32>,
    pub command: Option<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub cwd: Option<String>,
    pub has_unread_output: bool,
    pub last_output_preview: String,
    #[serde(default)]
    pub output_total_written_bytes: usize,
    #[serde(default)]
    pub output_retained_bytes: usize,
    #[serde(default)]
    pub output_dropped_bytes: usize,
    #[serde(default)]
    pub output_buffer_capacity: usize,
}

impl TerminalApp {
    const DEFAULT_EXEC_YIELD_TIME_MS: u64 = 10_000;
    const DEFAULT_WRITE_STDIN_YIELD_TIME_MS: u64 = 250;
    const DEFAULT_WAIT_YIELD_TIME_MS: u64 = 30_000;
    const MIN_EMPTY_POLL_YIELD_TIME_MS: u64 = 5_000;
    const MAX_WRITE_STDIN_YIELD_TIME_MS: u64 = 30_000;
    const MAX_WAIT_YIELD_TIME_MS: u64 = 120_000;
    const MAX_EXITED_SESSION_TOMBSTONES: usize = 4;

    pub fn new() -> Self {
        Self {
            sessions: BTreeMap::new(),
            next_session_index: 1,
            output_buffer_capacity: DEFAULT_OUTPUT_BUFFER_CAPACITY_BYTES,
        }
    }

    #[cfg(test)]
    fn new_with_output_buffer_capacity(output_buffer_capacity: usize) -> Self {
        Self {
            sessions: BTreeMap::new(),
            next_session_index: 1,
            output_buffer_capacity,
        }
    }

    pub fn session_state(&self, session_id: &str) -> Result<TerminalSessionState> {
        self.sessions
            .get(session_id)
            .map(|session| session.state.clone())
            .ok_or_else(|| miette!("unknown terminal session `{session_id}`"))
    }

    fn forbidden_input_reason(text: &str) -> Option<&'static str> {
        let normalized = text.trim().to_ascii_lowercase();
        let forbidden_prefixes = [
            "gh auth login",
            "gh auth refresh",
            "gh auth setup-git",
            "docker login",
            "npm login",
            "pnpm login",
            "yarn login",
            "huggingface-cli login",
            "hf auth login",
        ];

        forbidden_prefixes
            .iter()
            .find(|prefix| normalized.starts_with(**prefix))
            .map(|_| "interactive authentication/login commands are not allowed in Terminal; abort and use a non-interactive alternative")
    }

    fn cwd_from_shell_input(text: &str) -> Option<String> {
        let trimmed = text.trim();
        if cfg!(windows) {
            let prefix = "Set-Location -LiteralPath '";
            let suffix = "'";
            return trimmed
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(suffix))
                .map(|path| path.replace("''", "'"));
        }

        let prefix = "cd -- '";
        let suffix = "'";
        trimmed
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
            .map(|path| path.replace("'\"'\"'", "'"))
    }

    pub async fn exec_command_with_progress<F>(
        &mut self,
        request: TerminalExecCommandRequest<'_>,
        mut on_progress: F,
    ) -> Result<TerminalToolResult>
    where
        F: FnMut(&TerminalSessionState, &str) + Send,
    {
        let TerminalExecCommandRequest {
            command,
            session_id,
            workdir,
            sandbox_policy,
            yield_time_ms,
            max_chars,
        } = request;

        let target_session_id = session_id.unwrap_or_else(|| self.create_session());
        if let Some(reason) = Self::forbidden_input_reason(&command) {
            bail!(reason);
        }
        let output_buffer_capacity = self.output_buffer_capacity;
        let effective_workdir = workdir;
        let session = self.session_mut(&target_session_id)?;
        if session.state.status == "running" {
            bail!("terminal session `{target_session_id}` already has a running process");
        }
        session.process = Some(
            spawn_terminal_process(
                &command,
                effective_workdir.as_deref(),
                sandbox_policy,
                output_buffer_capacity,
            )
            .map_err(|err| miette!("failed to spawn terminal process: {err}"))?,
        );
        session.output_offset = 0;
        session.state.command = Some(command);
        session.state.status = "running".to_string();
        session.state.exit_code = None;
        session.state.output_buffer_capacity = output_buffer_capacity;
        if let Some(workdir) = effective_workdir.clone() {
            session.state.cwd = Some(workdir);
        }
        session.state.has_unread_output = true;
        let start_offset = session
            .process
            .as_ref()
            .map(|process| process.output_len())
            .unwrap_or(0);
        let mut progress_offset = start_offset;
        session.last_activity = Instant::now();
        let timeout =
            Duration::from_millis(yield_time_ms.unwrap_or(Self::DEFAULT_EXEC_YIELD_TIME_MS));
        let started_at = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            refresh_terminal_session(session);
            let chunk = session
                .process
                .as_ref()
                .map(|process| process.output_since(progress_offset))
                .unwrap_or_else(|| empty_terminal_output_chunk(progress_offset, &session.state));
            progress_offset = chunk.next_offset;
            apply_terminal_output_stats(&mut session.state, chunk.stats);
            let delta = format_terminal_output_chunk(&chunk, max_chars);
            if !delta.is_empty() {
                on_progress(&session.state, &delta);
            }
            if session.state.status != "running" || started_at.elapsed() >= timeout {
                break;
            }
        }
        refresh_terminal_session(session);
        let chunk = session
            .process
            .as_ref()
            .map(|process| process.output_since(start_offset))
            .unwrap_or_else(|| empty_terminal_output_chunk(start_offset, &session.state));
        session.output_offset = chunk.next_offset;
        apply_terminal_output_stats(&mut session.state, chunk.stats);
        session.last_activity = Instant::now();
        session.state.has_unread_output = false;
        let state = session.state.clone();
        let output = format_terminal_output_chunk(&chunk, max_chars);
        let output_missed_bytes = chunk.missed_bytes;
        let output_stats = chunk.stats;
        self.prune_exited_sessions();
        Ok(TerminalToolResult {
            session: state,
            output,
            output_missed_bytes,
            output_dropped_bytes: output_stats.dropped_bytes,
            output_retained_bytes: output_stats.retained_bytes,
            output_buffer_capacity: output_stats.buffer_capacity,
        })
    }

    pub async fn write_stdin_with_progress<F>(
        &mut self,
        session_id: &str,
        text: String,
        wait_mode: TerminalWaitMode,
        yield_time_ms: Option<u64>,
        max_chars: Option<usize>,
        mut on_progress: F,
    ) -> Result<TerminalToolResult>
    where
        F: FnMut(&TerminalSessionState, &str) + Send,
    {
        if let Some(reason) = Self::forbidden_input_reason(&text) {
            bail!(reason);
        }
        let session = self.session_mut(session_id)?;
        let Some(process) = session.process.as_mut() else {
            bail!("terminal session `{session_id}` has no running process");
        };
        if let Some(updated_cwd) = Self::cwd_from_shell_input(&text) {
            session.state.cwd = Some(updated_cwd);
        }
        let start_offset = process.output_len();
        let mut progress_offset = start_offset;
        process
            .write(&text)
            .await
            .map_err(|err| miette!("failed to write stdin to terminal process: {err}"))?;
        session.last_activity = Instant::now();
        session.state.has_unread_output = true;
        let requested_yield_ms = yield_time_ms.unwrap_or(match wait_mode {
            TerminalWaitMode::AnyOutput => Self::DEFAULT_WRITE_STDIN_YIELD_TIME_MS,
            TerminalWaitMode::Timeout => Self::DEFAULT_WAIT_YIELD_TIME_MS,
        });
        let effective_yield_ms = match wait_mode {
            TerminalWaitMode::AnyOutput if text.is_empty() => requested_yield_ms.clamp(
                Self::MIN_EMPTY_POLL_YIELD_TIME_MS,
                Self::MAX_WRITE_STDIN_YIELD_TIME_MS,
            ),
            TerminalWaitMode::AnyOutput => {
                requested_yield_ms.min(Self::MAX_WRITE_STDIN_YIELD_TIME_MS)
            }
            TerminalWaitMode::Timeout => requested_yield_ms.min(Self::MAX_WAIT_YIELD_TIME_MS),
        };
        let timeout = Duration::from_millis(effective_yield_ms);
        let started_at = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            refresh_terminal_session(session);
            let previous_progress_offset = progress_offset;
            let chunk = session
                .process
                .as_ref()
                .map(|process| process.output_since(progress_offset))
                .unwrap_or_else(|| empty_terminal_output_chunk(progress_offset, &session.state));
            progress_offset = chunk.next_offset;
            apply_terminal_output_stats(&mut session.state, chunk.stats);
            let has_output_update =
                chunk.missed_bytes > 0 || chunk.next_offset > previous_progress_offset;
            if wait_mode == TerminalWaitMode::AnyOutput {
                let delta = format_terminal_output_chunk(&chunk, max_chars);
                if !delta.is_empty() {
                    on_progress(&session.state, &delta);
                }
            }
            if session.state.status != "running"
                || started_at.elapsed() >= timeout
                || (wait_mode == TerminalWaitMode::AnyOutput && has_output_update)
            {
                break;
            }
        }
        refresh_terminal_session(session);
        let chunk = session
            .process
            .as_ref()
            .map(|process| process.output_since(start_offset))
            .unwrap_or_else(|| empty_terminal_output_chunk(start_offset, &session.state));
        session.output_offset = chunk.next_offset;
        apply_terminal_output_stats(&mut session.state, chunk.stats);
        session.last_activity = Instant::now();
        session.state.has_unread_output = false;
        let state = session.state.clone();
        let output = format_terminal_output_chunk(&chunk, max_chars);
        let output_missed_bytes = chunk.missed_bytes;
        let output_stats = chunk.stats;
        self.prune_exited_sessions();
        Ok(TerminalToolResult {
            session: state,
            output,
            output_missed_bytes,
            output_dropped_bytes: output_stats.dropped_bytes,
            output_retained_bytes: output_stats.retained_bytes,
            output_buffer_capacity: output_stats.buffer_capacity,
        })
    }

    pub async fn terminate_session(&mut self, session_id: &str) -> Result<TerminalSessionState> {
        let state = {
            let session = self.session_mut(session_id)?;
            if let Some(process) = session.process.as_mut() {
                let _ = process.start_kill();
            }
            session.state.status = "terminating".to_string();
            session.state.has_unread_output = true;
            session.last_activity = Instant::now();
            refresh_terminal_session(session);
            session.state.clone()
        };
        self.sessions.remove(session_id);
        Ok(state)
    }

    fn session_mut(&mut self, session_id: &str) -> Result<&mut TerminalSession> {
        self.sessions
            .get_mut(session_id)
            .ok_or_else(|| miette::miette!("unknown terminal session `{session_id}`"))
    }

    #[cfg(test)]
    fn refresh_all_sessions(&mut self) {
        for session in self.sessions.values_mut() {
            refresh_terminal_session(session);
        }
        self.prune_exited_sessions();
    }

    fn create_session(&mut self) -> String {
        let session_id = format!("terminal-session-{}", self.next_session_index);
        self.next_session_index += 1;
        let session = TerminalSession {
            process: None,
            output_offset: 0,
            last_activity: Instant::now(),
            state: TerminalSessionState {
                session_id: session_id.clone(),
                process_id: None,
                command: None,
                status: "idle".to_string(),
                exit_code: None,
                cwd: None,
                has_unread_output: true,
                last_output_preview: String::new(),
                output_total_written_bytes: 0,
                output_retained_bytes: 0,
                output_dropped_bytes: 0,
                output_buffer_capacity: self.output_buffer_capacity,
            },
        };
        self.sessions.insert(session_id.clone(), session);
        session_id
    }

    fn prune_exited_sessions(&mut self) {
        let exited_ids = self
            .sessions
            .iter()
            .filter(|(_, session)| session.state.status.starts_with("exited"))
            .map(|(session_id, session)| (session_id.clone(), session.last_activity))
            .collect::<Vec<_>>();

        if exited_ids.len() <= Self::MAX_EXITED_SESSION_TOMBSTONES {
            return;
        }

        let mut sorted = exited_ids;
        sorted.sort_by_key(|(_, last_activity)| *last_activity);
        let remove_count = sorted.len() - Self::MAX_EXITED_SESSION_TOMBSTONES;
        for (session_id, _) in sorted.into_iter().take(remove_count) {
            self.sessions.remove(&session_id);
        }
    }
}

fn parse_terminal_tool_args<T: for<'de> Deserialize<'de>>(call: &AgentToolCall) -> Result<T> {
    serde_json::from_value(call.arguments.clone()).map_err(|err| {
        miette!(
            "invalid arguments for terminal tool `{}`: {}; arguments={}",
            call.name,
            err,
            call.arguments
        )
    })
}

fn display_session_label(session_id: &str) -> String {
    session_id.to_string()
}

fn terminal_progress_mode(text: &str) -> &'static str {
    if text.is_empty() { "poll" } else { "continue" }
}

fn terminal_wait_mode_label(wait_mode: TerminalWaitMode) -> &'static str {
    match wait_mode {
        TerminalWaitMode::AnyOutput => "any_output",
        TerminalWaitMode::Timeout => "timeout",
    }
}

fn terminal_session_meta(session: &TerminalSessionState) -> String {
    let mut meta = format!(
        "{}  {}  exit={}  cwd={}",
        display_session_label(&session.session_id),
        session.status,
        session
            .exit_code
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        session.cwd.as_deref().unwrap_or("-")
    );
    if session.output_dropped_bytes > 0 {
        meta.push_str(&format!(
            "  dropped={}B  buffer={}/{}B",
            session.output_dropped_bytes,
            session.output_retained_bytes,
            session.output_buffer_capacity
        ));
    }
    meta
}

fn apply_terminal_output_stats(session: &mut TerminalSessionState, stats: TerminalOutputStats) {
    session.output_total_written_bytes = stats.total_written_bytes;
    session.output_retained_bytes = stats.retained_bytes;
    session.output_dropped_bytes = stats.dropped_bytes;
    session.output_buffer_capacity = stats.buffer_capacity;
}

fn empty_terminal_output_chunk(
    next_offset: usize,
    session: &TerminalSessionState,
) -> TerminalOutputChunk {
    TerminalOutputChunk {
        text: String::new(),
        next_offset,
        missed_bytes: 0,
        stats: TerminalOutputStats {
            buffer_capacity: session.output_buffer_capacity,
            total_written_bytes: next_offset,
            retained_bytes: session.output_retained_bytes,
            dropped_bytes: session.output_dropped_bytes,
        },
    }
}

fn format_terminal_output_chunk(chunk: &TerminalOutputChunk, max_chars: Option<usize>) -> String {
    let output = truncate_terminal_output(chunk.text.clone(), max_chars);
    if chunk.missed_bytes == 0 {
        return output;
    }

    let notice = format!(
        "[terminal output truncated: {} byte(s) dropped before this read]",
        chunk.missed_bytes
    );
    if output.is_empty() {
        notice
    } else {
        format!("{notice}\n{output}")
    }
}

fn terminal_output_metadata_lines(result: &TerminalToolResult) -> Vec<String> {
    if result.output_missed_bytes == 0 && result.output_dropped_bytes == 0 {
        return Vec::new();
    }
    vec![format!(
        "output_missed_bytes={} output_dropped_bytes={} output_retained_bytes={} output_buffer_capacity={}",
        result.output_missed_bytes,
        result.output_dropped_bytes,
        result.output_retained_bytes,
        result.output_buffer_capacity
    )]
}

fn summarize_terminal_inline_text(text: &str) -> String {
    const MAX_CHARS: usize = 120;
    let compact = text.replace('\n', "\\n");
    let mut chars = compact.chars();
    let summary = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{summary}...")
    } else {
        summary
    }
}

fn resolve_terminal_path(
    context: &AppToolExecutionContext,
    raw: &str,
    base: Option<&Path>,
) -> std::path::PathBuf {
    context.resolve_tool_path(Path::new(raw), base)
}

fn spawn_terminal_process(
    command: &str,
    workdir: Option<&str>,
    sandbox_policy: &RuntimeSandboxPolicy,
    output_buffer_capacity: usize,
) -> std::io::Result<TerminalProcess> {
    if output_buffer_capacity == DEFAULT_OUTPUT_BUFFER_CAPACITY_BYTES {
        TerminalProcess::spawn(command, workdir, sandbox_policy)
    } else {
        TerminalProcess::spawn_with_output_capacity(
            command,
            workdir,
            sandbox_policy,
            output_buffer_capacity,
        )
    }
}

fn command_mentions_protected_paths(context: &AppToolExecutionContext, text: &str) -> bool {
    if context.sandbox_policy.is_disabled() {
        return false;
    }
    let lowered = text.to_ascii_lowercase();
    if lowered.contains(".daat-locus") {
        return true;
    }
    context.sandbox_policy.protected_paths().iter().any(|root| {
        let rendered = root.display().to_string();
        !rendered.is_empty() && text.contains(&rendered)
    })
}

fn terminal_protection_error(label: &str) -> miette::Report {
    miette!("terminal access to protected runtime path is not allowed ({label})")
}

fn compact_terminal_model_content(
    summary: &str,
    session: &TerminalSessionState,
    output: &str,
    extra_lines: &[String],
    max_tokens: usize,
) -> String {
    let mut lines = vec![
        format!("summary={summary}"),
        format!("session={}", terminal_session_meta(session)),
    ];
    lines.extend(extra_lines.iter().cloned());
    if !output.trim().is_empty() {
        lines.push("output=".to_string());
        lines.push(truncate_terminal_output_for_model(output, max_tokens));
    }
    crate::context_budget::truncate_text_to_token_budget(&lines.join("\n"), max_tokens)
}

#[async_trait]
impl App for TerminalApp {
    fn id(&self) -> AppId {
        AppId::terminal()
    }

    fn render_state(&self) -> AppStateRender {
        let running_sessions = self
            .sessions
            .values()
            .filter(|session| session.state.status == "running")
            .count();
        let unread_session_ids = self
            .sessions
            .values()
            .filter(|session| session.state.has_unread_output)
            .map(|session| session.state.session_id.clone())
            .collect::<Vec<_>>();
        let mut lines = vec![
            "kind=terminal".to_string(),
            if unread_session_ids.is_empty() {
                "unread_sessions=none".to_string()
            } else {
                format!("unread_sessions={}", unread_session_ids.join(","))
            },
        ];

        if self.sessions.is_empty() {
            lines.push("sessions=none".to_string());
        } else {
            lines.push(format!("active_sessions={running_sessions}"));
            for session in self.sessions.values() {
                lines.push(render_session_state_line(&session.state));
            }
        }

        AppStateRender {
            title: "Terminal".to_string(),
            lines,
        }
    }

    fn usage(&self) -> AppUsage {
        AppUsage {
            description: TERMINAL_USAGE_PURPOSE.to_string(),
            when_to_focus: TERMINAL_WHEN_TO_FOCUS
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
            body_markdown: None,
        }
    }

    fn how_to_use(&self) -> AppHowToUse {
        AppHowToUse {
            lines: TERMINAL_HOW_TO_USE_LINES
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
            body_markdown: None,
        }
    }

    fn focused_tool_scopes(&self) -> &'static [AppToolScope] {
        &[AppToolScope::Terminal]
    }

    fn tool_specs(&self) -> Result<Vec<AppToolSpec>> {
        Ok(vec![
            AppToolSpec {
                name: "terminal_exec".to_string(),
                description: "Start a terminal command and return output after the current output window ends. If `session_id` is provided, reuse that session; otherwise create a new session. If the command is still running, the result keeps the session so later calls can continue with terminal_write_stdin.".to_string(),
                scope: AppToolScope::Terminal,
                input_schema: serde_json::to_value(schema_for!(TerminalExecArgs)).unwrap(),
            },
            AppToolSpec {
                name: "terminal_write_stdin".to_string(),
                description: "Continue a running terminal session. Send text to write stdin; send empty text to wait. By default `wait_mode=any_output` returns after new output, exit, or timeout; use `wait_mode=timeout` to wait for the full yield window or process exit without streaming intermediate progress.".to_string(),
                scope: AppToolScope::Terminal,
                input_schema: serde_json::to_value(schema_for!(TerminalWriteStdinArgs)).unwrap(),
            },
            AppToolSpec {
                name: "terminal_terminate".to_string(),
                description: "Terminate the current foreground process in the specified terminal session and return the updated session state.".to_string(),
                scope: AppToolScope::Terminal,
                input_schema: serde_json::to_value(schema_for!(TerminalTerminateArgs)).unwrap(),
            },
        ])
    }

    fn summarize_tool_call(&self, call: &AgentToolCall) -> Result<EpisodeActionRecord> {
        match call.name.as_str() {
            "terminal_exec" => {
                let args: TerminalExecArgs = parse_terminal_tool_args(call)?;
                Ok(EpisodeActionRecord {
                    kind: "terminal_exec".to_string(),
                    summary: format!(
                        "command={} session={} workdir={} yield_time_ms={}",
                        summarize_terminal_inline_text(&args.command),
                        args.session_id.unwrap_or_else(|| "new".to_string()),
                        args.workdir.unwrap_or_else(|| "none".to_string()),
                        args.yield_time_ms
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "default".to_string())
                    ),
                })
            }
            "terminal_write_stdin" => {
                let args: TerminalWriteStdinArgs = parse_terminal_tool_args(call)?;
                let wait_mode = args.wait_mode.unwrap_or(TerminalWaitMode::AnyOutput);
                Ok(EpisodeActionRecord {
                    kind: "terminal_write_stdin".to_string(),
                    summary: format!(
                        "session={} mode={} wait_mode={} yield_time_ms={}",
                        args.session_id,
                        terminal_progress_mode(&args.text),
                        terminal_wait_mode_label(wait_mode),
                        args.yield_time_ms
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "default".to_string())
                    ),
                })
            }
            "terminal_terminate" => {
                let args: TerminalTerminateArgs = parse_terminal_tool_args(call)?;
                Ok(EpisodeActionRecord {
                    kind: "terminal_terminate".to_string(),
                    summary: format!("session={}", args.session_id),
                })
            }
            _ => Err(miette!("unknown terminal tool `{}`", call.name)),
        }
    }

    fn render_tool_call_ui(&self, call: &AgentToolCall) -> Result<ToolCallUiEvent> {
        match call.name.as_str() {
            "terminal_exec" => {
                let args: TerminalExecArgs = parse_terminal_tool_args(call)?;
                Ok(ToolCallUiEvent::terminal(
                    TerminalUiAction::Execute,
                    summarize_terminal_inline_text(&args.command),
                    vec![format!(
                        "session={} workdir={} yield_time_ms={}",
                        args.session_id.unwrap_or_else(|| "new".to_string()),
                        args.workdir.unwrap_or_else(|| "-".to_string()),
                        args.yield_time_ms
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "default".to_string())
                    )],
                ))
            }
            "terminal_write_stdin" => {
                let args: TerminalWriteStdinArgs = parse_terminal_tool_args(call)?;
                let wait_mode = args.wait_mode.unwrap_or(TerminalWaitMode::AnyOutput);
                Ok(ToolCallUiEvent::terminal(
                    if args.text.is_empty() {
                        TerminalUiAction::Poll
                    } else {
                        TerminalUiAction::Continue
                    },
                    summarize_terminal_inline_text(&args.session_id),
                    vec![format!(
                        "wait_mode={} yield_time_ms={}",
                        terminal_wait_mode_label(wait_mode),
                        args.yield_time_ms
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "default".to_string())
                    )],
                ))
            }
            "terminal_terminate" => {
                let args: TerminalTerminateArgs = parse_terminal_tool_args(call)?;
                Ok(ToolCallUiEvent::terminal(
                    TerminalUiAction::Terminate,
                    format!("terminate {}", args.session_id),
                    Vec::new(),
                ))
            }
            _ => Err(miette!("unknown terminal tool `{}`", call.name)),
        }
    }

    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        match call.name.as_str() {
            "terminal_exec" => {
                let args: TerminalExecArgs = parse_terminal_tool_args(call)?;
                let effective_workdir = args
                    .workdir
                    .as_deref()
                    .map(|workdir| {
                        resolve_terminal_path(context, workdir, Some(&context.execution_cwd))
                    })
                    .unwrap_or_else(|| context.execution_cwd.clone());
                context
                    .sandbox_policy
                    .ensure_path_readable(&effective_workdir, "terminal workdir")
                    .map_err(|_| {
                        terminal_protection_error(&format!(
                            "workdir={}",
                            effective_workdir.display()
                        ))
                    })?;
                if command_mentions_protected_paths(context, &args.command) {
                    return Err(terminal_protection_error(
                        "command references protected path",
                    ));
                }
                let effective_workdir = args
                    .workdir
                    .clone()
                    .or_else(|| Some(context.execution_cwd.display().to_string()));
                let dashboard_tx = context.dashboard_tx.clone();
                let result = self
                    .exec_command_with_progress(
                        TerminalExecCommandRequest {
                            command: args.command.clone(),
                            session_id: args.session_id.clone(),
                            workdir: effective_workdir,
                            sandbox_policy: &context.sandbox_policy,
                            yield_time_ms: args.yield_time_ms,
                            max_chars: args.max_chars,
                        },
                        move |session, delta| {
                            if let Some(tx) = &dashboard_tx {
                                tx.send_modify(|state| {
                                    apply_activity_event(
                                        state,
                                        DashboardActivityEvent::ExecUpdate {
                                            key: call.id.clone(),
                                            meta: Some(terminal_session_meta(session)),
                                            output_lines: compact_body_lines(delta, 10),
                                        },
                                    );
                                });
                            }
                        },
                    )
                    .await?;
                let running = result.session.status == "running";
                let summary = if running {
                    format!(
                        "started `{}` in {}",
                        summarize_terminal_inline_text(
                            result.session.command.as_deref().unwrap_or(&args.command)
                        ),
                        display_session_label(&result.session.session_id)
                    )
                } else {
                    format!(
                        "ran `{}` in {}",
                        summarize_terminal_inline_text(
                            result.session.command.as_deref().unwrap_or(&args.command)
                        ),
                        display_session_label(&result.session.session_id)
                    )
                };
                let mut extra_lines = vec![
                    format!("running={running}"),
                    format!(
                        "yield_time_ms={}",
                        args.yield_time_ms
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "default".to_string())
                    ),
                ];
                extra_lines.extend(terminal_output_metadata_lines(&result));
                let model_content = compact_terminal_model_content(
                    &summary,
                    &result.session,
                    &result.output,
                    &extra_lines,
                    context.tool_output_max_tokens,
                );
                Ok(AppToolExecutionResult {
                    summary,
                    payload: json!({
                        "session": result.session,
                        "output": result.output,
                        "output_missed_bytes": result.output_missed_bytes,
                        "output_dropped_bytes": result.output_dropped_bytes,
                        "output_retained_bytes": result.output_retained_bytes,
                        "output_buffer_capacity": result.output_buffer_capacity,
                        "running": running,
                        "yield_time_ms": args.yield_time_ms,
                        "max_chars": args.max_chars,
                    }),
                    model_content: Some(model_content),
                    ui_event: ToolUiEvent::terminal(
                        if running {
                            TerminalUiAction::Execute
                        } else {
                            TerminalUiAction::Continue
                        },
                        summarize_terminal_inline_text(
                            result.session.command.as_deref().unwrap_or(&args.command),
                        ),
                        {
                            let mut body = vec![terminal_session_meta(&result.session)];
                            body.extend(terminal_output_metadata_lines(&result));
                            body.extend(compact_body_lines(&result.output, 10));
                            body
                        },
                    ),
                    turn_boundary_reason: None,
                })
            }
            "terminal_write_stdin" => {
                let args: TerminalWriteStdinArgs = parse_terminal_tool_args(call)?;
                let wait_mode = args.wait_mode.unwrap_or(TerminalWaitMode::AnyOutput);
                let session = self.session_state(&args.session_id)?;
                if let Some(cwd) = session.cwd.as_deref() {
                    let resolved_cwd = resolve_terminal_path(context, cwd, None);
                    context
                        .sandbox_policy
                        .ensure_path_readable(&resolved_cwd, "terminal session cwd")
                        .map_err(|_| {
                            terminal_protection_error(&format!(
                                "session cwd={}",
                                resolved_cwd.display()
                            ))
                        })?;
                }
                if command_mentions_protected_paths(context, &args.text) {
                    return Err(terminal_protection_error("stdin references protected path"));
                }
                let dashboard_tx = context.dashboard_tx.clone();
                let result = self
                    .write_stdin_with_progress(
                        &args.session_id,
                        args.text.clone(),
                        wait_mode,
                        args.yield_time_ms,
                        args.max_chars,
                        move |session, delta| {
                            if let Some(tx) = &dashboard_tx {
                                tx.send_modify(|state| {
                                    apply_activity_event(
                                        state,
                                        DashboardActivityEvent::ExecUpdate {
                                            key: call.id.clone(),
                                            meta: Some(terminal_session_meta(session)),
                                            output_lines: compact_body_lines(delta, 10),
                                        },
                                    );
                                });
                            }
                        },
                    )
                    .await?;
                let mode = terminal_progress_mode(&args.text);
                let wait_mode_label = terminal_wait_mode_label(wait_mode);
                let running = result.session.status == "running";
                let command_label = summarize_terminal_inline_text(
                    result
                        .session
                        .command
                        .as_deref()
                        .unwrap_or(&args.session_id),
                );
                let summary = match (mode, running) {
                    ("poll", true) => {
                        format!(
                            "continued {}",
                            display_session_label(&result.session.session_id)
                        )
                    }
                    ("poll", false) => {
                        format!(
                            "completed {}",
                            display_session_label(&result.session.session_id)
                        )
                    }
                    ("continue", true) => format!(
                        "continued {} with stdin",
                        display_session_label(&result.session.session_id)
                    ),
                    ("continue", false) => format!(
                        "completed {} after stdin",
                        display_session_label(&result.session.session_id)
                    ),
                    _ => format!(
                        "continued {}",
                        display_session_label(&result.session.session_id)
                    ),
                };
                let mut extra_lines = vec![
                    format!("mode={mode}"),
                    format!("wait_mode={wait_mode_label}"),
                    format!("running={running}"),
                    format!(
                        "yield_time_ms={}",
                        args.yield_time_ms
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "default".to_string())
                    ),
                ];
                if !args.text.is_empty() {
                    extra_lines.push(format!(
                        "stdin={}",
                        summarize_terminal_inline_text(&args.text)
                    ));
                }
                extra_lines.extend(terminal_output_metadata_lines(&result));
                let model_content = compact_terminal_model_content(
                    &summary,
                    &result.session,
                    &result.output,
                    &extra_lines,
                    context.tool_output_max_tokens,
                );
                Ok(AppToolExecutionResult {
                    summary,
                    payload: json!({
                        "session": result.session,
                        "output": result.output,
                        "output_missed_bytes": result.output_missed_bytes,
                        "output_dropped_bytes": result.output_dropped_bytes,
                        "output_retained_bytes": result.output_retained_bytes,
                        "output_buffer_capacity": result.output_buffer_capacity,
                        "running": running,
                        "mode": mode,
                        "wait_mode": wait_mode_label,
                        "yield_time_ms": args.yield_time_ms,
                        "max_chars": args.max_chars,
                    }),
                    model_content: Some(model_content),
                    ui_event: ToolUiEvent::terminal(
                        if running {
                            if args.text.is_empty() {
                                TerminalUiAction::Poll
                            } else {
                                TerminalUiAction::Continue
                            }
                        } else {
                            TerminalUiAction::Continue
                        },
                        command_label,
                        {
                            let mut body = vec![terminal_session_meta(&result.session)];
                            body.extend(terminal_output_metadata_lines(&result));
                            body.extend(compact_body_lines(&result.output, 10));
                            body
                        },
                    ),
                    turn_boundary_reason: None,
                })
            }
            "terminal_terminate" => {
                let args: TerminalTerminateArgs = parse_terminal_tool_args(call)?;
                let session = self.terminate_session(&args.session_id).await?;
                Ok(AppToolExecutionResult {
                    summary: format!("terminated {}", display_session_label(&session.session_id)),
                    payload: json!({ "session": session }),
                    model_content: None,
                    ui_event: ToolUiEvent::terminal(
                        TerminalUiAction::Terminate,
                        summarize_terminal_inline_text(
                            session.command.as_deref().unwrap_or(&args.session_id),
                        ),
                        vec![terminal_session_meta(&session)],
                    ),
                    turn_boundary_reason: None,
                })
            }
            _ => Err(miette!("unknown terminal tool `{}`", call.name)),
        }
    }

    fn notice_reason(&self) -> Option<String> {
        let unread_count = self
            .sessions
            .values()
            .filter(|session| session.state.has_unread_output)
            .count();
        if unread_count > 0 {
            Some(format!(
                "{unread_count} terminal session(s) have unread output"
            ))
        } else {
            None
        }
    }

    async fn wait_until_settled(&self, silence_duration: Duration, timeout: Duration) -> bool {
        let Some(session) = self
            .sessions
            .values()
            .find(|session| session.state.has_unread_output)
            .or_else(|| {
                self.sessions
                    .values()
                    .find(|session| session.state.status == "running")
            })
            .or_else(|| self.sessions.values().next())
        else {
            return true;
        };
        match session.process.as_ref() {
            Some(process) => process.wait_until_silent(silence_duration, timeout).await,
            None => true,
        }
    }
}

fn refresh_terminal_session(session: &mut TerminalSession) {
    session.state.process_id = session
        .process
        .as_ref()
        .and_then(|process| process.process_id());
    let mut process_running = session.process.is_some();
    if let Some(process) = session.process.as_mut() {
        match process.try_wait() {
            Ok(Some(exit_status)) => {
                session.state.exit_code = exit_status.code();
                session.state.has_unread_output = true;
                session.state.status = "exited".to_string();
                process_running = false;
            }
            Ok(None) => {
                process_running = true;
            }
            Err(_) => {}
        }
    }
    let output_tail = session
        .process
        .as_ref()
        .map(|process| process.output_tail(800))
        .unwrap_or_default();
    if let Some(process) = session.process.as_ref() {
        apply_terminal_output_stats(&mut session.state, process.output_stats());
    }
    session.state.last_output_preview = summarize_terminal_preview(&output_tail);
    session.state.has_unread_output = session
        .process
        .as_ref()
        .map(|process| process.output_len() > session.output_offset)
        .unwrap_or(false);
    if !session.state.status.starts_with("exited") {
        session.state.status = if process_running {
            "running".to_string()
        } else {
            "idle".to_string()
        };
    }
}

fn render_session_state_line(state: &TerminalSessionState) -> String {
    format!(
        "session={} status={} pid={} exit={} cwd={} unread={} output_total_bytes={} output_retained_bytes={} output_dropped_bytes={} output_buffer_capacity={} command={} preview={}",
        state.session_id,
        state.status,
        state
            .process_id
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        state
            .exit_code
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        state.cwd.as_deref().unwrap_or("unknown"),
        state.has_unread_output,
        state.output_total_written_bytes,
        state.output_retained_bytes,
        state.output_dropped_bytes,
        state.output_buffer_capacity,
        state.command.as_deref().unwrap_or("<none>"),
        state.last_output_preview
    )
}

fn summarize_terminal_preview(screen: &str) -> String {
    screen
        .lines()
        .rev()
        .find_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.chars().take(120).collect::<String>())
            }
        })
        .unwrap_or_default()
}

fn truncate_terminal_output(content: String, max_chars: Option<usize>) -> String {
    truncate_terminal_output_tail(content, max_chars, "terminal output shortened by max_chars")
}

fn truncate_terminal_output_for_model(content: &str, max_tokens: usize) -> String {
    let output_token_budget = max_tokens.saturating_sub(128).max(1);
    let max_chars = output_token_budget
        .saturating_mul(crate::context_budget::APPROX_BYTES_PER_TOKEN)
        .max(1);
    truncate_terminal_output_tail(
        content.to_string(),
        Some(max_chars),
        "terminal output shortened for model context",
    )
}

fn truncate_terminal_output_tail(
    content: String,
    max_chars: Option<usize>,
    reason: &str,
) -> String {
    if let Some(limit) = max_chars
        && content.chars().count() > limit
    {
        let chars = content.chars().collect::<Vec<_>>();
        let omitted = chars.len().saturating_sub(limit);
        let tail = chars[chars.len().saturating_sub(limit)..]
            .iter()
            .collect::<String>();
        let notice = format!("[{reason}: {omitted} char(s) omitted; showing tail]");
        return if tail.is_empty() {
            notice
        } else {
            format!("{notice}\n{tail}")
        };
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env,
        time::{Duration, Instant},
    };

    use crate::sandbox::{
        FileSystemSandboxPolicy, RuntimeSandboxPolicy, StrongFilesystemSandboxMode,
    };

    struct EnvOverride {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvOverride {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var(key).ok();
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvOverride {
        fn drop(&mut self) {
            match &self.previous {
                Some(previous) => unsafe {
                    env::set_var(self.key, previous);
                },
                None => unsafe {
                    env::remove_var(self.key);
                },
            }
        }
    }

    fn test_sandbox_policy() -> RuntimeSandboxPolicy {
        RuntimeSandboxPolicy {
            filesystem: FileSystemSandboxPolicy {
                full_disk_read: true,
                full_disk_write: true,
                readable_roots: Vec::new(),
                writable_roots: Vec::new(),
                deny_read_paths: Vec::new(),
                deny_write_paths: Vec::new(),
            },
            protected_env_vars: Vec::new(),
            strong_filesystem: StrongFilesystemSandboxMode::Off,
        }
    }

    fn echo_command(text: &str) -> String {
        if cfg!(windows) {
            format!("Write-Output '{text}'")
        } else {
            format!("printf '%s\\n' '{text}'")
        }
    }

    fn long_running_command() -> String {
        if cfg!(windows) {
            "Start-Sleep -Seconds 30".to_string()
        } else {
            "sleep 30".to_string()
        }
    }

    fn high_output_command(byte_count: usize) -> String {
        if cfg!(windows) {
            format!("[Console]::Out.Write(('x' * {byte_count}))")
        } else {
            format!("printf '%{byte_count}s' '' | tr ' ' x")
        }
    }

    fn delayed_output_command(text: &str) -> String {
        if cfg!(windows) {
            format!("Start-Sleep -Milliseconds 100; Write-Output '{text}'")
        } else {
            format!("sleep 0.1; printf '%s\\n' '{text}'")
        }
    }

    fn delayed_output_then_sleep_command(text: &str) -> String {
        if cfg!(windows) {
            format!("Start-Sleep -Milliseconds 100; Write-Output '{text}'; Start-Sleep -Seconds 2")
        } else {
            format!("sleep 0.1; printf '%s\\n' '{text}'; sleep 2")
        }
    }

    fn env_value_command(name: &str) -> String {
        if cfg!(windows) {
            format!(
                "if ($env:{name}) {{ [Console]::Out.Write($env:{name}) }} else {{ [Console]::Out.Write('missing') }}"
            )
        } else {
            format!("printf '%s' \"${{{name}-missing}}\"")
        }
    }

    #[tokio::test]
    async fn creates_new_sessions_and_lists_them() {
        let mut app = TerminalApp::new();
        let sandbox_policy = test_sandbox_policy();

        let created = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: echo_command("session-a"),
                    session_id: None,
                    workdir: None,
                    sandbox_policy: &sandbox_policy,
                    yield_time_ms: None,
                    max_chars: None,
                },
                |_session, _delta| {},
            )
            .await
            .expect("create new session should succeed");

        app.refresh_all_sessions();
        let sessions = app
            .sessions
            .values()
            .map(|session| session.state.clone())
            .collect::<Vec<_>>();
        assert!(
            sessions
                .iter()
                .any(|session| session.session_id == created.session.session_id)
        );
    }

    #[tokio::test]
    async fn terminate_removes_non_main_session() {
        let mut app = TerminalApp::new();
        let sandbox_policy = test_sandbox_policy();

        let created = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: long_running_command(),
                    session_id: None,
                    workdir: None,
                    sandbox_policy: &sandbox_policy,
                    yield_time_ms: None,
                    max_chars: None,
                },
                |_session, _delta| {},
            )
            .await
            .expect("create long-running session should succeed");

        let terminated = app
            .terminate_session(&created.session.session_id)
            .await
            .expect("terminate should succeed");
        assert_eq!(terminated.session_id, created.session.session_id);

        app.refresh_all_sessions();
        let sessions = app
            .sessions
            .values()
            .map(|session| session.state.clone())
            .collect::<Vec<_>>();
        assert!(
            sessions
                .iter()
                .all(|session| session.session_id != created.session.session_id)
        );
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn focus_clears_when_last_session_is_terminated() {
        let mut app = TerminalApp::new();
        let sandbox_policy = test_sandbox_policy();

        let created = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: long_running_command(),
                    session_id: None,
                    workdir: None,
                    sandbox_policy: &sandbox_policy,
                    yield_time_ms: None,
                    max_chars: None,
                },
                |_session, _delta| {},
            )
            .await
            .expect("create long-running session should succeed");
        app.terminate_session(&created.session.session_id)
            .await
            .expect("terminate should succeed");
        assert!(app.sessions.is_empty());
    }

    #[tokio::test]
    async fn prunes_old_exited_non_main_sessions() {
        let mut app = TerminalApp::new();
        let sandbox_policy = test_sandbox_policy();

        for idx in 0..(TerminalApp::MAX_EXITED_SESSION_TOMBSTONES + 2) {
            let created = app
                .exec_command_with_progress(
                    TerminalExecCommandRequest {
                        command: echo_command(&format!("session-{idx}")),
                        session_id: None,
                        workdir: None,
                        sandbox_policy: &sandbox_policy,
                        yield_time_ms: None,
                        max_chars: None,
                    },
                    |_session, _delta| {},
                )
                .await
                .expect("create short-lived session should succeed");
            tokio::time::sleep(Duration::from_millis(5)).await;
            app.refresh_all_sessions();
            let _ = created;
        }

        app.refresh_all_sessions();
        let sessions = app
            .sessions
            .values()
            .map(|session| session.state.clone())
            .collect::<Vec<_>>();
        let exited_sessions = sessions
            .iter()
            .filter(|session| session.status.starts_with("exited"))
            .count();
        assert!(exited_sessions <= TerminalApp::MAX_EXITED_SESSION_TOMBSTONES);
    }

    #[tokio::test]
    async fn exec_returns_running_session_when_yield_window_ends_first() {
        let mut app = TerminalApp::new();
        let sandbox_policy = test_sandbox_policy();

        let result = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: long_running_command(),
                    session_id: None,
                    workdir: None,
                    sandbox_policy: &sandbox_policy,
                    yield_time_ms: Some(100),
                    max_chars: None,
                },
                |_session, _delta| {},
            )
            .await
            .expect("exec should succeed");

        assert!(
            result.session.status == "running" || result.session.status.starts_with("exited"),
            "unexpected session status: {}",
            result.session.status
        );
        if result.session.status == "running" {
            assert!(result.session.exit_code.is_none());
            assert!(result.output.trim().is_empty());
        }
    }

    #[tokio::test]
    async fn high_output_command_reports_bounded_buffer_truncation() {
        let mut app = TerminalApp::new_with_output_buffer_capacity(1024);
        let sandbox_policy = test_sandbox_policy();

        let result = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: high_output_command(4096),
                    session_id: None,
                    workdir: None,
                    sandbox_policy: &sandbox_policy,
                    yield_time_ms: Some(5_000),
                    max_chars: None,
                },
                |_session, _delta| {},
            )
            .await
            .expect("high-output command should succeed");

        assert!(
            result.output_missed_bytes > 0,
            "expected stale read to report missed bytes"
        );
        assert!(
            result.output_dropped_bytes > 0,
            "expected bounded buffer to drop old bytes"
        );
        assert_eq!(result.output_buffer_capacity, 1024);
        assert!(
            result.output.contains("[terminal output truncated:"),
            "expected output truncation notice, got {:?}",
            result.output
        );
        assert!(
            result.output.chars().filter(|ch| *ch == 'x').count() <= 1024,
            "output should retain only the bounded tail"
        );
    }

    #[tokio::test]
    async fn terminal_exec_strips_protected_env_vars() {
        let _env = EnvOverride::set("DAAT_LOCUS_TEST_ENV", "super-secret-value");
        let mut app = TerminalApp::new();
        let mut sandbox_policy = test_sandbox_policy();
        sandbox_policy
            .protected_env_vars
            .push("DAAT_LOCUS_TEST_ENV".to_string());

        let result = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: env_value_command("DAAT_LOCUS_TEST_ENV"),
                    session_id: None,
                    workdir: None,
                    sandbox_policy: &sandbox_policy,
                    yield_time_ms: Some(5_000),
                    max_chars: None,
                },
                |_session, _delta| {},
            )
            .await
            .expect("env check command should run");

        assert!(
            result.output.contains("missing"),
            "protected env var should be removed from terminal child process: {:?}",
            result.output
        );
        assert!(!result.output.contains("super-secret-value"));
    }

    #[tokio::test]
    async fn empty_stdin_poll_continues_running_session_until_output_arrives() {
        let mut app = TerminalApp::new();
        let sandbox_policy = test_sandbox_policy();

        let started = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: if cfg!(windows) {
                        "powershell.exe".to_string()
                    } else {
                        "bash".to_string()
                    },
                    session_id: None,
                    workdir: None,
                    sandbox_policy: &sandbox_policy,
                    yield_time_ms: Some(50),
                    max_chars: None,
                },
                |_session, _delta| {},
            )
            .await
            .expect("exec should succeed");

        assert!(
            started.session.status == "running" || started.session.status.starts_with("exited"),
            "unexpected start status: {}",
            started.session.status
        );

        if started.session.status == "running" {
            let input = if cfg!(windows) {
                "Write-Output 'continued-output'\nexit\n".to_string()
            } else {
                "echo continued-output\nexit\n".to_string()
            };

            let polled = app
                .write_stdin_with_progress(
                    &started.session.session_id,
                    input,
                    TerminalWaitMode::AnyOutput,
                    Some(1000),
                    None,
                    |_session, _delta| {},
                )
                .await
                .expect("stdin write should succeed");

            assert!(
                polled.session.status == "running" || polled.session.status.starts_with("exited"),
                "unexpected polled status: {}",
                polled.session.status
            );
        }
    }

    #[tokio::test]
    async fn any_output_wait_mode_returns_after_output_update() {
        let mut app = TerminalApp::new();
        let sandbox_policy = test_sandbox_policy();

        let started = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: if cfg!(windows) {
                        "powershell.exe".to_string()
                    } else {
                        "bash".to_string()
                    },
                    session_id: None,
                    workdir: None,
                    sandbox_policy: &sandbox_policy,
                    yield_time_ms: Some(50),
                    max_chars: None,
                },
                |_session, _delta| {},
            )
            .await
            .expect("exec should succeed");

        if started.session.status == "running" {
            let input = format!("{}\n", delayed_output_then_sleep_command("any-output-mode"));
            let started_at = Instant::now();
            let result = app
                .write_stdin_with_progress(
                    &started.session.session_id,
                    input,
                    TerminalWaitMode::AnyOutput,
                    Some(5_000),
                    None,
                    |_session, _delta| {},
                )
                .await
                .expect("any_output wait should succeed");

            assert!(
                result.output.contains("any-output-mode"),
                "expected output update before timeout: {:?}",
                result.output
            );
            assert!(
                result.session.status == "running",
                "process should still be running after first output update: {}",
                result.session.status
            );
            assert!(
                started_at.elapsed() < Duration::from_secs(2),
                "any_output mode should return before the full command exits"
            );
            app.terminate_session(&result.session.session_id)
                .await
                .expect("terminate should succeed");
        }
    }

    #[tokio::test]
    async fn timeout_wait_mode_suppresses_progress_until_timeout() {
        let mut app = TerminalApp::new();
        let sandbox_policy = test_sandbox_policy();

        let started = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: if cfg!(windows) {
                        "powershell.exe".to_string()
                    } else {
                        "bash".to_string()
                    },
                    session_id: None,
                    workdir: None,
                    sandbox_policy: &sandbox_policy,
                    yield_time_ms: Some(50),
                    max_chars: None,
                },
                |_session, _delta| {},
            )
            .await
            .expect("exec should succeed");

        if started.session.status == "running" {
            let input = format!("{}\n", delayed_output_command("timeout-mode"));
            let started_at = Instant::now();
            let mut progress_calls = 0usize;
            let waited = app
                .write_stdin_with_progress(
                    &started.session.session_id,
                    input,
                    TerminalWaitMode::Timeout,
                    Some(400),
                    None,
                    |_session, _delta| {
                        progress_calls += 1;
                    },
                )
                .await
                .expect("timeout wait should succeed");

            assert_eq!(progress_calls, 0, "timeout wait must not stream progress");
            assert!(
                started_at.elapsed() >= Duration::from_millis(350),
                "timeout wait should wait for the requested yield window"
            );
            assert!(
                waited.output.contains("timeout-mode"),
                "final output should still include data produced while waiting: {:?}",
                waited.output
            );
            app.terminate_session(&waited.session.session_id)
                .await
                .expect("terminate should succeed");
        }
    }
}
