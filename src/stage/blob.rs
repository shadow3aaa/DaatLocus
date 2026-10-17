//! Immutable, bounded content. Hashes identify bytes; Actor grants authorize reads.
use super::{
    Result, StageError, StageId, digest, ledger,
    protocol::{BlobRef, CHUNK_BYTES},
};
use crate::actors::store::ActorStore;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

pub const CACHE_BYTES: u64 = 64 * 1024 * 1024;
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub content: BlobRef,
    pub chunks: Vec<String>,
}
impl Manifest {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let content = BlobRef {
            hash: digest(bytes),
            size: bytes.len() as u64,
            version: 1,
        };
        content.validate()?;
        let chunks = bytes.chunks(CHUNK_BYTES).map(digest).collect();
        Ok(Self { content, chunks })
    }
    pub fn validate(&self) -> Result<()> {
        self.content.validate()?;
        if self.chunks.len() != self.content.size.div_ceil(CHUNK_BYTES as u64) as usize
            || self
                .chunks
                .iter()
                .any(|hash| hash.len() != 64 || hex::decode(hash).is_err())
        {
            return Err(StageError::Invalid("Invalid content manifest"));
        }
        Ok(())
    }
}
pub(crate) fn initialize(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_blobs(hash TEXT PRIMARY KEY,size INTEGER NOT NULL,manifest TEXT NOT NULL,bytes BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS stage_blob_access(hash TEXT PRIMARY KEY,last_used INTEGER NOT NULL,pinned_until INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS stage_blob_actors(actor TEXT NOT NULL,hash TEXT NOT NULL REFERENCES stage_blobs(hash) ON DELETE CASCADE,PRIMARY KEY(actor,hash));
        CREATE TABLE IF NOT EXISTS stage_downloads(peer TEXT NOT NULL,hash TEXT NOT NULL,manifest TEXT NOT NULL,PRIMARY KEY(peer,hash));
        CREATE TABLE IF NOT EXISTS stage_blob_parts(peer TEXT NOT NULL,hash TEXT NOT NULL,chunk INTEGER NOT NULL,bytes BLOB NOT NULL,PRIMARY KEY(peer,hash,chunk));
        CREATE TABLE IF NOT EXISTS stage_download_activity(peer TEXT NOT NULL,hash TEXT NOT NULL,touched INTEGER NOT NULL,PRIMARY KEY(peer,hash));
        CREATE TABLE IF NOT EXISTS stage_content_sources(actor TEXT NOT NULL,hash TEXT NOT NULL,message TEXT NOT NULL,field TEXT NOT NULL,PRIMARY KEY(actor,hash));")?;
    Ok(())
}
fn storage_bytes(tx: &Transaction<'_>) -> Result<u64> {
    let bytes:i64=tx.query_row("SELECT (SELECT COALESCE(SUM(length(bytes)),0) FROM stage_blobs)+(SELECT COALESCE(SUM(length(bytes)),0) FROM stage_blob_parts)",[],|row|row.get(0))?;
    Ok(bytes as u64)
}
fn touch(tx: &Transaction<'_>, hash: &str) -> Result<()> {
    let now = chrono::Utc::now().timestamp_millis();
    tx.execute("INSERT INTO stage_blob_access VALUES(?1,?2,?3) ON CONFLICT(hash) DO UPDATE SET last_used=excluded.last_used,pinned_until=MAX(pinned_until,excluded.pinned_until)",params![hash,now,now+30000])?;
    Ok(())
}
fn make_room(tx: &Transaction<'_>, additional: u64) -> Result<()> {
    // Evict cold content toward the cache target without rejecting a larger
    // resource or an active transfer. Durable sources can repopulate cold bytes.
    let now = chrono::Utc::now().timestamp_millis();
    let candidates=tx.prepare("SELECT b.hash,length(b.bytes) FROM stage_blobs b LEFT JOIN stage_blob_access a ON a.hash=b.hash WHERE length(b.bytes)>0 AND COALESCE(a.pinned_until,0)<?1 ORDER BY COALESCE(a.last_used,0)")?.query_map([now],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
    let mut used = storage_bytes(tx)?;
    for (hash, size) in candidates {
        if used.saturating_add(additional) <= CACHE_BYTES {
            break;
        }
        // Accepted deliveries and their exported content stay pinned until
        // the ledger reaches a terminal/acknowledged state.
        let table: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='stage_outbox')",
            [],
            |r| r.get(0),
        )?;
        let outbox=table && tx.query_row("SELECT EXISTS(SELECT 1 FROM stage_outbox o,json_tree(CAST(o.record AS TEXT)) j WHERE (o.received=0 OR o.completed=0) AND j.key='hash' AND j.value=?1)",[&hash],|r|r.get::<_,bool>(0))?;
        let table: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='stage_control_exports')",
            [],
            |r| r.get(0),
        )?;
        let exported = table
            && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM stage_control_exports WHERE hash=?1 AND expires>?2)",
                params![hash, now],
                |r| r.get::<_, bool>(0),
            )?;
        if outbox || exported {
            continue;
        }
        tx.execute("UPDATE stage_blobs SET bytes=x'' WHERE hash=?1", [&hash])?;
        used = used.saturating_sub(size as u64);
    }
    Ok(())
}
pub(crate) fn put(tx: &Transaction<'_>, actor: &str, bytes: &[u8]) -> Result<Manifest> {
    let manifest = Manifest::from_bytes(bytes)?;
    let existing: Option<(i64, i64)> = tx
        .query_row(
            "SELECT size,length(bytes) FROM stage_blobs WHERE hash=?1",
            [&manifest.content.hash],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if existing.is_none_or(|(_, size)| size != bytes.len() as i64) {
        if existing.is_some_and(|(size, _)| size != bytes.len() as i64) {
            return Err(StageError::Conflict("Immutable content changed"));
        }
        make_room(tx, bytes.len() as u64)?;
        tx.execute(
            "INSERT INTO stage_blobs(hash,size,manifest,bytes) VALUES(?1,?2,?3,?4) ON CONFLICT(hash) DO UPDATE SET bytes=excluded.bytes",
            params![
                manifest.content.hash,
                bytes.len() as i64,
                serde_json::to_string(&manifest)
                    .map_err(|_| StageError::Invalid("Cannot encode content manifest"))?,
                bytes
            ],
        )?;
    } else if existing.is_some_and(|(size, _)| size != bytes.len() as i64) {
        return Err(StageError::Conflict("Immutable content changed"));
    }
    touch(tx, &manifest.content.hash)?;
    tx.execute(
        "INSERT OR IGNORE INTO stage_blob_actors(actor,hash) VALUES(?1,?2)",
        params![actor, manifest.content.hash],
    )?;
    Ok(manifest)
}
fn manifest(tx: &Transaction<'_>, hash: &str) -> Result<Manifest> {
    let value: String = tx
        .query_row(
            "SELECT manifest FROM stage_blobs WHERE hash=?1",
            [hash],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(StageError::Unavailable("Content is unavailable"))?;
    serde_json::from_str(&value).map_err(|_| StageError::Invalid("Invalid stored manifest"))
}
fn authorize(tx: &Transaction<'_>, peer: StageId, actor: &str, hash: &str, now: i64) -> Result<()> {
    let peer = ledger::peer(tx, peer, now)?;
    ledger::actor_exists(tx, actor)?;
    if !ledger::core_read(tx, actor)? && !ledger::grant(tx, &peer, actor)?.read {
        return Err(StageError::Forbidden("Content is outside Actor grants"));
    }
    let associated: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM stage_blob_actors WHERE actor=?1 AND hash=?2) OR EXISTS(SELECT 1 FROM stage_content_sources WHERE actor=?1 AND hash=?2)",
        params![actor, hash],
        |row| row.get(0),
    )?;
    if !associated {
        return Err(StageError::Forbidden(
            "Content is not associated with this Actor",
        ));
    }
    Ok(())
}
pub(crate) fn materialize(tx: &Transaction<'_>, actor: &str, reference: &BlobRef) -> Result<()> {
    let cached: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM stage_blobs WHERE hash=?1 AND length(bytes)=size)",
        [&reference.hash],
        |r| r.get(0),
    )?;
    if cached {
        touch(tx, &reference.hash)?;
        return Ok(());
    }
    let core: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='core_rows')",
        [],
        |r| r.get(0),
    )?;
    if actor == super::shared::ACTOR_ID && core {
        let record: Option<Vec<u8>> = tx
            .query_row(
                "SELECT record FROM core_rows WHERE content_hash=?1",
                [&reference.hash],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(record) = record {
            if digest(&record) != reference.hash || record.len() as u64 != reference.size {
                return Err(StageError::Conflict("Canonical content changed"));
            }
            put(tx, actor, &record)?;
            return Ok(());
        }
    }
    if actor == super::shared::ACTOR_ID {
        let snapshots: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='core_snapshot_rows')",
            [],
            |r| r.get(0),
        )?;
        if snapshots && let Some(record)=tx.query_row("SELECT record FROM core_snapshot_rows WHERE json_extract(change,'$.content.hash')=?1 LIMIT 1",[&reference.hash],|r|r.get::<_,Vec<u8>>(0)).optional()? {
            if digest(&record)!=reference.hash || record.len() as u64!=reference.size{return Err(StageError::Conflict("Snapshot content changed"));}put(tx,actor,&record)?;return Ok(());
        }
    }
    let resources: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='stage_resource_files')",
        [],
        |r| r.get(0),
    )?;
    if resources {
        let file:Option<(String,String)>=tx.query_row("SELECT f.root,f.path FROM stage_resources r JOIN stage_resource_files f ON f.actor=r.actor AND f.id=r.id WHERE r.actor=?1 AND json_extract(r.record,'$.content.hash')=?2",params![actor,reference.hash],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((root, path)) = file {
            let mut policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
            let root = std::path::PathBuf::from(root);
            policy.filesystem.full_disk_read = false;
            policy.filesystem.readable_roots = vec![root.clone()];
            policy.filesystem.bound_roots = vec![root];
            let bytes =
                crate::file_io::read_bytes(std::path::Path::new(&path), reference.size, &policy)
                    .map_err(|_| StageError::Forbidden("Resource storage binding changed"))?;
            if bytes.len() as u64 != reference.size || digest(&bytes) != reference.hash {
                return Err(StageError::Conflict("Resource bytes changed"));
            }
            put(tx, actor, &bytes)?;
            return Ok(());
        }
    }
    let source: Option<(String, String)> = tx
        .query_row(
            "SELECT message,field FROM stage_content_sources WHERE actor=?1 AND hash=?2",
            params![actor, reference.hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (message, field) = source.ok_or(StageError::Unavailable("Content is unavailable"))?;
    if !matches!(field.as_str(), "content" | "reply" | "error") {
        return Err(StageError::Invalid("Invalid content source"));
    }
    let text: Option<String> = tx.query_row(
        "SELECT json_extract(record,?3) FROM messages WHERE id=?1 AND actor_id=?2",
        params![message, actor, format!("$.{field}")],
        |r| r.get(0),
    )?;
    let text = text.ok_or(StageError::Unavailable("Source content is unavailable"))?;
    if text.len() as u64 != reference.size || digest(text.as_bytes()) != reference.hash {
        return Err(StageError::Conflict("Source content changed"));
    }
    put(tx, actor, text.as_bytes())?;
    Ok(())
}
pub struct ContentStore<'a> {
    store: &'a ActorStore,
}
impl<'a> ContentStore<'a> {
    pub fn new(store: &'a ActorStore) -> Self {
        Self { store }
    }
    pub fn store_source(&self, actor: &str, bytes: &[u8]) -> Result<Manifest> {
        self.store.stage_transaction(|tx| {
            ledger::actor_exists(tx, actor)?;
            put(tx, actor, bytes)
        })
    }
    pub fn manifest(
        &self,
        peer: StageId,
        actor: &str,
        reference: &BlobRef,
        now: i64,
    ) -> Result<Manifest> {
        reference.validate()?;
        self.store.stage_transaction(|tx| {
            authorize(tx, peer, actor, &reference.hash, now)?;
            materialize(tx, actor, reference)?;
            let value = manifest(tx, &reference.hash)?;
            if &value.content != reference {
                return Err(StageError::Conflict("Content version changed"));
            }
            Ok(value)
        })
    }
    pub fn chunk(
        &self,
        peer: StageId,
        actor: &str,
        reference: &BlobRef,
        index: u32,
        now: i64,
    ) -> Result<Vec<u8>> {
        reference.validate()?;
        self.store.stage_transaction(|tx| {
            authorize(tx, peer, actor, &reference.hash, now)?;
            materialize(tx, actor, reference)?;
            let value = manifest(tx, &reference.hash)?;
            if &value.content != reference || index as usize >= value.chunks.len() {
                return Err(StageError::Invalid("Content chunk is out of range"));
            }
            let bytes: Vec<u8> = tx.query_row(
                "SELECT substr(bytes,?2,?3) FROM stage_blobs WHERE hash=?1",
                params![
                    reference.hash,
                    index as i64 * CHUNK_BYTES as i64 + 1,
                    CHUNK_BYTES as i64
                ],
                |row| row.get(0),
            )?;
            if digest(&bytes) != value.chunks[index as usize] {
                return Err(StageError::Conflict("Stored content checksum failed"));
            }
            Ok(bytes)
        })
    }
    /// Returns only missing chunks. All consumers of a peer/hash share this ledger.
    /// Connection code must bind the manifest to an authorized request before calling.
    pub(crate) fn begin_download(
        &self,
        peer: StageId,
        value: &Manifest,
        now: i64,
    ) -> Result<Vec<u32>> {
        value.validate()?;
        if value.content.size == 0 && value.content.hash != digest(&[]) {
            return Err(StageError::Conflict("Empty content checksum failed"));
        }
        self.store.stage_transaction(|tx| {
            ledger::peer(tx, peer, now)?;
            tx.execute("DELETE FROM stage_blob_parts WHERE EXISTS(SELECT 1 FROM stage_download_activity a WHERE a.peer=stage_blob_parts.peer AND a.hash=stage_blob_parts.hash AND a.touched<?1)",[now-3600000])?;
            tx.execute("DELETE FROM stage_downloads WHERE EXISTS(SELECT 1 FROM stage_download_activity a WHERE a.peer=stage_downloads.peer AND a.hash=stage_downloads.hash AND a.touched<?1)",[now-3600000])?;
            tx.execute("DELETE FROM stage_download_activity WHERE touched<?1",[now-3600000])?;
            tx.execute("INSERT INTO stage_download_activity VALUES(?1,?2,?3) ON CONFLICT(peer,hash) DO UPDATE SET touched=excluded.touched",params![peer.to_string(),value.content.hash,now])?;
            let record = serde_json::to_string(value)
                .map_err(|_| StageError::Invalid("Cannot encode manifest"))?;
            let existing: Option<String> = tx
                .query_row(
                    "SELECT manifest FROM stage_downloads WHERE peer=?1 AND hash=?2",
                    params![peer.to_string(), value.content.hash],
                    |row| row.get(0),
                )
                .optional()?;
            if existing.as_ref().is_some_and(|old| old != &record) {
                return Err(StageError::Conflict("Download manifest changed"));
            }
            let cached: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM stage_blobs WHERE hash=?1 AND size=?2 AND length(bytes)=size)",
                params![value.content.hash, value.content.size as i64],
                |row| row.get(0),
            )?;
            if cached {return Ok(vec![]);}
            if value.content.size==0 {
                tx.execute("INSERT OR IGNORE INTO stage_blobs(hash,size,manifest,bytes) VALUES(?1,0,?2,?3)",params![value.content.hash,record,Vec::<u8>::new()])?;
                return Ok(vec![]);
            }
            tx.execute(
                "INSERT OR IGNORE INTO stage_downloads(peer,hash,manifest) VALUES(?1,?2,?3)",
                params![peer.to_string(), value.content.hash, record],
            )?;
            let existing = tx
                .prepare("SELECT chunk FROM stage_blob_parts WHERE peer=?1 AND hash=?2")?
                .query_map(params![peer.to_string(), value.content.hash], |row| {
                    row.get::<_, u32>(0)
                })?
                .collect::<std::result::Result<std::collections::HashSet<_>, _>>()?;
            Ok((0..value.chunks.len() as u32)
                .filter(|chunk| !existing.contains(chunk))
                .collect())
        })
    }
    pub(crate) fn receive_chunk(
        &self,
        peer: StageId,
        hash: &str,
        index: u32,
        bytes: &[u8],
        now: i64,
    ) -> Result<bool> {
        self.store.stage_transaction(|tx| {
            ledger::peer(tx,peer,now)?;
            tx.execute("UPDATE stage_download_activity SET touched=?3 WHERE peer=?1 AND hash=?2",params![peer.to_string(),hash,now])?;
            let value:Option<String>=tx.query_row("SELECT manifest FROM stage_downloads WHERE peer=?1 AND hash=?2",params![peer.to_string(),hash],|row|row.get(0)).optional()?;
            let Some(value)=value else {
                let known=manifest(tx,hash)?;
                if known.chunks.get(index as usize).is_none_or(|expected|*expected!=digest(bytes)){return Err(StageError::Conflict("Duplicate content chunk changed"));}
                return Ok(true);
            };
            let value:Manifest=serde_json::from_str(&value).map_err(|_|StageError::Invalid("Invalid stored download"))?;
            let expected_len=(value.content.size.saturating_sub(index as u64*CHUNK_BYTES as u64)).min(CHUNK_BYTES as u64);
            if index as usize>=value.chunks.len() || bytes.len() as u64!=expected_len || digest(bytes)!=value.chunks[index as usize] {return Err(StageError::Conflict("Content chunk checksum failed"));}
            let existing:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM stage_blob_parts WHERE peer=?1 AND hash=?2 AND chunk=?3)",params![peer.to_string(),hash,index],|row|row.get(0))?;
            if !existing {make_room(tx,bytes.len() as u64)?;}
            tx.execute("INSERT OR IGNORE INTO stage_blob_parts(peer,hash,chunk,bytes) VALUES(?1,?2,?3,?4)",params![peer.to_string(),hash,index,bytes])?;
            let count:i64=tx.query_row("SELECT COUNT(*) FROM stage_blob_parts WHERE peer=?1 AND hash=?2",params![peer.to_string(),hash],|row|row.get(0))?;
            if count as usize!=value.chunks.len() {return Ok(false);}
            let parts=tx.prepare("SELECT bytes FROM stage_blob_parts WHERE peer=?1 AND hash=?2 ORDER BY chunk")?.query_map(params![peer.to_string(),hash],|row|row.get::<_,Vec<u8>>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
            let bytes:Vec<u8>=parts.into_iter().flatten().collect();
            if bytes.len() as u64!=value.content.size || digest(&bytes)!=hash {return Err(StageError::Conflict("Complete content checksum failed"));}
            tx.execute("INSERT INTO stage_blobs(hash,size,manifest,bytes) VALUES(?1,?2,?3,?4) ON CONFLICT(hash) DO UPDATE SET bytes=excluded.bytes",params![hash,bytes.len() as i64,serde_json::to_string(&value).map_err(|_|StageError::Invalid("Cannot encode manifest"))?,bytes])?;
            touch(tx,hash)?;
            tx.execute("DELETE FROM stage_blob_parts WHERE peer=?1 AND hash=?2",params![peer.to_string(),hash])?;
            tx.execute("DELETE FROM stage_downloads WHERE peer=?1 AND hash=?2",params![peer.to_string(),hash])?;
            Ok(true)
        })
    }
    pub(crate) fn verified_bytes(&self, reference: &BlobRef) -> Result<Vec<u8>> {
        reference.validate()?;
        self.store.stage_transaction(|tx| {
            let value = manifest(tx, &reference.hash)?;
            if &value.content != reference {
                return Err(StageError::Conflict("Content reference changed"));
            }
            let bytes: Vec<u8> = tx.query_row(
                "SELECT bytes FROM stage_blobs WHERE hash=?1",
                [&reference.hash],
                |row| row.get(0),
            )?;
            if digest(&bytes) != reference.hash {
                return Err(StageError::Conflict("Content checksum failed"));
            }
            Ok(bytes)
        })
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    #[test]
    fn a_resource_larger_than_the_cache_target_is_stored_without_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let store = ActorStore::open(&dir.path().join("actors.sqlite")).unwrap();
        let content = ContentStore::new(&store);
        let bytes = vec![b'x'; 65 * 1024 * 1024];
        let reference = store
            .stage_transaction(|tx| {
                initialize(tx)?;
                Ok(put(tx, "root", &bytes)?.content)
            })
            .unwrap();
        assert_eq!(reference.size, 65 * 1024 * 1024);
        assert_eq!(content.verified_bytes(&reference).unwrap(), bytes);
    }

    #[test]
    fn cold_content_is_evicted_and_immutable_bytes_can_repopulate_within_budget() {
        let dir = tempfile::tempdir().unwrap();
        let store = ActorStore::open(&dir.path().join("actors.sqlite")).unwrap();
        let mut first = None;
        for value in 0..3u8 {
            let bytes = vec![value; 24 * 1024 * 1024];
            store
                .stage_transaction(|tx| {
                    initialize(tx)?;
                    let manifest = put(tx, "root", &bytes)?;
                    if value == 0 {
                        first = Some(manifest.content);
                    }
                    tx.execute("UPDATE stage_blob_access SET pinned_until=0", [])?;
                    assert!(storage_bytes(tx)? <= CACHE_BYTES);
                    Ok(())
                })
                .unwrap();
        }
        assert!(
            ContentStore::new(&store)
                .verified_bytes(&first.unwrap())
                .is_err()
        );
        store
            .stage_transaction(|tx| {
                put(tx, "root", &vec![0; 24 * 1024 * 1024])?;
                assert!(storage_bytes(tx)? <= CACHE_BYTES);
                Ok(())
            })
            .unwrap();
    }
}
