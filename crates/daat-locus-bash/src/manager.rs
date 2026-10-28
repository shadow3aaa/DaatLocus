use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use crate::process::{
    DEFAULT_OUTPUT_BUFFER_CAPACITY_BYTES, TerminalOutputChunk, TerminalOutputStats, TerminalProcess,
};
use miette::{Result, bail, miette};
use serde::{Deserialize, Serialize};

use crate::{ProcessPolicy, TerminalWaitMode};

pub struct TerminalManager<P: ProcessPolicy> {
    sessions: BTreeMap<String, TerminalSession<P>>,
    output_buffer_capacity: usize,
}

pub struct TerminalExecCommandRequest<'a, P: ProcessPolicy> {
    pub command: String,
    pub session_id: Option<String>,
    pub workdir: Option<String>,
    pub sandbox_policy: &'a P,
    pub yield_time_ms: Option<u64>,
    pub max_chars: Option<usize>,
}

struct TerminalSession<P: ProcessPolicy> {
    process: Option<TerminalProcess<P>>,
    output_offset: usize,
    last_activity: Instant,
    state: TerminalSessionState,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TerminalToolResult {
    pub session: TerminalSessionState,
    #[schemars(extend("x-dl-content" = "text"))]
    pub output: String,
    pub output_missed_bytes: usize,
    pub output_dropped_bytes: usize,
    pub output_retained_bytes: usize,
    pub output_buffer_capacity: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TerminalSessionState {
    #[schemars(skip)]
    pub session_id: String,
    #[schemars(skip)]
    pub process_id: Option<u32>,
    pub command: Option<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub cwd: Option<String>,
    pub has_unread_output: bool,
    #[serde(default)]
    pub started_at_ms: Option<i64>,
    #[serde(default)]
    pub finished_at_ms: Option<i64>,
    #[serde(default)]
    pub output_total_written_bytes: usize,
    #[serde(default)]
    pub output_retained_bytes: usize,
    #[serde(default)]
    pub output_dropped_bytes: usize,
    #[serde(default)]
    pub output_buffer_capacity: usize,
}

impl<P: ProcessPolicy> Default for TerminalManager<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: ProcessPolicy> TerminalManager<P> {
    const DEFAULT_EXEC_YIELD_TIME_MS: u64 = 10_000;
    const DEFAULT_WRITE_STDIN_YIELD_TIME_MS: u64 = 250;
    const DEFAULT_WAIT_YIELD_TIME_MS: u64 = 30_000;
    const MIN_EMPTY_POLL_YIELD_TIME_MS: u64 = 5_000;
    const MAX_WRITE_STDIN_YIELD_TIME_MS: u64 = 30_000;
    const MAX_WAIT_YIELD_TIME_MS: u64 = 120_000;
    const TRAILING_OUTPUT_DRAIN_GRACE_MS: u64 = 100;
    const FAST_EXIT_OBSERVATION_GRACE_MS: u64 = 500;
    pub const MAX_EXITED_SESSION_TOMBSTONES: usize = 4;

    pub const fn new() -> Self {
        Self {
            sessions: BTreeMap::new(),
            output_buffer_capacity: DEFAULT_OUTPUT_BUFFER_CAPACITY_BYTES,
        }
    }

    pub const fn new_with_output_buffer_capacity(output_buffer_capacity: usize) -> Self {
        Self {
            sessions: BTreeMap::new(),
            output_buffer_capacity,
        }
    }

    pub fn session_state(&self, session_id: &str) -> Result<TerminalSessionState> {
        self.sessions
            .get(session_id)
            .map(|session| session.state.clone())
            .ok_or_else(|| miette!("unknown terminal session `{session_id}`"))
    }

    pub fn cwd_from_shell_input(text: &str) -> Option<String> {
        let trimmed = text.trim();
        let words = tokenize_shell_words(trimmed)?;
        if words.len() == 3 && words[0] == "cd" && words[1] == "--" {
            Some(words[2].clone())
        } else {
            None
        }
    }

    pub async fn exec_command_with_progress<F>(
        &mut self,
        request: TerminalExecCommandRequest<'_, P>,
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

        let target_session_id = match session_id {
            Some(id) if !id.trim().is_empty() && id.trim().to_lowercase() != "null" => id,
            _ => self.create_session(),
        };
        let output_buffer_capacity = self.output_buffer_capacity;
        let effective_workdir = workdir;
        let session = self.session_mut(&target_session_id).map_err(|_| {
            miette!(
                "terminal session `{target_session_id}` does not exist; \
                 pass JSON null for `session_id` to create a new session"
            )
        })?;
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
            .await
            .map_err(|err| miette!("failed to spawn terminal process: {err}"))?,
        );

        session.output_offset = 0;
        session.state.command = Some(command);
        session.state.status = "running".to_string();
        session.state.exit_code = None;
        session.state.started_at_ms = Some(chrono::Utc::now().timestamp_millis());
        session.state.finished_at_ms = None;
        session.state.output_buffer_capacity = output_buffer_capacity;
        if let Some(workdir) = effective_workdir.clone() {
            session.state.cwd = Some(workdir);
        }
        session.state.has_unread_output = true;
        let start_offset = session.output_offset;
        let mut progress_offset = start_offset;
        session.last_activity = Instant::now();
        let timeout = Duration::from_millis(Self::initial_exec_yield_time_ms(yield_time_ms));
        let started_at = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            refresh_terminal_session(session);
            let chunk = session.process.as_ref().map_or_else(
                || empty_terminal_output_chunk(progress_offset, &session.state),
                |process| process.output_since(progress_offset),
            );
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
        wait_for_fast_exit_after_short_initial_yield(
            session,
            start_offset,
            timeout,
            Duration::from_millis(Self::FAST_EXIT_OBSERVATION_GRACE_MS),
        )
        .await;
        wait_for_trailing_terminal_output(session).await;
        refresh_terminal_session(session);
        let chunk = session.process.as_ref().map_or_else(
            || empty_terminal_output_chunk(start_offset, &session.state),
            |process| process.output_since(start_offset),
        );
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
        let session = self.session_mut(session_id)?;
        let start_offset = session.output_offset;
        let Some(process) = session.process.as_mut() else {
            bail!("terminal session `{session_id}` has no running process");
        };
        if let Some(updated_cwd) = Self::cwd_from_shell_input(&text) {
            session.state.cwd = Some(updated_cwd);
        }
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
            let chunk = session.process.as_ref().map_or_else(
                || empty_terminal_output_chunk(progress_offset, &session.state),
                |process| process.output_since(progress_offset),
            );
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
        wait_for_trailing_terminal_output(session).await;
        refresh_terminal_session(session);
        let chunk = session.process.as_ref().map_or_else(
            || empty_terminal_output_chunk(start_offset, &session.state),
            |process| process.output_since(start_offset),
        );
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

    pub fn initial_exec_yield_time_ms(yield_time_ms: Option<u64>) -> u64 {
        yield_time_ms.unwrap_or(Self::DEFAULT_EXEC_YIELD_TIME_MS)
    }

    pub fn terminate_session(&mut self, session_id: &str) -> Result<TerminalSessionState> {
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

    fn session_mut(&mut self, session_id: &str) -> Result<&mut TerminalSession<P>> {
        self.sessions
            .get_mut(session_id)
            .ok_or_else(|| miette::miette!("unknown terminal session `{session_id}`"))
    }

    pub fn refresh_all_sessions(&mut self) {
        for session in self.sessions.values_mut() {
            refresh_terminal_session(session);
        }
        self.prune_exited_sessions();
    }

    pub fn create_session(&mut self) -> String {
        // Instance reconstruction must never let an old reference target a
        // different process, including after idle passivation or restart.
        let session_id = format!("terminal-session-{}", uuid::Uuid::new_v4());
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
                started_at_ms: None,
                finished_at_ms: None,
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

impl<P: ProcessPolicy> TerminalManager<P> {
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
    pub fn len(&self) -> usize {
        self.sessions.len()
    }
    pub fn session_states(&self) -> Vec<TerminalSessionState> {
        self.sessions
            .values()
            .map(|session| session.state.clone())
            .collect()
    }
    pub fn ensure_policy(&self, id: &str, policy: &P) -> Result<()> {
        self.sessions
            .get(id)
            .ok_or_else(|| miette!("unknown terminal session"))?
            .process
            .as_ref()
            .ok_or_else(|| miette!("Terminal process is unavailable"))?
            .ensure_policy(policy)
    }
    pub fn take_denial_report(&mut self, id: &str) -> Option<daat_locus_sandbox::DenialReport> {
        self.sessions
            .get_mut(id)?
            .process
            .as_mut()
            .map(TerminalProcess::take_denial_report)
    }
    pub fn shutdown(&mut self) {
        self.sessions.clear();
    }
}
const fn apply_terminal_output_stats(
    session: &mut TerminalSessionState,
    stats: TerminalOutputStats,
) {
    session.output_total_written_bytes = stats.total_written_bytes;
    session.output_retained_bytes = stats.retained_bytes;
    session.output_dropped_bytes = stats.dropped_bytes;
    session.output_buffer_capacity = stats.buffer_capacity;
}

const fn empty_terminal_output_chunk(
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

async fn spawn_terminal_process<P: ProcessPolicy>(
    command: &str,
    workdir: Option<&str>,
    sandbox_policy: &P,
    output_buffer_capacity: usize,
) -> std::io::Result<TerminalProcess<P>> {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct CancelStartup(Arc<AtomicBool>);
    impl Drop for CancelStartup {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel_on_drop = CancelStartup(cancelled.clone());
    let command = command.to_owned();
    let workdir = workdir.map(str::to_owned);
    let policy = sandbox_policy.clone();
    tokio::task::spawn_blocking(move || {
        TerminalProcess::spawn_cancellable(
            &command,
            workdir.as_deref(),
            &policy,
            output_buffer_capacity,
            Some(cancelled),
        )
    })
    .await
    .map_err(std::io::Error::other)?
}

/// POSIX-like words. `None` means an open quote or trailing backslash.
pub fn tokenize_shell_words(input: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_word = false;
    let mut quote: Option<char> = None;

    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    current.push(ch);
                }
                in_word = true;
            }
            Some('"') => {
                if ch == '\\' {
                    match chars.next() {
                        Some(escaped @ ('"' | '\\' | '$' | '`')) => current.push(escaped),
                        Some('\n') => {}
                        Some(other) => {
                            current.push('\\');
                            current.push(other);
                        }
                        None => return None,
                    }
                    in_word = true;
                } else if ch == '"' {
                    quote = None;
                    in_word = true;
                } else {
                    current.push(ch);
                    in_word = true;
                }
            }
            None if ch == '\\' => match chars.next() {
                Some('\n') => {}
                Some(escaped) => {
                    current.push(escaped);
                    in_word = true;
                }
                None => return None,
            },
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                in_word = true;
            }
            None if ch.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            None => {
                current.push(ch);
                in_word = true;
            }
            Some(_) => unreachable!(),
        }
    }

    if quote.is_some() {
        return None;
    }
    if in_word {
        words.push(current);
    }
    Some(words)
}

fn refresh_terminal_session<P: ProcessPolicy>(session: &mut TerminalSession<P>) {
    session.state.process_id = session
        .process
        .as_ref()
        .and_then(TerminalProcess::process_id);
    let mut process_running = session.process.is_some();
    if let Some(process) = session.process.as_mut() {
        match process.try_wait() {
            Ok(Some(exit_status)) => {
                session.state.exit_code = exit_status.code();
                session.state.has_unread_output = true;
                session.state.status = "exited".to_string();
                session
                    .state
                    .finished_at_ms
                    .get_or_insert_with(|| chrono::Utc::now().timestamp_millis());
                process_running = false;
            }
            Ok(None) => {
                process_running = true;
            }
            Err(_) => {}
        }
    }

    if let Some(process) = session.process.as_ref() {
        apply_terminal_output_stats(&mut session.state, process.output_stats());
    }
    session.state.has_unread_output = session
        .process
        .as_ref()
        .is_some_and(|process| process.output_len() > session.output_offset);
    if !session.state.status.starts_with("exited") {
        session.state.status = if process_running {
            "running".to_string()
        } else {
            "idle".to_string()
        };
    }
}

async fn wait_for_fast_exit_after_short_initial_yield<P: ProcessPolicy>(
    session: &mut TerminalSession<P>,
    start_offset: usize,
    initial_yield: Duration,
    grace: Duration,
) {
    if initial_yield > grace || session.state.status != "running" {
        return;
    }
    let Some(process) = session.process.as_ref() else {
        return;
    };
    if process.output_len() > start_offset {
        return;
    }

    let started_at = Instant::now();
    loop {
        refresh_terminal_session(session);
        if session.state.status != "running" {
            return;
        }
        let Some(process) = session.process.as_ref() else {
            return;
        };
        if process.output_len() > start_offset || started_at.elapsed() >= grace {
            return;
        }
        let remaining = grace.saturating_sub(started_at.elapsed());
        tokio::time::sleep(remaining.min(Duration::from_millis(10))).await;
    }
}

async fn wait_for_trailing_terminal_output<P: ProcessPolicy>(session: &TerminalSession<P>) {
    if session.state.status == "running" {
        return;
    }
    let Some(process) = session.process.as_ref() else {
        return;
    };
    let _ = process
        .wait_for_output_drained(Duration::from_millis(
            TerminalManager::<P>::TRAILING_OUTPUT_DRAIN_GRACE_MS,
        ))
        .await;
}

fn truncate_terminal_output(content: String, max_chars: Option<usize>) -> String {
    truncate_terminal_output_tail(content, max_chars, "terminal output shortened by max_chars")
}

pub fn truncate_terminal_output_tail(
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
