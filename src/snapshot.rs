//! 本模块定义快照，即LLM应当看到的输入。

use std::collections::HashSet;
use std::fmt::Display;

use crate::{
    context::Context,
    context_budget::truncate_text_to_token_budget,
    device::{AttentionLevel, DeviceId, DeviceStateRender},
    events::{EventPayload, EventStatus, EventStore, EventView},
    system_info::SystemInfo,
    todo_board::TodoBoard,
    work_state::WorkState,
};

const SNAPSHOT_SENSORY_MAX_TOKENS: usize = 400;
const SNAPSHOT_TODO_MAX_TOKENS: usize = 1_600;
const SNAPSHOT_WORK_STATE_MAX_TOKENS: usize = 700;
const SNAPSHOT_EVENTS_MAX_TOKENS: usize = 1_800;
const SNAPSHOT_DEVICES_MAX_TOKENS: usize = 1_600;
const SNAPSHOT_TODO_MAX_ITEMS: usize = 8;
const SNAPSHOT_EVENT_MAX_ITEMS: usize = 8;
const SNAPSHOT_DEVICE_HINT_MAX_ITEMS: usize = 4;
const SNAPSHOT_DEVICE_LINES_PER_DEVICE: usize = 8;

/// 快照保存着当前agent的大脑状态
///
/// 这包括 todo、当前工作状态和感官输入。
pub struct Snapshot {
    sensory: Sensory,
    todo_board: TodoBoard,
    work_state: WorkState,
    events: EventSnapshot,
    devices: DeviceSnapshot,
}

impl Snapshot {
    pub async fn new(context: &mut Context) -> Self {
        Self::new_with_claimed_events(context, &[]).await
    }

    pub async fn new_with_claimed_events(
        context: &mut Context,
        claimed_events: &[EventView],
    ) -> Self {
        let devices = DeviceSnapshot::new(context);
        Self {
            sensory: Sensory::new(),
            todo_board: context.todo_board.clone(),
            work_state: context.work_state.clone(),
            events: EventSnapshot::new(&context.events, claimed_events),
            devices,
        }
    }

    pub fn to_runtime_text(&self) -> String {
        [
            ("感官：", self.render_sensory_runtime()),
            ("TodoBoard：", self.render_todo_board_runtime()),
            ("当前工作状态：", self.render_work_state_runtime()),
            ("事件列表：", self.render_events_runtime()),
            ("设备：", self.render_devices_runtime()),
        ]
        .into_iter()
        .map(|(title, body)| format!("{title}\n{body}"))
        .collect::<Vec<_>>()
        .join("\n")
    }

    fn render_sensory_runtime(&self) -> String {
        truncate_text_to_token_budget(&self.sensory.to_string(), SNAPSHOT_SENSORY_MAX_TOKENS)
    }

    fn render_todo_board_runtime(&self) -> String {
        let mut items = self.todo_board.active_items().collect::<Vec<_>>();
        if items.is_empty() {
            return "当前没有 todo。".to_string();
        }

        items.sort_by(|left, right| {
            right
                .1
                .last_updated_at_ms
                .cmp(&left.1.last_updated_at_ms)
                .then_with(|| left.0.cmp(&right.0))
        });

        let omitted = items.len().saturating_sub(SNAPSHOT_TODO_MAX_ITEMS);
        let mut lines = Vec::new();
        for (index, (id, item)) in items.into_iter().take(SNAPSHOT_TODO_MAX_ITEMS).enumerate() {
            if index > 0 {
                lines.push(String::new());
            }
            lines.push(format!(
                "- {id}. [{} / {}] {}",
                item.status, item.origin, item.title
            ));
            lines.push(format!(
                "  完成标准：{}",
                summarize_inline_text(&item.done_criteria)
            ));
            if let Some(notes) = item.notes.as_deref()
                && !notes.trim().is_empty()
            {
                lines.push(format!("  备注：{}", summarize_inline_text(notes)));
            }
        }
        if omitted > 0 {
            lines.push(String::new());
            lines.push(format!("... 还有 {omitted} 个 todo 未展示"));
        }
        truncate_text_to_token_budget(&lines.join("\n"), SNAPSHOT_TODO_MAX_TOKENS)
    }

    fn render_work_state_runtime(&self) -> String {
        truncate_text_to_token_budget(&self.work_state.to_string(), SNAPSHOT_WORK_STATE_MAX_TOKENS)
    }

