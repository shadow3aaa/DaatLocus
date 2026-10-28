use async_trait::async_trait;
pub use daat_locus_bash::{TerminalSessionState, TerminalToolResult};
use miette::{Result, bail, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;
use std::{fmt::Write as _, path::Path};
pub type TerminalExecCommandRequest<'a> =
    daat_locus_bash::TerminalExecCommandRequest<'a, RuntimeSandboxPolicy>;
use crate::{
    app::{App, AppId, AppToolExecutionContext, AppToolExecutionResult, AppToolSpec},
    core::{TerminalExecArgs, TerminalTerminateArgs, TerminalWaitMode, TerminalWriteStdinArgs},
    model_protocol::AgentToolCall,
    sandbox::RuntimeSandboxPolicy,
    schema_utils::model_schema_for,
};
use daat_locus_bash::{TerminalManager, truncate_terminal_output_tail};
mod backend;
#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
mod command_tests;
const LOGIN_COMMAND_REJECTION: &str = "interactive authentication/login commands are not allowed in Terminal; abort and use a non-interactive alternative";
#[derive(Serialize, JsonSchema)]
struct TerminalObservation<'a> {
    #[serde(flatten)]
    result: &'a TerminalToolResult,
    running: bool,
    yield_time_ms: Option<u64>,
    max_chars: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wait_mode: Option<&'a str>,
}
#[derive(Serialize, JsonSchema)]
struct TerminatedSession {
    session: TerminalSessionState,
}
pub struct TerminalApp {
    terminal: TerminalManager<RuntimeSandboxPolicy>,
}
impl Default for TerminalApp {
    fn default() -> Self {
        Self::new()
    }
}
impl TerminalApp {
    fn forbidden_input_reason(text: &str) -> Option<&'static str> {
        let forbidden = [
            ["gh", "auth", "login"].as_slice(),
            &["gh", "auth", "refresh"],
            &["gh", "auth", "setup-git"],
            &["docker", "login"],
            &["npm", "login"],
            &["pnpm", "login"],
            &["huggingface-cli", "login"],
            &["hf", "auth", "login"],
        ];
        let Some(argv) = daat_locus_bash::tokenize_shell_words(text) else {
            let normalized = text.trim().to_ascii_lowercase();
            return forbidden
                .iter()
                .find(|prefix| {
                    let rendered = prefix.join(" ");
                    normalized == rendered
                        || normalized.starts_with(&(rendered.clone() + " "))
                        || normalized.starts_with(&(rendered + "\t"))
                })
                .map(|_| LOGIN_COMMAND_REJECTION);
        };
        let argv = argv
            .iter()
            .map(|word| word.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let head = argv.iter().map(String::as_str).collect::<Vec<_>>();
        forbidden
            .iter()
            .find(|prefix| head.len() >= prefix.len() && head[..prefix.len()] == prefix[..])
            .map(|_| LOGIN_COMMAND_REJECTION)
    }

