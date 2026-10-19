use super::{LEASE_MS, MAX_SNAPSHOT, NodeId, Outcome, Proposal, Types};
use crate::{
    actors::ActorSystem,
    stage::{
        Result, StageError, StageId,
        shared::{self, Candidate, Head, Lease, Operation},
    },
};
use openraft::{
    Entry, EntryPayload, LogId, LogState, OptionalSend, RaftLogReader, RaftSnapshotBuilder,
    Snapshot, SnapshotMeta, StorageError, StorageIOError, StoredMembership, Vote,
    storage::{LogFlushed, RaftLogStorage, RaftStateMachine},
};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    fmt::Debug,
    io::Cursor,
    ops::{Bound, RangeBounds},
};
type Error = StorageError<NodeId>;
fn error(error: impl std::fmt::Display) -> Error {
    StorageIOError::read_state_machine(&std::io::Error::other(error.to_string())).into()
}
fn get<T: DeserializeOwned>(tx: &Transaction<'_>, key: &str) -> Result<Option<T>> {
    let value: Option<Vec<u8>> = tx
        .query_row(
            "SELECT value FROM device_consensus_meta WHERE key=?1",
            [key],
            |row| row.get(0),
        )
        .optional()?;
    value
        .map(|bytes| {
            serde_json::from_slice(&bytes)
                .map_err(|_| StageError::Invalid("Invalid device consensus storage"))
        })
        .transpose()
}
fn put<T: Serialize>(tx: &Transaction<'_>, key: &str, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| StageError::Invalid("Cannot encode device consensus storage"))?;
    if bytes.len() > MAX_SNAPSHOT {
        return Err(StageError::Capacity("Device metadata budget exceeded"));
    }
    tx.execute("INSERT INTO device_consensus_meta VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,bytes])?;
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Advertised {
    candidate: Candidate,
    time: i64,
    instance: uuid::Uuid,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    work_generation: Option<crate::stage::reset::Generation>,
    last: Option<LogId<NodeId>>,
    membership: StoredMembership<NodeId, openraft::EmptyNode>,
    term: u64,
    #[serde(default)]
    time: Option<i64>,
    head: Head,
    epoch: u64,
    lease: Option<Lease>,
    candidates: BTreeMap<StageId, Advertised>,
    lease_instance: Option<uuid::Uuid>,
    handoff_after: i64,
}
impl State {
    fn apply(&mut self, id: LogId<NodeId>, proposal: &Proposal) -> Outcome {
        if proposal.work_generation < self.work_generation {
            return self.outcome(false, self.time.unwrap_or(proposal.time).max(proposal.time));
        }
        if proposal.work_generation > self.work_generation {
            self.work_generation = proposal.work_generation.clone();
            self.head = Head::default();
            self.epoch = self.epoch.saturating_add(1);
            self.lease = None;
            self.lease_instance = None;
            self.candidates.clear();
            self.handoff_after = 0;
        }
        if self.term != id.leader_id.term {
            self.term = id.leader_id.term;
            self.time = None;
            self.lease = None;
            self.candidates.clear();
            self.lease_instance = None;
            self.handoff_after = 0;
        }
        // Concurrent commands can enter the Raft log in a different order from
        // their timestamp samples. Never rewind this term's applied clock: an
        // older Inspect must not make a newer lease appear longer than LEASE_MS,
        // and an older Renew must not resurrect an already expired lease.
        let now = self.time.unwrap_or(proposal.time).max(proposal.time);
        self.time = Some(now);
        self.candidates.retain(|_, entry| entry.time + 10000 > now);
        let caller = proposal.caller;
        let own = self.lease_instance == Some(proposal.instance)
            && self
                .lease
                .as_ref()
                .is_some_and(|lease| lease.leader == caller && lease.expires_at_ms > now);
        let mut accepted = false;
        match &proposal.operation {
            Operation::Advertise {
                head,
                eligible,
                priority,
            } => {
                if self
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.leader == caller)
                    && self.lease_instance != Some(proposal.instance)
                {
                    self.handoff_after = self.lease.as_ref().unwrap().expires_at_ms;
                    self.lease = None;
                    self.lease_instance = None;
                }
                if self.candidates.len() < 64 || self.candidates.contains_key(&caller) {
                    self.candidates.insert(
                        caller,
                        Advertised {
                            candidate: Candidate {
                                stage_id: caller,
                                head: head.clone(),
                                eligible: *eligible,
                                priority: *priority,
                            },
                            time: now,
                            instance: proposal.instance,
                        },
                    );
                    accepted = true;
                }
            }
            Operation::Inspect => accepted = true,
            Operation::Claim { head } => {
                let live = self
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.expires_at_ms > now);
                let next = self
                    .candidates
                    .values()
                    .filter(|entry| entry.candidate.eligible && entry.candidate.head == self.head)
                    .min_by_key(|entry| (entry.candidate.priority, entry.candidate.stage_id));
                if *head == self.head
                    && (own
                        || !live
                            && next.is_some_and(|next| {
                                next.candidate.stage_id == caller
                                    && next.instance == proposal.instance
                            })
                            && (self.epoch == 0 || proposal.claim_ready))
                    && now >= self.handoff_after
                    && self.epoch < 9_007_199_254_740_991
                {
                    if !own {
                        self.epoch += 1;
                    }
                    self.lease = Some(Lease {
                        epoch: self.epoch,
                        leader: caller,
                        expires_at_ms: now + LEASE_MS,
                    });
                    self.lease_instance = Some(proposal.instance);
                    accepted = true;
                }
            }
            Operation::Renew { epoch, head } => {
                if own && *epoch == self.epoch && *head == self.head {
                    self.lease.as_mut().unwrap().expires_at_ms = now + LEASE_MS;
                    accepted = true;
                }
            }
            Operation::Release { epoch } => {
                if own && *epoch == self.epoch {
                    self.lease = None;
                    self.lease_instance = None;
                    self.candidates.remove(&caller);
                    accepted = true;
                }
            }
            Operation::Reconcile { epoch, head } => {
                let durable =
                    self.membership
                        .membership()
                        .get_joint_config()
                        .iter()
                        .all(|voters| {
                            proposal
                                .acknowledgements
                                .iter()
                                .filter(|id| voters.contains(&NodeId::from(**id)))
                                .collect::<std::collections::BTreeSet<_>>()
                                .len()
                                > voters.len() / 2
                        });
                if caller == StageId::from(id.leader_id.node_id)
                    && durable
                    && *epoch > self.epoch
                    && *epoch < 9_007_199_254_740_991
                    && self
                        .lease
                        .as_ref()
                        .is_none_or(|lease| lease.expires_at_ms <= now)
                {
                    self.head = head.clone();
                    self.epoch = *epoch;
                    self.candidates.clear();
                    self.lease = None;
                    self.lease_instance = None;
                    accepted = true;
                }
            }
            Operation::Commit {
                epoch,
                previous,
                head,
            } => {
                let durable =
                    self.membership
                        .membership()
                        .get_joint_config()
                        .iter()
                        .all(|voters| {
                            proposal
                                .acknowledgements
                                .iter()
                                .filter(|id| voters.contains(&NodeId::from(**id)))
                                .collect::<std::collections::BTreeSet<_>>()
                                .len()
                                > voters.len() / 2
                        });
                if own
                    && *epoch == self.epoch
                    && (*head == self.head
                        || *previous == self.head
                            && head.sequence == self.head.sequence + 1
                            && durable)
                {
                    self.head = head.clone();
                    accepted = true;
                }
            }
        }
        self.outcome(accepted, now)
    }
    fn outcome(&self, accepted: bool, now: i64) -> Outcome {
        Outcome {
            reply: shared::Reply {
                request_id: uuid::Uuid::nil(),
                accepted,
                server_time_ms: now,
                head: self.head.clone(),
                epoch: self.epoch,
                lease: self.lease.clone().filter(|lease| lease.expires_at_ms > now),
                peers: self
                    .candidates
                    .values()
                    .map(|entry| entry.candidate.clone())
                    .collect(),
            },
        }
    }
}
#[derive(Clone)]
pub struct Store(ActorSystem);
impl Store {
    pub fn open(store: ActorSystem, head: Head) -> Result<Self> {
        store.store().coordination_transaction(|tx| {
            tx.execute_batch("CREATE TABLE IF NOT EXISTS device_consensus_meta(key TEXT PRIMARY KEY,value BLOB NOT NULL);CREATE TABLE IF NOT EXISTS device_consensus_log(seq INTEGER PRIMARY KEY,value BLOB NOT NULL);")?;
            if get::<State>(tx,"state")?.is_none() { put(tx,"state",&State { head, ..Default::default() })?; }
            Ok(())
        })?;
        Ok(Self(store))
    }
    // OpenRaft fixes the storage error type, including its node-ID diagnostics.
    #[allow(clippy::result_large_err)]
    fn read<T: DeserializeOwned>(&self, key: &str) -> std::result::Result<Option<T>, Error> {
        self.0
            .store()
            .coordination_transaction(|tx| get(tx, key))
            .map_err(error)
    }
    #[allow(clippy::result_large_err)]
    fn write<T: Serialize>(&self, key: &str, value: &T) -> std::result::Result<(), Error> {
        self.0
            .store()
            .coordination_transaction(|tx| put(tx, key, value))
            .map_err(error)
    }
}
impl RaftLogReader<Types> for Store {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + OptionalSend>(
        &mut self,
        range: RB,
    ) -> std::result::Result<Vec<Entry<Types>>, Error> {
        let start = match range.start_bound() {
            Bound::Included(x) => *x,
            Bound::Excluded(x) => x.saturating_add(1),
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(x) => x.saturating_add(1),
            Bound::Excluded(x) => *x,
            Bound::Unbounded => i64::MAX as u64,
        };
        self.0
            .store()
            .coordination_transaction(|tx| {
                let mut stmt = tx.prepare(
                    "SELECT value FROM device_consensus_log WHERE seq>=?1 AND seq<?2 ORDER BY seq",
                )?;
                let rows = stmt.query_map(
                    params![
                        start.min(i64::MAX as u64) as i64,
                        end.min(i64::MAX as u64) as i64
                    ],
                    |row| row.get::<_, Vec<u8>>(0),
                )?;
                rows.map(|row| {
                    serde_json::from_slice(&row?)
                        .map_err(|_| StageError::Invalid("Invalid device log"))
                })
                .collect()
            })
            .map_err(error)
    }
}
impl RaftLogStorage<Types> for Store {
    type LogReader = Self;
    async fn get_log_reader(&mut self) -> Self {
        self.clone()
    }
    async fn get_log_state(&mut self) -> std::result::Result<LogState<Types>, Error> {
        let purged: Option<LogId<NodeId>> = self.read("purged")?;
        let last = self
            .0
            .store()
            .coordination_transaction(|tx| {
                let bytes: Option<Vec<u8>> = tx
                    .query_row(
                        "SELECT value FROM device_consensus_log ORDER BY seq DESC LIMIT 1",
                        [],
                        |row| row.get(0),
                    )
                    .optional()?;
                bytes
                    .map(|bytes| {
                        serde_json::from_slice::<Entry<Types>>(&bytes)
                            .map(|entry| entry.log_id)
                            .map_err(|_| StageError::Invalid("Invalid last device log"))
                    })
                    .transpose()
            })
            .map_err(error)?;
        Ok(LogState {
            last_purged_log_id: purged,
            last_log_id: last.or(purged),
        })
    }
    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> std::result::Result<(), Error> {
        self.write("vote", vote)
    }
    async fn read_vote(&mut self) -> std::result::Result<Option<Vote<NodeId>>, Error> {
        self.read("vote")
    }
    async fn save_committed(
        &mut self,
        id: Option<LogId<NodeId>>,
    ) -> std::result::Result<(), Error> {
        self.write("committed", &id)
    }
    async fn read_committed(&mut self) -> std::result::Result<Option<LogId<NodeId>>, Error> {
        Ok(self.read::<Option<LogId<NodeId>>>("committed")?.flatten())
    }
    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<Types>,
    ) -> std::result::Result<(), Error>
    where
        I: IntoIterator<Item = Entry<Types>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let entries = entries.into_iter().collect::<Vec<_>>();
        self.0.store().coordination_transaction(|tx| {
            for entry in entries {
                let bytes=serde_json::to_vec(&entry).map_err(|_|StageError::Invalid("Cannot encode device log"))?;
                if bytes.len()>64*1024 || entry.log_id.index>i64::MAX as u64 {return Err(StageError::Capacity("Device log entry budget exceeded"));}
                tx.execute("INSERT INTO device_consensus_log VALUES(?1,?2) ON CONFLICT(seq) DO UPDATE SET value=excluded.value",params![entry.log_id.index as i64,bytes])?;
            } Ok(())
        }).map_err(error)?;
        callback.log_io_completed(Ok(()));
        Ok(())
    }
    async fn truncate(&mut self, id: LogId<NodeId>) -> std::result::Result<(), Error> {
        self.0
            .store()
            .coordination_transaction(|tx| {
                tx.execute(
                    "DELETE FROM device_consensus_log WHERE seq>=?1",
                    [id.index as i64],
                )?;
                Ok(())
            })
            .map_err(error)
    }
    async fn purge(&mut self, id: LogId<NodeId>) -> std::result::Result<(), Error> {
        self.0
            .store()
            .coordination_transaction(|tx| {
                tx.execute(
                    "DELETE FROM device_consensus_log WHERE seq<=?1",
                    [id.index as i64],
                )?;
                put(tx, "purged", &id)
            })
            .map_err(error)
    }
}
impl RaftStateMachine<Types> for Store {
    type SnapshotBuilder = Self;
    async fn applied_state(
        &mut self,
    ) -> std::result::Result<
        (
            Option<LogId<NodeId>>,
            StoredMembership<NodeId, openraft::EmptyNode>,
        ),
        Error,
    > {
        let state: State = self.read("state")?.unwrap_or_default();
        Ok((state.last, state.membership))
    }
    async fn apply<I>(&mut self, entries: I) -> std::result::Result<Vec<Outcome>, Error>
    where
        I: IntoIterator<Item = Entry<Types>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let entries = entries.into_iter().collect::<Vec<_>>();
        self.0
            .store()
            .coordination_transaction(|tx| {
                let mut state: State =
                    get(tx, "state")?.ok_or(StageError::Invalid("Device state missing"))?;
                let mut replies = vec![];
                for entry in entries {
                    let outcome = match entry.payload {
                        EntryPayload::Normal(proposal) => state.apply(entry.log_id, &proposal),
                        EntryPayload::Membership(membership) => {
                            state.membership =
                                StoredMembership::new(Some(entry.log_id), membership);
                            Outcome {
                                reply: shared::Reply {
                                    request_id: uuid::Uuid::nil(),
                                    accepted: true,
                                    server_time_ms: 0,
                                    head: state.head.clone(),
                                    epoch: state.epoch,
                                    lease: None,
                                    peers: vec![],
                                },
                            }
                        }
                        EntryPayload::Blank => Outcome {
                            reply: shared::Reply {
                                request_id: uuid::Uuid::nil(),
                                accepted: true,
                                server_time_ms: 0,
                                head: state.head.clone(),
                                epoch: state.epoch,
                                lease: None,
                                peers: vec![],
                            },
                        },
                    };
                    state.last = Some(entry.log_id);
                    replies.push(outcome);
                }
                put(tx, "state", &state)?;
                Ok(replies)
            })
            .map_err(error)
    }
    async fn get_snapshot_builder(&mut self) -> Self {
        self.clone()
    }
    async fn begin_receiving_snapshot(
        &mut self,
    ) -> std::result::Result<Box<Cursor<Vec<u8>>>, Error> {
        Ok(Box::new(Cursor::new(vec![])))
    }
    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<NodeId, openraft::EmptyNode>,
        snapshot: Box<Cursor<Vec<u8>>>,
    ) -> std::result::Result<(), Error> {
        let bytes = snapshot.into_inner();
        if bytes.len() > MAX_SNAPSHOT {
            return Err(error("Device snapshot too large"));
        }
        let state: State = serde_json::from_slice(&bytes).map_err(error)?;
        if state.last != meta.last_log_id || state.membership != meta.last_membership {
            return Err(error("Device snapshot metadata mismatch"));
        }
        self.0
            .store()
            .coordination_transaction(|tx| {
                put(tx, "state", &state)?;
                put(tx, "snapshot_meta", meta)?;
                put(tx, "snapshot", &bytes)
            })
            .map_err(error)
    }
    async fn get_current_snapshot(
        &mut self,
    ) -> std::result::Result<Option<Snapshot<Types>>, Error> {
        let Some(meta) = self.read("snapshot_meta")? else {
            return Ok(None);
        };
        let bytes: Vec<u8> = self
            .read("snapshot")?
            .ok_or_else(|| error("Device snapshot missing"))?;
        Ok(Some(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(bytes)),
        }))
    }
}
impl RaftSnapshotBuilder<Types> for Store {
    async fn build_snapshot(&mut self) -> std::result::Result<Snapshot<Types>, Error> {
        let state: State = self
            .read("state")?
            .ok_or_else(|| error("Device state missing"))?;
        let bytes = serde_json::to_vec(&state).map_err(error)?;
        if bytes.len() > MAX_SNAPSHOT {
            return Err(error("Device snapshot too large"));
        }
        let meta = SnapshotMeta {
            last_log_id: state.last,
            last_membership: state.membership,
            snapshot_id: uuid::Uuid::new_v4().to_string(),
        };
        self.0
            .store()
            .coordination_transaction(|tx| {
                put(tx, "snapshot_meta", &meta)?;
                put(tx, "snapshot", &bytes)
            })
            .map_err(error)?;
        Ok(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(bytes)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn propose(state: &mut State, proposal: Proposal) -> shared::Reply {
        let id = LogId::new(
            openraft::CommittedLeaderId::new(1, NodeId::from(proposal.caller)),
            1,
        );
        state.apply(id, &proposal).reply
    }

    fn active(caller: StageId, instance: uuid::Uuid) -> State {
        State {
            term: 1,
            epoch: 7,
            lease: Some(Lease {
                epoch: 7,
                leader: caller,
                expires_at_ms: 6000,
            }),
            lease_instance: Some(instance),
            ..Default::default()
        }
    }

    #[test]
    fn reordered_commands_keep_leases_bounded_and_cannot_revive_expired_workers() {
        let device = StageId::new();
        let instance = uuid::Uuid::new_v4();
        let mut state = active(device, instance);
        let renewal = Proposal {
            work_generation: None,
            caller: device,
            operation: Operation::Renew {
                epoch: 7,
                head: Head::default(),
            },
            acknowledgements: vec![],
            time: 2000,
            claim_ready: true,
            instance,
        };
        assert!(propose(&mut state, renewal.clone()).accepted);
        // Restoring persisted state or a snapshot must preserve the applied
        // timestamp watermark before another command is replayed.
        state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        // An Inspect sampled before the renewal may be submitted after it.
        let inspect = Proposal {
            operation: Operation::Inspect,
            time: 1000,
            ..renewal.clone()
        };
        let reply = propose(&mut state, inspect.clone());
        assert!(reply.accepted);
        assert_eq!(reply.server_time_ms, 2000);
        assert_eq!(reply.lease.as_ref().unwrap().expires_at_ms, 8000);
        let sent = std::time::Instant::now();
        let received = sent + std::time::Duration::from_millis(100);
        let mut fence = shared::Fence::default();
        fence.observe(&reply, sent, received).unwrap();
        assert!(fence.permits(device, 7, received));
        assert!(!fence.permits(device, 7, sent + std::time::Duration::from_millis(6000)));

        let expired = propose(
            &mut state,
            Proposal {
                time: 8000,
                ..inspect
            },
        );
        assert!(expired.lease.is_none());
        let delayed = propose(
            &mut state,
            Proposal {
                time: 3000,
                ..renewal
            },
        );
        assert!(
            !delayed.accepted,
            "an old sample cannot revive an expired lease"
        );
        assert!(delayed.lease.is_none());
    }

    #[test]
    fn a_new_term_uses_its_own_clock_and_invalidates_the_previous_lease() {
        let device = StageId::new();
        let instance = uuid::Uuid::new_v4();
        let mut state = active(device, instance);
        state.time = Some(5000);
        let proposal = Proposal {
            work_generation: None,
            caller: device,
            operation: Operation::Inspect,
            acknowledgements: vec![],
            time: -1000,
            claim_ready: true,
            instance,
        };
        let id = LogId::new(openraft::CommittedLeaderId::new(2, device.into()), 2);
        let reply = state.apply(id, &proposal).reply;
        assert!(reply.accepted);
        assert_eq!(reply.server_time_ms, -1000);
        assert!(reply.lease.is_none());
        let renewal = Proposal {
            operation: Operation::Renew {
                epoch: 7,
                head: Head::default(),
            },
            ..proposal
        };
        assert!(!state.apply(id, &renewal).reply.accepted);
    }

    #[test]
    fn reset_retires_old_execution_without_changing_raft_membership() {
        let device = StageId::default();
        let instance = uuid::Uuid::new_v4();
        let mut state = active(device, instance);
        state.head = Head {
            sequence: 42,
            hash: "a".repeat(64),
        };
        let membership = serde_json::to_value(&state.membership).unwrap();
        let old_renewal = Proposal {
            work_generation: None,
            caller: device,
            operation: Operation::Renew {
                epoch: 7,
                head: state.head.clone(),
            },
            acknowledgements: vec![],
            time: 1000,
            claim_ready: true,
            instance,
        };
        assert!(propose(&mut state, old_renewal.clone()).accepted);
        let reset = Proposal {
            work_generation: Some(crate::stage::reset::Generation {
                revision: 1,
                writer: device,
            }),
            operation: Operation::Inspect,
            ..old_renewal.clone()
        };
        let reply = propose(&mut state, reset);
        assert!(reply.accepted);
        assert_eq!(reply.head, Head::default());
        assert!(reply.lease.is_none());
        assert!(reply.epoch > 7);
        assert_eq!(serde_json::to_value(&state.membership).unwrap(), membership);
        assert!(!propose(&mut state, old_renewal).accepted);
        assert_eq!(state.head, Head::default());
        assert!(state.lease.is_none());
    }
    #[test]
    fn business_commit_requires_distinct_majorities_of_both_joint_configs() {
        let a = StageId::default();
        let b = StageId::default();
        let c = StageId::default();
        let d = StageId::default();
        let instance = uuid::Uuid::new_v4();
        let mut state = active(a, instance);
        state.membership = StoredMembership::new(
            None,
            openraft::Membership::new(
                vec![
                    std::collections::BTreeSet::from([a.into(), b.into(), c.into()]),
                    std::collections::BTreeSet::from([b.into(), c.into(), d.into()]),
                ],
                (),
            ),
        );
        let next = Head {
            sequence: 1,
            hash: "a".repeat(64),
        };
        let proposal = Proposal {
            work_generation: None,
            caller: a,
            operation: Operation::Commit {
                epoch: 7,
                previous: Head::default(),
                head: next.clone(),
            },
            acknowledgements: vec![a, a, b],
            time: 1000,
            claim_ready: true,
            instance,
        };
        assert!(!propose(&mut state, proposal.clone()).accepted);
        assert_eq!(state.head, Head::default());
        let mut majority = proposal;
        majority.acknowledgements = vec![a, b, c];
        assert!(propose(&mut state, majority).accepted);
        assert_eq!(state.head, next);
    }

    #[test]
    fn restarted_execution_cannot_restore_or_renew_the_previous_instance_lease() {
        let device = StageId::default();
        let old = uuid::Uuid::new_v4();
        let new = uuid::Uuid::new_v4();
        let original = active(device, old);
        let mut restored: State =
            serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
        let mut proposal = Proposal {
            work_generation: None,
            caller: device,
            operation: Operation::Advertise {
                head: Head::default(),
                eligible: true,
                priority: 0,
            },
            acknowledgements: vec![],
            time: 1000,
            claim_ready: true,
            instance: new,
        };
        assert!(propose(&mut restored, proposal.clone()).accepted);
        assert!(restored.lease.is_none());
        proposal.operation = Operation::Renew {
            epoch: 7,
            head: Head::default(),
        };
        proposal.instance = old;
        assert!(!propose(&mut restored, proposal.clone()).accepted);
        proposal.operation = Operation::Claim {
            head: Head::default(),
        };
        proposal.instance = new;
        assert!(!propose(&mut restored, proposal.clone()).accepted);
        proposal.time = 6001;
        let granted = propose(&mut restored, proposal.clone());
        assert!(granted.accepted);
        assert_eq!(granted.epoch, 8);
        proposal.instance = old;
        proposal.operation = Operation::Renew {
            epoch: 8,
            head: Head::default(),
        };
        assert!(!propose(&mut restored, proposal).accepted);
    }
}