    fn render_events_runtime(&self) -> String {
        self.events
            .render_runtime(SNAPSHOT_EVENT_MAX_ITEMS, SNAPSHOT_EVENTS_MAX_TOKENS)
    }

    fn render_devices_runtime(&self) -> String {
        self.devices.render_runtime(
            SNAPSHOT_DEVICE_HINT_MAX_ITEMS,
            SNAPSHOT_DEVICE_LINES_PER_DEVICE,
            SNAPSHOT_DEVICES_MAX_TOKENS,
        )
    }
}

impl Display for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "感官：")?;
        writeln!(f, "{}", self.sensory)?;
        writeln!(f, "TodoBoard：")?;
        writeln!(f, "{}", self.todo_board)?;
        writeln!(f, "当前工作状态：")?;
        writeln!(f, "{}", self.work_state)?;
        writeln!(f, "事件列表：")?;
        writeln!(f, "{}", self.events)?;
        writeln!(f, "设备：")?;
        write!(f, "{}", self.devices)
    }
}

struct Sensory {
    time: String,
    machine_status: SystemInfo,
}

impl Sensory {
    fn new() -> Self {
        let local = chrono::Local::now();
        let time = local.format("%Y-%m-%d %H:%M:%S %z").to_string();
        let machine_status = SystemInfo::sample();
        Self {
            time,
            machine_status,
        }
    }
}

impl Display for Sensory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "当前时间：{}", self.time)?;
        write!(f, "机器状态：\n{}", self.machine_status)
    }
}

struct DeviceSnapshot {
    focused_device: Option<DeviceId>,
    states: Vec<(DeviceId, DeviceStateRender)>,
}

struct EventSnapshot {
    events: Vec<EventView>,
}

impl DeviceSnapshot {
    fn new(context: &Context) -> Self {
        Self {
            focused_device: context.devices.focused(),
            states: context.devices.state_renders(),
        }
    }
}

impl EventSnapshot {
    fn new(events: &EventStore, claimed_events: &[EventView]) -> Self {
        let mut merged = Vec::new();
        let mut seen = HashSet::new();

        for event in claimed_events {
            if seen.insert(event.event_id) {
                merged.push(event.clone());
            }
        }
        for event in events.attention_events() {
            if seen.insert(event.event_id) {
                merged.push(event);
            }
        }

        Self { events: merged }
    }
}

impl Display for DeviceSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.focused_device {
            Some(device) => writeln!(f, "当前前景设备：{device}")?,
            None => writeln!(f, "当前前景设备：无")?,
        }

        let attention_hints = self
            .states
            .iter()
            .filter(|(_, state)| !state.is_focused)
            .filter_map(|(id, state)| device_attention_hint(*id, state));
        let attention_hints = attention_hints.collect::<Vec<_>>();
        if !attention_hints.is_empty() {
            writeln!(f, "后台设备提醒：")?;
            for hint in attention_hints {
                writeln!(f, "- {hint}")?;
            }
        }

        writeln!(f, "设备结构状态：")?;
        for (id, state) in &self.states {
            let focus_state = if state.is_focused { "前景" } else { "后台" };
            writeln!(
                f,
                "- {id} / {}：{}，注意力等级={}",
                state.title, focus_state, state.attention
            )?;
            for line in &state.lines {
                writeln!(f, "  {line}")?;
            }
        }
        Ok(())
    }
}

impl Display for EventSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.events.is_empty() {
            return write!(f, "当前没有待处理事件。");
        }

        for (index, event) in self.events.iter().enumerate() {
            if index > 0 {
                writeln!(f)?;
            }
            match &event.payload {
                EventPayload::TelegramIncoming(payload) => {
                    writeln!(
                        f,
                        "- {}. [{} / {}] {} @ {} (chat_id={}): {}",
                        event.event_id,
                        event.source,
                        event.status,
                        payload.sender,
                        payload.chat_title,
                        payload.chat_id,
                        summarize_inline_text(&payload.incoming_text)
                    )?;
                    writeln!(
                        f,
                        "  latest_outgoing={}",
                        payload
                            .latest_outgoing_preview
                            .as_deref()
                            .map(summarize_inline_text)
                            .unwrap_or_else(|| "<none>".to_string())
                    )?;
                    if let Some(error) = event.last_error.as_deref() {
                        writeln!(f, "  last_error={}", summarize_inline_text(error))?;
                    }
                }
            }
        }

        Ok(())
    }
}

