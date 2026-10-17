//! Durable transfer state in the Actor database. Acceptance never acknowledges a failed write.
use super::{
    Result, StageError, StageId,
    identity::{DeviceProfile, Enrollment, TrustRoot, verify_enrollment},
    protocol::{BlobRef, Body, Envelope, InvocationState},
};
use crate::actors::store::ActorStore;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::HashSet;
use uuid::Uuid;

impl ActorStore {
    pub(crate) fn stage_local_id(&self) -> Result<StageId> {
        self.stage_transaction(|tx| {
            let id: String = tx.query_row(
                "SELECT value FROM stage_meta WHERE key='local'",
                [],
                |row| row.get(0),
            )?;
            Ok(StageId(Uuid::parse_str(&id).map_err(|_| {
                StageError::Invalid("Invalid Stage identity metadata")
            })?))
        })
    }
    pub(crate) fn stage_peer_for_authority(&self, id: StageId) -> Result<PeerRecord> {
        self.stage_transaction(|tx| peer(tx, id, chrono::Utc::now().timestamp_millis()))
    }
}

pub(crate) fn actor_message_state(
    tx: &Transaction<'_>,
    message: &crate::actors::MessageRecord,
) -> Result<()> {
    use crate::actors::{MessageKind, MessageStatus, ReplyTarget};
    if message.kind != MessageKind::Request {
        return Ok(());
    }
    let ReplyTarget::Remote {
        stage_id,
        invocation_id,
        ..
    } = message.reply_to
    else {
        return Ok(());
    };
    let Some((_, mut receipt)) = receipt(tx, stage_id, invocation_id)? else {
        return Err(StageError::Invalid(
            "Remote Actor input has no durable receipt",
        ));
    };
    if receipt.message_id.as_deref() != Some(&message.id) {
        return Err(StageError::Conflict(
            "Remote receipt points to another input",
        ));
    }
    receipt.state = match message.status {
        MessageStatus::Queued => InvocationState::DurableReceived,
        MessageStatus::Running => InvocationState::Running,
        MessageStatus::Completed => InvocationState::Completed,
        MessageStatus::Failed => InvocationState::Failed,
        MessageStatus::Cancelled => InvocationState::Cancelled,
        MessageStatus::Interrupted => InvocationState::ResultUnknown,
    };
    let content = message.reply.as_deref().or(message.error.as_deref());
    receipt.result = content.map(|text| BlobRef {
        hash: super::digest(text.as_bytes()),
        size: text.len() as u64,
        version: 1,
    });
    if let Some(reference) = &receipt.result {
        tx.execute("INSERT OR IGNORE INTO stage_message_content(actor,hash,message,field,size) VALUES(?1,?2,?3,?4,?5)",params![message.actor_id,reference.hash,message.id,if message.reply.is_some(){"reply"}else{"error"},reference.size as i64])?;
    }
    tx.execute(
        "UPDATE stage_inbox SET record=?1,finished=?2 WHERE peer=?3 AND invocation=?4",
        params![
            encode(&receipt)?,
            if matches!(
                message.status,
                MessageStatus::Completed | MessageStatus::Failed | MessageStatus::Cancelled
            ) {
                message.finished_at_ms
            } else {
                None
            },
            stage_id.to_string(),
            invocation_id.to_string()
        ],
    )?;
    Ok(())
}

pub const DEFAULT_BACKLOG_BYTES: u64 = 128 * 1024 * 1024;
const RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActorGrant {
    pub actor_id: String,
    pub read: bool,
    pub converse: bool,
    pub delegate: bool,
    pub read_only: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRecord {
    pub device: DeviceProfile,
    pub enrollment: Enrollment,
    pub grants: Vec<ActorGrant>,
    pub revoked: bool,
    pub generation: u64,
    pub connection_nonce: Option<Uuid>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub invocation: Uuid,
    pub actor_id: String,
    pub state: InvocationState,
    pub result: Option<BlobRef>,
    pub duplicate: bool,
    #[serde(default)]
    pub message_id: Option<String>,
}
pub struct StageLedger<'a> {
    store: &'a ActorStore,
    local: StageId,
    backlog_bytes: u64,
}
fn encode<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|_| StageError::Invalid("Cannot encode Stage storage"))
}
fn decode<T: DeserializeOwned>(value: String) -> Result<T> {
    serde_json::from_str(&value).map_err(|_| StageError::Invalid("Invalid Stage storage"))
}
pub(crate) fn peer(tx: &Transaction<'_>, id: StageId, now: i64) -> Result<PeerRecord> {
    let record: String = tx
        .query_row(
            "SELECT record FROM stage_peers WHERE id=?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(StageError::Forbidden("Device is not authorized"))?;
    let record: PeerRecord = decode(record)?;
    if record.revoked {
        return Err(StageError::Forbidden("Device is revoked"));
    }
    let root: String =
        tx.query_row("SELECT value FROM stage_meta WHERE key='root'", [], |row| {
            row.get(0)
        })?;
    let revoked = super::revocation::removed_in(tx)?;
    let member = verify_enrollment(
        &decode::<TrustRoot>(root)?,
        &record.enrollment.chain,
        now,
        &revoked,
    )?;
    if member.device != record.device {
        return Err(StageError::Forbidden(
            "Device profile does not match enrollment",
        ));
    }
    Ok(record)
}
pub(crate) fn grant(tx: &Transaction<'_>, peer: &PeerRecord, actor: &str) -> Result<ActorGrant> {
    if core_read(tx, actor)? {
        return Ok(ActorGrant {
            actor_id: actor.into(),
            read: true,
            converse: true,
            delegate: false,
            read_only: false,
        });
    }
    if let Some(grant) = peer.grants.iter().find(|grant| grant.actor_id == actor) {
        return Ok(grant.clone());
    }
    let owned: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='stage_account_actors')",
        [],
        |r| r.get(0),
    )?;
    if owned
        && tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM stage_account_actors WHERE actor=?1 AND account=?2)",
            params![actor, peer.enrollment.root.group_id],
            |r| r.get::<_, bool>(0),
        )?
    {
        return Ok(ActorGrant {
            actor_id: actor.into(),
            read: true,
            converse: true,
            delegate: true,
            read_only: false,
        });
    }
    Err(StageError::Forbidden("Actor is outside device grants"))
}
pub fn inherit_account_ownership(store: &ActorStore, parent: &str, actor: &str) -> Result<()> {
    store.stage_transaction(|tx|{
        tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_account_actors(actor TEXT PRIMARY KEY REFERENCES actors(id) ON DELETE CASCADE,account TEXT NOT NULL);")?;
        let account:Option<String>=if parent==super::shared::ACTOR_ID {
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='core_meta')",[],|r|r.get(0))?;
            if exists {tx.query_row("SELECT value FROM core_meta WHERE key='account'",[],|r|r.get(0)).optional()?}else{None}
        }else {tx.query_row("SELECT account FROM stage_account_actors WHERE actor=?1",[parent],|r|r.get(0)).optional()?};
        if let Some(account)=account{tx.execute("INSERT INTO stage_account_actors VALUES(?1,?2)",params![actor,account])?;}Ok(())
    })
}
/// Bind the device's existing Actor tree when its verified account core starts.
/// Never overwrite a recorded account or traverse through another account's Actor.
pub(crate) fn initialize_account_ownership(store: &ActorStore, account: &str) -> Result<()> {
    store.stage_transaction(|tx| {
        let trusted: TrustRoot = decode(tx.query_row(
            "SELECT value FROM stage_meta WHERE key='root'", [], |r| r.get(0),
        )?)?;
        let core: String = tx.query_row(
            "SELECT value FROM core_meta WHERE key='account'", [], |r| r.get(0),
        )?;
        if trusted.group_id != account || core != account {
            return Err(StageError::Forbidden("Actor account does not match device trust"));
        }
        tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_account_actors(actor TEXT PRIMARY KEY REFERENCES actors(id) ON DELETE CASCADE,account TEXT NOT NULL);")?;
        tx.execute(
            "WITH RECURSIVE owned(actor) AS (
                SELECT id FROM actors WHERE id IN ('root','core')
                  AND NOT EXISTS(SELECT 1 FROM stage_account_actors WHERE actor=id AND account!=?1)
                UNION
                SELECT a.id FROM actors a JOIN owned o ON a.parent_id=o.actor
                  WHERE NOT EXISTS(SELECT 1 FROM stage_account_actors WHERE actor=a.id AND account!=?1)
             ) INSERT OR IGNORE INTO stage_account_actors SELECT actor,?1 FROM owned",
            [account],
        )?;
        Ok(())
    })
}
pub(crate) fn core_read(tx: &Transaction<'_>, actor: &str) -> Result<bool> {
    if actor != super::shared::ACTOR_ID {
        return Ok(false);
    }
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='core_meta')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(false);
    }
    let active: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM core_meta WHERE key='active' AND value='1')",
        [],
        |r| r.get(0),
    )?;
    if !active {
        return Ok(false);
    }
    let account: String =
        tx.query_row("SELECT value FROM core_meta WHERE key='account'", [], |r| {
            r.get(0)
        })?;
    let trusted: TrustRoot = decode(tx.query_row(
        "SELECT value FROM stage_meta WHERE key='root'",
        [],
        |r| r.get(0),
    )?)?;
    Ok(account == trusted.group_id)
}
pub(crate) fn actor_exists(tx: &Transaction<'_>, actor: &str) -> Result<()> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM actors WHERE id=?1)",
        [actor],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(StageError::Forbidden("Actor is deleted or unavailable"));
    }
    Ok(())
}
fn receipt(
    tx: &Transaction<'_>,
    peer: StageId,
    invocation: Uuid,
) -> Result<Option<(String, Receipt)>> {
    tx.query_row(
        "SELECT digest,record FROM stage_inbox WHERE peer=?1 AND invocation=?2",
        params![peer.to_string(), invocation.to_string()],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )
    .optional()?
    .map(|(hash, value)| Ok((hash, decode(value)?)))
    .transpose()
}

