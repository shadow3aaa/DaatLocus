//! The Actor's working-window anchor and model-specific token calibration.
//! Durable history is never replaced by a summary or deleted during a reset.
use crate::{context_budget::TokenEstimateBaseline, model_protocol::AgentMessage};
use serde::{Deserialize, Serialize};

use super::{MessageKind, MessageRecord};

const RELATED_LIMIT: usize = 8;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentContext {
    pub anchor: Option<HistoryAnchor>,
    pub model_binding: String,
    pub token_estimate: TokenEstimateBaseline,
    #[serde(default)]
    pub usage: crate::core::TokenUsageInfo,
    #[serde(default)]
    pub model_usage: Vec<ModelUsage>,
}

/// The configured model identity at request time, retained after edits/removal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    pub model: String,
    pub provider: String,
    pub model_id: String,
    #[serde(flatten)]
    pub usage: crate::core::TokenUsageInfo,
}

impl ModelUsage {
    pub(crate) fn new(model: &str, config: &crate::config::ModelConfig) -> Self {
        Self {
            model: model.into(),
            provider: config.provider.clone(),
            model_id: config.model_id.clone(),
            usage: Default::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryAnchor {
    /// The execution input whose budget was reset, not necessarily its task root.
    pub message_id: String,
    pub reset_id: uuid::Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<TaskHandoff>,
}

/// Runtime task locators and the Actor's handoff note.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskHandoff {
    pub task_message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
    /// Other directly linked inputs on this same Actor, for scoped follow-up reads.
    pub related_message_ids: Vec<String>,
}

impl TaskHandoff {
    pub(crate) fn from_message(
        current: &MessageRecord,
        mut lookup: impl FnMut(&str) -> Option<MessageRecord>,
    ) -> Self {
        let mut source = current.clone();
        let mut seen = vec![current.id.clone()];
        // Request inputs always start their own task. In particular, never
        // inherit the last anchor or parse a "Current request" ID from prose.
        while matches!(source.kind, MessageKind::Result | MessageKind::Progress)
            && seen.len() <= RELATED_LIMIT
        {
            let Some(next) = source.in_reply_to.as_deref() else {
                break;
            };
            if seen.iter().any(|id| id == next) {
                break;
            }
            let Some(parent) = lookup(next).filter(|parent| parent.actor_id == current.actor_id)
            else {
                break;
            };
            seen.push(parent.id.clone());
            source = parent;
        }
        let resolved = source.kind == MessageKind::Request;
        let task_message_id = if resolved {
            source.id
        } else {
            current.id.clone()
        };
        seen.retain(|id| *id != task_message_id);
        Self {
            task_message_id,
            checkpoint: None,
            related_message_ids: seen,
        }
    }

    fn valid(&self) -> bool {
        valid_id(&self.task_message_id)
            && self
                .checkpoint
                .as_ref()
                .is_none_or(|note| !note.trim().is_empty())
            && self.related_message_ids.len() <= RELATED_LIMIT
            && self.related_message_ids.iter().all(|id| valid_id(id))
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100
}

impl AgentContext {
    pub(crate) fn valid(&self) -> bool {
        self.model_binding.len() <= 64
            && self.usage.valid()
            && self.model_usage.iter().all(|model| model.usage.valid())
            && self.anchor.as_ref().is_none_or(|anchor| {
                valid_id(&anchor.message_id)
                    && !anchor.reset_id.is_nil()
                    && anchor.handoff.as_ref().is_none_or(TaskHandoff::valid)
            })
    }
}

impl HistoryAnchor {
    /// A nested input can reset the Actor-wide window before its caller resumes.
    /// Re-render for the actual execution input; never borrow the other task's note.
    pub(crate) fn for_input(&self, input: &str, handoff: Option<TaskHandoff>) -> Self {
        Self {
            message_id: input.to_owned(),
            reset_id: self.reset_id,
            handoff: if self.message_id == input {
                self.handoff.clone().or(handoff)
            } else {
                handoff
            },
        }
    }

    pub fn recovery_message(&self) -> AgentMessage {
        let mut note = self.handoff.clone().unwrap_or_else(|| TaskHandoff {
            task_message_id: self.message_id.clone(),
            checkpoint: None,
            related_message_ids: Vec::new(),
        });
        let checkpoint = note
            .checkpoint
            .take()
            .map(|text| format!("\n\n{text}"))
            .unwrap_or_default();
        let note = serde_json::to_string(&note).expect("task note JSON");
        // Each reset needs a distinct archived row even when its task note is unchanged.
        AgentMessage::user(format!(
            "The context has been reset. This is a new window.\n\nNow:\n1. Read the task handoff information first: {note} (reset: {}){checkpoint}\n2. Use the history-reading tool to recover details not covered by the checkpoint.\n3. Retrieve earlier user messages and tool results as needed.",
            self.reset_id
        ))
    }
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
