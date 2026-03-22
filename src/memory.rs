//! 此模块定义记忆
//!
//! 记忆当前只保留一层近场 working memory:
//!
//! - L1: 最近几步的输入/输出消息流
//!   - 每一步只记录 assistant/tool 历史消息
use std::{collections::VecDeque, fmt::Display};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    get_spinova_home,
    hindsight::{HindsightRetainItem, HindsightRetainJob},
    reasoning::runtime::PromptMessage,
};

pub struct Memory {
    l1: L1Memory,
    queued_retain_ids: std::collections::HashSet<Uuid>,
    retained_ids: std::collections::HashSet<Uuid>,
}

pub struct MemoryRetainPlan {
    pub jobs: Vec<HindsightRetainJob>,
    pub must_flush_before_continue: bool,
}

impl Memory {
    pub async fn new() -> Self {
        let l1 = L1Memory::new().await;
        let retained_ids = l1.trail.iter().map(|item| item.id).collect();
        Self {
            l1,
            queued_retain_ids: std::collections::HashSet::new(),
            retained_ids,
        }
    }

    pub async fn empty() -> Self {
        Self {
            l1: L1Memory::default(),
            queued_retain_ids: std::collections::HashSet::new(),
            retained_ids: std::collections::HashSet::new(),
        }
    }

    pub async fn record_agent_turn(
        &mut self,
        current_doing: String,
        messages: Vec<PromptMessage>,
        retain_text: String,
    ) -> MemoryRetainPlan {
        let _ = self
            .l1
            .update_messages(current_doing, messages, retain_text);
        let jobs = self.collect_retain_jobs();
        let must_flush_before_continue = self.front_is_pending_retain();
        MemoryRetainPlan {
            jobs,
            must_flush_before_continue,
        }
    }

    pub fn current_thread_focus(&self) -> Option<String> {
        self.l1.trail.back().map(|item| item.current_doing.clone())
    }

    pub fn trail(&self) -> Vec<String> {
        self.l1
            .trail
            .clone()
            .into_iter()
            .flat_map(|item| item.render_messages())
            .collect()
    }

    pub fn prompt_messages(&self) -> Vec<PromptMessage> {
        self.l1
            .trail
            .iter()
            .flat_map(|item| item.prompt_messages())
            .collect()
    }

    pub fn mark_pending_retained(&mut self) {
        self.retained_ids.extend(self.queued_retain_ids.drain());
        self.compact_l1();
    }

    pub async fn shutdown(mut self) {
        self.mark_pending_retained();
        self.l1.sync_to_disk().await;
    }

    fn collect_retain_jobs(&mut self) -> Vec<HindsightRetainJob> {
        let mut jobs = Vec::new();
        for item in self.l1.retention_candidates() {
            if self.retained_ids.contains(&item.id) || self.queued_retain_ids.contains(&item.id) {
                continue;
            }
            self.queued_retain_ids.insert(item.id);
            jobs.push(HindsightRetainJob {
                items: vec![item.to_hindsight_item()],
                document_id: Some(format!("l1-step:{}", item.id)),
                document_tags: vec!["spinova".to_string(), "l1-step".to_string()],
            });
        }
        jobs
    }

    fn front_is_pending_retain(&self) -> bool {
        self.l1
            .trail
            .front()
            .map(|item| {
                self.queued_retain_ids.contains(&item.id) && !self.retained_ids.contains(&item.id)
            })
            .unwrap_or(false)
    }

    fn compact_l1(&mut self) {
        while self.l1.trail.len() > L1Memory::MAX_CAPACITY {
            let can_drop = self
                .l1
                .trail
                .front()
                .map(|item| self.retained_ids.contains(&item.id))
                .unwrap_or(true);
            if !can_drop {
                break;
            }
            if let Some(item) = self.l1.trail.pop_front() {
                self.retained_ids.remove(&item.id);
                self.queued_retain_ids.remove(&item.id);
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct L1Memory {
    trail: VecDeque<L1Item>,
}

#[derive(Clone, Serialize, Deserialize)]
struct L1Item {
    id: Uuid,
    current_doing: String,
    retain_text: String,
    messages: Vec<PromptMessage>,
}

impl Display for L1Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.render_for_memory())
    }
}

impl L1Item {
    fn render_for_memory(&self) -> String {
        self.messages
            .iter()
            .map(format_message_for_memory)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_messages(&self) -> Vec<String> {
        self.messages
            .iter()
            .map(format_message_for_memory)
            .collect()
    }

    fn prompt_messages(&self) -> Vec<PromptMessage> {
        self.messages.clone()
    }

    fn to_hindsight_item(&self) -> HindsightRetainItem {
        HindsightRetainItem {
            content: self.retain_text.clone(),
            timestamp: None,
            context: Some("runtime raw l1 step".to_string()),
            metadata: Some(std::collections::HashMap::from([
                ("current_doing".to_string(), self.current_doing.clone()),
                ("entry_id".to_string(), self.id.to_string()),
            ])),
            document_id: Some(format!("l1-step:{}", self.id)),
            tags: Some(vec!["spinova".to_string(), "l1-step".to_string()]),
        }
    }
}

impl L1Memory {
    // Raw step-level history needs a wider working window than the old
    // summarized memory entries, otherwise recently relevant context
    // rolls out before the long-term retain queue can absorb it smoothly.
    const MAX_CAPACITY: usize = 24;
    const RETAIN_GUARD_REGION: usize = 8;

