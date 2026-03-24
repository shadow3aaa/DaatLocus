use miette::Result;
use serde_json::json;

use crate::{
    context::Context,
    core::{TerminalExecArgs, TerminalTerminateArgs, TerminalWriteStdinArgs},
    dashboard::{DashboardActivityEvent, apply_activity_event},
    device::DeviceToolScope,
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    tool_ui::{TerminalUiAction, ToolCallUiEvent, ToolUiEvent, compact_body_lines},
};

use super::{
    RuntimeTool, StaticRuntimeTool, ToolExecutionResult, ToolFuture, parse_tool_args,
    summarize_inline_text,
};

fn display_session_label(session_id: &str) -> String {
    session_id.to_string()
}

fn terminal_progress_mode(text: &str) -> &'static str {
    if text.is_empty() { "poll" } else { "continue" }
}

fn terminal_session_meta(session: &crate::terminal_device::TerminalSessionState) -> String {
    format!(
        "{}  {}  exit={}  cwd={}",
        display_session_label(&session.session_id),
        session.status,
        session
            .exit_code
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        session.cwd.as_deref().unwrap_or("-")
    )
}

pub(super) fn register_tools() -> Vec<Box<dyn RuntimeTool>> {
    vec![
        Box::new(StaticRuntimeTool::new::<TerminalExecArgs>(
            "terminal_exec",
            "启动一条终端命令，并在当前输出窗口结束后返回输出。如果命令仍在运行，结果中会保留 session，后续继续使用 terminal_write_stdin。",
            Some(DeviceToolScope::Terminal),
            summarize_terminal_exec_tool,
            render_terminal_exec_call_ui,
            execute_terminal_exec_tool,
        )),
        Box::new(StaticRuntimeTool::new::<TerminalWriteStdinArgs>(
            "terminal_write_stdin",
            "继续一个正在运行的 terminal session。发送文本可写入 stdin；发送空文本可仅等待下一段输出。",
            Some(DeviceToolScope::Terminal),
            summarize_terminal_write_stdin_tool,
            render_terminal_write_stdin_call_ui,
            execute_terminal_write_stdin_tool,
        )),
        Box::new(StaticRuntimeTool::new::<TerminalTerminateArgs>(
            "terminal_terminate",
            "终止指定 terminal session 的当前前台进程，并返回更新后的 session 状态。",
            Some(DeviceToolScope::Terminal),
            summarize_terminal_terminate_tool,
            render_terminal_terminate_call_ui,
            execute_terminal_terminate_tool,
        )),
    ]
}

fn summarize_terminal_exec_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: TerminalExecArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "terminal_exec".to_string(),
        summary: format!(
            "command={} session={} new_session={} workdir={} yield_time_ms={}",
            summarize_inline_text(&args.command),
            args.session_id.unwrap_or_else(|| "focused".to_string()),
            args.create_new_session,
            args.workdir.unwrap_or_else(|| "none".to_string()),
            args.yield_time_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "default".to_string())
        ),
    })
}

fn render_terminal_exec_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: TerminalExecArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::terminal(
        TerminalUiAction::Execute,
        summarize_inline_text(&args.command),
        vec![format!(
            "session={} new_session={} workdir={} yield_time_ms={}",
            args.session_id.unwrap_or_else(|| "focused".to_string()),
            args.create_new_session,
            args.workdir.unwrap_or_else(|| "-".to_string()),
            args.yield_time_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "default".to_string())
        )],
    ))
}

fn execute_terminal_exec_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: TerminalExecArgs = parse_tool_args(call)?;
        let effective_workdir = args
            .workdir
            .clone()
            .or_else(|| Some(context.execution_cwd.display().to_string()));
        let dashboard_tx = context.dashboard_tx.clone();
        let result = context
            .devices
            .terminal_exec_with_progress(
                args.command.clone(),
                args.session_id.clone(),
                args.create_new_session,
                effective_workdir,
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
        let running = result.session.status == "running";
        let summary = if running {
            format!(
                "started `{}` in {}",
                summarize_inline_text(result.session.command.as_deref().unwrap_or(&args.command)),
                display_session_label(&result.session.session_id)
            )
        } else {
            format!(
                "ran `{}` in {}",
                summarize_inline_text(result.session.command.as_deref().unwrap_or(&args.command)),
                display_session_label(&result.session.session_id)
            )
        };
        Ok(ToolExecutionResult::new(
            summary,
            json!({
                "session": result.session,
                "output": result.output,
                "running": running,
                "yield_time_ms": args.yield_time_ms,
                "max_chars": args.max_chars,
            }),
            ToolUiEvent::terminal(
                if running {
                    TerminalUiAction::Execute
                } else {
                    TerminalUiAction::Continue
                },
                summarize_inline_text(result.session.command.as_deref().unwrap_or(&args.command)),
                {
                    let mut body = vec![terminal_session_meta(&result.session)];
                    body.extend(compact_body_lines(&result.output, 10));
                    body
                },
            ),
        ))
    })
}