impl DeviceSnapshot {
    fn render_runtime(
        &self,
        max_hints: usize,
        max_lines_per_device: usize,
        max_tokens: usize,
    ) -> String {
        let mut lines = Vec::new();
        match self.focused_device {
            Some(device) => lines.push(format!("当前前景设备：{device}")),
            None => lines.push("当前前景设备：无".to_string()),
        }

        let attention_hints = self
            .states
            .iter()
            .filter(|(_, state)| !state.is_focused)
            .filter_map(|(id, state)| device_attention_hint(*id, state))
            .take(max_hints)
            .collect::<Vec<_>>();
        if !attention_hints.is_empty() {
            lines.push("后台设备提醒：".to_string());
            lines.extend(attention_hints.into_iter().map(|hint| format!("- {hint}")));
        }

        lines.push("设备结构状态：".to_string());
        for (id, state) in &self.states {
            let focus_state = if state.is_focused { "前景" } else { "后台" };
            lines.push(format!(
                "- {id} / {}：{}，注意力等级={}",
                state.title, focus_state, state.attention
            ));
            let rendered_lines = state.lines.iter().take(max_lines_per_device);
            lines.extend(rendered_lines.map(|line| format!("  {line}")));
            let omitted = state.lines.len().saturating_sub(max_lines_per_device);
            if omitted > 0 {
                lines.push(format!("  ... 还有 {omitted} 行未展示"));
            }
        }

        truncate_text_to_token_budget(&lines.join("\n"), max_tokens)
    }
}

impl EventSnapshot {
    fn render_runtime(&self, max_items: usize, max_tokens: usize) -> String {
        if self.events.is_empty() {
            return "当前没有待处理事件。".to_string();
        }

        let omitted = self.events.len().saturating_sub(max_items);
        let mut lines = Vec::new();
        if self
            .events
            .iter()
            .any(|event| matches!(event.status, EventStatus::Claimed))
        {
            lines.push(
                "提交提示：当前存在已领取事件。你输出的文本回复不会自动发给用户；只有显式调用 `finish_and_send` 并提供 `reply_message`，才会真正提交最终答复。".to_string(),
            );
            lines.push(String::new());
        }
        for (index, event) in self.events.iter().take(max_items).enumerate() {
            if index > 0 {
                lines.push(String::new());
            }
            match &event.payload {
                EventPayload::TelegramIncoming(payload) => {
                    lines.push(format!(
                        "- {}. [{} / {}] {} @ {} (chat_id={}): {}",
                        event.event_id,
                        event.source,
                        event.status,
                        payload.sender,
                        payload.chat_title,
                        payload.chat_id,
                        summarize_inline_text(&payload.incoming_text)
                    ));
                    lines.push(format!(
                        "  latest_outgoing={}",
                        payload
                            .latest_outgoing_preview
                            .as_deref()
                            .map(summarize_inline_text)
                            .unwrap_or_else(|| "<none>".to_string())
                    ));
                    if let Some(error) = event.last_error.as_deref() {
                        lines.push(format!("  last_error={}", summarize_inline_text(error)));
                    }
                }
            }
        }
        if omitted > 0 {
            lines.push(String::new());
            lines.push(format!("... 还有 {omitted} 个事件未展示"));
        }
        truncate_text_to_token_budget(&lines.join("\n"), max_tokens)
    }
}

fn device_attention_hint(device_id: DeviceId, state: &DeviceStateRender) -> Option<String> {
    match device_id {
        DeviceId::Terminal
            if !state.is_focused && matches!(state.attention, AttentionLevel::Notice) =>
        {
            let session_id = state
                .lines
                .iter()
                .find_map(|line| line.strip_prefix("active_session="))
                .unwrap_or("unknown");
            if numeric_field(&state.lines, "sessions_with_unread_output") > 0 {
                Some(format!("Terminal 会话 {session_id} 有未读输出"))
            } else {
                Some(format!("Terminal 会话 {session_id} 需要注意"))
            }
        }
        _ => None,
    }
}

fn summarize_inline_text(text: &str) -> String {
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

fn numeric_field(lines: &[String], key: &str) -> usize {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
}