    async fn new() -> Self {
        let l1_persistence_path = get_spinova_home().await.join("l1_memory");
        tokio::fs::read(l1_persistence_path)
            .await
            .ok()
            .and_then(|data| postcard::from_bytes::<Self>(&data).ok())
            .unwrap_or_default()
    }

    fn update_messages(
        &mut self,
        current_doing: String,
        messages: Vec<PromptMessage>,
        retain_text: String,
    ) -> Option<L1Item> {
        let item = L1Item {
            id: Uuid::new_v4(),
            current_doing,
            retain_text,
            messages,
        };
        self.trail.push_back(item);
        None
    }
    async fn sync_to_disk(&self) {
        let l1_persistence_path = get_spinova_home().await.join("l1_memory");
        let data = postcard::to_allocvec(self).unwrap();
        tokio::fs::write(l1_persistence_path, data).await.unwrap();
    }

    fn retention_candidates(&self) -> Vec<&L1Item> {
        let retention_cutoff = self.trail.len().saturating_sub(Self::RETAIN_GUARD_REGION);
        self.trail.iter().take(retention_cutoff).collect()
    }
}

fn format_message_for_memory(message: &PromptMessage) -> String {
    let role = match message.role {
        crate::reasoning::runtime::PromptRole::System => "system",
        crate::reasoning::runtime::PromptRole::User => "user",
        crate::reasoning::runtime::PromptRole::Assistant => "assistant",
        crate::reasoning::runtime::PromptRole::Tool => "tool",
    };
    let mut parts = Vec::new();
    if !message.content.trim().is_empty() {
        parts.push(message.content.clone());
    }
    if !message.tool_call_ui_events.is_empty() {
        let rendered = message
            .tool_call_ui_events
            .iter()
            .map(format_tool_call_ui_event_for_memory)
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(rendered);
    }
    format!("{role}:\n{}", parts.join("\n"))
}

fn format_tool_call_ui_event_for_memory(event: &crate::tool_ui::ToolCallUiEvent) -> String {
    match event {
        crate::tool_ui::ToolCallUiEvent::Exec(data)
        | crate::tool_ui::ToolCallUiEvent::Work(data)
        | crate::tool_ui::ToolCallUiEvent::Device(data)
        | crate::tool_ui::ToolCallUiEvent::Error(data) => {
            let mut lines = vec![format!("tool_call: {}", data.title)];
            lines.extend(data.body_lines.iter().map(|line| format!("  {line}")));
            lines.join("\n")
        }
        crate::tool_ui::ToolCallUiEvent::Telegram(data) => {
            let mut lines = vec![format!("tool_call: {}", data.title)];
            lines.extend(data.detail_lines.iter().map(|line| format!("  {line}")));
            lines.extend(data.message_lines.iter().map(|line| format!("  {line}")));
            lines.join("\n")
        }
        crate::tool_ui::ToolCallUiEvent::Terminal(data) => {
            let mut lines = vec![format!("tool_call: {}", data.title)];
            lines.extend(data.body_lines.iter().map(|line| format!("  {line}")));
            lines.join("\n")
        }
        crate::tool_ui::ToolCallUiEvent::Patch(data) => {
            let mut lines = vec![
                format!("tool_call: {}", data.title),
                format!("  {}", data.summary_line),
            ];
            lines.extend(data.files.iter().map(|file| {
                let marker = match file.operation.as_str() {
                    "add" => "+",
                    "delete" => "-",
                    _ => "~",
                };
                format!(
                    "  {marker} {} (+{} -{})",
                    file.path, file.added_lines, file.removed_lines
                )
            }));
            lines.join("\n")
        }
    }
}