    pub const fn new() -> Self {
        Self {
            terminal: TerminalManager::new(),
        }
    }
    #[cfg(test)]
    const fn new_with_output_buffer_capacity(capacity: usize) -> Self {
        Self {
            terminal: TerminalManager::new_with_output_buffer_capacity(capacity),
        }
    }
    pub fn session_state(&self, id: &str) -> Result<TerminalSessionState> {
        self.terminal.session_state(id)
    }
    pub fn terminate_session(&mut self, id: &str) -> Result<TerminalSessionState> {
        self.terminal.terminate_session(id)
    }
    pub async fn exec_command_with_progress<F>(
        &mut self,
        request: TerminalExecCommandRequest<'_>,
        progress: F,
    ) -> Result<TerminalToolResult>
    where
        F: FnMut(&TerminalSessionState, &str) + Send,
    {
        if let Some(reason) = Self::forbidden_input_reason(&request.command) {
            bail!(reason);
        }
        self.terminal
            .exec_command_with_progress(request, progress)
            .await
    }
    pub async fn write_stdin_with_progress<F>(
        &mut self,
        id: &str,
        text: String,
        mode: TerminalWaitMode,
        yield_time_ms: Option<u64>,
        max_chars: Option<usize>,
        progress: F,
    ) -> Result<TerminalToolResult>
    where
        F: FnMut(&TerminalSessionState, &str) + Send,
    {
        if let Some(reason) = Self::forbidden_input_reason(&text) {
            bail!(reason);
        }
        self.terminal
            .write_stdin_with_progress(
                id,
                text,
                match mode {
                    TerminalWaitMode::AnyOutput => daat_locus_bash::TerminalWaitMode::AnyOutput,
                    TerminalWaitMode::Timeout => daat_locus_bash::TerminalWaitMode::Timeout,
                },
                yield_time_ms,
                max_chars,
                progress,
            )
            .await
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

const fn terminal_progress_mode(text: &str) -> &'static str {
    if text.is_empty() { "poll" } else { "continue" }
}

const fn terminal_wait_mode_label(wait_mode: TerminalWaitMode) -> &'static str {
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
            .map_or_else(|| "-".to_string(), |value| value.to_string()),
        session.cwd.as_deref().unwrap_or("-")
    );
    if session.output_dropped_bytes > 0 {
        let _ = write!(
            meta,
            "  dropped={}B  buffer={}/{}B",
            session.output_dropped_bytes,
            session.output_retained_bytes,
            session.output_buffer_capacity
        );
    }
    meta
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
    fn can_passivate(&self) -> bool {
        // Retain both live processes and unread/retained output. Session IDs
        // must not silently become invalid (or be reused) across inputs.
        self.terminal.is_empty()
    }
    async fn shutdown(&mut self) -> Result<()> {
        self.terminal.shutdown();
        Ok(())
    }
    fn id(&self) -> AppId {
        AppId::terminal()
    }

    fn tool_specs(&self) -> Vec<AppToolSpec> {
        vec![
            AppToolSpec {
                name: "terminal_exec".to_string(),
                description: "Start a terminal command and return output after the current yield window ends. Host reads and executable locations need no additional filesystem grant; writes remain scoped. Set `session_id` to null to create a new session, or pass an existing session id returned by a prior terminal call to reuse that session. Never invent a session id. If the command is still running, the result keeps the session so later calls can continue with terminal_write_stdin.".to_string(),
                input_schema: model_schema_for::<TerminalExecArgs>(),
            },
            AppToolSpec {
                name: "terminal_write_stdin".to_string(),
                description: "Continue a running terminal session. Send text to write stdin; send empty text to wait. By default `wait_mode=any_output` returns after new output, exit, or timeout; use `wait_mode=timeout` to wait for the full yield window or process exit without streaming intermediate progress.".to_string(),
                input_schema: model_schema_for::<TerminalWriteStdinArgs>(),
            },
            AppToolSpec {
                name: "terminal_terminate".to_string(),
                description: "Terminate the current foreground process in the specified terminal session and return the updated session state.".to_string(),
                input_schema: model_schema_for::<TerminalTerminateArgs>(),
            },
        ]
    }

    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        if context
            .actor_call
            .as_ref()
            .is_some_and(|caller| caller.remote_read_only)
        {
            return Err(miette!(
                "Remote read-only authority cannot run native processes"
            ));
        }
        let process_policy = terminal_process_policy(context);

        if call.name == "terminal_write_stdin" {
            let args: TerminalWriteStdinArgs = parse_terminal_tool_args(call)?;
            if !args.text.is_empty() {
                self.terminal
                    .ensure_policy(&args.session_id, &process_policy)?;
            } else {
                self.terminal.session_state(&args.session_id)?;
            }
        }
        match call.name.as_str() {
            "terminal_exec" => {
                let args: TerminalExecArgs = parse_terminal_tool_args(call)?;
                let effective_workdir = args.workdir.as_deref().map_or_else(
                    || context.execution_cwd.clone(),
                    |workdir| resolve_terminal_path(context, workdir, Some(&context.execution_cwd)),
                );
                terminal_process_policy(context)
                    .ensure_path_readable(&effective_workdir, "terminal workdir")?;
                let effective_workdir = args
                    .workdir
                    .clone()
                    .or_else(|| Some(context.execution_cwd.display().to_string()));
                let process_policy = terminal_process_policy(context);
                let result = self
                    .exec_command_with_progress(
                        TerminalExecCommandRequest {
                            command: args.command.clone(),
                            session_id: args.session_id.clone(),
                            workdir: effective_workdir,
                            sandbox_policy: &process_policy,
                            yield_time_ms: args.yield_time_ms,
                            max_chars: args.max_chars,
                        },
                        |_session, _delta| {},
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
                            .map_or_else(|| "default".to_string(), |value| value.to_string())
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
                let session_id = result.session.session_id.clone();
                let mut tool_result = AppToolExecutionResult::structured(
                    summary,
                    &TerminalObservation {
                        result: &result,
                        running,
                        yield_time_ms: args.yield_time_ms,
                        max_chars: args.max_chars,
                        mode: None,
                        wait_mode: None,
                    },
                    Some(model_content),
                )?;
                tool_result.sandbox = self.terminal.take_denial_report(&session_id);
                Ok(tool_result)
            }
            "terminal_write_stdin" => {
                let args: TerminalWriteStdinArgs = parse_terminal_tool_args(call)?;
                let wait_mode = args.wait_mode.unwrap_or(TerminalWaitMode::AnyOutput);
                let session = self.session_state(&args.session_id)?;
                if let Some(cwd) = session.cwd.as_deref() {
                    let resolved_cwd = resolve_terminal_path(context, cwd, None);
                    terminal_process_policy(context)
                        .ensure_path_readable(&resolved_cwd, "terminal session cwd")?;
                }
                let result = self
                    .write_stdin_with_progress(
                        &args.session_id,
                        args.text.clone(),
                        wait_mode,
                        args.yield_time_ms,
                        args.max_chars,
                        |_session, _delta| {},
                    )
                    .await?;
                let mode = terminal_progress_mode(&args.text);
                let wait_mode_label = terminal_wait_mode_label(wait_mode);
                let running = result.session.status == "running";

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
                            .map_or_else(|| "default".to_string(), |value| value.to_string())
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
                let session_id = result.session.session_id.clone();
                let mut tool_result = AppToolExecutionResult::structured(
                    summary,
                    &TerminalObservation {
                        result: &result,
                        running,
                        yield_time_ms: args.yield_time_ms,
                        max_chars: args.max_chars,
                        mode: Some(mode),
                        wait_mode: Some(wait_mode_label),
                    },
                    Some(model_content),
                )?;
                tool_result.sandbox = self.terminal.take_denial_report(&session_id);
                Ok(tool_result)
            }
            "terminal_terminate" => {
                let args: TerminalTerminateArgs = parse_terminal_tool_args(call)?;
                let session = self.terminate_session(&args.session_id)?;
                let mut tool_result = AppToolExecutionResult::structured(
                    format!("terminated {}", display_session_label(&session.session_id)),
                    &TerminatedSession { session },
                    None,
                )?;
                tool_result.sandbox = self.terminal.take_denial_report(&args.session_id);
                Ok(tool_result)
            }
            _ => Err(miette!("unknown terminal tool `{}`", call.name)),
        }
    }
}