impl<'a> StageLedger<'a> {
    pub fn initialize(store: &'a ActorStore, local: StageId, root: &TrustRoot) -> Result<Self> {
        store.stage_transaction(|tx| {
            tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
              CREATE TABLE IF NOT EXISTS stage_peers(id TEXT PRIMARY KEY,record TEXT NOT NULL,revoked INTEGER NOT NULL);
              CREATE TABLE IF NOT EXISTS stage_joins(id TEXT PRIMARY KEY,profile TEXT NOT NULL);
              CREATE TABLE IF NOT EXISTS stage_channel_prepare(peer TEXT PRIMARY KEY,generation INTEGER NOT NULL,nonce TEXT NOT NULL);
              CREATE TABLE IF NOT EXISTS stage_outbox(seq INTEGER PRIMARY KEY AUTOINCREMENT,peer TEXT NOT NULL,message TEXT NOT NULL,invocation TEXT,expires INTEGER NOT NULL,received INTEGER NOT NULL DEFAULT 0,completed INTEGER NOT NULL DEFAULT 0,record BLOB NOT NULL,UNIQUE(peer,message));
              CREATE TABLE IF NOT EXISTS stage_received_results(peer TEXT NOT NULL,invocation TEXT NOT NULL,record TEXT NOT NULL,PRIMARY KEY(peer,invocation));
              CREATE TABLE IF NOT EXISTS stage_message_content(actor TEXT NOT NULL,hash TEXT NOT NULL,message TEXT NOT NULL,field TEXT NOT NULL,size INTEGER NOT NULL,PRIMARY KEY(actor,hash));
              CREATE TABLE IF NOT EXISTS stage_inbox(peer TEXT NOT NULL,invocation TEXT NOT NULL,actor TEXT NOT NULL,digest TEXT NOT NULL,expires INTEGER NOT NULL,finished INTEGER,confirmed INTEGER NOT NULL DEFAULT 0,record TEXT NOT NULL,frame BLOB NOT NULL,PRIMARY KEY(peer,invocation));")?;
            let saved:Option<String>=tx.query_row("SELECT value FROM stage_meta WHERE key='local'",[],|row|row.get(0)).optional()?;
            let public:Option<String>=tx.query_row("SELECT value FROM stage_public_meta WHERE key='stage'",[],|row|row.get(0)).optional()?;
            if public.as_deref().is_some_and(|id|id!=local.to_string()){return Err(StageError::Conflict("Public Stage identity cannot change during enrollment"));}
            tx.execute("INSERT OR IGNORE INTO stage_public_meta VALUES('stage',?1)",[local.to_string()])?;
            if saved.as_deref().is_some_and(|id|id!=local.to_string()) { return Err(StageError::Conflict("Stage identity cannot be replaced")); }
            let trusted:Option<String>=tx.query_row("SELECT value FROM stage_meta WHERE key='root'",[],|row|row.get(0)).optional()?;
            if let Some(trusted)=trusted && decode::<TrustRoot>(trusted)?!=*root { return Err(StageError::Conflict("Group trust cannot change implicitly")); }
            tx.execute("INSERT OR IGNORE INTO stage_meta(key,value) VALUES('local',?1)",[local.to_string()])?;
            tx.execute("INSERT OR IGNORE INTO stage_meta(key,value) VALUES('root',?1)",[encode(root)?])?;
            // A process may have performed an effect before its final result was recorded.
            let mut pending=tx.prepare("SELECT peer,invocation,record FROM stage_inbox WHERE json_extract(record,'$.state')='running'")?;
            let rows=pending.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;drop(pending);
            for (peer,id,value) in rows { let mut value:Receipt=decode(value)?;value.state=InvocationState::ResultUnknown;tx.execute("UPDATE stage_inbox SET record=?1 WHERE peer=?2 AND invocation=?3",params![encode(&value)?,peer,id])?; }
            Ok(())
        })?;
        Ok(Self {
            store,
            local,
            backlog_bytes: DEFAULT_BACKLOG_BYTES,
        })
    }
    /// Attach to already initialized storage; does not perform recovery on each request.
    pub fn attach(store: &'a ActorStore, local: StageId) -> Self {
        Self {
            store,
            local,
            backlog_bytes: DEFAULT_BACKLOG_BYTES,
        }
    }
    pub fn with_backlog_budget(mut self, bytes: u64) -> Self {
        self.backlog_bytes = bytes;
        self
    }
    pub fn export_control_content(
        &self,
        peer_id: StageId,
        export: Uuid,
        content: &BlobRef,
        now: i64,
    ) -> Result<()> {
        content.validate()?;
        self.store.stage_transaction(|tx|{peer(tx,peer_id,now)?;tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_control_exports(peer TEXT NOT NULL,nonce TEXT NOT NULL,hash TEXT NOT NULL,size INTEGER NOT NULL,expires INTEGER NOT NULL,PRIMARY KEY(peer,nonce));")?;
            tx.execute("DELETE FROM stage_control_exports WHERE expires<?1",[now])?;let count:i64=tx.query_row("SELECT COUNT(*) FROM stage_control_exports",[],|r|r.get(0))?;if count>=128{return Err(StageError::Capacity("Control export budget reached"));}
            tx.execute("INSERT INTO stage_control_exports VALUES(?1,?2,?3,?4,?5)",params![peer_id.to_string(),export.to_string(),content.hash,content.size as i64,now+60000])?;Ok(())})
    }
    pub fn actor_grant(&self, id: StageId, actor: &str, now: i64) -> Result<ActorGrant> {
        self.store.stage_transaction(|tx| {
            let current = peer(tx, id, now)?;
            grant(tx, &current, actor)
        })
    }
    pub fn manages_actor(&self, id: StageId, actor: &str, now: i64) -> Result<bool> {
        self.store.stage_transaction(|tx| {
            let member = peer(tx, id, now)?;
            if core_read(tx, actor)? {
                return Ok(true);
            }
            let table: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='stage_account_actors')",
                [],
                |r| r.get(0),
            )?;
            if !table {
                return Ok(false);
            }
            Ok(tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM stage_account_actors WHERE actor=?1 AND account=?2)",
                params![actor, member.enrollment.root.group_id],
                |r| r.get(0),
            )?)
        })
    }
    /// Owner-only entry point. Membership grants network eligibility, not Actor access.
    pub fn import_peer(
        &self,
        enrollment: Enrollment,
        grants: Vec<ActorGrant>,
        now: i64,
    ) -> Result<PeerRecord> {
        self.store.stage_transaction(|tx| {
            let root:String=tx.query_row("SELECT value FROM stage_meta WHERE key='root'",[],|row|row.get(0))?;
            let root:TrustRoot=decode(root)?;
            if root!=enrollment.root { return Err(StageError::Forbidden("Wrong device group")); }
            let revoked = super::revocation::removed_in(tx)?;
            let member=verify_enrollment(&root,&enrollment.chain,now,&revoked)?;
            if member.device.stage_id==self.local { return Err(StageError::Invalid("Invalid peer")); }
            let mut names=HashSet::new();
            for grant in &grants {
                super::ActorAddress {stage_id:self.local,actor_id:grant.actor_id.clone()}.validate()?;
                if !names.insert(&grant.actor_id) || grant.converse&&!grant.read || grant.delegate&&!grant.converse { return Err(StageError::Invalid("Invalid Actor grants")); }
                let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM actors WHERE id=?1)",[&grant.actor_id],|row|row.get(0))?;
                if !exists { return Err(StageError::Invalid("Actor grant target does not exist")); }
            }
            let old:Option<String>=tx.query_row("SELECT record FROM stage_peers WHERE id=?1",[member.device.stage_id.to_string()],|row|row.get(0)).optional()?;
            let old=old.map(decode::<PeerRecord>).transpose()?;
            let records = tx.prepare("SELECT record FROM stage_peers")?
                .query_map([],|row|row.get::<_,String>(0))?
                .collect::<std::result::Result<Vec<_>,_>>()?;
            for record in records {
                let record:PeerRecord = decode(record)?;
                if record.device.stage_id != member.device.stage_id
                    && (record.device.public_key == member.device.public_key || record.device.tls_sha256 == member.device.tls_sha256) {
                    return Err(StageError::Conflict("Device key is already bound to another identity"));
                }
            }
            if old.as_ref().is_some_and(|old|old.device!=member.device||old.revoked) { return Err(StageError::Conflict("Device key changed or authorization was revoked")); }
            let seen:Option<String>=tx.query_row("SELECT profile FROM stage_joins WHERE id=?1",[member.join_id.to_string()],|row|row.get(0)).optional()?;
            let profile=encode(&member.device)?;
            if seen.as_deref().is_some_and(|seen|seen!=profile) { return Err(StageError::Conflict("Join credential was already consumed")); }
            tx.execute("INSERT OR IGNORE INTO stage_joins(id,profile) VALUES(?1,?2)",params![member.join_id.to_string(),profile])?;
            let record=PeerRecord {device:member.device,enrollment,grants,revoked:false,generation:old.as_ref().map_or(0,|peer|peer.generation),connection_nonce:old.and_then(|peer|peer.connection_nonce)};
            tx.execute("INSERT INTO stage_peers(id,record,revoked) VALUES(?1,?2,0) ON CONFLICT(id) DO UPDATE SET record=excluded.record",params![record.device.stage_id.to_string(),encode(&record)?])?;
            Ok(record)
        })
    }
    pub fn peer(&self, id: StageId, now: i64) -> Result<PeerRecord> {
        self.store.stage_transaction(|tx| peer(tx, id, now))
    }
    pub(crate) fn stored_peer(&self, id: StageId) -> Result<Option<PeerRecord>> {
        self.store.stage_transaction(|tx| {
            let data: Option<String> = tx
                .query_row(
                    "SELECT record FROM stage_peers WHERE id=?1",
                    [id.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            data.map(decode).transpose()
        })
    }
    pub fn peers(&self, now: i64) -> Result<Vec<PeerRecord>> {
        self.store.stage_transaction(|tx| {
            let ids = tx
                .prepare("SELECT id FROM stage_peers ORDER BY id")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut values = vec![];
            for id in ids {
                let id = StageId(
                    Uuid::parse_str(&id)
                        .map_err(|_| StageError::Invalid("Invalid stored Stage identity"))?,
                );
                match peer(tx, id, now) {
                    Ok(peer) => values.push(peer),
                    Err(StageError::Forbidden(_)) => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(values)
        })
    }
    pub fn exported_manifest(
        &self,
        id: StageId,
        invocation: Uuid,
        reference: &BlobRef,
        now: i64,
    ) -> Result<super::blob::Manifest> {
        self.store.stage_transaction(|tx| {
            export_scope(tx, id, invocation, reference, now)?;
            let record: String = tx.query_row(
                "SELECT manifest FROM stage_blobs WHERE hash=?1 AND size=?2",
                params![reference.hash, reference.size as i64],
                |r| r.get(0),
            )?;
            let manifest: super::blob::Manifest = decode(record)?;
            if &manifest.content != reference {
                return Err(StageError::Conflict("Exported content changed"));
            }
            Ok(manifest)
        })
    }
    pub fn exported_chunk(
        &self,
        id: StageId,
        invocation: Uuid,
        reference: &BlobRef,
        chunk: u32,
        now: i64,
    ) -> Result<Vec<u8>> {
        self.store.stage_transaction(|tx| {
            export_scope(tx, id, invocation, reference, now)?;
            let record: String = tx.query_row(
                "SELECT manifest FROM stage_blobs WHERE hash=?1 AND size=?2",
                params![reference.hash, reference.size as i64],
                |r| r.get(0),
            )?;
            let manifest: super::blob::Manifest = decode(record)?;
            if &manifest.content != reference || chunk as usize >= manifest.chunks.len() {
                return Err(StageError::Invalid("Exported chunk is out of range"));
            }
            let bytes: Vec<u8> = tx.query_row(
                "SELECT substr(bytes,?2,?3) FROM stage_blobs WHERE hash=?1",
                params![
                    reference.hash,
                    chunk as i64 * super::protocol::CHUNK_BYTES as i64 + 1,
                    super::protocol::CHUNK_BYTES as i64
                ],
                |r| r.get(0),
            )?;
            if super::digest(&bytes) != manifest.chunks[chunk as usize] {
                return Err(StageError::Conflict("Exported content checksum failed"));
            }
            Ok(bytes)
        })
    }
    pub fn prepare_connection(&self, id: StageId, next: u64, nonce: Uuid, now: i64) -> Result<()> {
        self.store.stage_transaction(|tx| {
            let current=peer(tx,id,now)?;
            if nonce.is_nil() || next!=current.generation.checked_add(1).ok_or(StageError::Capacity("Connection generation exhausted"))? || next>(1u64<<53)-1 {return Err(StageError::Conflict("Connection preparation changed"));}
            tx.execute("INSERT INTO stage_channel_prepare VALUES(?1,?2,?3) ON CONFLICT(peer) DO UPDATE SET generation=excluded.generation,nonce=excluded.nonce",params![id.to_string(),next as i64,nonce.to_string()])?;Ok(())
        })
    }
    /// Recover only a generation that this device durably prepared on an earlier
    /// mutually authenticated link. A peer's claimed counter alone grants nothing.
    pub fn recover_connection(
        &self,
        id: StageId,
        foreign: u64,
        nonce: Option<Uuid>,
        now: i64,
    ) -> Result<u64> {
        self.store.stage_transaction(|tx| {
            let mut current = peer(tx, id, now)?;
            if foreign == current.generation {
                if foreign > 0 && nonce != current.connection_nonce {
                    return Err(StageError::Conflict("Connection history diverged"));
                }
                return Ok(current.generation);
            }
            if foreign.checked_add(1) == Some(current.generation) {
                return Ok(current.generation);
            }
            if current.generation.checked_add(1) != Some(foreign) {
                return Err(StageError::Conflict("Connection history requires recovery"));
            }
            let saved: Option<(i64, String)> = tx
                .query_row(
                    "SELECT generation,nonce FROM stage_channel_prepare WHERE peer=?1",
                    [id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if !saved.is_some_and(|(generation, saved)| {
                generation as u64 == foreign
                    && nonce.is_some_and(|nonce| nonce.to_string() == saved)
            }) {
                return Err(StageError::Conflict(
                    "Peer generation was not prepared here",
                ));
            }
            current.generation = foreign;
            current.connection_nonce = nonce;
            tx.execute(
                "UPDATE stage_peers SET record=?1 WHERE id=?2",
                params![encode(&current)?, id.to_string()],
            )?;
            Ok(foreign)
        })
    }
    pub fn message_content(
        &self,
        id: StageId,
        actor: &str,
        reference: &BlobRef,
        chunk: u32,
        now: i64,
    ) -> Result<Vec<u8>> {
        reference.validate()?;
        self.store.stage_transaction(|tx| {
            let peer = peer(tx, id, now)?;
            actor_exists(tx, actor)?;
            if !grant(tx, &peer, actor)?.read {
                return Err(StageError::Forbidden("Content is outside Actor grants"));
            }
            let (message, field): (String, String) = tx
                .query_row(
                    "SELECT message,field FROM stage_message_content WHERE actor=?1 AND hash=?2",
                    params![actor, reference.hash],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or(StageError::Forbidden(
                    "Content is not associated with this Actor",
                ))?;
            let record: String = tx.query_row(
                "SELECT record FROM messages WHERE id=?1 AND actor_id=?2",
                params![message, actor],
                |row| row.get(0),
            )?;
            let record: crate::actors::MessageRecord = decode(record)?;
            let bytes = if field == "reply" {
                record.reply
            } else {
                record.error
            }
            .ok_or(StageError::Invalid("Content is unavailable"))?
            .into_bytes();
            if bytes.len() as u64 != reference.size || super::digest(&bytes) != reference.hash {
                return Err(StageError::Conflict("Content version changed"));
            }
            let start = (chunk as usize)
                .checked_mul(super::protocol::CHUNK_BYTES)
                .ok_or(StageError::Invalid("Invalid content chunk"))?;
            if start >= bytes.len() && !(start == 0 && bytes.is_empty()) {
                return Err(StageError::Invalid("Content chunk is out of range"));
            }
            Ok(bytes[start..bytes.len().min(start + super::protocol::CHUNK_BYTES)].to_vec())
        })
    }
    pub fn revoke(&self, id: StageId) -> Result<()> {
        self.store.stage_transaction(|tx| {
            let record: String = tx.query_row(
                "SELECT record FROM stage_peers WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )?;
            let mut record: PeerRecord = decode(record)?;
            record.revoked = true;
            tx.execute(
                "UPDATE stage_peers SET record=?1,revoked=1 WHERE id=?2",
                params![encode(&record)?, id.to_string()],
            )?;
            Ok(())
        })
    }
    /// The connection controller calls this only after mutual authentication/progress/commit.
    pub fn commit_generation(
        &self,
        id: StageId,
        expected: u64,
        next: u64,
        nonce: Uuid,
        now: i64,
    ) -> Result<()> {
        self.store.stage_transaction(|tx| {
            let mut record = peer(tx, id, now)?;
            if record.generation != expected
                || next
                    != expected
                        .checked_add(1)
                        .ok_or(StageError::Capacity("Connection generation exhausted"))?
            {
                return Err(StageError::Conflict(
                    "Concurrent connection generation changed",
                ));
            }
            record.generation = next;
            record.connection_nonce = Some(nonce);
            tx.execute(
                "UPDATE stage_peers SET record=?1 WHERE id=?2",
                params![encode(&record)?, id.to_string()],
            )?;
            Ok(())
        })
    }
    pub fn enqueue(&self, frame: &Envelope, now: i64) -> Result<()> {
        if frame.source != self.local || frame.expires_at_ms <= now {
            return Err(StageError::Invalid("Invalid outgoing transfer"));
        }
        self.store
            .stage_transaction(|tx| enqueue_tx(tx, frame, now, self.backlog_bytes))
    }
    pub fn pending(&self, id: StageId, now: i64, limit: usize) -> Result<Vec<Envelope>> {
        self.store.stage_transaction(|tx| { peer(tx,id,now)?;let mut query=tx.prepare("SELECT record FROM stage_outbox WHERE peer=?1 AND received=0 ORDER BY seq LIMIT ?2")?;let rows=query.query_map(params![id.to_string(),limit.clamp(1,100) as i64],|row|row.get::<_,Vec<u8>>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;rows.iter().map(|row|Envelope::decode(row)).collect() })
    }
    pub fn acknowledge_received(&self, id: StageId, invocation: Uuid, now: i64) -> Result<()> {
        self.store.stage_transaction(|tx| {
            peer(tx, id, now)?;
            tx.execute(
                "UPDATE stage_outbox SET received=1 WHERE peer=?1 AND invocation=?2 AND json_extract(CAST(record AS TEXT),'$.body.type')='invoke'",
                params![id.to_string(), invocation.to_string()],
            )?;
            Ok(())
        })
    }
    #[cfg(test)]
    fn accept(&self, id: StageId, frame: &Envelope, now: i64) -> Result<Receipt> {
        self.accept_inner(id, frame, None, now)
    }
    /// Content is verified before acceptance; inbox, dedup and Actor mailbox share one commit.
    pub fn accept_actor_input(
        &self,
        id: StageId,
        frame: &Envelope,
        content: String,
        now: i64,
    ) -> Result<Receipt> {
        self.accept_inner(id, frame, Some(content), now)
    }
    fn accept_inner(
        &self,
        id: StageId,
        frame: &Envelope,
        content: Option<String>,
        now: i64,
    ) -> Result<Receipt> {
        let invocation = frame
            .invocation_id
            .ok_or(StageError::Invalid("Invocation identifier required"))?;
        let Body::Invoke { target, origin, .. } = &frame.body else {
            return Err(StageError::Invalid("Expected an invocation"));
        };
        let bytes = frame.encode()?;
        let hash = frame.invocation_digest()?;
        self.store.stage_transaction(|tx| {
            let peer=peer(tx,id,now)?;frame.validate(id,self.local,peer.generation,now)?;
            actor_exists(tx,&target.actor_id)?;
            let permission=grant(tx,&peer,&target.actor_id)?;
            if !permission.converse || matches!(origin,super::protocol::Origin::Actor{..})&&!permission.delegate { return Err(StageError::Forbidden("Device cannot invoke this Actor")); }
            if let Some((previous,mut receipt))=receipt(tx,id,invocation)? { if previous!=hash { return Err(StageError::Conflict("Invocation identifier changed content")); }receipt.duplicate=true;return Ok(receipt); }
            let pending:i64=tx.query_row("SELECT COALESCE(SUM(length(frame)+COALESCE(json_extract(CAST(frame AS TEXT),'$.body.content.size'),0)+COALESCE((SELECT SUM(json_extract(value,'$.content.size')) FROM json_each(CAST(frame AS TEXT),'$.body.attachments')),0)),0) FROM stage_inbox WHERE finished IS NULL",[],|row|row.get(0))?;
            if (pending as u64).saturating_add(transfer_size(frame,bytes.len()))>self.backlog_bytes { return Err(StageError::Capacity("Persistent inbox is full")); }
            let message_id=if let Some(content)=content {
                let Body::Invoke {content:reference,attachments,origin,..}=&frame.body else {return Err(StageError::Invalid("Expected invocation input"));};
                if content.len()>64*1024 || (content.trim().is_empty() && attachments.is_empty()) || super::digest(content.as_bytes())!=reference.hash || content.len() as u64!=reference.size {return Err(StageError::Invalid("Invocation content is unavailable or not verified"));}
                let mut admitted_attachments = Vec::new();
                for attachment in attachments {
                    attachment.validate()?;
                    let record:String=tx.query_row("SELECT record FROM stage_resources WHERE actor=?1 AND id=?2",params![target.actor_id,attachment.id],|r|r.get(0)).optional()?.ok_or(StageError::Forbidden("Attachment is outside the delivery ledger"))?;
                    let stored:crate::resources::Resource=serde_json::from_str(&record).map_err(|_|StageError::Invalid("Invalid resource descriptor"))?;
                    if stored != *attachment{return Err(StageError::Conflict("Attachment content changed"));}
                    admitted_attachments.push(crate::actors::Attachment{id:stored.id.clone(),name:stored.name,media_type:stored.media_type,size_bytes:stored.content.size,uri:format!("/v1/actors/{}/resources/{}",target.actor_id,stored.id)});
                }
                let actor=crate::actors::store::actor_tx(tx,&target.actor_id).map_err(|_|StageError::Forbidden("Actor is unavailable"))?;
                let remote_ceiling = match origin {super::protocol::Origin::Actor{ceiling,..}=>Some(ceiling),super::protocol::Origin::User=>None};
                let read_only=actor.config.permissions.read_only||permission.read_only||remote_ceiling.is_some_and(|ceiling|ceiling.read_only);
                let workspace=actor.config.workspace.clone();
                if !actor.config.permissions.readable_roots.iter().any(|root|workspace.starts_with(root)) {return Err(StageError::Forbidden("Actor workspace is unavailable"));}
                let mut capabilities=actor.config.permissions.clone();capabilities.read_only=read_only;capabilities.readable_roots=vec![workspace.clone()];capabilities.writable_roots=if read_only||!actor.config.permissions.writable_roots.iter().any(|root|workspace.starts_with(root)) {vec![]}else{vec![workspace]};capabilities.allow_children=false;
                let mut apps=actor.config.apps;
                if let Some(ceiling)=remote_ceiling {
                    apps.retain(|app|ceiling.apps.contains(app));
                    if let Some(tools)=&ceiling.tools { capabilities.tools=Some(capabilities.tools.as_ref().map_or_else(||tools.clone(),|current|current.iter().filter(|tool|tools.contains(tool)).cloned().collect())); }
                    if crate::actors::permissions::approval_strength(capabilities.approval_mode) < crate::actors::permissions::approval_strength(ceiling.approval_mode) { capabilities.approval_mode=ceiling.approval_mode; }
                }
                let user=matches!(origin,super::protocol::Origin::User);
                let source=match origin {super::protocol::Origin::User=>crate::actors::MessageSource::RemoteUser {stage_id:id,invocation_id:invocation},super::protocol::Origin::Actor {actor,..}=>crate::actors::MessageSource::RemoteActor {stage_id:id,actor_id:actor.actor_id.clone(),invocation_id:invocation}};
                let input=crate::actors::MessageRecord {seq:0,id:Uuid::new_v4().to_string(),actor_id:target.actor_id.clone(),source,reply_to:crate::actors::ReplyTarget::Remote {stage_id:id,invocation_id:invocation,user},kind:crate::actors::MessageKind::Request,in_reply_to:None,delegation_grant:Some(crate::actors::DelegationGrant {apps,permissions:capabilities}),remote_authority:Some(crate::actors::RemoteAuthority {peer:id,grant_actor:target.actor_id.clone(),reply_for:None}),content,attachments:admitted_attachments,status:crate::actors::MessageStatus::Queued,cancel_requested:false,started_at_ms:None,finished_at_ms:None,final_generation:None,reply:None,error:None,created_at_ms:now,updated_at_ms:now};
                Some(crate::actors::store::insert_message(tx,input).map_err(|_|StageError::Unavailable("Actor inbox could not be persisted"))?.id)
            }else{None};
            let receipt=Receipt {invocation,actor_id:target.actor_id.clone(),state:InvocationState::DurableReceived,result:None,duplicate:false,message_id};
            tx.execute("INSERT INTO stage_inbox(peer,invocation,actor,digest,expires,record,frame) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![id.to_string(),invocation.to_string(),target.actor_id,hash,frame.expires_at_ms,encode(&receipt)?,bytes])?;
            Ok(receipt)
        })
    }
    #[cfg(test)]
    fn begin(&self, id: StageId, invocation: Uuid, now: i64) -> Result<Receipt> {
        self.store.stage_transaction(|tx| {
            let peer = peer(tx, id, now)?;
            let (_, mut value) = receipt(tx, id, invocation)?
                .ok_or(StageError::Invalid("Invocation was not received"))?;
            actor_exists(tx, &value.actor_id)?;
            if !grant(tx, &peer, &value.actor_id)?.converse {
                return Err(StageError::Forbidden("Actor capability was revoked"));
            }
            if value.state != InvocationState::DurableReceived {
                return Err(StageError::Conflict("Invocation cannot execute again"));
            }
            value.state = InvocationState::Running;
            tx.execute(
                "UPDATE stage_inbox SET record=?1 WHERE peer=?2 AND invocation=?3",
                params![encode(&value)?, id.to_string(), invocation.to_string()],
            )?;
            Ok(value)
        })
    }
    #[cfg(test)]
    fn finish(
        &self,
        id: StageId,
        invocation: Uuid,
        state: InvocationState,
        content: Option<BlobRef>,
        now: i64,
    ) -> Result<Receipt> {
        if matches!(
            state,
            InvocationState::DurableReceived | InvocationState::Running
        ) {
            return Err(StageError::Invalid("A result must be terminal"));
        }
        if let Some(content) = &content {
            content.validate()?;
        }
        self.store.stage_transaction(|tx| {
            let (_, mut value) = receipt(tx, id, invocation)?
                .ok_or(StageError::Invalid("Invocation was not received"))?;
            if matches!(
                value.state,
                InvocationState::Completed | InvocationState::Failed | InvocationState::Cancelled
            ) {
                if value.state == state && value.result == content {
                    return Ok(value);
                }
                return Err(StageError::Conflict("Invocation result is immutable"));
            }
            value.state = state;
            value.result = content;
            tx.execute(
                "UPDATE stage_inbox SET record=?1,finished=?2 WHERE peer=?3 AND invocation=?4",
                params![
                    encode(&value)?,
                    if value.state == InvocationState::ResultUnknown {
                        None
                    } else {
                        Some(now)
                    },
                    id.to_string(),
                    invocation.to_string()
                ],
            )?;
            Ok(value)
        })
    }
    pub fn result(&self, id: StageId, invocation: Uuid, now: i64) -> Result<Receipt> {
        self.store.stage_transaction(|tx| {
            let peer = peer(tx, id, now)?;
            let (_, value) = receipt(tx, id, invocation)?
                .ok_or(StageError::Invalid("Invocation was not received"))?;
            actor_exists(tx, &value.actor_id)?;
            if !grant(tx, &peer, &value.actor_id)?.read {
                return Err(StageError::Forbidden("Result access was revoked"));
            }
            Ok(value)
        })
    }
    pub fn confirm_result(&self, id: StageId, invocation: Uuid, now: i64) -> Result<()> {
        self.store.stage_transaction(|tx| {peer(tx,id,now)?;tx.execute("UPDATE stage_inbox SET confirmed=1 WHERE peer=?1 AND invocation=?2 AND finished IS NOT NULL",params![id.to_string(),invocation.to_string()])?;Ok(())})
    }
    pub fn collect_confirmed(&self, now: i64) -> Result<usize> {
        self.store.stage_transaction(|tx| {
            let count = tx.execute(
                "DELETE FROM stage_inbox WHERE confirmed=1 AND finished<?1 AND expires<?2",
                params![now.saturating_sub(RETENTION_MS), now],
            )?;
            tx.execute(
                "DELETE FROM stage_outbox WHERE received=1 AND completed=1 AND expires<?1",
                [now.saturating_sub(RETENTION_MS)],
            )?;
            Ok(count)
        })
    }
    pub fn record_result(
        &self,
        id: StageId,
        invocation: Uuid,
        state: InvocationState,
        content: Option<BlobRef>,
        now: i64,
    ) -> Result<()> {
        self.store
            .stage_transaction(|tx| record_result(tx, id, invocation, state, content, now))
    }
}
fn export_scope(
    tx: &Transaction<'_>,
    id: StageId,
    invocation: Uuid,
    reference: &BlobRef,
    now: i64,
) -> Result<()> {
    reference.validate()?;
    peer(tx, id, now)?;
    let controls: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='stage_control_exports')",
        [],
        |r| r.get(0),
    )?;
    if controls && tx.query_row("SELECT EXISTS(SELECT 1 FROM stage_control_exports WHERE peer=?1 AND nonce=?2 AND hash=?3 AND size=?4 AND expires>?5)",params![id.to_string(),invocation.to_string(),reference.hash,reference.size as i64,now],|r|r.get::<_,bool>(0))? {return Ok(());}
    let frame:Vec<u8>=tx.query_row("SELECT record FROM stage_outbox WHERE peer=?1 AND invocation=?2 AND expires>?3 LIMIT 1",params![id.to_string(),invocation.to_string(),now],|r|r.get(0)).optional()?.ok_or(StageError::Forbidden("No content export to this device"))?;
    let frame = Envelope::decode(&frame)?;
    let Body::Invoke {
        content,
        attachments,
        ..
    } = frame.body
    else {
        return Err(StageError::Forbidden("Invocation has no content export"));
    };
    if &content != reference
        && !attachments
            .iter()
            .any(|attachment| &attachment.content == reference)
    {
        return Err(StageError::Forbidden(
            "Content is outside invocation export",
        ));
    }
    Ok(())
}

pub(crate) fn enqueue_tx(
    tx: &Transaction<'_>,
    frame: &Envelope,
    now: i64,
    budget: u64,
) -> Result<()> {
    peer(tx, frame.target, now)?;
    if frame.expires_at_ms <= now {
        return Err(StageError::Invalid("Outgoing transfer expired"));
    }
    let bytes = frame.encode()?;
    let existing: Option<Vec<u8>> = tx
        .query_row(
            "SELECT record FROM stage_outbox WHERE peer=?1 AND message=?2",
            params![frame.target.to_string(), frame.message_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != bytes {
            return Err(StageError::Conflict("Outgoing message identifier changed"));
        }
        return Ok(());
    }
    let pending:i64=tx.query_row("SELECT COALESCE(SUM(length(record)+COALESCE(json_extract(CAST(record AS TEXT),'$.body.content.size'),0)+COALESCE((SELECT SUM(json_extract(value,'$.content.size')) FROM json_each(CAST(record AS TEXT),'$.body.attachments')),0)),0) FROM stage_outbox WHERE completed=0",[],|row|row.get(0))?;
    if (pending as u64).saturating_add(transfer_size(frame, bytes.len())) > budget {
        return Err(StageError::Capacity("Persistent outbox is full"));
    }
    tx.execute(
        "INSERT INTO stage_outbox(peer,message,invocation,expires,record) VALUES(?1,?2,?3,?4,?5)",
        params![
            frame.target.to_string(),
            frame.message_id.to_string(),
            frame.invocation_id.map(|id| id.to_string()),
            frame.expires_at_ms,
            bytes
        ],
    )?;
    Ok(())
}
pub(crate) fn record_result(
    tx: &Transaction<'_>,
    id: StageId,
    invocation: Uuid,
    state: InvocationState,
    content: Option<BlobRef>,
    now: i64,
) -> Result<()> {
    if matches!(
        state,
        InvocationState::DurableReceived | InvocationState::Running
    ) {
        return Err(StageError::Invalid("Received result must be terminal"));
    }
    if let Some(content) = &content {
        content.validate()?;
    }
    peer(tx, id, now)?;
    let original:Option<Vec<u8>>=tx.query_row("SELECT record FROM stage_outbox WHERE peer=?1 AND invocation=?2 AND json_extract(record,'$.body.type')='invoke'",params![id.to_string(),invocation.to_string()],|row|row.get(0)).optional()?;
    let original = Envelope::decode(&original.ok_or(StageError::Forbidden(
        "Result does not match an outgoing invocation",
    ))?)?;
    let Body::Invoke { target, .. } = original.body else {
        return Err(StageError::Invalid("Invalid outgoing request"));
    };
    let result = Receipt {
        invocation,
        actor_id: target.actor_id,
        state,
        result: content,
        duplicate: false,
        message_id: None,
    };
    let encoded = encode(&result)?;
    let previous: Option<String> = tx
        .query_row(
            "SELECT record FROM stage_received_results WHERE peer=?1 AND invocation=?2",
            params![id.to_string(), invocation.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(previous) = previous {
        if previous == encoded {
            return Ok(());
        } else if decode::<Receipt>(previous)?.state != InvocationState::ResultUnknown {
            return Err(StageError::Conflict("Received result changed"));
        }
    }
    tx.execute("INSERT INTO stage_received_results(peer,invocation,record) VALUES(?1,?2,?3) ON CONFLICT(peer,invocation) DO UPDATE SET record=excluded.record",params![id.to_string(),invocation.to_string(),encoded])?;
    tx.execute(
        "UPDATE stage_outbox SET received=1,completed=?3 WHERE peer=?1 AND invocation=?2",
        params![
            id.to_string(),
            invocation.to_string(),
            result.state != InvocationState::ResultUnknown
        ],
    )?;
    Ok(())
}

fn transfer_size(frame: &Envelope, encoded: usize) -> u64 {
    let mut size = encoded as u64;
    if let Body::Invoke {
        content,
        attachments,
        ..
    } = &frame.body
    {
        size = size.saturating_add(content.size);
        for reference in attachments {
            size = size.saturating_add(reference.content.size);
        }
    }
    if let Body::Result {
        content: Some(reference),
        ..
    }
    | Body::Delivery {
        content: reference, ..
    } = &frame.body
    {
        size = size.saturating_add(reference.size);
    }
    size
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actors::{ActorConfig, ActorPermissions, ActorRecord, ActorStatus};
    use crate::stage::{
        ActorAddress, digest,
        identity::DeviceIdentity,
        protocol::{Origin, invocation_id},
    };
    struct Fixture {
        temp: tempfile::TempDir,
        store: ActorStore,
        local: DeviceIdentity,
        remote: DeviceIdentity,
        enrollment: Enrollment,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let store = ActorStore::open(&temp.path().join("actors.sqlite3")).unwrap();
            store
                .save_actor(&ActorRecord {
                    context: Default::default(),
                    id: "root".into(),
                    parent_id: None,
                    depth: 0,
                    config: ActorConfig {
                        name: "test host".into(),
                        workspace: temp.path().to_path_buf(),
                        apps: vec!["files".into()],
                        model: None,
                        instructions: String::new(),
                        permissions: ActorPermissions::workspace(temp.path().to_path_buf(), true),
                    },
                    status: ActorStatus::Idle,
                    created_at_ms: 1000,
                    updated_at_ms: 1000,
                })
                .unwrap();
            let mut local = DeviceIdentity::generate().unwrap();
            let remote = DeviceIdentity::generate().unwrap();
            let root = local
                .initialize_group(1000, 1000 + 90 * 24 * 60 * 60 * 1000)
                .unwrap();
            let enrollment = local
                .authorize(
                    &root,
                    &remote.join_request(1000).unwrap(),
                    1000,
                    1000 + 60 * 24 * 60 * 60 * 1000,
                    false,
                )
                .unwrap();
            let fixture = Self {
                temp,
                store,
                local,
                remote,
                enrollment,
            };
            let ledger = StageLedger::initialize(
                &fixture.store,
                fixture.local.profile().stage_id,
                &fixture.enrollment.root,
            )
            .unwrap();
            ledger
                .import_peer(
                    fixture.enrollment.clone(),
                    vec![ActorGrant {
                        actor_id: "root".into(),
                        read: true,
                        converse: true,
                        delegate: false,
                        read_only: true,
                    }],
                    1000,
                )
                .unwrap();
            ledger
                .commit_generation(
                    fixture.remote.profile().stage_id,
                    0,
                    1,
                    Uuid::new_v4(),
                    1000,
                )
                .unwrap();
            fixture
        }
        fn ledger(&self) -> StageLedger<'_> {
            StageLedger::attach(&self.store, self.local.profile().stage_id)
        }
        fn request(&self) -> Envelope {
            Envelope {
                version: 1,
                source: self.remote.profile().stage_id,
                target: self.local.profile().stage_id,
                generation: 1,
                message_id: Uuid::new_v4(),
                invocation_id: Some(invocation_id(2000).unwrap()),
                in_reply_to: None,
                created_at_ms: 2000,
                expires_at_ms: 12000,
                body: Body::Invoke {
                    origin: Origin::User,
                    target: ActorAddress {
                        stage_id: self.local.profile().stage_id,
                        actor_id: "root".into(),
                    },
                    content: BlobRef {
                        hash: digest(b"request"),
                        size: 7,
                        version: 1,
                    },
                    attachments: vec![],
                },
            }
        }
    }
    #[test]
    fn remote_actor_sender_ceiling_survives_admission_as_private_server_state() {
        use crate::actors::ApprovalMode;
        use crate::stage::protocol::RemoteCeiling;
        let fixture = Fixture::new();
        let peer = fixture.remote.profile().stage_id;
        let mut actor = fixture.store.actor("root").unwrap();
        actor.config.permissions.read_only = false;
        actor.config.permissions.writable_roots = vec![fixture.temp.path().to_path_buf()];
        actor.config.permissions.approval_mode = ApprovalMode::None;
        actor.config.apps = vec!["files".into(), "terminal".into(), "browser".into()];
        fixture.store.save_actor(&actor).unwrap();
        fixture
            .ledger()
            .import_peer(
                fixture.enrollment.clone(),
                vec![ActorGrant {
                    actor_id: "root".into(),
                    read: true,
                    converse: true,
                    delegate: true,
                    read_only: false,
                }],
                2000,
            )
            .unwrap();
        let mut frame = fixture.request();
        if let Body::Invoke { origin, .. } = &mut frame.body {
            *origin = Origin::Actor {
                actor: ActorAddress {
                    stage_id: peer,
                    actor_id: "root".into(),
                },
                request_id: Uuid::new_v4(),
                ceiling: RemoteCeiling {
                    apps: vec!["files".into()],
                    tools: Some(vec!["files__read_file".into()]),
                    read_only: true,
                    approval_mode: ApprovalMode::Ask,
                },
            };
        }
        let receipt = fixture
            .ledger()
            .accept_actor_input(peer, &frame, "request".into(), 2001)
            .unwrap();
        let input = fixture.store.message(&receipt.message_id.unwrap()).unwrap();
        let ceiling = input.delegation_grant.as_ref().unwrap();
        assert_eq!(ceiling.apps, vec!["files".to_string()]);
        assert_eq!(ceiling.permissions.approval_mode, ApprovalMode::Ask);
        assert!(ceiling.permissions.read_only);
        assert!(ceiling.permissions.writable_roots.is_empty());
        assert_eq!(
            ceiling.permissions.tools,
            Some(vec!["files__read_file".into()])
        );
        assert!(input.remote_authority.is_some());
        assert!(
            serde_json::to_value(&input)
                .unwrap()
                .get("remote_authority")
                .is_none()
        );
    }
    #[test]
    fn invocation_export_is_exactly_bound_to_the_recipient_and_content() {
        use crate::stage::blob::{ContentStore, Manifest};
        let fixture = Fixture::new();
        let peer = fixture.remote.profile().stage_id;
        let content = ContentStore::new(&fixture.store);
        let manifest = content.store_source("root", b"request").unwrap();
        let mut frame = fixture.request();
        frame.source = fixture.local.profile().stage_id;
        frame.target = peer;
        if let Body::Invoke { target, .. } = &mut frame.body {
            target.stage_id = peer;
        }
        let id = frame.invocation_id.unwrap();
        fixture.ledger().enqueue(&frame, 2001).unwrap();
        assert_eq!(
            fixture
                .ledger()
                .exported_chunk(peer, id, &manifest.content, 0, 2002)
                .unwrap(),
            b"request"
        );
        let guessed = content.store_source("root", b"private").unwrap();
        assert!(
            fixture
                .ledger()
                .exported_manifest(peer, id, &guessed.content, 2002)
                .is_err()
        );
        let empty = Manifest::from_bytes(&[]).unwrap();
        assert!(
            content
                .begin_download(peer, &empty, 2003)
                .unwrap()
                .is_empty()
        );
        assert!(content.verified_bytes(&empty.content).unwrap().is_empty());
        fixture.ledger().revoke(peer).unwrap();
        assert!(
            fixture
                .ledger()
                .exported_manifest(peer, id, &manifest.content, 2004)
                .is_err()
        );
    }
    #[test]
    fn incremental_encoded_traffic_scales_with_appended_text() {
        use crate::actors::ActorEvent;
        use crate::stage::{journal::Journal, protocol::SubscriptionLevel};
        let fixture = Fixture::new();
        let peer = fixture.remote.profile().stage_id;
        let receipt = fixture
            .ledger()
            .accept_actor_input(peer, &fixture.request(), "request".into(), 2001)
            .unwrap();
        let message = receipt.message_id.unwrap();
        let journal = Journal::new(&fixture.store);
        let mut cursor = journal
            .page(peer, "root", None, SubscriptionLevel::Detail, 2002, 100)
            .unwrap()
            .base;
        let mut totals = vec![];
        let text = "x".repeat(1024);
        let actor = ActorAddress {
            stage_id: fixture.local.profile().stage_id,
            actor_id: "root".into(),
        };
        for count in [32, 64] {
            for _ in 0..count {
                fixture
                    .store
                    .append_event(
                        "root",
                        ActorEvent::GenerationDelta {
                            message_id: message.clone(),
                            generation: 1,
                            channel: "text".into(),
                            text: text.clone(),
                        },
                    )
                    .unwrap();
            }
            let mut frames = 0usize;
            let mut bytes = 0usize;
            loop {
                let page = journal
                    .page(
                        peer,
                        "root",
                        Some(&cursor),
                        SubscriptionLevel::Detail,
                        2003,
                        100,
                    )
                    .unwrap();
                cursor = page.base.clone();
                let done = page.caught_up;
                let frame = Envelope {
                    version: 1,
                    source: fixture.local.profile().stage_id,
                    target: peer,
                    generation: 1,
                    message_id: Uuid::new_v4(),
                    invocation_id: None,
                    in_reply_to: None,
                    created_at_ms: 2000,
                    expires_at_ms: 10000,
                    body: Body::ActorEvents {
                        actor: actor.clone(),
                        page,
                    },
                };
                let encoded = frame.encode().unwrap();
                assert!(encoded.len() <= 65536);
                bytes += encoded.len() + 4;
                frames += 1;
                if done {
                    break;
                }
            }
            totals.push(serde_json::json!({"appended_bytes":count*1024,"encoded_tcp_frame_bytes":bytes,"frames":frames}));
        }
        let small = totals[0]["encoded_tcp_frame_bytes"].as_u64().unwrap();
        let large = totals[1]["encoded_tcp_frame_bytes"].as_u64().unwrap();
        assert!(large < small * 23 / 10);
        assert!(large < 64 * 1024 * 2);
    }
    #[test]
    fn snapshot_restores_active_text_then_applies_only_the_incremental_tail() {
        use crate::actors::ActorEvent;
        use crate::stage::{
            blob::ContentStore,
            journal::{Fact, Journal},
            protocol::SubscriptionLevel,
            sync::Replica,
        };
        let fixture = Fixture::new();
        let peer = fixture.remote.profile().stage_id;
        let receipt = fixture
            .ledger()
            .accept_actor_input(peer, &fixture.request(), "request".into(), 2001)
            .unwrap();
        let id = receipt.message_id.unwrap();
        fixture.store.claim_next("root").unwrap();
        fixture
            .store
            .append_event(
                "root",
                ActorEvent::GenerationDelta {
                    message_id: id.clone(),
                    generation: 1,
                    channel: "text".into(),
                    text: "你好".into(),
                },
            )
            .unwrap();
        let journal = Journal::new(&fixture.store);
        let snapshot = journal.create_snapshot(peer, "root", 2002).unwrap();
        assert!(
            snapshot
                .entries
                .iter()
                .any(|fact| matches!(fact, Fact::TextState { version: 1, .. }))
        );
        ContentStore::new(&fixture.store)
            .store_source("root", b"request")
            .unwrap();
        let address = ActorAddress {
            stage_id: peer,
            actor_id: "root".into(),
        };
        let replica = Replica::new(&fixture.store);
        assert!(replica.receive_snapshot(&address, &snapshot, 2003).unwrap());
        fixture
            .store
            .append_event(
                "root",
                ActorEvent::GenerationDelta {
                    message_id: id,
                    generation: 1,
                    channel: "text".into(),
                    text: " world".into(),
                },
            )
            .unwrap();
        let page = journal
            .page(
                peer,
                "root",
                Some(&snapshot.base),
                SubscriptionLevel::Detail,
                2004,
                100,
            )
            .unwrap();
        assert!(replica.receive(&address, &page, 2004).unwrap().caught_up);
        let text: String = fixture
            .store
            .stage_transaction(|tx| {
                Ok(tx.query_row("SELECT text FROM stage_replica_text", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(text, "你好 world");
        journal.compact("root", &snapshot.base).unwrap();
        let original_global = fixture.store.event_cursor().unwrap();
        fixture
            .store
            .stage_transaction(crate::stage::journal::initialize)
            .unwrap();
        assert_eq!(fixture.store.event_cursor().unwrap(), original_global);
        assert_eq!(
            journal
                .page(
                    peer,
                    "root",
                    Some(&snapshot.base),
                    SubscriptionLevel::Detail,
                    2005,
                    100
                )
                .unwrap()
                .entries
                .len(),
            1
        );
    }
    #[test]
    fn claimed_generation_cannot_advance_without_a_local_preparation() {
        let fixture = Fixture::new();
        let peer = fixture.remote.profile().stage_id;
        assert!(
            fixture
                .ledger()
                .recover_connection(peer, 2, Some(Uuid::new_v4()), 2000)
                .is_err()
        );
        let nonce = Uuid::new_v4();
        fixture
            .ledger()
            .prepare_connection(peer, 2, nonce, 2001)
            .unwrap();
        assert!(
            fixture
                .ledger()
                .recover_connection(peer, 2, Some(Uuid::new_v4()), 2002)
                .is_err()
        );
        assert_eq!(
            fixture
                .ledger()
                .recover_connection(peer, 2, Some(nonce), 2003)
                .unwrap(),
            2
        );
        assert!(
            fixture
                .ledger()
                .recover_connection(peer, 2, Some(Uuid::new_v4()), 2004)
                .is_err()
        );
    }
    #[tokio::test]
    async fn two_stages_recover_half_committed_generation_over_real_mutual_tls() {
        use crate::stage::{connection, transport::tls};
        let fixture = Fixture::new();
        let second_dir = tempfile::tempdir().unwrap();
        let second = ActorStore::open(&second_dir.path().join("actors.sqlite3")).unwrap();
        second
            .save_actor(&fixture.store.actor("root").unwrap())
            .unwrap();
        let first_id = fixture.local.profile().stage_id;
        let second_id = fixture.remote.profile().stage_id;
        let second_ledger =
            StageLedger::initialize(&second, second_id, &fixture.enrollment.root).unwrap();
        second_ledger
            .import_peer(
                Enrollment {
                    root: fixture.enrollment.root.clone(),
                    chain: fixture.enrollment.chain[..1].to_vec(),
                },
                vec![ActorGrant {
                    actor_id: "root".into(),
                    read: true,
                    converse: true,
                    delegate: false,
                    read_only: true,
                }],
                2000,
            )
            .unwrap();
        let nonce = fixture
            .ledger()
            .peer(second_id, 2000)
            .unwrap()
            .connection_nonce
            .unwrap();
        second_ledger
            .prepare_connection(first_id, 1, nonce, 2000)
            .unwrap();
        let acceptor = tls::server(&fixture.local, &[fixture.remote.profile()]).unwrap();
        let connector = tls::client(&fixture.remote, &fixture.local.profile()).unwrap();
        let (left, right) = tokio::io::duplex(65536);
        let (left, right) = tokio::join!(
            tls::accept(&acceptor, left),
            tls::connect(&connector, first_id, right)
        );
        let mut left = left.unwrap();
        let mut right = right.unwrap();
        let peer_left = tls::identify_client(&left, &[fixture.remote.profile()]).unwrap();
        let peer_right = tls::identify_server(&right, &fixture.local.profile()).unwrap();
        let binding_left = hex::encode(
            left.get_ref()
                .1
                .export_keying_material([0; 32], b"EXPORTER-DaatLocus-Stage-v1", None)
                .unwrap(),
        );
        let binding_right = hex::encode(
            right
                .get_ref()
                .1
                .export_keying_material([0; 32], b"EXPORTER-DaatLocus-Stage-v1", None)
                .unwrap(),
        );
        assert_eq!(binding_left, binding_right);
        let first_ledger = fixture.ledger();
        let (a, b) = tokio::join!(
            connection::handshake(
                &mut left,
                &fixture.local,
                &first_ledger,
                &peer_left,
                &binding_left,
                2001
            ),
            connection::handshake(
                &mut right,
                &fixture.remote,
                &second_ledger,
                &peer_right,
                &binding_right,
                2001
            )
        );
        let a = a.unwrap();
        let b = b.unwrap();
        assert_eq!(a, b);
        assert_eq!(a.0, 2);
        assert_eq!(
            first_ledger.peer(second_id, 2002).unwrap().connection_nonce,
            Some(a.1)
        );
        assert_eq!(second_ledger.peer(first_id, 2002).unwrap().generation, 2);
    }
    #[test]
    fn replica_waits_for_missing_body_and_gap_without_duplicate_upstream_views() {
        use crate::actors::ActorEvent;
        use crate::stage::{
            blob::{ContentStore, Manifest},
            journal::Journal,
            protocol::SubscriptionLevel,
            sync::{Replica, Subscriptions},
        };
        let fixture = Fixture::new();
        let peer = fixture.remote.profile().stage_id;
        let request = fixture.request();
        let receipt = fixture
            .ledger()
            .accept_actor_input(peer, &request, "request".into(), 2001)
            .unwrap();
        let id = receipt.message_id.unwrap();
        for text in ["hello", " world"] {
            fixture
                .store
                .append_event(
                    "root",
                    ActorEvent::GenerationDelta {
                        message_id: id.clone(),
                        generation: 1,
                        channel: "text".into(),
                        text: text.into(),
                    },
                )
                .unwrap();
        }
        let journal = Journal::new(&fixture.store);
        let first = journal
            .page(peer, "root", None, SubscriptionLevel::Detail, 2002, 2)
            .unwrap();
        let second = journal
            .page(
                peer,
                "root",
                Some(&first.base),
                SubscriptionLevel::Detail,
                2002,
                2,
            )
            .unwrap();
        let address = ActorAddress {
            stage_id: peer,
            actor_id: "root".into(),
        };
        let replica = Replica::new(&fixture.store);
        let out_of_order = replica.receive(&address, &second, 2003).unwrap();
        assert_eq!(out_of_order.durable_received.sequence, 4);
        assert_eq!(out_of_order.contiguous_received.sequence, 0);
        assert!(!out_of_order.caught_up);
        let blocked = replica.receive(&address, &first, 2004).unwrap();
        assert_eq!(blocked.contiguous_received.sequence, 4);
        assert_eq!(blocked.contiguous_applied.sequence, 0);
        let missing = replica.missing_content(&address, 2005).unwrap();
        assert_eq!(missing.len(), 1);
        let cache = ContentStore::new(&fixture.store);
        let manifest = Manifest::from_bytes(b"request").unwrap();
        cache.begin_download(peer, &manifest, 2005).unwrap();
        cache
            .receive_chunk(peer, &manifest.content.hash, 0, b"request", 2006)
            .unwrap();
        let applied = replica.apply_available(&address, 2007).unwrap();
        assert!(applied.caught_up);
        assert_eq!(applied.contiguous_applied.sequence, 4);
        assert!(replica.receive(&address, &second, 2008).unwrap().caught_up);
        let mut views = Subscriptions::default();
        let view_a = Uuid::new_v4();
        let view_b = Uuid::new_v4();
        assert!(
            views
                .attach(address.clone(), view_a, SubscriptionLevel::Summary)
                .unwrap()
        );
        assert!(
            !views
                .attach(address.clone(), view_b, SubscriptionLevel::Detail)
                .unwrap()
        );
        assert_eq!(views.actors().len(), 1);
        assert_eq!(views.level(&address), Some(SubscriptionLevel::Detail));
        assert!(!views.detach(&address, view_b));
        assert_eq!(views.level(&address), Some(SubscriptionLevel::Summary));
        assert!(views.detach(&address, view_a));
    }
    #[test]
    fn control_budget_stays_available_when_bulk_is_full_and_content_remains_fair() {
        use crate::stage::scheduler::{Class, FairQueue};
        let fixture = Fixture::new();
        let mut queue = FairQueue::new(1024 * 1024);
        let mut frame = fixture.request();
        frame.body = Body::BlobChunk {
            content: BlobRef {
                hash: digest(&vec![1u8; 32768]),
                size: 32768,
                version: 1,
            },
            chunk: 0,
            chunk_hash: digest(&vec![1u8; 32768]),
            bytes: vec![1u8; 32768],
        };
        let mut accepted = 0;
        while queue.push(frame.clone(), Class::Blob).is_ok() {
            accepted += 1;
        }
        assert!(accepted > 0);
        frame.body = Body::Cancel {
            invocation: frame.invocation_id.unwrap(),
        };
        for _ in 0..10 {
            queue.push(frame.clone(), Class::Control).unwrap();
        }
        let first = queue.pop().unwrap();
        assert_eq!(first.class, Class::Control);
        let mut content_seen = false;
        for _ in 0..12 {
            if let Some(item) = queue.pop() {
                content_seen |= item.class == Class::Blob;
            }
        }
        assert!(content_seen);
        assert!(queue.metrics.queue_peak_bytes <= 1024 * 1024);
    }
    #[test]
    fn snapshots_transfer_all_messages_beyond_former_history_and_page_quotas() {
        use crate::stage::{blob::ContentStore, journal::Journal, sync::Replica};
        let fixture = Fixture::new();
        let peer = fixture.remote.profile().stage_id;
        let receipt = fixture
            .ledger()
            .accept_actor_input(peer, &fixture.request(), "request".into(), 2001)
            .unwrap();
        let template = fixture.store.message(&receipt.message_id.unwrap()).unwrap();
        fixture
            .store
            .stage_transaction(|tx| {
                for index in 0..10_201 {
                    let mut message = template.clone();
                    message.id = format!("history-{index}");
                    crate::actors::store::insert_message(tx, message)
                        .map_err(|_| StageError::Invalid("Cannot seed retained history"))?;
                }
                Ok(())
            })
            .unwrap();
        ContentStore::new(&fixture.store)
            .store_source("root", b"request")
            .unwrap();
        let journal = Journal::new(&fixture.store);
        let replica = Replica::new(&fixture.store);
        let address = ActorAddress {
            stage_id: peer,
            actor_id: "root".into(),
        };
        let mut page = journal.create_snapshot(peer, "root", 2002).unwrap();
        let mut count = 0;
        loop {
            count += page.entries.len();
            let complete = replica.receive_snapshot(&address, &page, 2003).unwrap();
            assert_eq!(complete, page.last);
            if page.last {
                break;
            }
            page = journal
                .snapshot_page(peer, "root", page.token, page.page + 1, 2003)
                .unwrap();
        }
        assert_eq!(count, 10_202);
        assert!(page.page > 316);
        let (restored, last): (i64, String) = fixture.store.stage_transaction(|tx| {
            let restored = tx.query_row("SELECT COUNT(*) FROM stage_replica_messages WHERE stage=?1 AND actor='root'", [peer.to_string()], |row| row.get(0))?;
            let last = tx.query_row("SELECT json_extract(fact,'$.metadata.id') FROM stage_replica_messages WHERE stage=?1 AND actor='root' AND message='history-10200'", [peer.to_string()], |row| row.get(0))?;
            Ok((restored, last))
        }).unwrap();
        assert_eq!(restored, 10_202);
        assert_eq!(last, "history-10200");
    }

    #[test]
    fn journal_has_dense_actor_offsets_and_consistent_snapshot_boundary() {
        use crate::actors::ActorEvent;
        use crate::stage::journal::{Fact, Journal};
        use crate::stage::protocol::SubscriptionLevel;
        let fixture = Fixture::new();
        let peer = fixture.remote.profile().stage_id;
        let receipt = fixture
            .ledger()
            .accept_actor_input(peer, &fixture.request(), "request".into(), 2001)
            .unwrap();
        let id = receipt.message_id.unwrap();
        for text in ["你好", " world"] {
            fixture
                .store
                .append_event(
                    "root",
                    ActorEvent::GenerationDelta {
                        message_id: id.clone(),
                        generation: 1,
                        channel: "text".into(),
                        text: text.into(),
                    },
                )
                .unwrap();
        }
        let journal = Journal::new(&fixture.store);
        let page = journal
            .page(peer, "root", None, SubscriptionLevel::Detail, 2002, 100)
            .unwrap();
        assert!(page.caught_up);
        assert_eq!(page.base.sequence, 4);
        let deltas: Vec<_> = page
            .entries
            .iter()
            .filter_map(|entry| match &entry.fact {
                Fact::TextAppend {
                    offset, version, ..
                } => Some((*offset, *version)),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec![(0, 1), (6, 2)]);
        let snapshot = journal.create_snapshot(peer, "root", 2003).unwrap();
        fixture
            .store
            .append_event(
                "root",
                ActorEvent::GenerationDelta {
                    message_id: id,
                    generation: 1,
                    channel: "text".into(),
                    text: "!".into(),
                },
            )
            .unwrap();
        assert_eq!(
            journal
                .snapshot_page(peer, "root", snapshot.token, 0, 2004)
                .unwrap()
                .base,
            snapshot.base
        );
        let tail = journal
            .page(
                peer,
                "root",
                Some(&snapshot.base),
                SubscriptionLevel::Detail,
                2004,
                100,
            )
            .unwrap();
        assert_eq!(tail.entries.len(), 1);
        assert!(matches!(
            tail.entries[0].fact,
            Fact::TextAppend {
                offset: 12,
                version: 3,
                ..
            }
        ));
        journal.compact("root", &snapshot.base).unwrap();
        assert!(
            journal
                .page(peer, "root", None, SubscriptionLevel::Detail, 2004, 100)
                .is_err()
        );
        assert_eq!(
            fixture
                .store
                .messages("root", None, 100)
                .unwrap()
                .items
                .len(),
            1
        );
        fixture.ledger().revoke(peer).unwrap();
        assert!(
            journal
                .snapshot_page(peer, "root", snapshot.token, 0, 2005)
                .is_err()
        );
    }
    #[test]
    fn content_resume_reuses_chunks_and_hash_does_not_grant_access() {
        use crate::stage::blob::{ContentStore, Manifest};
        use crate::stage::protocol::CHUNK_BYTES;
        let fixture = Fixture::new();
        let peer = fixture.remote.profile().stage_id;
        let content = ContentStore::new(&fixture.store);
        let bytes = vec![7u8; CHUNK_BYTES * 2 + 17];
        let manifest = Manifest::from_bytes(&bytes).unwrap();
        assert_eq!(
            content.begin_download(peer, &manifest, 2001).unwrap(),
            vec![0, 1, 2]
        );
        assert!(
            !content
                .receive_chunk(peer, &manifest.content.hash, 0, &bytes[..CHUNK_BYTES], 2002)
                .unwrap()
        );
        assert_eq!(
            content.begin_download(peer, &manifest, 2003).unwrap(),
            vec![1, 2]
        );
        assert!(
            !content
                .receive_chunk(
                    peer,
                    &manifest.content.hash,
                    1,
                    &bytes[CHUNK_BYTES..CHUNK_BYTES * 2],
                    2004
                )
                .unwrap()
        );
        assert!(
            content
                .receive_chunk(
                    peer,
                    &manifest.content.hash,
                    2,
                    &bytes[CHUNK_BYTES * 2..],
                    2005
                )
                .unwrap()
        );
        assert_eq!(content.verified_bytes(&manifest.content).unwrap(), bytes);
        assert!(
            content
                .chunk(peer, "root", &manifest.content, 0, 2006)
                .is_err()
        );
        content.store_source("root", &bytes).unwrap();
        assert_eq!(
            content
                .chunk(peer, "root", &manifest.content, 2, 2007)
                .unwrap(),
            vec![7u8; 17]
        );
        fixture.ledger().revoke(peer).unwrap();
        assert!(
            content
                .manifest(peer, "root", &manifest.content, 2008)
                .is_err()
        );
    }
    #[test]
    fn durable_receive_retry_and_link_change_do_not_restart_a_claimed_invocation() {
        let fixture = Fixture::new();
        let mut frame = fixture.request();
        let id = frame.invocation_id.unwrap();
        let peer = frame.source;
        let ledger = fixture.ledger();
        assert!(!ledger.accept(peer, &frame, 2001).unwrap().duplicate);
        assert!(ledger.accept(peer, &frame, 2002).unwrap().duplicate);
        ledger.begin(peer, id, 2003).unwrap();
        assert!(ledger.begin(peer, id, 2004).is_err());
        ledger
            .commit_generation(peer, 1, 2, Uuid::new_v4(), 2005)
            .unwrap();
        assert!(ledger.accept(peer, &frame, 2006).is_err());
        frame.generation = 2;
        frame.message_id = Uuid::new_v4();
        assert_eq!(
            ledger.accept(peer, &frame, 2007).unwrap().state,
            InvocationState::Running
        );
        let recovered = StageLedger::initialize(
            &fixture.store,
            fixture.local.profile().stage_id,
            &fixture.enrollment.root,
        )
        .unwrap();
        assert_eq!(
            recovered.result(peer, id, 2008).unwrap().state,
            InvocationState::ResultUnknown
        );
        assert!(recovered.begin(peer, id, 2009).is_err());
        assert!(fixture.temp.path().join("actors.sqlite3").is_file());
    }
    #[test]
    fn conflicting_retry_revocation_and_missing_actor_do_not_accept_an_invocation() {
        let fixture = Fixture::new();
        let mut frame = fixture.request();
        let ledger = fixture.ledger();
        ledger.accept(frame.source, &frame, 2001).unwrap();
        if let Body::Invoke { content, .. } = &mut frame.body {
            content.hash = digest(b"different");
        }
        assert!(ledger.accept(frame.source, &frame, 2002).is_err());
        ledger.revoke(frame.source).unwrap();
        assert!(
            ledger
                .result(frame.source, frame.invocation_id.unwrap(), 2003)
                .is_err()
        );
        let fixture = Fixture::new();
        let frame = fixture.request();
        fixture
            .store
            .stage_transaction(|tx| {
                tx.execute("DELETE FROM actors WHERE id='root'", [])?;
                Ok(())
            })
            .unwrap();
        assert!(fixture.ledger().accept(frame.source, &frame, 2001).is_err());
    }
    #[test]
    fn account_tree_migration_preserves_foreign_ownership_and_device_revocation() {
        let fixture = Fixture::new();
        let account = fixture.enrollment.root.group_id.clone();
        super::super::shared_store::initialize(&fixture.store, &account).unwrap();
        let root = fixture.store.actor("root").unwrap();
        for (id, parent) in [
            ("legacy", "root"),
            ("nested", "legacy"),
            ("foreign", "root"),
            ("foreign-child", "foreign"),
        ] {
            let mut actor = root.clone();
            actor.id = id.into();
            actor.parent_id = Some(parent.into());
            actor.depth = 1;
            fixture.store.save_actor(&actor).unwrap();
        }
        fixture.store.stage_transaction(|tx| {
            tx.execute_batch("CREATE TABLE stage_account_actors(actor TEXT PRIMARY KEY REFERENCES actors(id) ON DELETE CASCADE,account TEXT NOT NULL); INSERT INTO stage_account_actors VALUES('foreign','another-account');")?;
            Ok(())
        }).unwrap();
        initialize_account_ownership(&fixture.store, &account).unwrap();
        initialize_account_ownership(&fixture.store, &account).unwrap();
        let peer = fixture.remote.profile().stage_id;
        for id in ["legacy", "nested"] {
            assert!(fixture.ledger().actor_grant(peer, id, 2001).unwrap().read);
            assert!(fixture.ledger().manages_actor(peer, id, 2001).unwrap());
        }
        for id in ["foreign", "foreign-child"] {
            assert!(fixture.ledger().actor_grant(peer, id, 2001).is_err());
            assert!(!fixture.ledger().manages_actor(peer, id, 2001).unwrap());
        }
        assert!(initialize_account_ownership(&fixture.store, "another-account").is_err());
        fixture.ledger().revoke(peer).unwrap();
        assert!(fixture.ledger().actor_grant(peer, "legacy", 2001).is_err());
    }
    #[test]
    fn old_retry_cannot_reexecute_after_confirmed_dedup_cleanup() {
        let fixture = Fixture::new();
        let frame = fixture.request();
        let ledger = fixture.ledger();
        let peer = frame.source;
        let id = frame.invocation_id.unwrap();
        ledger.accept(peer, &frame, 2001).unwrap();
        ledger.begin(peer, id, 2002).unwrap();
        ledger
            .finish(
                peer,
                id,
                InvocationState::Completed,
                Some(BlobRef {
                    hash: digest(b"result"),
                    size: 6,
                    version: 1,
                }),
                2003,
            )
            .unwrap();
        ledger.confirm_result(peer, id, 2004).unwrap();
        let later = RETENTION_MS + 20000;
        assert_eq!(ledger.collect_confirmed(later).unwrap(), 1);
        assert!(ledger.accept(peer, &frame, later).is_err());
        let mut refreshed = frame;
        refreshed.created_at_ms = later;
        refreshed.expires_at_ms = later + 10000;
        assert!(ledger.accept(peer, &refreshed, later + 1).is_err());
    }
    #[test]
    fn full_outbox_is_explicit_and_receive_ack_does_not_discard_pending_results() {
        let fixture = Fixture::new();
        let mut outgoing = fixture.request();
        outgoing.source = fixture.local.profile().stage_id;
        outgoing.target = fixture.remote.profile().stage_id;
        if let Body::Invoke { target, .. } = &mut outgoing.body {
            target.stage_id = outgoing.target;
        }
        let size = transfer_size(&outgoing, outgoing.encode().unwrap().len());
        let ledger = fixture.ledger().with_backlog_budget(size);
        ledger.enqueue(&outgoing, 2001).unwrap();
        let peer = outgoing.target;
        let id = outgoing.invocation_id.unwrap();
        ledger.acknowledge_received(peer, id, 2002).unwrap();
        let mut second = outgoing.clone();
        second.message_id = Uuid::new_v4();
        second.invocation_id = Some(invocation_id(2000).unwrap());
        assert!(matches!(
            ledger.enqueue(&second, 2003),
            Err(StageError::Capacity(_))
        ));
        ledger
            .record_result(peer, id, InvocationState::ResultUnknown, None, 2004)
            .unwrap();
        assert!(ledger.enqueue(&second, 2005).is_err());
        ledger
            .record_result(peer, id, InvocationState::Completed, None, 2006)
            .unwrap();
        ledger.enqueue(&second, 2007).unwrap();
        assert_eq!(ledger.pending(peer, 2008, 100).unwrap().len(), 1);
    }
    #[test]
    fn verified_input_receipt_and_actor_mailbox_commit_together_and_keep_remote_provenance() {
        let fixture = Fixture::new();
        let frame = fixture.request();
        let ledger = fixture.ledger();
        let peer = frame.source;
        let first = ledger
            .accept_actor_input(peer, &frame, "request".into(), 2001)
            .unwrap();
        let repeated = ledger
            .accept_actor_input(peer, &frame, "request".into(), 2002)
            .unwrap();
        assert_eq!(first.message_id, repeated.message_id);
        assert!(repeated.duplicate);
        let messages = fixture.store.messages("root", None, 100).unwrap();
        assert_eq!(messages.items.len(), 1);
        let message = &messages.items[0];
        assert!(matches!(
            message.source,
            crate::actors::MessageSource::RemoteUser { .. }
        ));
        assert!(message.remote_authority.is_some());
        assert!(
            message
                .delegation_grant
                .as_ref()
                .unwrap()
                .permissions
                .read_only
        );
        assert!(
            !serde_json::to_value(message)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("remote_authority")
        );
        fixture.store.claim_next("root").unwrap().unwrap();
        assert_eq!(
            ledger
                .result(peer, frame.invocation_id.unwrap(), 2003)
                .unwrap()
                .state,
            InvocationState::Running
        );
        fixture
            .store
            .finish_message(
                &message.id,
                crate::actors::MessageStatus::Completed,
                Some("verified reply"),
                None,
            )
            .unwrap();
        let finished = ledger
            .result(peer, frame.invocation_id.unwrap(), 2004)
            .unwrap();
        assert_eq!(finished.state, InvocationState::Completed);
        assert_eq!(
            ledger
                .message_content(peer, "root", &finished.result.unwrap(), 0, 2005)
                .unwrap(),
            b"verified reply"
        );
    }
}
