use std::{collections::VecDeque, path::PathBuf, sync::Arc};

use miette::{Result, miette};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{app::AppId, persistence::PersistenceStore};

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
        let persistence = PersistenceStore::runtime().await;
        let path = persistence.state_file(PENDING_WORK_FILE_NAME);
        let mut state: PersistedPendingWorkQueue = persistence
            .read_postcard_state_or_default(PENDING_WORK_FILE_NAME, "pending work queue")
            .await;
        reset_claimed_entries_on_startup(&mut state);
        Self {
            inner: Arc::new(Mutex::new(PendingWorkQueueInner { path, state })),
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub fn empty() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PendingWorkQueueInner {
                path: crate::daat_locus_paths::daat_locus_paths_sync()
                    .state_file(PENDING_WORK_FILE_NAME),
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

    pub fn clear_events(&self) -> Result<usize> {
        let mut inner = self.inner.lock();
        let before = inner.state.queue.len();
        inner
            .state
            .queue
            .retain(|entry| !matches!(entry.work, PendingWork::Event { .. }));
        let cleared = before.saturating_sub(inner.state.queue.len());
        if cleared > 0 {
            persist_locked(&inner)?;
        }
        Ok(cleared)
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
    crate::persistence::write_postcard_atomic_sync(
        &inner.path,
        &inner.state,
        crate::persistence::PersistenceFileMode::Default,
    )
    .map_err(|err| miette!("persist pending work queue failed: {err}"))
}

fn reset_claimed_entries_on_startup(state: &mut PersistedPendingWorkQueue) {
    for entry in &mut state.queue {
        if matches!(entry.state, PendingWorkEntryState::Claimed) {
            entry.state = PendingWorkEntryState::Pending;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_QUEUE_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn test_queue() -> PendingWorkQueue {
        let unique = TEST_QUEUE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "daat-locus-pending-work-test-{}-{}.bin",
            std::process::id(),
            unique
        ));
        let _ = std::fs::remove_file(&path);
        PendingWorkQueue {
            inner: Arc::new(Mutex::new(PendingWorkQueueInner {
                path,
                state: PersistedPendingWorkQueue::default(),
            })),
        }
    }

    #[test]
    fn persisted_pending_work_queue_postcard_round_trips() {
        let event_id = Uuid::parse_str("33333333-3333-4333-8333-333333333333").expect("event uuid");
        let state = PersistedPendingWorkQueue {
            queue: VecDeque::from([
                PendingWorkEntry {
                    work: PendingWork::Event { event_id },
                    state: PendingWorkEntryState::Pending,
                },
                PendingWorkEntry {
                    work: PendingWork::AppNotice {
                        app: AppId::terminal(),
                        reason: "terminal changed".to_string(),
                    },
                    state: PendingWorkEntryState::Claimed,
                },
            ]),
        };

        let bytes = postcard::to_allocvec(&state).expect("encode pending work");
        let restored: PersistedPendingWorkQueue =
            postcard::from_bytes(&bytes).expect("decode pending work");

        assert_eq!(restored.queue.len(), 2);
        match &restored.queue[0].work {
            PendingWork::Event {
                event_id: restored_event_id,
            } => assert_eq!(*restored_event_id, event_id),
            other => panic!("expected event work, got {other:?}"),
        }
        assert!(matches!(
            restored.queue[0].state,
            PendingWorkEntryState::Pending
        ));
        match &restored.queue[1].work {
            PendingWork::AppNotice { app, reason } => {
                assert_eq!(*app, AppId::terminal());
                assert_eq!(reason, "terminal changed");
            }
            other => panic!("expected app notice work, got {other:?}"),
        }
        assert!(matches!(
            restored.queue[1].state,
            PendingWorkEntryState::Claimed
        ));
    }

    #[test]
    fn claim_batch_prioritizes_events_over_app_notices() {
        let queue = test_queue();
        let event_id = Uuid::new_v4();
        queue
            .enqueue(PendingWork::AppNotice {
                app: AppId::terminal(),
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
    fn app_notice_enqueue_updates_reason_for_same_app() {
        let queue = test_queue();
        queue
            .enqueue(PendingWork::AppNotice {
                app: AppId::terminal(),
                reason: "old reason".to_string(),
            })
            .expect("enqueue old app notice");
        queue
            .enqueue(PendingWork::AppNotice {
                app: AppId::terminal(),
                reason: "new reason".to_string(),
            })
            .expect("update app notice reason");

        let claimed = queue.claim_batch(2).expect("claim work");
        assert_eq!(claimed.len(), 1);
        match &claimed[0] {
            PendingWork::AppNotice { app, reason } => {
                assert_eq!(*app, AppId::terminal());
                assert_eq!(reason, "new reason");
            }
            other => panic!("expected app notice, got {other:?}"),
        }
    }

    #[test]
    fn requeue_front_reactivates_claimed_event_driver() {
        let queue = test_queue();
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

    #[test]
    fn startup_releases_claimed_pending_work_entries() {
        let event_id = Uuid::new_v4();
        let mut state = PersistedPendingWorkQueue::default();
        state.queue.push_back(PendingWorkEntry {
            work: PendingWork::Event { event_id },
            state: PendingWorkEntryState::Claimed,
        });
        state.queue.push_back(PendingWorkEntry {
            work: PendingWork::AppNotice {
                app: AppId::terminal(),
                reason: "terminal changed".to_string(),
            },
            state: PendingWorkEntryState::Claimed,
        });

        reset_claimed_entries_on_startup(&mut state);

        assert!(
            state
                .queue
                .iter()
                .all(|entry| matches!(entry.state, PendingWorkEntryState::Pending))
        );
    }

    #[test]
    fn clear_events_removes_event_work_and_preserves_app_notices() {
        let queue = test_queue();
        let claimed_event_id = Uuid::new_v4();
        let pending_event_id = Uuid::new_v4();
        queue
            .enqueue(PendingWork::Event {
                event_id: claimed_event_id,
            })
            .expect("enqueue event");
        queue
            .enqueue(PendingWork::AppNotice {
                app: AppId::terminal(),
                reason: "terminal changed".to_string(),
            })
            .expect("enqueue app notice");
        queue.claim_batch(1).expect("claim first event work");
        queue
            .enqueue(PendingWork::Event {
                event_id: pending_event_id,
            })
            .expect("enqueue pending event");

        assert_eq!(queue.clear_events().expect("clear events"), 2);

        let remaining = queue.claim_batch(2).expect("claim remaining work");
        assert_eq!(remaining.len(), 1);
        assert!(matches!(remaining[0], PendingWork::AppNotice { .. }));
    }
}