fn summarize_terminal_write_stdin_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: TerminalWriteStdinArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "terminal_write_stdin".to_string(),
        summary: format!(
            "session={} mode={} text={} yield_time_ms={}",
            args.session_id,
            terminal_progress_mode(&args.text),
            summarize_inline_text(&args.text),
            args.yield_time_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "default".to_string())
        ),
    })
}

fn render_terminal_write_stdin_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: TerminalWriteStdinArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::terminal(
        if args.text.is_empty() {
            TerminalUiAction::Poll
        } else {
            TerminalUiAction::Continue
        },
        format!(
            "{} {}",
            terminal_progress_mode(&args.text),
            display_session_label(&args.session_id)
        ),
        if args.text.is_empty() {
            vec![format!(
                "yield_time_ms={}",
                args.yield_time_ms
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "default".to_string())
            )]
        } else {
            let mut lines = compact_body_lines(&args.text, 2);
            lines.push(format!(
                "yield_time_ms={}",
                args.yield_time_ms
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "default".to_string())
            ));
            lines
        },
    ))
}

fn execute_terminal_write_stdin_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: TerminalWriteStdinArgs = parse_tool_args(call)?;
        let dashboard_tx = context.dashboard_tx.clone();
        let result = context
            .devices
            .terminal_write_stdin_with_progress(
                &args.session_id,
                args.text.clone(),
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
        let running = result.session.status == "running";
        let command_label = summarize_inline_text(
            result
                .session
                .command
                .as_deref()
                .unwrap_or(&args.session_id),
        );
        let summary = match (mode, running) {
            ("poll", true) => format!(
                "continued {}",
                display_session_label(&result.session.session_id)
            ),
            ("poll", false) => format!(
                "completed {}",
                display_session_label(&result.session.session_id)
            ),
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
        Ok(ToolExecutionResult::new(
            summary,
            json!({
                "session": result.session,
                "output": result.output,
                "text": args.text,
                "mode": mode,
                "running": running,
                "yield_time_ms": args.yield_time_ms,
                "max_chars": args.max_chars,
            }),
            ToolUiEvent::terminal(
                match mode {
                    "poll" => TerminalUiAction::Poll,
                    _ => TerminalUiAction::Continue,
                },
                if mode == "poll" {
                    format!("Waited for {command_label}")
                } else {
                    command_label
                },
                if args.text.is_empty() {
                    compact_body_lines(&result.output, 10)
                } else {
                    let mut body = compact_body_lines(&args.text, 2);
                    body.extend(compact_body_lines(&result.output, 10));
                    body
                },
            ),
        ))
    })
}

fn summarize_terminal_terminate_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: TerminalTerminateArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "terminal_terminate".to_string(),
        summary: format!("session_id={} terminate_process", args.session_id),
    })
}

fn render_terminal_terminate_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: TerminalTerminateArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::terminal(
        TerminalUiAction::Terminate,
        format!("terminate {}", display_session_label(&args.session_id)),
        Vec::new(),
    ))
}

fn execute_terminal_terminate_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: TerminalTerminateArgs = parse_tool_args(call)?;
        let session = context.devices.terminal_terminate(&args.session_id).await?;
        Ok(ToolExecutionResult::new(
            format!(
                "terminated session {}",
                display_session_label(&session.session_id)
            ),
            json!({ "session": session }),
            ToolUiEvent::terminal(
                TerminalUiAction::Terminate,
                format!("terminated {}", display_session_label(&session.session_id)),
                vec![format!(
                    "{}  {}  exit={}",
                    display_session_label(&session.session_id),
                    session.status,
                    session
                        .exit_code
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".to_string())
                )],
            ),
        ))
    })
}