fn terminal_process_policy(context: &AppToolExecutionContext) -> RuntimeSandboxPolicy {
    let mut policy = context.sandbox_policy.clone();
    policy.filesystem.deny_read_paths.sort();
    policy.filesystem.deny_read_paths.dedup();
    policy.filesystem.deny_write_paths.sort();
    policy.filesystem.deny_write_paths.dedup();
    policy
}

/// Takes the native denial records an exited session captured, if any.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_sessions_prevent_passivation_and_reconstruction_never_reuses_ids() {
        let mut app = TerminalApp::new();
        assert!(app.can_passivate());
        let old = app.terminal.create_session();
        assert!(!app.can_passivate());
        app.terminate_session(&old).unwrap();
        assert!(app.can_passivate());
        let mut rebuilt = TerminalApp::new();
        let fresh = rebuilt.terminal.create_session();
        assert_ne!(old, fresh);
        assert!(rebuilt.session_state(&old).is_err());
    }
    use std::{
        env,
        sync::atomic::{AtomicBool, Ordering},
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
            administrator: None,
            builtin_resources: Vec::new(),
            native: crate::sandbox::SandboxPolicy {
                filesystem: FileSystemSandboxPolicy {
                    full_disk_read: true,
                    deny_all_writes: false,
                    full_disk_write: true,
                    readable_roots: Vec::new(),
                    writable_roots: Vec::new(),
                    deny_read_paths: Vec::new(),
                    bound_roots: Vec::new(),
                    deny_write_paths: Vec::new(),
                },
                strong_filesystem: StrongFilesystemSandboxMode::Off,
            },
        }
    }

    fn echo_command(text: &str) -> String {
        format!("printf '%s\\n' '{text}'")
    }
    fn long_running_command() -> String {
        "sleep 30".into()
    }
    fn high_output_command(byte_count: usize) -> String {
        format!("printf '%{byte_count}s' ''")
    }
    fn marked_high_output_command(byte_count: usize) -> String {
        format!("printf 'HEAD-MARKER'; printf '%{byte_count}s' ''; printf 'TAIL-MARKER'")
    }
    fn delayed_output_after_ms_then_sleep_command(text: &str, delay_ms: u64) -> String {
        let seconds = delay_ms / 1_000;
        let milliseconds = delay_ms % 1_000;
        format!("sleep {seconds}.{milliseconds:03}; printf '%s\\n' '{text}'; sleep 2")
    }
    fn interactive_shell_command() -> String {
        "exec \"$BASH\" -s".into()
    }

    async fn start_stdin_echo_session(
        app: &mut TerminalApp,
        sandbox_policy: &RuntimeSandboxPolicy,
    ) -> String {
        let marker = "daat-locus-bash-ready";
        // Exercise real stdin, including delayed output, inside the Rubash child.
        let command = format!(
            "printf '%s' '{marker}'; while IFS= read -r line; do sleep 0.1; printf '%s\\n' \"$line\"; done"
        );
        let mut output = String::new();
        // Startup has its own bounded deadline, separate from wait-mode checks.
        // AnyOutput can return before the complete marker arrives.
        let ready = tokio::time::timeout(Duration::from_secs(30), async {
            let mut result = app
                .exec_command_with_progress(
                    TerminalExecCommandRequest {
                        command,
                        session_id: None,
                        workdir: None,
                        sandbox_policy,
                        yield_time_ms: Some(50),
                        max_chars: None,
                    },
                    |_session, _delta| {},
                )
                .await
                .expect("stdin echo session should start");
            loop {
                output.push_str(&result.output);
                assert_eq!(
                    result.session.status, "running",
                    "stdin echo session must stay alive: {output:?}"
                );
                if output.contains(marker) {
                    break result.session.session_id;
                }
                result = app
                    .write_stdin_with_progress(
                        &result.session.session_id,
                        String::new(),
                        TerminalWaitMode::AnyOutput,
                        Some(5_000),
                        None,
                        |_session, _delta| {},
                    )
                    .await
                    .expect("stdin echo readiness poll should succeed");
            }
        })
        .await;
        ready.unwrap_or_else(|_| panic!("stdin echo readiness marker timed out: {output:?}"))
    }

    fn env_value_command(name: &str) -> String {
        format!("printf '%s' \"${{{name}-missing}}\"")
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_shell_output_is_utf8_without_system_utf8_mode() {
        let mut app = TerminalApp::new();
        let sandbox_policy = test_sandbox_policy();

        let created = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: "printf '中文输出\\n'".to_string(),
                    session_id: None,
                    workdir: None,
                    sandbox_policy: &sandbox_policy,
                    yield_time_ms: None,
                    max_chars: None,
                },
                |_session, _delta| {},
            )
            .await
            .expect("unicode echo command should succeed");

        assert!(
            created.output.contains("中文输出"),
            "windows shell output must stay valid utf-8 when the active code page is not utf-8: {:?}",
            created.output
        );
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

        app.terminal.refresh_all_sessions();
        let sessions = app.terminal.session_states();
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
            .expect("terminate should succeed");
        assert_eq!(terminated.session_id, created.session.session_id);

        app.terminal.refresh_all_sessions();
        let sessions = app.terminal.session_states();
        assert!(
            sessions
                .iter()
                .all(|session| session.session_id != created.session.session_id)
        );
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn session_list_clears_when_last_session_is_terminated() {
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
            .expect("terminate should succeed");
        assert!(app.terminal.is_empty());
    }

    #[tokio::test]
    async fn prunes_old_exited_non_main_sessions() {
        let mut app = TerminalApp::new();
        let sandbox_policy = test_sandbox_policy();

        for idx in 0..(TerminalManager::<RuntimeSandboxPolicy>::MAX_EXITED_SESSION_TOMBSTONES + 2) {
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
            app.terminal.refresh_all_sessions();
            let _ = created;
        }

        app.terminal.refresh_all_sessions();
        let sessions = app.terminal.session_states();
        let exited_sessions = sessions
            .iter()
            .filter(|session| session.status.starts_with("exited"))
            .count();
        assert!(
            exited_sessions
                <= TerminalManager::<RuntimeSandboxPolicy>::MAX_EXITED_SESSION_TOMBSTONES
        );
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
    async fn exec_captures_fast_output_with_short_yield() {
        let mut app = TerminalApp::new();
        let sandbox_policy = test_sandbox_policy();

        let result = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: echo_command("fast-output"),
                    session_id: None,
                    workdir: None,
                    sandbox_policy: &sandbox_policy,
                    yield_time_ms: Some(1),
                    max_chars: None,
                },
                |_session, _delta| {},
            )
            .await
            .expect("fast command should run");

        assert!(
            result.output.contains("fast-output"),
            "short initial yield should still capture fast output: {:?}",
            result.output
        );
    }

    #[tokio::test]
    async fn empty_stdin_poll_returns_unread_output_produced_between_calls() {
        let mut app = TerminalApp::new();
        let sandbox_policy = test_sandbox_policy();
        let marker = "between-poll-output";
        let output_delay_ms = if cfg!(windows) { 2_000 + 500 } else { 2_000 };

        let started = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: delayed_output_after_ms_then_sleep_command(marker, output_delay_ms),
                    session_id: None,
                    workdir: None,
                    sandbox_policy: &sandbox_policy,
                    yield_time_ms: Some(50),
                    max_chars: None,
                },
                |_session, _delta| {},
            )
            .await
            .expect("exec should start delayed output command");

        assert_eq!(started.session.status, "running");
        assert!(
            started.output.trim().is_empty(),
            "initial short yield should not see delayed output: {:?}",
            started.output
        );

        tokio::time::sleep(Duration::from_millis(output_delay_ms + 300)).await;
        let polled = app
            .write_stdin_with_progress(
                &started.session.session_id,
                String::new(),
                TerminalWaitMode::AnyOutput,
                Some(5_000),
                None,
                |_session, _delta| {},
            )
            .await
            .expect("empty stdin poll should succeed");

        assert!(
            polled.output.contains(marker),
            "poll should return unread output produced between tool calls: {:?}",
            polled.output
        );

        if polled.session.status == "running" {
            app.terminate_session(&polled.session.session_id)
                .expect("terminate should succeed");
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
    async fn high_output_preserves_head_and_tail() {
        let mut app = TerminalApp::new_with_output_buffer_capacity(128);
        let sandbox_policy = test_sandbox_policy();

        let result = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: marked_high_output_command(4096),
                    session_id: None,
                    workdir: None,
                    sandbox_policy: &sandbox_policy,
                    yield_time_ms: Some(5_000),
                    max_chars: None,
                },
                |_session, _delta| {},
            )
            .await
            .expect("marked high-output command should succeed");

        assert!(
            result.output_missed_bytes > 0,
            "expected bounded buffer to report omitted middle bytes"
        );
        assert!(
            result.output.contains("HEAD-MARKER"),
            "head marker should be retained: {:?}",
            result.output
        );
        assert!(
            result.output.contains("TAIL-MARKER"),
            "tail marker should be retained: {:?}",
            result.output
        );
    }

    #[tokio::test]
    async fn terminal_exec_injects_stable_env_defaults() {
        let mut app = TerminalApp::new();
        let sandbox_policy = test_sandbox_policy();

        let result = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: env_value_command("PAGER"),
                    session_id: None,
                    workdir: None,
                    sandbox_policy: &sandbox_policy,
                    yield_time_ms: Some(5_000),
                    max_chars: None,
                },
                |_session, _delta| {},
            )
            .await
            .expect("env default check command should run");

        assert_eq!(result.output.trim(), "cat");
    }

    #[tokio::test]
    async fn terminal_exec_inherits_host_environment_without_name_filtering() {
        let _env = EnvOverride::set("DAAT_LOCUS_TEST_SECRET_TOKEN", "synthetic inherited value");
        let mut app = TerminalApp::new();
        let mut sandbox_policy = test_sandbox_policy();
        // Exercise inheritance with a scoped policy rather than the disabled shortcut.
        sandbox_policy.filesystem.full_disk_write = false;

        let result = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: env_value_command("DAAT_LOCUS_TEST_SECRET_TOKEN"),
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

        assert_eq!(result.output.trim(), "synthetic inherited value");
    }

    #[tokio::test]
    async fn failed_command_retains_output_without_replaying_effects() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("effect.txt");
        let command = format!(
            "printf x >> '{}'; printf 'synthetic command failure\\n' >&2; exit 7",
            marker.to_string_lossy().replace('\'', "'\"'\"'")
        );
        let context = AppToolExecutionContext {
            restart_native: false,
            access_broker: None,
            execution_cwd: directory.path().to_path_buf(),
            sandbox_policy: test_sandbox_policy(),
            tool_output_max_tokens: 1000,
            turn_epoch: 1,
            actor_call: None,
        };
        let call = AgentToolCall {
            id: "partial-failure".into(),
            name: "terminal_exec".into(),
            arguments: json!({
                "command":command,"session_id":null,"workdir":null,
                "yield_time_ms":5000,"max_chars":2000
            }),
        };
        let mut app = TerminalApp::new();
        let (result, output) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let mut result = app.execute_tool(&call, &context).await.unwrap();
            let mut output = result.payload["output"].as_str().unwrap().to_string();
            while result.payload["running"].as_bool() == Some(true) {
                let poll = AgentToolCall {
                    id: "poll-partial-failure".into(),
                    name: "terminal_write_stdin".into(),
                    arguments: json!({
                        "session_id":result.payload["session"]["session_id"],
                        "text":"","wait_mode":"timeout","yield_time_ms":1000,"max_chars":2000
                    }),
                };
                result = app.execute_tool(&poll, &context).await.unwrap();
                output.push_str(result.payload["output"].as_str().unwrap());
            }
            (result, output)
        })
        .await
        .expect("failed command did not finish");
        assert_eq!(result.payload["session"]["exit_code"], 7);
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "x");
        assert!(output.contains("synthetic command failure"));
        assert_eq!(result.payload["running"], false);
        assert!(
            result.payload.get("approval").is_none(),
            "a failed process is not itself an approval decision"
        );
        app.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn empty_stdin_poll_continues_running_session_until_output_arrives() {
        let mut app = TerminalApp::new();
        let sandbox_policy = test_sandbox_policy();

        let started = app
            .exec_command_with_progress(
                TerminalExecCommandRequest {
                    command: interactive_shell_command(),
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
            let input = "echo continued-output\nexit\n".to_string();

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
        let session_id = start_stdin_echo_session(&mut app, &sandbox_policy).await;
        let received_progress = AtomicBool::new(false);
        let result = {
            let waiting = app.write_stdin_with_progress(
                &session_id,
                "any-output-mode\n".to_string(),
                TerminalWaitMode::AnyOutput,
                Some(30_000),
                None,
                |_session, _delta| {
                    received_progress.store(true, Ordering::Relaxed);
                },
            );
            tokio::pin!(waiting);
            // Native pipe latency and OS scheduling may delay the first output.
            // Once output is delivered for this live process, AnyOutput must
            // complete rather than await another timer or input. Record failure
            // here, but clean up the child before asserting outside the future.
            std::future::poll_fn(|cx| match waiting.as_mut().poll(cx) {
                std::task::Poll::Ready(result) => std::task::Poll::Ready(Some(result)),
                std::task::Poll::Pending if received_progress.load(Ordering::Relaxed) => {
                    std::task::Poll::Ready(None)
                }
                std::task::Poll::Pending => std::task::Poll::Pending,
            })
            .await
        };

        // Clean up before assertions so a regression cannot leave a child pipe
        // blocking the Windows runtime while the test panic unwinds.
        let terminated = app.terminate_session(&session_id);
        let result = result
            .expect("any_output kept waiting after delivering output")
            .expect("any_output wait should succeed");
        terminated.expect("terminate should succeed");
        assert!(received_progress.load(Ordering::Relaxed));
        assert!(
            result.output.contains("any-output-mode"),
            "expected output update before timeout: {:?}",
            result.output
        );
        assert_eq!(
            result.session.status, "running",
            "process should still be running after first output update"
        );
    }

    #[tokio::test]
    async fn timeout_wait_mode_suppresses_progress_until_timeout() {
        let mut app = TerminalApp::new();
        let sandbox_policy = test_sandbox_policy();
        let session_id = start_stdin_echo_session(&mut app, &sandbox_policy).await;
        let started_at = Instant::now();
        let mut progress_calls = 0usize;
        let waited = app
            .write_stdin_with_progress(
                &session_id,
                "timeout-mode\n".to_string(),
                TerminalWaitMode::Timeout,
                Some(1_000),
                None,
                |_session, _delta| {
                    progress_calls += 1;
                },
            )
            .await
            .expect("timeout wait should succeed");

        assert_eq!(progress_calls, 0, "timeout wait must not stream progress");
        assert!(
            started_at.elapsed() >= Duration::from_millis(850),
            "timeout wait should wait for the requested yield window"
        );
        assert!(
            waited.output.contains("timeout-mode"),
            "final output should still include data produced while waiting: {:?}",
            waited.output
        );
        assert_eq!(waited.session.status, "running");
        app.terminate_session(&waited.session.session_id)
            .expect("terminate should succeed");
    }

    #[test]
    fn shell_tokenizers_preserve_quotes_and_login_argv() {
        assert_eq!(
            daat_locus_bash::tokenize_shell_words("cd -- 'a b'").as_deref(),
            Some(["cd".to_string(), "--".to_string(), "a b".to_string()].as_slice())
        );
        assert_eq!(
            TerminalManager::<RuntimeSandboxPolicy>::cwd_from_shell_input("cd -- '/tmp/my dir'")
                .as_deref(),
            Some("/tmp/my dir")
        );
        assert!(TerminalApp::forbidden_input_reason("gh auth login --web").is_some());
        assert!(TerminalApp::forbidden_input_reason("echo gh auth login").is_none());
        assert!(TerminalApp::forbidden_input_reason("'gh auth login'").is_none());
        assert!(TerminalApp::forbidden_input_reason("GH AUTH LOGIN extra").is_some());
    }

    #[tokio::test]
    async fn terminal_reads_granted_runtime_and_signing_paths_without_a_name_blacklist() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        for name in [".daat-locus", ".daat-locus-signing"] {
            let root = directory.path().join(name);
            std::fs::create_dir(&root).unwrap();
            let file = root.join("fixture.txt");
            std::fs::write(&file, "granted runtime fixture").unwrap();
            let mut policy = test_sandbox_policy();
            policy.filesystem.full_disk_read = false;
            policy.filesystem.full_disk_write = false;
            policy.filesystem.readable_roots = vec![workspace.clone(), root];
            let context = AppToolExecutionContext {
                restart_native: false,
                access_broker: None,
                execution_cwd: workspace.clone(),
                sandbox_policy: policy,
                tool_output_max_tokens: 1000,
                turn_epoch: 0,
                actor_call: None,
            };
            let command = format!(
                "while IFS= read -r line || [[ -n $line ]]; do printf '%s\\n' \"$line\"; done < '{}'",
                file.display()
            );
            let call = AgentToolCall {
                id: "read-fixture".into(),
                name: "terminal_exec".into(),
                arguments: json!({"command":command,"session_id":null,"workdir":null,
                    "yield_time_ms":5000,"max_chars":2000}),
            };
            let mut app = TerminalApp::new();
            let result = app.execute_tool(&call, &context).await.unwrap();
            assert_eq!(result.payload["session"]["exit_code"], 0);
            assert_eq!(
                result.payload["output"].as_str().unwrap().trim(),
                "granted runtime fixture"
            );
            app.shutdown().await.unwrap();
        }
    }
}

