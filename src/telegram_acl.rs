use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use miette::{Result, miette};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::get_spinova_home;

const TELEGRAM_ACL_FILE_NAME: &str = "telegram_acl.json";

#[derive(Clone)]
pub struct TelegramAclHandle {
    inner: Arc<Mutex<TelegramAclInner>>,
}

struct TelegramAclInner {
    path: PathBuf,
    state: TelegramAclState,
}

#[derive(Default, Serialize, Deserialize)]
struct TelegramAclState {
    approved: HashSet<i64>,
    #[serde(default)]
    approved_meta: BTreeMap<i64, ApprovedChat>,
    blocked: HashSet<i64>,
    pending: BTreeMap<i64, PendingAccessRequest>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ApprovedChat {
    pub chat_id: i64,
    pub title: String,
    pub sender: String,
    pub last_message_preview: String,
    pub approved_at_ms: i64,
    pub last_seen_at_ms: i64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PendingAccessRequest {
    pub chat_id: i64,
    pub title: String,
    pub sender: String,
    pub last_message_preview: String,
    pub first_seen_at_ms: i64,
    pub last_seen_at_ms: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AccessDecision {
    Approved,
    Blocked,
    Unknown,
}

impl TelegramAclHandle {
    pub async fn load() -> Self {
        let path = get_spinova_home().await.join(TELEGRAM_ACL_FILE_NAME);
        let state = tokio::fs::read(&path)
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<TelegramAclState>(&bytes).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(Mutex::new(TelegramAclInner { path, state })),
        }
    }

    pub fn classify(&self, chat_id: i64) -> AccessDecision {
        let inner = self.inner.lock();
        if inner.state.approved.contains(&chat_id) {
            AccessDecision::Approved
        } else if inner.state.blocked.contains(&chat_id) {
            AccessDecision::Blocked
        } else {
            AccessDecision::Unknown
        }
    }

    pub fn register_pending(
        &self,
        chat_id: i64,
        title: impl Into<String>,
        sender: impl Into<String>,
        last_message_preview: impl Into<String>,
        seen_at_ms: i64,
    ) -> Result<()> {
        let mut inner = self.inner.lock();
        if inner.state.approved.contains(&chat_id) || inner.state.blocked.contains(&chat_id) {
            return Ok(());
        }

        let title = title.into();
        let sender = sender.into();
        let last_message_preview = last_message_preview.into();
        inner
            .state
            .pending
            .entry(chat_id)
            .and_modify(|request| {
                request.title = title.clone();
                request.sender = sender.clone();
                request.last_message_preview = last_message_preview.clone();
                request.last_seen_at_ms = seen_at_ms;
            })
            .or_insert_with(|| PendingAccessRequest {
                chat_id,
                title,
                sender,
                last_message_preview,
                first_seen_at_ms: seen_at_ms,
                last_seen_at_ms: seen_at_ms,
            });
        persist_locked(&inner)
    }

    pub fn pending_requests(&self) -> Vec<PendingAccessRequest> {
        let inner = self.inner.lock();
        let mut requests = inner.state.pending.values().cloned().collect::<Vec<_>>();
        requests.sort_by(|left, right| {
            right
                .last_seen_at_ms
                .cmp(&left.last_seen_at_ms)
                .then_with(|| left.chat_id.cmp(&right.chat_id))
        });
        requests
    }

    pub fn approved_chats(&self) -> Vec<ApprovedChat> {
        let inner = self.inner.lock();
        let mut chats = inner
            .state
            .approved_meta
            .values()
            .cloned()
            .collect::<Vec<_>>();
        chats.sort_by(|left, right| {
            right
                .last_seen_at_ms
                .cmp(&left.last_seen_at_ms)
                .then_with(|| left.chat_id.cmp(&right.chat_id))
        });
        chats
    }

    pub fn approve(&self, chat_id: i64) -> Result<()> {
        let mut inner = self.inner.lock();
        let pending = inner.state.pending.remove(&chat_id);
        inner.state.blocked.remove(&chat_id);
        inner.state.approved.insert(chat_id);
        if let Some(pending) = pending {
            inner.state.approved_meta.insert(
                chat_id,
                ApprovedChat {
                    chat_id,
                    title: pending.title,
                    sender: pending.sender,
                    last_message_preview: pending.last_message_preview,
                    approved_at_ms: pending.last_seen_at_ms,
                    last_seen_at_ms: pending.last_seen_at_ms,
                },
            );
        }
        persist_locked(&inner)
    }

    pub fn reject(&self, chat_id: i64) -> Result<()> {
        let mut inner = self.inner.lock();
        inner.state.pending.remove(&chat_id);
        inner.state.approved.remove(&chat_id);
        inner.state.approved_meta.remove(&chat_id);
        inner.state.blocked.insert(chat_id);
        persist_locked(&inner)
    }

    pub fn observe_approved(
        &self,
        chat_id: i64,
        title: impl Into<String>,
        sender: impl Into<String>,
        last_message_preview: impl Into<String>,
        seen_at_ms: i64,
    ) -> Result<()> {
        let mut inner = self.inner.lock();
        if !inner.state.approved.contains(&chat_id) {
            return Ok(());
        }
        let title = title.into();
        let sender = sender.into();
        let last_message_preview = last_message_preview.into();
        inner
            .state
            .approved_meta
            .entry(chat_id)
            .and_modify(|chat| {
                chat.title = title.clone();
                chat.sender = sender.clone();
                chat.last_message_preview = last_message_preview.clone();
                chat.last_seen_at_ms = seen_at_ms;
            })
            .or_insert(ApprovedChat {
                chat_id,
                title,
                sender,
                last_message_preview,
                approved_at_ms: seen_at_ms,
                last_seen_at_ms: seen_at_ms,
            });
        persist_locked(&inner)
    }
}

fn persist_locked(inner: &TelegramAclInner) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(&inner.state)
        .map_err(|err| miette!("serialize telegram acl failed: {err}"))?;
    std::fs::write(&inner.path, bytes)
        .map_err(|err| miette!("write telegram acl file failed: {err}"))?;
    Ok(())
}
