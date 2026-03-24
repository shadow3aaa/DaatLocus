use miette::Result;
use serde_json::json;

use crate::{
    context::Context,
    core::{
        TelegramListChatsArgs, TelegramReadChatArgs, TelegramSelectChatArgs,
    },
    device::DeviceToolScope,
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    tool_ui::{TelegramUiAction, ToolCallUiEvent, ToolUiEvent},
};

use super::{
    RuntimeTool, StaticRuntimeTool, ToolExecutionResult, ToolFuture, parse_tool_args,
    summarize_inline_text,
};

pub(super) fn register_tools() -> Vec<Box<dyn RuntimeTool>> {
    vec![
        Box::new(StaticRuntimeTool::new::<TelegramListChatsArgs>(
            "telegram_list_chats",
            "列出当前已知 Telegram 会话及其结构化状态。",
            Some(DeviceToolScope::Telegram),
            summarize_telegram_list_chats_tool,
            render_telegram_list_chats_call_ui,
            execute_telegram_list_chats_tool,
        )),
        Box::new(StaticRuntimeTool::new::<TelegramReadChatArgs>(
            "telegram_read_chat",
            "读取一个 Telegram 会话的最近消息；chat_id 为空时默认读取当前已打开会话。",
            Some(DeviceToolScope::Telegram),
            summarize_telegram_read_chat_tool,
            render_telegram_read_chat_call_ui,
            execute_telegram_read_chat_tool,
        )),
        Box::new(StaticRuntimeTool::new::<TelegramSelectChatArgs>(
            "telegram_select_chat",
            "打开 Telegram 的指定会话。",
            Some(DeviceToolScope::Telegram),
            summarize_telegram_select_chat_tool,
            render_telegram_select_chat_call_ui,
            execute_telegram_select_chat_tool,
        )),
    ]
}

fn render_telegram_list_chats_call_ui(_call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    Ok(ToolCallUiEvent::telegram(
        TelegramUiAction::ListChats,
        "telegram_list_chats",
        vec!["list known chats".to_string()],
        Vec::new(),
    ))
}

fn summarize_telegram_list_chats_tool(_call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    Ok(EpisodeActionRecord {
        kind: "telegram_list_chats".to_string(),
        summary: "list telegram chats".to_string(),
    })
}

fn execute_telegram_list_chats_tool<'a>(
    context: &'a mut Context,
    _call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let chats = context.telegram.list_chat_summaries();
        Ok(ToolExecutionResult::new(
            format!("listed {} telegram chat(s)", chats.len()),
            json!({ "chats": chats }),
            ToolUiEvent::telegram(
                TelegramUiAction::ListChats,
                format!("listed {} telegram chat(s)", chats.len()),
                chats
                    .iter()
                    .take(8)
                    .map(|line| summarize_chat_summary_line(line))
                    .collect(),
                Vec::new(),
            ),
        ))
    })
}

fn summarize_telegram_read_chat_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: TelegramReadChatArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "telegram_read_chat".to_string(),
        summary: format!(
            "chat_id={} max_messages={}",
            args.chat_id.unwrap_or_else(|| "selected".to_string()),
            args.max_messages
                .map(|value| value.to_string())
                .unwrap_or_else(|| "default".to_string())
        ),
    })
}

fn render_telegram_read_chat_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: TelegramReadChatArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::telegram(
        TelegramUiAction::ReadChat,
        format!(
            "telegram_read_chat {}",
            args.chat_id
                .clone()
                .unwrap_or_else(|| "selected".to_string())
        ),
        vec![format!(
            "max_messages={}",
            args.max_messages
                .map(|value| value.to_string())
                .unwrap_or_else(|| "default".to_string())
        )],
        Vec::new(),
    ))
}