/// Fixtures that exercise missing-scope behavior must stay outside the
/// system temporary directories: the baseline temporary authorization
/// would otherwise mask the boundary this test verifies.
#[cfg(test)]
fn non_temporary_scratch() -> tempfile::TempDir {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("notemp-terminal-")
        .tempdir_in(&base)
        .unwrap()
}

#[tokio::test]
async fn live_terminal_rejects_new_input_after_scope_change_but_keeps_observation_available() {
    if !crate::sandbox::test_support::native_sandbox_available() {
        return;
    }
    let dir = non_temporary_scratch();
    let mut policy = RuntimeSandboxPolicy::disabled();
    policy.filesystem.full_disk_read = false;
    policy.filesystem.full_disk_write = false;
    policy.filesystem.readable_roots = vec![dir.path().into()];
    policy.strong_filesystem = crate::sandbox::StrongFilesystemSandboxMode::Required;
    let base = AppToolExecutionContext {
        restart_native: false,
        access_broker: None,
        execution_cwd: dir.path().into(),
        sandbox_policy: policy,
        tool_output_max_tokens: 1000,
        turn_epoch: 1,
        actor_call: None,
    };
    let mut app = TerminalApp::new();
    let start = AgentToolCall {
        id: "start".into(),
        name: "terminal_exec".into(),
        arguments: json!({"command":"sleep 20",
            "session_id":null,"workdir":null,"yield_time_ms":25,"max_chars":1000}),
    };
    let started = app.execute_tool(&start, &base).await.unwrap();
    let session = started.payload["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut changed = base.clone();
    changed
        .sandbox_policy
        .filesystem
        .readable_roots
        .push(dir.path().join("new-scope"));
    let input = AgentToolCall {
        id: "continue".into(),
        name: "terminal_write_stdin".into(),
        arguments: json!({"session_id":session,"text":"echo must-not-be-replayed\n","wait_mode":null,"yield_time_ms":25,"max_chars":1000}),
    };
    let error = app.execute_tool(&input, &changed).await.unwrap_err();
    assert!(error.to_string().contains("Explicitly restart"), "{error}");
    let mut poll = input;
    poll.arguments["text"] = json!("");
    let observed = app.execute_tool(&poll, &changed).await.unwrap();
    assert_eq!(observed.payload["session"]["session_id"], session);
    assert_eq!(
        app.terminal.len(),
        1,
        "scope changes cannot silently recreate a process"
    );
    assert!(
        !observed.payload["output"]
            .as_str()
            .unwrap()
            .contains("must-not-be-replayed")
    );
    app.shutdown().await.unwrap();
}

#[test]
fn terminal_scope_roots_are_absent_from_the_declared_schema() {
    let schema = model_schema_for::<TerminalExecArgs>();
    crate::schema_utils::validate_model_facing_schema(&schema).unwrap();
    for name in ["additional_read_roots", "additional_write_roots"] {
        assert!(schema["properties"].get(name).is_none());
        assert!(
            !schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == name)
        );
    }
}

