//! Explicit Owner reset of account work, independent of credentials and membership.
use super::{
    Result, StageError, StageId,
    host::Host,
    identity::{Enrollment, Membership, SignedProof, verify_enrollment},
};
use crate::actors::{ActorRecord, store::ActorStore};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use parking_lot::Mutex;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generation {
    pub(crate) revision: u64,
    pub(crate) writer: StageId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    enrollment: Enrollment,
    proof: SignedProof,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    version: u16,
    account: String,
    issued_at_ms: i64,
    generation: Generation,
}

#[derive(Default)]
pub(crate) struct Pending {
    pub record: Mutex<Option<Record>>,
    pub active: AtomicBool,
    generation: parking_lot::RwLock<Option<Generation>>,
}

pub fn current(store: &ActorStore) -> Result<Option<Record>> {
    store.stage_transaction(|tx| {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS account_work_reset(singleton INTEGER PRIMARY KEY CHECK(singleton=1),record TEXT NOT NULL,cleanup_pending INTEGER NOT NULL);")?;
        let value: Option<String> = tx.query_row(
            "SELECT record FROM account_work_reset WHERE singleton=1", [], |row| row.get(0),
        ).optional()?;
        value.map(|value| serde_json::from_str(&value)
            .map_err(|_| StageError::Invalid("Invalid account reset record"))).transpose()
    })
}

pub fn generation(host: &Host) -> Result<Option<Generation>> {
    Ok(host.work_reset.generation.read().clone())
}
pub(crate) fn initialize(host: &Host) -> Result<()> {
    *host.work_reset.generation.write() = current(host.system.store())?
        .as_ref()
        .map(|record| verify(host, record))
        .transpose()?;
    Ok(())
}