fn execute_telegram_read_chat_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: TelegramReadChatArgs = parse_tool_args(call)?;
        let content = context
            .telegram
            .read_chat(args.chat_id.as_deref(), args.max_messages)?;
        let chat_label = args
            .chat_id
            .clone()
            .unwrap_or_else(|| "selected".to_string());
        let (title, detail_lines, message_lines) =
            summarize_read_chat_content(&chat_label, &content);
        Ok(ToolExecutionResult::new(
            format!("read telegram chat {}", chat_label),
            json!({
                "chat_id": args.chat_id,
                "max_messages": args.max_messages,
                "content": content,
            }),
            ToolUiEvent::telegram(
                TelegramUiAction::ReadChat,
                title,
                detail_lines,
                message_lines,
            ),
        ))
    })
}

fn summarize_telegram_select_chat_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: TelegramSelectChatArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "telegram_select_chat".to_string(),
        summary: format!("chat_id={}", args.chat_id),
    })
}

fn render_telegram_select_chat_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: TelegramSelectChatArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::telegram(
        TelegramUiAction::SelectChat,
        format!("telegram_select_chat {}", args.chat_id),
        vec![format!("chat_id={}", args.chat_id)],
        Vec::new(),
    ))
}

fn execute_telegram_select_chat_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: TelegramSelectChatArgs = parse_tool_args(call)?;
        context
            .devices
            .telegram_select_chat(args.chat_id.clone())
            .await?;
        Ok(ToolExecutionResult::new(
            format!("selected telegram chat {}", args.chat_id),
            json!({ "chat_id": args.chat_id }),
            ToolUiEvent::telegram(
                TelegramUiAction::SelectChat,
                format!("selected telegram chat {}", args.chat_id),
                vec![format!("chat_id={}", args.chat_id)],
                Vec::new(),
            ),
        ))
    })
}

fn summarize_chat_summary_line(line: &str) -> String {
    let chat_id = extract_field(line, "chat_id").unwrap_or("unknown");
    let title = extract_field(line, "title").unwrap_or(chat_id);
    let unread = extract_field(line, "unread").unwrap_or("0");
    let pending_resolution = extract_field(line, "pending_resolution").unwrap_or("false");
    let needs_reply = extract_field(line, "needs_reply").unwrap_or("false");
    format!(
        "{title}  chat_id={chat_id}  unread={unread}  judge={pending_resolution}  reply={needs_reply}"
    )
}

fn summarize_read_chat_content(
    chat_label: &str,
    content: &str,
) -> (String, Vec<String>, Vec<String>) {
    let title = extract_line_value(content, "title")
        .map(|value| format!("telegram {}", value))
        .unwrap_or_else(|| format!("telegram {}", chat_label));
    let detail_lines = [
        extract_line_value(content, "chat_id").map(|value| format!("chat_id={value}")),
        extract_line_value(content, "unread").map(|value| format!("unread={value}")),
        extract_line_value(content, "pending_resolution").map(|value| format!("judge={value}")),
        extract_line_value(content, "needs_reply").map(|value| format!("reply={value}")),
        extract_line_value(content, "latest_incoming_text")
            .map(|value| format!("latest incoming: {}", summarize_inline_text(value))),
        extract_line_value(content, "latest_outgoing_text")
            .map(|value| format!("latest outgoing: {}", summarize_inline_text(value))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let message_lines = content
        .lines()
        .filter_map(summarize_chat_message_line)
        .take(6)
        .collect::<Vec<_>>();
    (title, detail_lines, message_lines)
}

fn summarize_chat_message_line(line: &str) -> Option<String> {
    let body = line.strip_prefix("- [")?;
    let (meta, rest) = body.split_once("] ")?;
    let (sender, text) = rest.split_once(": ")?;
    let direction = meta.split('|').next()?;
    let prefix = match direction {
        "incoming" => "in ",
        "outgoing" => "out",
        _ => "msg",
    };
    Some(format!(
        "{prefix}  {sender}: {}",
        summarize_inline_text(text)
    ))
}

fn extract_line_value<'a>(content: &'a str, key: &str) -> Option<&'a str> {
    content
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
}

fn extract_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("{key}=");
    let start = line.find(&marker)? + marker.len();
    let rest = &line[start..];
    let end = rest.find(' ').unwrap_or(rest.len());
    Some(&rest[..end])
}
