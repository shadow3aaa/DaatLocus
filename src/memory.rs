//! 此模块定义记忆
//!
//! 记忆当前只保留一层近场 working memory:
//!
//! - L1: 最近几步的原始输入/输出消息流
//!   - 每一步直接记录当时给模型看的 snapshot_text
//!   - 以及模型原始输出中的 observation/description/current_doing/effect
//!   - 不再淘汰进本地 L2/L3
use std::{collections::VecDeque, fmt::Display};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    core::Output,
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

    pub async fn record_runtime_step(
        &mut self,
        snapshot_text: String,
        output: &Output,
    ) -> MemoryRetainPlan {
        let _ = self.l1.update(snapshot_text, output);
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
            .map(|item| self.queued_retain_ids.contains(&item.id) && !self.retained_ids.contains(&item.id))
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
    snapshot_text: String,
    observation: String,
    description: String,
    current_doing: String,
    effect: String,
    #[serde(default)]
    messages: Vec<L1Message>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct L1Message {
    role: L1MessageRole,
    content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum L1MessageRole {
    Snapshot,
    Observation,
    Description,
    Doing,
    Effect,
}

impl Display for L1Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.render_for_memory())
    }
}

impl L1Item {
    const SNAPSHOT_PREVIEW_LIMIT: usize = 1200;
    const TEXT_PREVIEW_LIMIT: usize = 600;

    fn build_messages(snapshot_text: &str, output: &Output, effect: &str) -> Vec<L1Message> {
        vec![
            L1Message {
                role: L1MessageRole::Snapshot,
                content: format!(
                    "输入快照：\n{}",
                    Self::truncate(snapshot_text, Self::SNAPSHOT_PREVIEW_LIMIT)
                ),
            },
            L1Message {
                role: L1MessageRole::Observation,
                content: format!(
                    "模型观察：\n{}",
                    Self::truncate(&output.observation, Self::TEXT_PREVIEW_LIMIT)
                ),
            },
            L1Message {
                role: L1MessageRole::Description,
                content: format!(
                    "模型说明：\n{}",
                    Self::truncate(&output.description, Self::TEXT_PREVIEW_LIMIT)
                ),
            },
            L1Message {
                role: L1MessageRole::Doing,
                content: format!(
                    "当前进行：\n{}",
                    Self::truncate(&output.current_doing, Self::TEXT_PREVIEW_LIMIT)
                ),
            },
            L1Message {
                role: L1MessageRole::Effect,
                content: format!("动作：\n{}", Self::truncate(effect, Self::TEXT_PREVIEW_LIMIT)),
            },
        ]
    }

    fn truncate(text: &str, max_chars: usize) -> String {
        let mut chars = text.chars();
        let preview = chars.by_ref().take(max_chars).collect::<String>();
        if chars.next().is_some() {
            format!("{preview}...")
        } else {
            preview
        }
    }

    fn render_for_memory(&self) -> String {
        self.messages
            .iter()
            .map(|message| message.content.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_messages(&self) -> Vec<String> {
        self.messages
            .iter()
            .map(|message| message.content.clone())
            .collect()
    }

    fn prompt_messages(&self) -> Vec<PromptMessage> {
        vec![
            PromptMessage::user(self.snapshot_text.clone()),
            PromptMessage::assistant(
                serde_json::to_string_pretty(&serde_json::json!({
                    "observation": self.observation,
                    "description": self.description,
                    "current_doing": self.current_doing,
                    "effect": serde_json::from_str::<serde_json::Value>(&self.effect)
                        .unwrap_or_else(|_| serde_json::Value::String(self.effect.clone())),
                }))
                .unwrap(),
            ),
        ]
    }

    fn to_hindsight_item(&self) -> HindsightRetainItem {
        HindsightRetainItem {
            content: format!(
                "L1 raw runtime step\nCurrent doing:\n{}\n\nInput snapshot:\n{}\n\nObservation:\n{}\n\nDescription:\n{}\n\nEffect:\n{}",
                self.current_doing,
                self.snapshot_text,
                self.observation,
                self.description,
                self.effect
            ),
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

    fn update(&mut self, snapshot_text: String, output: &Output) -> Option<L1Item> {
        let effect = serde_json::to_string(&output.effect)
            .unwrap_or_else(|_| format!("{:?}", output.effect));
        let messages = L1Item::build_messages(&snapshot_text, output, &effect);
        let item = L1Item {
            id: Uuid::new_v4(),
            snapshot_text,
            observation: output.observation.clone(),
            description: output.description.clone(),
            current_doing: output.current_doing.clone(),
            effect,
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