pub fn verify(host: &Host, record: &Record) -> Result<Generation> {
    if host.account_id().as_deref() != Some(&record.enrollment.root.group_id)
        || record.enrollment.chain.len() > 16
    {
        return Err(StageError::Forbidden("Reset belongs to another account"));
    }
    let leaf = record
        .enrollment
        .chain
        .last()
        .ok_or(StageError::Invalid("Reset signer is missing"))?;
    if leaf.payload.len() > 65536 || record.proof.payload.len() > 4096 {
        return Err(StageError::Capacity("Reset proof exceeds budget"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(&leaf.payload)
        .map_err(|_| StageError::Invalid("Invalid reset signer"))?;
    let leaf: Membership =
        serde_json::from_slice(&bytes).map_err(|_| StageError::Invalid("Invalid reset signer"))?;
    let change: Change = record
        .proof
        .verify("account-work-reset", &leaf.device.public_key)?;
    let now = chrono::Utc::now().timestamp_millis();
    if change.version != 1
        || change.account != record.enrollment.root.group_id
        || change.issued_at_ms < 0
        || change.issued_at_ms > now.saturating_add(60000)
        || change.generation.revision == 0
        || change.generation.revision >= 9_007_199_254_740_991
    {
        return Err(StageError::Invalid("Invalid account reset"));
    }
    // This is a durable historical fact. Its signer may since have expired or
    // been revoked. A live source must independently prove Owner authority below;
    // an authorized replica can relay an already-applied reset to a new device.
    let member = verify_enrollment(
        &record.enrollment.root,
        &record.enrollment.chain,
        change.issued_at_ms,
        &Default::default(),
    )?;
    if !member.can_enroll || member.device.stage_id != change.generation.writer {
        return Err(StageError::Forbidden(
            "Reset requires account Owner authority",
        ));
    }
    Ok(change.generation)
}

pub(crate) fn issue(host: &Host) -> Result<Record> {
    let previous = generation(host)?;
    let change = Change {
        version: 1,
        account: host
            .account_id()
            .ok_or(StageError::Unavailable("Account is unavailable"))?,
        issued_at_ms: chrono::Utc::now().timestamp_millis(),
        generation: Generation {
            revision: previous.map_or(1, |old| old.revision.saturating_add(1)),
            writer: host.local,
        },
    };
    let record = Record {
        enrollment: host.enrollment()?,
        proof: host.identity()?.sign("account-work-reset", &change)?,
    };
    verify(host, &record)?;
    Ok(record)
}

pub(crate) fn offer(host: &Host, source: StageId, record: Record) -> Result<()> {
    let next = verify(host, &record)?;
    let own = host.enrollment()?;
    let removed = super::revocation::removed(host.system.store())?;
    let enrollment = if source == host.local {
        own.clone()
    } else {
        super::ledger::StageLedger::attach(host.system.store(), host.local)
            .peer(source, chrono::Utc::now().timestamp_millis())?
            .enrollment
    };
    let member = verify_enrollment(
        &own.root,
        &enrollment.chain,
        chrono::Utc::now().timestamp_millis(),
        &removed,
    )?;
    if enrollment.root != own.root || !member.can_enroll {
        return Err(StageError::Forbidden(
            "Only account Owner devices may introduce a reset",
        ));
    }
    if generation(host)?.as_ref().is_some_and(|old| old >= &next) {
        return Ok(());
    }
    let mut pending = host.work_reset.record.lock();
    if pending
        .as_ref()
        .map(|old| verify(host, old))
        .transpose()?
        .as_ref()
        .is_none_or(|old| old < &next)
    {
        *pending = Some(record);
        host.system.store().changed().notify_waiters();
    }
    Ok(())
}
pub(crate) fn completed(host: &Host, record: &Record) {
    let mut pending = host.work_reset.record.lock();
    if pending
        .as_ref()
        .is_some_and(|value| value.proof.payload == record.proof.payload)
    {
        *pending = None;
    }
}

/// The marker and all work deletion commit together. Never reset SQLite sequence
/// counters: already-open clients must not mistake new events for old cursors.
pub(crate) fn replace(store: &ActorStore, record: &Record, root: &ActorRecord) -> Result<()> {
    store.stage_transaction(|tx| {
        let has_core: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='core_meta')", [], |row| row.get(0))?;
        let synchronized = has_core && tx.query_row("SELECT EXISTS(SELECT 1 FROM core_meta WHERE key='synchronized' AND value='1')", [], |row| row.get::<_, bool>(0))?;
        // Explicitly retain credentials, device identity/enrollment, revocations,
        // transport generations and Raft votes/logs/membership.
        const TABLES: &[&str] = &[
            "account_approval_refs", "stage_local_approvals", "approvals", "tools",
            "idempotency", "resources", "history", "messages", "actor_event_index",
            "events", "actor_event_heads", "actor_text_heads", "actor_journal_meta",
            "stage_snapshot_pages", "stage_account_actors", "actors",
            "stage_directory", "stage_notice_seen", "stage_replicas", "stage_replica_pages",
            "stage_replica_messages", "stage_replica_text", "stage_replica_snapshot",
            "stage_replica_snapshot_pages", "stage_client_events", "stage_outbox",
            "stage_received_results", "stage_message_content", "stage_inbox", "stage_sent",
            "stage_ready_deliveries", "stage_delivery_receipts", "stage_send_keys",
            "stage_resource_files", "stage_resources", "stage_control_exports",
            "stage_content_sources", "stage_downloads", "stage_blob_parts",
            "stage_download_activity", "stage_blob_actors", "stage_blob_access", "stage_blobs",
            "core_resource_sent", "core_app_effects", "core_snapshot_content",
            "core_snapshot_incoming", "core_snapshot_rows", "core_snapshot_age", "core_snapshots",
            "core_recovery_rows", "core_recovery_merged", "core_recovery_archives",
            "core_order", "core_prepared", "core_rows", "core_dirty", "core_meta",
        ];
        for table in TABLES {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [table], |row| row.get(0),
            )?;
            if exists { tx.execute(&format!("DELETE FROM {table}"), [])?; }
        }
        // A work reset must not remove a previously synchronized device's
        // eligibility for explicit local recovery while its authority is offline.
        if synchronized {
            tx.execute("INSERT INTO core_meta VALUES('synchronized','1')", [])?;
        }
        let peers = tx.prepare("SELECT id,record FROM stage_peers")?
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (id, value) in peers {
            let mut peer: super::ledger::PeerRecord = serde_json::from_str(&value)
                .map_err(|_| StageError::Invalid("Invalid device grants"))?;
            peer.grants.clear();
            tx.execute("UPDATE stage_peers SET record=?1 WHERE id=?2",
                params![serde_json::to_string(&peer).map_err(|_| StageError::Invalid("Cannot reset device grants"))?, id])?;
        }
        tx.execute("INSERT INTO actors(id,parent_id,depth,record) VALUES(?1,NULL,0,?2)",
            params![root.id, serde_json::to_string(root).map_err(|_| StageError::Invalid("Cannot reset Core Actor"))?])?;
        tx.execute("INSERT INTO account_work_reset VALUES(1,?1,1) ON CONFLICT(singleton) DO UPDATE SET record=excluded.record,cleanup_pending=1",
            [serde_json::to_string(record).map_err(|_| StageError::Invalid("Cannot save reset record"))?])?;
        Ok(())
    })
}

pub(crate) fn cleanup(system: &crate::actors::ActorSystem) -> Result<()> {
    let pending = system.store().stage_transaction(|tx| {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='account_work_reset')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(false);
        }
        Ok(tx
            .query_row(
                "SELECT cleanup_pending FROM account_work_reset WHERE singleton=1",
                [],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false))
    })?;
    if !pending {
        return Ok(());
    }
    system
        .remove_reset_app_data()
        .map_err(|_| StageError::Unavailable("Cannot remove old Actor resources"))?;
    system.store().stage_transaction(|tx| {
        tx.execute(
            "UPDATE account_work_reset SET cleanup_pending=0 WHERE singleton=1",
            [],
        )?;
        Ok(())
    })
}