#[cfg(all(test, windows))]
mod native_tests;

#[tokio::test]
async fn terminal_shutdown_reaps_rubash_background_descendants() {
    let directory = tempfile::tempdir().unwrap();
    let child_pid = directory.path().join("child.pid");
    let policy = RuntimeSandboxPolicy::disabled();
    let mut app = TerminalApp::new();
    let result = app
        .exec_command_with_progress(
            TerminalExecCommandRequest {
                command: r#"sleep 120 & printf '%s' "$!" > child.pid; wait $!"#.into(),
                session_id: None,
                workdir: Some(directory.path().to_string_lossy().into_owned()),
                sandbox_policy: &policy,
                yield_time_ms: Some(20),
                max_chars: None,
            },
            |_, _| {},
        )
        .await
        .unwrap();
    let pid = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&child_pid)
                && let Ok(pid) = text.parse::<u32>()
            {
                break pid;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("background child never started");
    assert!(
        sysinfo::System::new_all()
            .process(sysinfo::Pid::from_u32(pid))
            .is_some_and(|process| process.status() != sysinfo::ProcessStatus::Zombie),
        "fixture must identify a live OS descendant before checking cleanup"
    );
    assert_eq!(result.session.status, "running");
    app.shutdown().await.unwrap();
    assert!(app.session_state(&result.session.session_id).is_err());
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let system = sysinfo::System::new_all();
            if !system
                .process(sysinfo::Pid::from_u32(pid))
                .is_some_and(|process| process.status() != sysinfo::ProcessStatus::Zombie)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("background descendant survived terminal shutdown");
}

