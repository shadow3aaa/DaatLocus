//! 本模块定义快照，即LLM应当看到的输入。

use std::collections::HashSet;
use std::fmt::Display;

use crate::{
    app::{AppId, AppStateRender},
    context::Context,
    context_budget::truncate_text_to_token_budget,
    events::{EventPayload, EventStatus, EventStore, EventView},
    plan::Plan,
    system_info::SystemInfo,
};

const SNAPSHOT_SENSORY_MAX_TOKENS: usize = 400;
const SNAPSHOT_PLAN_MAX_TOKENS: usize = 1_600;
const SNAPSHOT_EVENTS_MAX_TOKENS: usize = 1_800;
const SNAPSHOT_PLAN_MAX_ITEMS: usize = 8;
const SNAPSHOT_EVENT_MAX_ITEMS: usize = 8;
const SNAPSHOT_APP_LINES_PER_APP: usize = 8;

/// 快照保存着当前agent的大脑状态
///
/// 这包括 plan、事件和感官输入。
pub struct Snapshot {
    sensory: Sensory,
    plan: Plan,
    events: EventSnapshot,
    apps: AppSnapshot,
}

#[derive(Clone)]
pub struct SnapshotAppStateEntry {
    pub app_id: String,
    pub title: String,
    pub lines: Vec<String>,
}

impl Snapshot {
    pub async fn new(context: &mut Context) -> Self {
        Self::new_with_claimed_events(context, &[]).await
    }

    pub async fn new_with_claimed_events(
        context: &mut Context,
        claimed_events: &[EventView],
    ) -> Self {
        let apps = AppSnapshot::new(context);
        Self {
            sensory: Sensory::new(),
            plan: context.plan.clone(),
            events: EventSnapshot::new(&context.events, claimed_events),
            apps,
        }
    }

    pub fn sensory_runtime_text(&self) -> String {
        truncate_text_to_token_budget(&self.sensory.to_string(), SNAPSHOT_SENSORY_MAX_TOKENS)
    }

    pub fn plan_runtime_text(&self) -> String {
        let steps = self.plan.steps();
        if steps.is_empty() {
            return "当前没有 plan。".to_string();
        }

        let omitted = steps.len().saturating_sub(SNAPSHOT_PLAN_MAX_ITEMS);
        let mut lines = Vec::new();
        for (index, step) in steps.iter().take(SNAPSHOT_PLAN_MAX_ITEMS).enumerate() {
            if index > 0 {
                lines.push(String::new());
            }
            lines.push(format!("{}. [{}] {}", index + 1, step.status, step.step));
        }
        if omitted > 0 {
            lines.push(String::new());
            lines.push(format!("... 还有 {omitted} 个 plan 未展示"));
        }
        truncate_text_to_token_budget(&lines.join("\n"), SNAPSHOT_PLAN_MAX_TOKENS)
    }

    pub fn events_runtime_text(&self) -> String {
        self.events
            .render_runtime(SNAPSHOT_EVENT_MAX_ITEMS, SNAPSHOT_EVENTS_MAX_TOKENS)
    }

    pub fn focused_app_runtime_text(&self) -> String {
        self.apps.focused_runtime_text()
    }

    pub fn app_state_entries(&self) -> Vec<SnapshotAppStateEntry> {
        self.apps.app_state_entries(SNAPSHOT_APP_LINES_PER_APP)
    }
}

impl Display for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "感官：")?;
        writeln!(f, "{}", self.sensory)?;
        writeln!(f, "Plan：")?;
        writeln!(f, "{}", self.plan)?;
        writeln!(f, "事件列表：")?;
        writeln!(f, "{}", self.events)?;
        writeln!(f, "应用：")?;
        write!(f, "{}", self.apps)
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

struct AppSnapshot {
    focused_app: Option<AppId>,
    states: Vec<(AppId, AppStateRender)>,
}

struct EventSnapshot {
    events: Vec<EventView>,
}

impl AppSnapshot {
    fn new(context: &Context) -> Self {
        Self {
            focused_app: context.apps.focused(),
            states: context.apps.state_renders(),
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

impl Display for AppSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.focused_app.as_ref() {
            Some(app) => writeln!(f, "当前前景应用：{app}")?,
            None => writeln!(f, "当前前景应用：无")?,
        }

        let attention_hints = self
            .states
            .iter()
            .filter(|(id, _)| self.focused_app.as_ref() != Some(id))
            .filter_map(|(id, state)| app_attention_hint(id.clone(), state));
        let attention_hints = attention_hints.collect::<Vec<_>>();
        if !attention_hints.is_empty() {
            writeln!(f, "后台应用提醒：")?;
            for hint in attention_hints {
                writeln!(f, "- {hint}")?;
            }
        }

        writeln!(f, "应用结构状态：")?;
        for (id, state) in &self.states {
            writeln!(f, "- {id} / {}：", state.title)?;
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
                        "  last_error={}",
                        event
                            .last_error
                            .as_deref()
                            .map(summarize_inline_text)
                            .unwrap_or_else(|| "<none>".to_string())
                    )?;
                }
            }
        }

        Ok(())
    }
}

impl AppSnapshot {
    fn focused_runtime_text(&self) -> String {
        match self.focused_app.as_ref() {
            Some(app) => app.to_string(),
            None => "无".to_string(),
        }
    }

    fn app_state_entries(&self, max_lines_per_device: usize) -> Vec<SnapshotAppStateEntry> {
        let Some(focused) = self.focused_app.as_ref() else {
            return Vec::new();
        };

        self.states
            .iter()
            .filter(|(id, _)| id == focused)
            .map(|(id, state)| {
                let mut lines = state
                    .lines
                    .iter()
                    .take(max_lines_per_device)
                    .cloned()
                    .collect::<Vec<_>>();
                let omitted = state.lines.len().saturating_sub(max_lines_per_device);
                if omitted > 0 {
                    lines.push(format!("... 还有 {omitted} 行未展示"));
                }
                SnapshotAppStateEntry {
                    app_id: id.to_string(),
                    title: state.title.clone(),
                    lines,
                }
            })
            .collect()
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

fn app_attention_hint(app_id: AppId, state: &AppStateRender) -> Option<String> {
    if app_id.is_terminal() {
        let session_id = state
            .lines
            .iter()
            .find_map(|line| line.strip_prefix("session="))
            .and_then(|line| line.split_whitespace().next())
            .unwrap_or("unknown");
        if list_field(&state.lines, "unread_sessions").is_empty() {
            None
        } else {
            Some(format!("Terminal 会话 {session_id} 有未读输出"))
        }
    } else {
        None
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

fn list_field(lines: &[String], key: &str) -> Vec<String> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .map(|value| {
            if value == "none" {
                Vec::new()
            } else {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(ToString::to_string)
                    .collect()
            }
        })
        .unwrap_or_default()
}
