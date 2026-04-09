use std::{collections::VecDeque, path::PathBuf, sync::Arc};

use miette::{Result, miette};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::AppId,
    daat_locus_paths::daat_locus_paths,
};

const PENDING_WORK_FILE_NAME: &str = "pending_work_queue";

#[derive(Clone)]
pub struct PendingWorkQueue {
    inner: Arc<Mutex<PendingWorkQueueInner>>,
}

struct PendingWorkQueueInner {
    path: PathBuf,
    state: PersistedPendingWorkQueue,
}

#[derive(Default, Serialize, Deserialize)]
struct PersistedPendingWorkQueue {
    queue: VecDeque<PendingWorkEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct PendingWorkEntry {
    work: PendingWork,
    state: PendingWorkEntryState,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum PendingWorkEntryState {
    Pending,
    Claimed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PendingWork {
    Event { event_id: Uuid },
    AppNotice { app: AppId, reason: String },
}

impl PartialEq for PendingWork {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Event { event_id: a }, Self::Event { event_id: b }) => a == b,
            (Self::AppNotice { app: a, .. }, Self::AppNotice { app: b, .. }) => a == b,
            _ => false,
        }
    }
}

impl Eq for PendingWork {}

impl PendingWork {
    fn priority(&self) -> u8 {
        match self {
            Self::Event { .. } => 0,
            Self::AppNotice { .. } => 1,
        }
    }
}

impl PendingWorkQueue {
    pub async fn new() -> Self {
        let path = daat_locus_paths().await.state_file(PENDING_WORK_FILE_NAME);
        let mut state = tokio::fs::read(&path)
            .await
            .ok()
            .and_then(|bytes| postcard::from_bytes::<PersistedPendingWorkQueue>(&bytes).ok())
            .unwrap_or_default();
        for entry in &mut state.queue {
            if matches!(entry.state, PendingWorkEntryState::Claimed) {
                entry.state = PendingWorkEntryState::Pending;
            }
        }
        Self {
            inner: Arc::new(Mutex::new(PendingWorkQueueInner { path, state })),
        }
    }