#[tokio::test]
async fn explicit_shell_read_denial_is_not_overridden_by_its_runtime_dependency() {
    if !crate::sandbox::test_support::native_sandbox_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let mut policy = RuntimeSandboxPolicy::disabled();
    policy.strong_filesystem = crate::sandbox::StrongFilesystemSandboxMode::Required;
    policy.filesystem.full_disk_write = false;
    let mut app = TerminalApp::new();
    let baseline = app
        .exec_command_with_progress(
            TerminalExecCommandRequest {
                command: "printf allowed > allowed.txt".into(),
                session_id: None,
                workdir: Some(directory.path().to_string_lossy().into_owned()),
                sandbox_policy: &policy,
                yield_time_ms: Some(5000),
                max_chars: None,
            },
            |_, _| {},
        )
        .await
        .expect("native shell must start before testing its explicit denial");
    assert_eq!(baseline.session.exit_code, Some(0));
    assert_eq!(
        std::fs::read_to_string(directory.path().join("allowed.txt")).unwrap(),
        "allowed"
    );
    let executable = daat_locus_bash::ProcessPolicy::shell_executable(&policy).unwrap();
    policy.filesystem.deny_read_paths.push(executable);
    let result = app
        .exec_command_with_progress(
            TerminalExecCommandRequest {
                command: "printf bypassed > forbidden.txt".into(),
                session_id: None,
                workdir: Some(directory.path().to_string_lossy().into_owned()),
                sandbox_policy: &policy,
                yield_time_ms: Some(5000),
                max_chars: None,
            },
            |_, _| {},
        )
        .await;
    if let Ok(result) = result {
        assert_ne!(result.session.exit_code, Some(0));
    }
    assert!(!directory.path().join("forbidden.txt").exists());
}