pub(crate) fn blocked(host: &Host) -> bool {
    host.work_reset.active.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{actors::ActorSystem, config::Config, stage::runtime::Runtime};
    use std::{sync::Arc, time::Duration};

    async fn hosts(directory: &std::path::Path) -> (Arc<Runtime>, Arc<Runtime>) {
        let mut hosts = Vec::new();
        for name in ["a", "b"] {
            let system = ActorSystem::open_account_gated(
                directory.join(name).join("actors"),
                directory.join(name).join("workspace"),
                Config::default(),
            )
            .await
            .unwrap();
            let host = Host::load(system).unwrap();
            let mut settings = host.settings();
            settings.listen = "127.0.0.1:0".into();
            // Only explicitly dialed loopback addresses are used in this test.
            settings.interfaces = vec!["fixture-no-lan".into()];
            host.save_settings(settings).unwrap();
            hosts.push(host);
        }
        hosts[0].initialize(true).unwrap();
        let request = hosts[1].initialize(false).unwrap();
        let enrollment = hosts[0].enroll_authenticated_device(&request).unwrap();
        hosts[1]
            .join(enrollment, &hosts[0].account_id().unwrap())
            .unwrap();
        (
            Runtime::new(hosts[0].clone()),
            Runtime::new(hosts[1].clone()),
        )
    }

    #[tokio::test]
    async fn returning_device_applies_reset_before_syncing_its_old_conversation() {
        let directory = tempfile::tempdir().unwrap();
        let (a, b) = hosts(directory.path()).await;
        a.start().await.unwrap();
        b.start().await.unwrap();
        super::super::recovery::mark_synchronized(b.host.system.store()).unwrap();
        b.host
            .system
            .send_user("core", "obsolete offline context".into(), vec![], None)
            .unwrap();
        let old = b.host.system.store().messages("core", None, 100).unwrap();
        assert_eq!(old.items.len(), 1);
        let address = a.address.read().unwrap();
        b.dial_tcp(a.host.local, address, None).await.unwrap();
        b.stop_abruptly().await;
        let peers_before = StageLedger::attach(b.host.system.store(), b.host.local)
            .peers(chrono::Utc::now().timestamp_millis())
            .unwrap();

        let reset = a.reset_account_work(None).await.unwrap();
        assert!(b.host.system.store().message(&old.items[0].id).is_ok());
        b.start().await.unwrap();
        let address = a.address.read().unwrap();
        // Discovery supplies a retryable address. The first channel intentionally
        // closes while B applies the reset, then the supervisor reconnects it.
        b.account_candidate(a.host.local, address);
        tokio::time::timeout(Duration::from_secs(20), async {
            while generation(&b.host).unwrap().as_ref() != Some(&reset)
                || b.host.system.store().message(&old.items[0].id).is_ok()
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("offline work must be erased before the device joins the new generation");
        assert!(a.host.system.store().message(&old.items[0].id).is_err());
        assert!(super::super::recovery::synchronized(b.host.system.store()).unwrap());
        assert!(
            b.host
                .system
                .store()
                .history("core", 100)
                .unwrap()
                .is_empty()
        );
        let peers_after = StageLedger::attach(b.host.system.store(), b.host.local)
            .peers(chrono::Utc::now().timestamp_millis())
            .unwrap();
        assert_eq!(peers_after.len(), peers_before.len());
        assert_eq!(peers_after[0].device, peers_before[0].device);
        let mut settings = b.host.settings();
        settings.label = "after-account-reset".into();
        b.host.save_settings(settings).unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            while a.host.device_label(b.host.local).unwrap().as_deref()
                != Some("after-account-reset")
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("preserved peers must reconnect and carry new business traffic");
        a.stop_abruptly().await;
        b.stop_abruptly().await;
        a.host.system.shutdown().await;
        b.host.system.shutdown().await;
    }

    #[tokio::test]
    async fn interrupted_reset_finishes_resource_cleanup_before_starting_the_new_core() {
        let directory = tempfile::tempdir().unwrap();
        let (a, b) = hosts(directory.path()).await;
        let attachment = a
            .host
            .system
            .actor_data_dir(crate::stage::shared::ACTOR_ID)
            .join("old-attachment.txt");
        std::fs::write(&attachment, "discarded attachment").unwrap();
        let record = issue(&a.host).unwrap();
        // Crash boundary: the deletion/marker transaction committed, file cleanup
        // and Core creation did not run. Reopen the real database and device keys.
        replace(
            a.host.system.store(),
            &record,
            &a.host.system.actor(crate::stage::shared::ACTOR_ID).unwrap(),
        )
        .unwrap();
        let system = ActorSystem::open_account_gated(
            a.host.system.infrastructure_dir(),
            a.host
                .system
                .actor(crate::stage::shared::ACTOR_ID)
                .unwrap()
                .config
                .workspace,
            Config::default(),
        )
        .await
        .unwrap();
        let reopened = Runtime::new(Host::load(system).unwrap());
        assert!(attachment.exists());
        reopened.start().await.unwrap();
        assert!(!attachment.exists());
        assert_eq!(
            generation(&reopened.host).unwrap(),
            Some(verify(&a.host, &record).unwrap())
        );
        assert!(reopened.host.system.actor("core").is_ok());
        let fresh = reopened
            .host
            .system
            .actor_data_dir(crate::stage::shared::ACTOR_ID)
            .join("fresh.txt");
        std::fs::write(&fresh, "new work").unwrap();
        cleanup(&reopened.host.system).unwrap();
        assert_eq!(std::fs::read_to_string(fresh).unwrap(), "new work");
        reopened.stop_abruptly().await;
        reopened.host.system.shutdown().await;
        a.host.system.shutdown().await;
        b.host.system.shutdown().await;
    }

    #[tokio::test]
    async fn reset_proofs_are_account_bound_and_cleanup_does_not_repeat_on_new_work() {
        let directory = tempfile::tempdir().unwrap();
        let (a, b) = hosts(directory.path()).await;
        let record = issue(&a.host).unwrap();
        let mut forged = record.clone();
        forged.proof.payload.push('x');
        assert!(offer(&b.host, a.host.local, forged).is_err());
        let other = ActorSystem::open_account_gated(
            directory.path().join("other/actors"),
            directory.path().join("other/workspace"),
            Config::default(),
        )
        .await
        .unwrap();
        let other = Host::load(other).unwrap();
        other.initialize(true).unwrap();
        assert!(offer(&other, a.host.local, record.clone()).is_err());
        assert!(generation(&other).unwrap().is_none());

        a.reset_account_work(None).await.unwrap();
        let first = current(a.host.system.store()).unwrap().unwrap();
        replace(
            b.host.system.store(),
            &first,
            &b.host.system.actor(crate::stage::shared::ACTOR_ID).unwrap(),
        )
        .unwrap();
        initialize(&b.host).unwrap();
        cleanup(&b.host.system).unwrap();
        b.reset_account_work(generation(&b.host).unwrap())
            .await
            .unwrap();
        let applied = current(b.host.system.store()).unwrap().unwrap();
        replace(
            a.host.system.store(),
            &applied,
            &a.host.system.actor(crate::stage::shared::ACTOR_ID).unwrap(),
        )
        .unwrap();
        initialize(&a.host).unwrap();
        cleanup(&a.host.system).unwrap();
        a.host.revoke_account_device(b.host.local).unwrap();
        assert!(
            verify(&a.host, &applied).is_ok(),
            "revocation must not invalidate a reset already applied"
        );
        assert!(
            offer(&a.host, b.host.local, issue(&b.host).unwrap()).is_err(),
            "a revoked source cannot introduce another reset"
        );
        let file = a
            .host
            .system
            .actor_data_dir(crate::stage::shared::ACTOR_ID)
            .join("new-work.txt");
        std::fs::write(&file, "new data").unwrap();
        offer(&a.host, a.host.local, applied).unwrap();
        assert!(a.host.work_reset.record.lock().is_none());
        cleanup(&a.host.system).unwrap();
        assert_eq!(std::fs::read_to_string(file).unwrap(), "new data");
        other.system.shutdown().await;
        a.host.system.shutdown().await;
        b.host.system.shutdown().await;
    }
    use crate::stage::ledger::StageLedger;
}
