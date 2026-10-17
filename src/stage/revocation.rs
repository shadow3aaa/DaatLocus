//! Permanent, signed account revocations; network paths never confer authority.
use super::{
    Result, StageError, StageId,
    identity::{Enrollment, SignedProof, verify_enrollment},
    ledger::StageLedger,
};
use crate::actors::store::ActorStore;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub enrollment: Enrollment,
    pub proof: SignedProof,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub version: u16,
    pub group_id: String,
    pub issuer: StageId,
    pub target: StageId,
    pub issued_at_ms: i64,
    pub nonce: uuid::Uuid,
}
fn ids(tx: &rusqlite::Transaction<'_>, query: &str) -> Result<HashSet<StageId>> {
    tx.prepare(query)?
        .query_map([], |r| r.get::<_, String>(0))?
        .map(|row| {
            let value = row?;
            Ok(StageId(uuid::Uuid::parse_str(&value).map_err(|_| {
                StageError::Invalid("Invalid revoked device identity")
            })?))
        })
        .collect()
}
pub fn removed(store: &ActorStore) -> Result<HashSet<StageId>> {
    store.stage_transaction(removed_in)
}
pub(crate) fn removed_in(tx: &rusqlite::Transaction<'_>) -> Result<HashSet<StageId>> {
    let mut result = ids(tx, "SELECT id FROM stage_peers WHERE revoked=1")?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='stage_revocations')",
        [],
        |r| r.get(0),
    )?;
    if exists {
        result.extend(ids(tx, "SELECT target FROM stage_revocations")?);
    }
    Ok(result)
}
fn unsigned_record(change: &Change) -> Result<Record> {
    use base64::Engine;
    if change.proof.payload.len() > 8192 {
        return Err(StageError::Capacity("Revocation proof exceeds budget"));
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&change.proof.payload)
        .map_err(|_| StageError::Invalid("Invalid revocation encoding"))?;
    serde_json::from_slice(&bytes).map_err(|_| StageError::Invalid("Invalid revocation record"))
}
pub fn target(change: &Change) -> Result<StageId> {
    Ok(unsigned_record(change)?.target)
}
pub fn apply(store: &ActorStore, local: StageId, change: &Change, now: i64) -> Result<StageId> {
    let ledger = StageLedger::attach(store, local);
    let root = store.stage_transaction(|tx| {
        let root: String =
            tx.query_row("SELECT value FROM stage_meta WHERE key='root'", [], |r| {
                r.get(0)
            })?;
        serde_json::from_str::<super::identity::TrustRoot>(&root)
            .map_err(|_| StageError::Invalid("Invalid account trust"))
    })?;
    if root != change.enrollment.root {
        return Err(StageError::Forbidden(
            "Revocation belongs to another account",
        ));
    }
    let proposed = unsigned_record(change)?;
    if proposed.issued_at_ms < 0 || proposed.issued_at_ms > now.saturating_add(60000) {
        return Err(StageError::Forbidden("Invalid revocation time"));
    }
    // A permanent removal remains verifiable after the signing enrollment expires.
    let current = verify_enrollment(
        &root,
        &change.enrollment.chain,
        proposed.issued_at_ms,
        &HashSet::new(),
    )?;
    let record: Record = change
        .proof
        .verify("account-revocation", &current.device.public_key)?;
    if record.version != 1
        || record.group_id != root.group_id
        || record.issuer != current.device.stage_id
        || !current.can_enroll
        || record.target.0.is_nil()
        || record.issued_at_ms < 0
        || record.issued_at_ms > now + 60000
        || record.nonce.is_nil()
    {
        return Err(StageError::Forbidden("Invalid revocation authority"));
    }
    if change.enrollment.chain.len() > 1 {
        let ancestor: super::identity::Membership =
            change.enrollment.chain[0].verify("membership", &root.public_key)?;
        if record.target == ancestor.device.stage_id {
            return Err(StageError::Forbidden(
                "Enrolled device cannot revoke the account authority",
            ));
        }
    }
    store.stage_transaction(|tx|{tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_revocations(target TEXT PRIMARY KEY,record TEXT NOT NULL,received INTEGER NOT NULL);")?;
        let previous:Option<String>=tx.query_row("SELECT record FROM stage_revocations WHERE target=?1",[record.target.to_string()],|r|r.get(0)).optional()?;
        if previous.is_none(){
            if removed_in(tx)?.contains(&record.issuer){return Err(StageError::Forbidden("Revoked issuer cannot issue new removals"));}
            let encoded=serde_json::to_string(change).map_err(|_|StageError::Invalid("Invalid revocation record"))?;tx.execute("INSERT INTO stage_revocations VALUES(?1,?2,?3)",params![record.target.to_string(),encoded,now])?;}Ok(())})?;
    let known = store.stage_transaction(|tx| {
        Ok(tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM stage_peers WHERE id=?1)",
            [record.target.to_string()],
            |r| r.get::<_, bool>(0),
        )?)
    })?;
    if record.target != local && known {
        ledger.revoke(record.target)?;
    }
    Ok(record.target)
}
pub fn page(store: &ActorStore, after: Option<StageId>, limit: usize) -> Result<Vec<Change>> {
    store.stage_transaction(|tx|{tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_revocations(target TEXT PRIMARY KEY,record TEXT NOT NULL,received INTEGER NOT NULL);")?;
        let rows=tx.prepare("SELECT record FROM stage_revocations WHERE target>?1 ORDER BY target LIMIT ?2")?.query_map(params![after.map(|id|id.to_string()).unwrap_or_default(),limit.clamp(1,33) as i64],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        rows.into_iter().map(|row|serde_json::from_str(&row).map_err(|_|StageError::Invalid("Invalid stored revocation"))).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn permanent_removals_replay_after_expiry_and_cannot_be_forged_or_reenrolled() {
        let dir = tempfile::tempdir().unwrap();
        let system = crate::actors::ActorSystem::open(
            dir.path().join("state"),
            dir.path().join("workspace"),
            Default::default(),
        )
        .await
        .unwrap();
        let local = super::super::host::Host::load(system.clone())
            .unwrap()
            .local;
        let mut authority =
            super::super::identity::DeviceIdentity::generate_for_stage(local).unwrap();
        let own = authority.initialize_group(1000, 10000).unwrap();
        let ledger = StageLedger::initialize(system.store(), local, &own.root).unwrap();
        let b = super::super::identity::DeviceIdentity::generate().unwrap();
        let enrollment = authority
            .authorize(&own, &b.join_request(1001).unwrap(), 1001, 9000, false)
            .unwrap();
        ledger
            .import_peer(enrollment.clone(), vec![], 1002)
            .unwrap();
        let change = Change {
            enrollment: own.clone(),
            proof: authority
                .sign(
                    "account-revocation",
                    &Record {
                        version: 1,
                        group_id: own.root.group_id.clone(),
                        issuer: local,
                        target: b.profile().stage_id,
                        issued_at_ms: 2000,
                        nonce: uuid::Uuid::new_v4(),
                    },
                )
                .unwrap(),
        };
        let mut forged = change.clone();
        forged.proof.signature = b
            .sign(
                "account-revocation",
                &Record {
                    version: 1,
                    group_id: own.root.group_id.clone(),
                    issuer: local,
                    target: b.profile().stage_id,
                    issued_at_ms: 2000,
                    nonce: uuid::Uuid::new_v4(),
                },
            )
            .unwrap()
            .signature;
        assert!(apply(system.store(), local, &forged, 2001).is_err());
        apply(system.store(), local, &change, 2001).unwrap();
        apply(system.store(), local, &change, 20000).unwrap();
        assert!(ledger.peer(b.profile().stage_id, 2002).is_err());
        assert!(ledger.import_peer(enrollment, vec![], 2003).is_err());
        for _ in 0..39 {
            let change = Change {
                enrollment: own.clone(),
                proof: authority
                    .sign(
                        "account-revocation",
                        &Record {
                            version: 1,
                            group_id: own.root.group_id.clone(),
                            issuer: local,
                            target: StageId::new(),
                            issued_at_ms: 2000,
                            nonce: uuid::Uuid::new_v4(),
                        },
                    )
                    .unwrap(),
            };
            apply(system.store(), local, &change, 2001).unwrap();
        }
        let first = page(system.store(), None, 32).unwrap();
        assert_eq!(first.len(), 32);
        let second = page(
            system.store(),
            Some(target(first.last().unwrap()).unwrap()),
            32,
        )
        .unwrap();
        assert_eq!(second.len(), 8);
        system.shutdown().await;
    }
}