    #[cfg(test)]
    pub fn empty() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PendingWorkQueueInner {
                path: crate::daat_locus_paths::daat_locus_paths_sync().state_file(PENDING_WORK_FILE_NAME),
                state: PersistedPendingWorkQueue::default(),
            })),
        }
    }

    pub fn pending_count(&self) -> usize {
        self.inner
            .lock()
            .state
            .queue
            .iter()
            .filter(|entry| matches!(entry.state, PendingWorkEntryState::Pending))
            .count()
    }

    pub fn enqueue(&self, work: PendingWork) -> Result<bool> {
        let mut inner = self.inner.lock();
        if let Some(existing) = inner
            .state
            .queue
            .iter_mut()
            .find(|entry| entry.work == work)
        {
            let changed = !same_work_payload(&existing.work, &work);
            existing.work = work;
            if changed {
                persist_locked(&inner)?;
            }
            return Ok(false);
        }
        inner.state.queue.push_back(PendingWorkEntry {
            work,
            state: PendingWorkEntryState::Pending,
        });
        persist_locked(&inner)?;
        Ok(true)
    }

    pub fn claim_batch(&self, max_items: usize) -> Result<Vec<PendingWork>> {
        if max_items == 0 {
            return Ok(Vec::new());
        }

        let mut inner = self.inner.lock();
        let mut claimed = Vec::new();
        for _ in 0..max_items {
            let Some(index) = select_next_pending_index(&inner.state.queue) else {
                break;
            };
            let entry = &mut inner.state.queue[index];
            entry.state = PendingWorkEntryState::Claimed;
            claimed.push(entry.work.clone());
        }
        if !claimed.is_empty() {
            persist_locked(&inner)?;
        }
        Ok(claimed)
    }

    pub fn release_claimed(&self, work: PendingWork) -> Result<bool> {
        let mut inner = self.inner.lock();
        let Some(entry) = inner
            .state
            .queue
            .iter_mut()
            .find(|entry| entry.work == work)
        else {
            return Ok(false);
        };
        if matches!(entry.state, PendingWorkEntryState::Claimed) {
            entry.state = PendingWorkEntryState::Pending;
            persist_locked(&inner)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn consume(&self, work: PendingWork) -> Result<bool> {
        let mut inner = self.inner.lock();
        let Some(index) = inner
            .state
            .queue
            .iter()
            .position(|entry| entry.work == work)
        else {
            return Ok(false);
        };
        inner.state.queue.remove(index);
        persist_locked(&inner)?;
        Ok(true)
    }

    pub fn requeue_front(&self, work: PendingWork) -> Result<bool> {
        let mut inner = self.inner.lock();
        if let Some(index) = inner
            .state
            .queue
            .iter()
            .position(|entry| entry.work == work)
        {
            let mut entry = inner
                .state
                .queue
                .remove(index)
                .expect("pending work index should be valid");
            entry.work = work;
            entry.state = PendingWorkEntryState::Pending;
            inner.state.queue.push_front(entry);
            persist_locked(&inner)?;
            return Ok(true);
        }
        inner.state.queue.push_front(PendingWorkEntry {
            work,
            state: PendingWorkEntryState::Pending,
        });
        persist_locked(&inner)?;
        Ok(true)
    }

    pub async fn shutdown(self) {
        let inner = self.inner.lock();
        let _ = persist_locked(&inner);
    }
}

fn select_next_pending_index(queue: &VecDeque<PendingWorkEntry>) -> Option<usize> {
    queue
        .iter()
        .enumerate()
        .filter(|(_, entry)| matches!(entry.state, PendingWorkEntryState::Pending))
        .min_by_key(|(index, entry)| (entry.work.priority(), *index))
        .map(|(index, _)| index)
}

fn same_work_payload(left: &PendingWork, right: &PendingWork) -> bool {
    match (left, right) {
        (PendingWork::Event { event_id: a }, PendingWork::Event { event_id: b }) => a == b,
        (
            PendingWork::AppNotice { app: a, reason: ra },
            PendingWork::AppNotice { app: b, reason: rb },
        ) => a == b && ra == rb,
        _ => false,
    }
}

fn persist_locked(inner: &PendingWorkQueueInner) -> Result<()> {
    let bytes = postcard::to_allocvec(&inner.state)
        .map_err(|err| miette!("serialize pending work queue failed: {err}"))?;
    std::fs::write(&inner.path, bytes)
        .map_err(|err| miette!("persist pending work queue failed: {err}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_batch_prioritizes_events_over_app_notices() {
        let queue = PendingWorkQueue::empty();
        let event_id = Uuid::new_v4();
        queue
            .enqueue(PendingWork::AppNotice {
                app: AppId::Terminal,
                reason: "terminal changed".to_string(),
            })
            .expect("enqueue app notice");
        queue
            .enqueue(PendingWork::Event { event_id })
            .expect("enqueue event");

        let claimed = queue.claim_batch(1).expect("claim work");
        assert_eq!(claimed.len(), 1);
        match &claimed[0] {
            PendingWork::Event {
                event_id: claimed_event_id,
            } => assert_eq!(*claimed_event_id, event_id),
            other => panic!("expected event to be claimed first, got {other:?}"),
        }
    }

    #[test]
    fn requeue_front_reactivates_claimed_event_driver() {
        let queue = PendingWorkQueue::empty();
        let event_id = Uuid::new_v4();
        let work = PendingWork::Event { event_id };
        queue.enqueue(work.clone()).expect("enqueue event");

        let claimed = queue.claim_batch(1).expect("claim event");
        assert_eq!(claimed.len(), 1);
        assert!(matches!(claimed[0], PendingWork::Event { .. }));
        assert_eq!(queue.pending_count(), 0);

        queue
            .requeue_front(work.clone())
            .expect("requeue claimed event");
        assert_eq!(queue.pending_count(), 1);

        let reclaimed = queue.claim_batch(1).expect("claim requeued event");
        assert_eq!(reclaimed.len(), 1);
        match &reclaimed[0] {
            PendingWork::Event {
                event_id: reclaimed_event_id,
            } => assert_eq!(*reclaimed_event_id, event_id),
            other => panic!("expected requeued event, got {other:?}"),
        }
    }
}
