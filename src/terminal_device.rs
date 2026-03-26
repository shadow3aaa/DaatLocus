use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use miette::{Result, bail, miette};
use serde::{Deserialize, Serialize};

use crate::{
    device::{AttentionLevel, Device, DeviceId, DeviceStateRender, DeviceToolScope},
    terminal_process::TerminalProcess,
};

pub struct TerminalDevice {
    sessions: BTreeMap<String, TerminalSession>,
    focused_session_id: Option<String>,
    next_session_index: usize,
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
}

impl TerminalDevice {
    const DEFAULT_EXEC_YIELD_TIME_MS: u64 = 10_000;
    const DEFAULT_WRITE_STDIN_YIELD_TIME_MS: u64 = 250;
    const MIN_EMPTY_POLL_YIELD_TIME_MS: u64 = 5_000;
    const MAX_WRITE_STDIN_YIELD_TIME_MS: u64 = 30_000;
    const MAX_EXITED_SESSION_TOMBSTONES: usize = 4;

    pub fn new() -> Self {
        Self {
            sessions: BTreeMap::new(),
            focused_session_id: None,
            next_session_index: 1,
        }
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
        command: String,
        session_id: Option<String>,
        create_new_session: bool,
        workdir: Option<String>,
        yield_time_ms: Option<u64>,
        max_chars: Option<usize>,
        mut on_progress: F,
    ) -> Result<TerminalToolResult>
    where
        F: FnMut(&TerminalSessionState, &str) + Send,
    {
        let target_session_id = if create_new_session {
            self.create_session()
        } else {
            session_id
                .or_else(|| self.focused_session_id.clone())
                .unwrap_or_else(|| self.create_session())
        };
        if let Some(reason) = Self::forbidden_input_reason(&command) {
            bail!(reason);
        }
        let effective_workdir = workdir;
        let session = self.session_mut(&target_session_id)?;
        if session.state.status == "running" {
            bail!("terminal session `{target_session_id}` already has a running process");
        }
        session.process = Some(
            TerminalProcess::spawn(&command, effective_workdir.as_deref())
                .map_err(|err| miette!("failed to spawn terminal process: {err}"))?,
        );
        session.output_offset = 0;
        session.state.command = Some(command);
        session.state.status = "running".to_string();
        session.state.exit_code = None;
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
            let (delta, next_offset) = session
                .process
                .as_ref()
                .map(|process| {
                    let (delta, next_offset) = process.output_since(progress_offset);
                    (delta, next_offset)
                })
                .unwrap_or_else(|| (String::new(), progress_offset));
            progress_offset = next_offset;
            if !delta.is_empty() {
                let delta = truncate_terminal_output(delta, max_chars);
                on_progress(&session.state, &delta);
            }
            if session.state.status != "running" || started_at.elapsed() >= timeout {
                break;
            }
        }
        refresh_terminal_session(session);
        let (output, next_offset) = session
            .process
            .as_ref()
            .map(|process| process.output_since(start_offset))
            .unwrap_or_else(|| (String::new(), start_offset));
        session.output_offset = next_offset;
        session.last_activity = Instant::now();
        session.state.has_unread_output = false;
        let state = session.state.clone();
        self.focused_session_id = Some(target_session_id.clone());
        self.prune_exited_sessions();
        Ok(TerminalToolResult {
            session: state,
            output: truncate_terminal_output(output, max_chars),
        })
    }

    pub async fn write_stdin_with_progress<F>(
        &mut self,
        session_id: &str,
        text: String,
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
        let requested_yield_ms = yield_time_ms.unwrap_or(Self::DEFAULT_WRITE_STDIN_YIELD_TIME_MS);
        let effective_yield_ms = if text.is_empty() {
            requested_yield_ms
                .max(Self::MIN_EMPTY_POLL_YIELD_TIME_MS)
                .min(Self::MAX_WRITE_STDIN_YIELD_TIME_MS)
        } else {
            requested_yield_ms.min(Self::MAX_WRITE_STDIN_YIELD_TIME_MS)
        };
        let timeout = Duration::from_millis(effective_yield_ms);
        let started_at = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            refresh_terminal_session(session);
            let (delta, next_offset) = session
                .process
                .as_ref()
                .map(|process| {
                    let (delta, next_offset) = process.output_since(progress_offset);
                    (delta, next_offset)
                })
                .unwrap_or_else(|| (String::new(), progress_offset));
            progress_offset = next_offset;
            if !delta.is_empty() {
                let delta = truncate_terminal_output(delta, max_chars);
                on_progress(&session.state, &delta);
            }
            if session.state.status != "running" || started_at.elapsed() >= timeout {
                break;
            }
        }
        refresh_terminal_session(session);
        let (output, next_offset) = session
            .process
            .as_ref()
            .map(|process| process.output_since(start_offset))
            .unwrap_or_else(|| (String::new(), start_offset));
        session.output_offset = next_offset;
        session.last_activity = Instant::now();
        session.state.has_unread_output = false;
        let state = session.state.clone();
        self.focused_session_id = Some(session_id.to_string());
        self.prune_exited_sessions();
        Ok(TerminalToolResult {
            session: state,
            output: truncate_terminal_output(output, max_chars),
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
        if self.focused_session_id.as_deref() == Some(session_id) {
            self.focused_session_id = self.sessions.keys().next().cloned();
        }
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
            },
        };
        self.sessions.insert(session_id.clone(), session);
        session_id
    }

    fn prune_exited_sessions(&mut self) {
        let exited_ids = self
            .sessions
            .iter()
            .filter(|(session_id, session)| {
                Some((*session_id).clone()) != self.focused_session_id
                    && session.state.status.starts_with("exited")
            })
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

#[async_trait]
impl Device for TerminalDevice {
    fn id(&self) -> DeviceId {
        DeviceId::Terminal
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn render_state(&self, is_focused: bool) -> DeviceStateRender {
        let active_session = self
            .focused_session_id
            .as_deref()
            .and_then(|session_id| self.sessions.get(session_id))
            .or_else(|| {
                self.sessions
                    .values()
                    .find(|session| session.state.has_unread_output)
            })
            .or_else(|| {
                self.sessions
                    .values()
                    .find(|session| session.state.status == "running")
            })
            .or_else(|| self.sessions.values().next());
        let running_sessions = self
            .sessions
            .values()
            .filter(|session| session.state.status == "running")
            .count();
        let unread_sessions = self
            .sessions
            .values()
            .filter(|session| session.state.has_unread_output)
            .count();
        let mut lines = vec![
            format!("focused={is_focused}"),
            "kind=terminal".to_string(),
            format!("active_sessions={}", self.sessions.len()),
            format!("running_sessions={running_sessions}"),
            format!("sessions_with_unread_output={unread_sessions}"),
        ];

        if let Some(session) = active_session {
            lines.push(format!("active_session={}", session.state.session_id));
            lines.push(format!(
                "active_process_id={}",
                session
                    .state
                    .process_id
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "none".to_string())
            ));
            lines.push(format!("active_status={}", session.state.status));
            lines.push(format!(
                "active_command={}",
                session.state.command.as_deref().unwrap_or("<none>")
            ));
            lines.push(format!(
                "active_exit_code={}",
                session
                    .state
                    .exit_code
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "none".to_string())
            ));
            lines.push(format!(
                "active_cwd={}",
                session.state.cwd.as_deref().unwrap_or("unknown")
            ));
            lines.push(format!(
                "active_has_unread_output={}",
                session.state.has_unread_output
            ));
            lines.push(format!(
                "active_last_output_preview={}",
                session.state.last_output_preview
            ));
        } else {
            lines.push("active_session=none".to_string());
            lines.push("active_status=idle".to_string());
            lines.push("active_command=<none>".to_string());
        }

        let attention = if !is_focused && unread_sessions > 0 {
            AttentionLevel::Notice
        } else {
            AttentionLevel::Quiet
        };
        DeviceStateRender {
            title: "Terminal".to_string(),
            lines,
            attention,
            is_focused,
        }
    }

    fn focused_tool_scopes(&self) -> &'static [DeviceToolScope] {
        &[DeviceToolScope::Terminal]
    }

    async fn wait_until_settled(&self, silence_duration: Duration, timeout: Duration) -> bool {
        let Some(session) = self
            .focused_session_id
            .as_ref()
            .and_then(|session_id| self.sessions.get(session_id))
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
    session.state.last_output_preview = summarize_terminal_preview(&output_tail);
    session.state.has_unread_output = session
        .process
        .as_ref()
        .map(|process| process.output_len() > session.output_offset)
        .unwrap_or(false);
    if !session.state.status.starts_with("exited") {
        session.state.status = if process_running {
            "running".to_string()
        } else if session.state.command.is_some() {
            "idle".to_string()
        } else {
            "idle".to_string()
        };
    }
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
    if let Some(limit) = max_chars
        && content.chars().count() > limit
    {
        let chars = content.chars().collect::<Vec<_>>();
        let tail = chars[chars.len().saturating_sub(limit)..]
            .iter()
            .collect::<String>();
        return format!("...{tail}");
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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

    fn delayed_output_command(delay_ms: u64, text: &str) -> String {
        if cfg!(windows) {
            format!("Start-Sleep -Milliseconds {delay_ms}; Write-Output '{text}'")
        } else {
            let delay_secs = (delay_ms as f64) / 1000.0;
            format!("sleep {delay_secs}; printf '%s\\n' '{text}'")
        }
    }

    #[tokio::test]
    async fn creates_new_sessions_and_lists_them() {
        let mut device = TerminalDevice::new();

        let created = device
            .exec_command_with_progress(
                echo_command("session-a"),
                None,
                true,
                None,
                None,
                None,
                |_session, _delta| {},
            )
            .await
            .expect("create new session should succeed");

        device.refresh_all_sessions();
        let sessions = device
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
        let mut device = TerminalDevice::new();

        let created = device
            .exec_command_with_progress(
                long_running_command(),
                None,
                true,
                None,
                None,
                None,
                |_session, _delta| {},
            )
            .await
            .expect("create long-running session should succeed");

        let terminated = device
            .terminate_session(&created.session.session_id)
            .await
            .expect("terminate should succeed");
        assert_eq!(terminated.session_id, created.session.session_id);

        device.refresh_all_sessions();
        let sessions = device
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
        let mut device = TerminalDevice::new();

        let created = device
            .exec_command_with_progress(
                long_running_command(),
                None,
                true,
                None,
                None,
                None,
                |_session, _delta| {},
            )
            .await
            .expect("create long-running session should succeed");
        device
            .terminate_session(&created.session.session_id)
            .await
            .expect("terminate should succeed");
        assert_eq!(device.focused_session_id, None);
        assert!(device.sessions.is_empty());
    }

    #[tokio::test]
    async fn prunes_old_exited_non_main_sessions() {
        let mut device = TerminalDevice::new();

        for idx in 0..(TerminalDevice::MAX_EXITED_SESSION_TOMBSTONES + 2) {
            let created = device
                .exec_command_with_progress(
                    echo_command(&format!("session-{idx}")),
                    None,
                    true,
                    None,
                    None,
                    None,
                    |_session, _delta| {},
                )
                .await
                .expect("create short-lived session should succeed");
            tokio::time::sleep(Duration::from_millis(5)).await;
            device.refresh_all_sessions();
            let _ = created;
        }

        device.refresh_all_sessions();
        let sessions = device
            .sessions
            .values()
            .map(|session| session.state.clone())
            .collect::<Vec<_>>();
        let exited_sessions = sessions
            .iter()
            .filter(|session| session.status.starts_with("exited"))
            .count();
        assert!(exited_sessions <= TerminalDevice::MAX_EXITED_SESSION_TOMBSTONES);
    }

    #[tokio::test]
    async fn exec_returns_running_session_when_yield_window_ends_first() {
        let mut device = TerminalDevice::new();

        let result = device
            .exec_command_with_progress(
                delayed_output_command(800, "done-late"),
                None,
                true,
                None,
                Some(100),
                None,
                |_session, _delta| {},
            )
            .await
            .expect("exec should succeed");

        assert_eq!(result.session.status, "running");
        assert!(result.session.exit_code.is_none());
        assert!(
            result.output.trim().is_empty(),
            "short initial yield should not have captured delayed output yet"
        );
    }

    #[tokio::test]
    async fn empty_stdin_poll_continues_running_session_until_output_arrives() {
        let mut device = TerminalDevice::new();

        let started = device
            .exec_command_with_progress(
                delayed_output_command(300, "continued-output"),
                None,
                true,
                None,
                Some(50),
                None,
                |_session, _delta| {},
            )
            .await
            .expect("exec should succeed");

        assert_eq!(started.session.status, "running");

        let polled = device
            .write_stdin_with_progress(
                &started.session.session_id,
                String::new(),
                Some(1000),
                None,
                |_session, _delta| {},
            )
            .await
            .expect("empty stdin poll should succeed");

        assert!(
            polled.output.contains("continued-output"),
            "poll should capture delayed command output"
        );
        assert!(
            polled.session.status.starts_with("exited"),
            "session should no longer be running after delayed command completes"
        );
    }
}
