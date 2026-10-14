//! One upstream Actor subscription shared by local views, and durable replicas
//! with separate receipt and application watermarks.
use super::{
    ActorAddress, Result, StageError,
    journal::{Fact, Page, SnapshotPage},
    ledger,
    protocol::{BlobRef, Cursor, SubscriptionLevel},
};
use crate::actors::store::ActorStore;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
pub const SUBSCRIPTION_BYTES: usize = 256 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Watermarks {
    pub durable_received: Cursor,
    pub contiguous_received: Cursor,
    pub contiguous_applied: Cursor,
    pub caught_up: bool,
}
pub(crate) fn initialize(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_replicas(stage TEXT NOT NULL,actor TEXT NOT NULL,epoch TEXT NOT NULL,durable INTEGER NOT NULL,received INTEGER NOT NULL,applied INTEGER NOT NULL,head INTEGER NOT NULL,PRIMARY KEY(stage,actor));
        CREATE TABLE IF NOT EXISTS stage_replica_pages(stage TEXT NOT NULL,actor TEXT NOT NULL,start INTEGER NOT NULL,finish INTEGER NOT NULL,digest TEXT NOT NULL,bytes BLOB NOT NULL,PRIMARY KEY(stage,actor,start));
        CREATE TABLE IF NOT EXISTS stage_replica_messages(stage TEXT NOT NULL,actor TEXT NOT NULL,message TEXT NOT NULL,fact TEXT NOT NULL,PRIMARY KEY(stage,actor,message));
        CREATE TABLE IF NOT EXISTS stage_replica_text(stage TEXT NOT NULL,actor TEXT NOT NULL,message TEXT NOT NULL,generation INTEGER NOT NULL,channel TEXT NOT NULL,version INTEGER NOT NULL,text TEXT NOT NULL,PRIMARY KEY(stage,actor,message,generation,channel));")?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_replica_snapshot(stage TEXT NOT NULL,actor TEXT NOT NULL,token TEXT NOT NULL,base TEXT NOT NULL,next INTEGER NOT NULL,complete INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(stage,actor));
        CREATE TABLE IF NOT EXISTS stage_replica_snapshot_pages(stage TEXT NOT NULL,actor TEXT NOT NULL,token TEXT NOT NULL,page INTEGER NOT NULL,bytes BLOB NOT NULL,processed INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(stage,actor,token,page));")?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_client_events(seq INTEGER PRIMARY KEY AUTOINCREMENT,stage TEXT NOT NULL,actor TEXT NOT NULL,record TEXT NOT NULL); CREATE INDEX IF NOT EXISTS stage_client_events_actor ON stage_client_events(stage,actor,seq);")?;
    Ok(())
}
fn references(fact: &Fact) -> Vec<&BlobRef> {
    match fact {
        Fact::TextState { content, .. } => vec![content],
        Fact::Message {
            content,
            reply,
            error,
            ..
        } => std::iter::once(content)
            .chain(reply.iter())
            .chain(error.iter())
            .collect(),
        _ => vec![],
    }
}
pub struct Replica<'a> {
    store: &'a ActorStore,
}
impl<'a> Replica<'a> {
    pub fn new(store: &'a ActorStore) -> Self {
        Self { store }
    }
    pub fn client_events(
        &self,
        address: &ActorAddress,
        after: i64,
    ) -> Result<Vec<serde_json::Value>> {
        self.store.stage_transaction(|tx| {
            ledger::peer(tx,address.stage_id,chrono::Utc::now().timestamp_millis())?;
            let minimum:Option<i64>=tx.query_row("SELECT MIN(seq) FROM stage_client_events WHERE stage=?1 AND actor=?2",params![address.stage_id.to_string(),address.actor_id],|r|r.get(0))?;
            if after>0 && minimum.is_some_and(|min|after<min-1){let latest:i64=tx.query_row("SELECT MAX(seq) FROM stage_client_events WHERE stage=?1 AND actor=?2",params![address.stage_id.to_string(),address.actor_id],|r|r.get(0))?;return Ok(vec![serde_json::json!({"seq":latest,"resync":true})]);}
            let rows=tx.prepare("SELECT seq,record FROM stage_client_events WHERE stage=?1 AND actor=?2 AND seq>?3 ORDER BY seq LIMIT 32")?.query_map(params![address.stage_id.to_string(),address.actor_id,after],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
            rows.into_iter().map(|(seq,data)|{let mut value:serde_json::Value=serde_json::from_str(&data).map_err(|_|StageError::Invalid("Invalid client event"))?;value["seq"]=seq.into();Ok(value)}).collect()
        })
    }
    pub fn client_cursor(&self) -> Result<i64> {
        self.store.stage_transaction(|tx| {
            Ok(tx.query_row(
                "SELECT COALESCE(MAX(seq),0) FROM stage_client_events",
                [],
                |r| r.get(0),
            )?)
        })
    }
    pub fn message(
        &self,
        address: &ActorAddress,
        id: &str,
    ) -> Result<crate::actors::MessageRecord> {
        self.store.stage_transaction(|tx| {
            ledger::peer(tx,address.stage_id,chrono::Utc::now().timestamp_millis())?;
            let data:String=tx.query_row("SELECT fact FROM stage_replica_messages WHERE stage=?1 AND actor=?2 AND message=?3",params![address.stage_id.to_string(),address.actor_id,id],|r|r.get(0)).optional()?.ok_or(StageError::Unavailable("Message has not synchronized"))?;
            let fact:Fact=serde_json::from_str(&data).map_err(|_|StageError::Invalid("Invalid message replica"))?;
            let Fact::Message {mut metadata,content,reply,error,..}=fact else{return Err(StageError::Invalid("Expected message fact"));};
            let text=|reference:&BlobRef|->Result<String>{let bytes:Vec<u8>=tx.query_row("SELECT bytes FROM stage_blobs WHERE hash=?1 AND size=?2",params![reference.hash,reference.size as i64],|r|r.get(0))?;if super::digest(&bytes)!=reference.hash{return Err(StageError::Conflict("Content changed"));}String::from_utf8(bytes).map_err(|_|StageError::Invalid("Content is not text"))};
            metadata.content=text(&content)?; metadata.reply=reply.as_ref().map(text).transpose()?; metadata.error=error.as_ref().map(text).transpose()?;Ok(*metadata)
        })
    }
    pub fn progress(&self, address: &ActorAddress, now: i64) -> Result<Watermarks> {
        self.store.stage_transaction(|tx| {
            ledger::peer(tx, address.stage_id, now)?;
            watermarks(tx, address)
        })
    }
    pub fn receive(&self, address: &ActorAddress, page: &Page, now: i64) -> Result<Watermarks> {
        address.validate()?;
        page.from.validate()?;
        page.base.validate()?;
        page.head.validate()?;
        if page.head.epoch != page.base.epoch
            || page.base.sequence > page.head.sequence
            || page.caught_up != (page.base.sequence == page.head.sequence)
        {
            return Err(StageError::Invalid("Invalid authoritative Actor head"));
        }
        if page.from.epoch != page.base.epoch
            || page.from.sequence > page.base.sequence
            || page.base.sequence - page.from.sequence > 100
            || page.entries.len() > 100
        {
            return Err(StageError::Invalid("Invalid Actor event page"));
        }
        let mut previous = page.from.sequence;
        for entry in &page.entries {
            if entry.cursor.epoch != page.base.epoch
                || entry.cursor.sequence <= previous
                || entry.cursor.sequence > page.base.sequence
                || page.level == SubscriptionLevel::Detail && entry.cursor.sequence != previous + 1
            {
                return Err(StageError::Invalid("Invalid Actor event ordering"));
            }
            previous = entry.cursor.sequence;
            for reference in references(&entry.fact) {
                reference.validate()?;
            }
        }
        if page.level == SubscriptionLevel::Detail && previous != page.base.sequence {
            return Err(StageError::Invalid("Actor detail page has a gap"));
        }
        let bytes = serde_json::to_vec(page)
            .map_err(|_| StageError::Invalid("Cannot encode Actor event page"))?;
        if bytes.len() > 48 * 1024 {
            return Err(StageError::Capacity("Actor event page exceeds budget"));
        }
        self.store.stage_transaction(|tx| {
            ledger::peer(tx,address.stage_id,now)?;
            tx.execute("INSERT OR IGNORE INTO stage_replicas VALUES(?1,?2,?3,0,0,0,0)",params![address.stage_id.to_string(),address.actor_id,page.base.epoch.to_string()])?;
            let (epoch,applied):(String,i64)=tx.query_row("SELECT epoch,applied FROM stage_replicas WHERE stage=?1 AND actor=?2",params![address.stage_id.to_string(),address.actor_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if epoch!=page.base.epoch.to_string(){return Err(StageError::Conflict("Actor snapshot required"));}
            tx.execute("UPDATE stage_replicas SET head=MAX(head,?3) WHERE stage=?1 AND actor=?2",params![address.stage_id.to_string(),address.actor_id,page.head.sequence as i64])?;
            if page.base.sequence<=applied as u64 {return watermarks(tx,address);}
            let existing:Option<String>=tx.query_row("SELECT digest FROM stage_replica_pages WHERE stage=?1 AND actor=?2 AND start=?3",params![address.stage_id.to_string(),address.actor_id,page.from.sequence as i64],|r|r.get(0)).optional()?;
            let digest=super::digest(&bytes);
            if existing.as_ref().is_some_and(|old|old!=&digest) {return Err(StageError::Conflict("Actor page retry changed content"));}
            let pending:i64=tx.query_row("SELECT COALESCE(SUM(length(bytes)),0) FROM stage_replica_pages WHERE stage=?1 AND actor=?2",params![address.stage_id.to_string(),address.actor_id],|r|r.get(0))?;
            if existing.is_none() && pending as usize+bytes.len()>SUBSCRIPTION_BYTES {return Err(StageError::Capacity("Actor subscription backlog is full"));}
            tx.execute("INSERT OR IGNORE INTO stage_replica_pages VALUES(?1,?2,?3,?4,?5,?6)",params![address.stage_id.to_string(),address.actor_id,page.from.sequence as i64,page.base.sequence as i64,digest,bytes])?;
            tx.execute("UPDATE stage_replicas SET durable=MAX(durable,?3) WHERE stage=?1 AND actor=?2",params![address.stage_id.to_string(),address.actor_id,page.base.sequence as i64])?;
            apply(tx,address)?;watermarks(tx,address)
        })
    }
    pub fn apply_available(&self, address: &ActorAddress, now: i64) -> Result<Watermarks> {
        self.store.stage_transaction(|tx| {
            ledger::peer(tx, address.stage_id, now)?;
            apply_snapshot(tx, address)?;
            apply(tx, address)?;
            watermarks(tx, address)
        })
    }
    pub fn receive_snapshot(
        &self,
        address: &ActorAddress,
        page: &SnapshotPage,
        now: i64,
    ) -> Result<bool> {
        address.validate()?;
        page.base.validate()?;
        if page.token.is_nil() || page.page > 316 || page.entries.len() > 32 {
            return Err(StageError::Invalid("Invalid Actor snapshot page"));
        }
        for fact in &page.entries {
            for reference in references(fact) {
                reference.validate()?;
            }
        }
        let bytes = serde_json::to_vec(page)
            .map_err(|_| StageError::Invalid("Cannot encode Actor snapshot"))?;
        if bytes.len() > 48 * 1024 {
            return Err(StageError::Capacity("Actor snapshot page exceeds budget"));
        }
        self.store.stage_transaction(|tx| {
            ledger::peer(tx,address.stage_id,now)?;
            let existing:Option<(String,String,u32)>=tx.query_row("SELECT token,base,next FROM stage_replica_snapshot WHERE stage=?1 AND actor=?2",params![address.stage_id.to_string(),address.actor_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let base=serde_json::to_string(&page.base).map_err(|_|StageError::Invalid("Cannot encode snapshot cursor"))?;
            if existing.as_ref().is_none_or(|(token,_,_)|token!=&page.token.to_string()) {
                if page.page!=0 {return Err(StageError::Conflict("Actor snapshot must begin at page zero"));}
                tx.execute("DELETE FROM stage_replica_snapshot_pages WHERE stage=?1 AND actor=?2",params![address.stage_id.to_string(),address.actor_id])?;
                tx.execute("INSERT INTO stage_replica_snapshot(stage,actor,token,base,next,complete) VALUES(?1,?2,?3,?4,0,0) ON CONFLICT(stage,actor) DO UPDATE SET token=excluded.token,base=excluded.base,next=0,complete=0",params![address.stage_id.to_string(),address.actor_id,page.token.to_string(),base])?;
            }else if existing.as_ref().is_some_and(|(_,previous,_)|previous!=&base){return Err(StageError::Conflict("Actor snapshot boundary changed"));}
            let next:u32=tx.query_row("SELECT next FROM stage_replica_snapshot WHERE stage=?1 AND actor=?2",params![address.stage_id.to_string(),address.actor_id],|r|r.get(0))?;
            if page.page>next {return Err(StageError::Conflict("Actor snapshot page requires replay"));}
            let previous:Option<Vec<u8>>=tx.query_row("SELECT bytes FROM stage_replica_snapshot_pages WHERE stage=?1 AND actor=?2 AND token=?3 AND page=?4",params![address.stage_id.to_string(),address.actor_id,page.token.to_string(),page.page],|r|r.get(0)).optional()?;
            if previous.as_ref().is_some_and(|previous|previous!=&bytes) {return Err(StageError::Conflict("Snapshot page retry changed content"));}
            let (all,pending):(i64,i64)=tx.query_row("SELECT COALESCE(SUM(length(bytes)),0),COALESCE(SUM(CASE WHEN processed=0 THEN length(bytes) ELSE 0 END),0) FROM stage_replica_snapshot_pages WHERE stage=?1 AND actor=?2",params![address.stage_id.to_string(),address.actor_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if previous.is_none() && (all as usize+bytes.len()>4*1024*1024 || pending as usize+bytes.len()>SUBSCRIPTION_BYTES) {return Err(StageError::Capacity("Actor snapshot cache is full"));}
            tx.execute("INSERT OR IGNORE INTO stage_replica_snapshot_pages(stage,actor,token,page,bytes) VALUES(?1,?2,?3,?4,?5)",params![address.stage_id.to_string(),address.actor_id,page.token.to_string(),page.page,bytes])?;
            apply_snapshot(tx,address)?;
            let complete:bool=tx.query_row("SELECT complete FROM stage_replica_snapshot WHERE stage=?1 AND actor=?2",params![address.stage_id.to_string(),address.actor_id],|r|r.get(0))?;Ok(complete)
        })
    }
    pub fn missing_content(&self, address: &ActorAddress, now: i64) -> Result<Vec<BlobRef>> {
        self.store.stage_transaction(|tx| {
            ledger::peer(tx,address.stage_id,now)?;
            let pages=tx.prepare("SELECT bytes FROM stage_replica_pages WHERE stage=?1 AND actor=?2 ORDER BY start LIMIT 100")?.query_map(params![address.stage_id.to_string(),address.actor_id],|r|r.get::<_,Vec<u8>>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
            let mut refs=BTreeMap::new();
            for bytes in pages {let page:Page=serde_json::from_slice(&bytes).map_err(|_|StageError::Invalid("Invalid persisted Actor page"))?;for entry in page.entries {for reference in references(&entry.fact){let available:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM stage_blobs WHERE hash=?1 AND size=?2 AND length(bytes)=size)",params![reference.hash,reference.size as i64],|r|r.get(0))?;if !available {refs.insert(reference.hash.clone(),reference.clone());}}}}
            let snapshots=tx.prepare("SELECT bytes FROM stage_replica_snapshot_pages WHERE stage=?1 AND actor=?2 AND processed=0 LIMIT 8")?.query_map(params![address.stage_id.to_string(),address.actor_id],|r|r.get::<_,Vec<u8>>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
            for bytes in snapshots {let page:SnapshotPage=serde_json::from_slice(&bytes).map_err(|_|StageError::Invalid("Invalid persisted snapshot"))?;for fact in page.entries {for reference in references(&fact){let available:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM stage_blobs WHERE hash=?1 AND size=?2 AND length(bytes)=size)",params![reference.hash,reference.size as i64],|r|r.get(0))?;if !available {refs.insert(reference.hash.clone(),reference.clone());}}}}
            Ok(refs.into_values().take(32).collect())
        })
    }
}
fn client_event(
    tx: &rusqlite::Transaction<'_>,
    address: &ActorAddress,
    fact: &Fact,
    time: i64,
) -> Result<()> {
    use crate::actors::ActorEvent;
    let event = match fact {
        Fact::Activity { event } => event.clone(),
        Fact::TextAppend {
            message_id,
            generation,
            channel,
            text,
            ..
        } => ActorEvent::GenerationDelta {
            message_id: message_id.clone(),
            generation: *generation,
            channel: channel.clone(),
            text: text.clone(),
        },
        Fact::Message {
            message_id, status, ..
        } => ActorEvent::MessageUpdated {
            message_id: message_id.clone(),
            status: *status,
        },
        Fact::TextState { .. } => return Ok(()),
    };
    let record = serde_json::json!({"actor_id":format!("{}:{}",address.stage_id,address.actor_id),"created_at_ms":time,"event":event});
    let bytes = serde_json::to_string(&record)
        .map_err(|_| StageError::Invalid("Cannot encode client event"))?;
    tx.execute(
        "INSERT INTO stage_client_events(stage,actor,record) VALUES(?1,?2,?3)",
        params![address.stage_id.to_string(), address.actor_id, bytes],
    )?;
    // This client replay cache is disposable; the authoritative journal is retained separately.
    tx.execute("DELETE FROM stage_client_events WHERE stage=?1 AND actor=?2 AND seq < (SELECT COALESCE(MAX(seq),0)-256 FROM stage_client_events WHERE stage=?1 AND actor=?2)",params![address.stage_id.to_string(),address.actor_id])?;
    Ok(())
}
fn apply_snapshot(tx: &rusqlite::Transaction<'_>, address: &ActorAddress) -> Result<()> {
    let state:Option<(String,String,u32,bool)>=tx.query_row("SELECT token,base,next,complete FROM stage_replica_snapshot WHERE stage=?1 AND actor=?2",params![address.stage_id.to_string(),address.actor_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((token, base, mut next, false)) = state else {
        return Ok(());
    };
    let base: Cursor =
        serde_json::from_str(&base).map_err(|_| StageError::Invalid("Invalid snapshot cursor"))?;
    loop {
        let bytes:Option<Vec<u8>>=tx.query_row("SELECT bytes FROM stage_replica_snapshot_pages WHERE stage=?1 AND actor=?2 AND token=?3 AND page=?4",params![address.stage_id.to_string(),address.actor_id,token,next],|r|r.get(0)).optional()?;
        let Some(bytes) = bytes else {
            return Ok(());
        };
        let page: SnapshotPage = serde_json::from_slice(&bytes)
            .map_err(|_| StageError::Invalid("Invalid snapshot page"))?;
        for fact in &page.entries {
            for reference in references(fact) {
                let available: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM stage_blobs WHERE hash=?1 AND size=?2 AND length(bytes)=size)",
                    params![reference.hash, reference.size as i64],
                    |r| r.get(0),
                )?;
                if !available {
                    return Ok(());
                }
            }
        }
        tx.execute("UPDATE stage_replica_snapshot_pages SET processed=1 WHERE stage=?1 AND actor=?2 AND token=?3 AND page=?4",params![address.stage_id.to_string(),address.actor_id,token,next])?;
        next += 1;
        tx.execute(
            "UPDATE stage_replica_snapshot SET next=?3 WHERE stage=?1 AND actor=?2",
            params![address.stage_id.to_string(), address.actor_id, next],
        )?;
        if !page.last {
            continue;
        }
        tx.execute(
            "DELETE FROM stage_replica_messages WHERE stage=?1 AND actor=?2",
            params![address.stage_id.to_string(), address.actor_id],
        )?;
        tx.execute(
            "DELETE FROM stage_replica_text WHERE stage=?1 AND actor=?2",
            params![address.stage_id.to_string(), address.actor_id],
        )?;
        let mut query=tx.prepare("SELECT bytes FROM stage_replica_snapshot_pages WHERE stage=?1 AND actor=?2 AND token=?3 ORDER BY page")?;
        for bytes in query.query_map(
            params![address.stage_id.to_string(), address.actor_id, token],
            |r| r.get::<_, Vec<u8>>(0),
        )? {
            let page: SnapshotPage = serde_json::from_slice(&bytes?)
                .map_err(|_| StageError::Invalid("Invalid snapshot page"))?;
            for fact in page.entries {
                match &fact {
                    Fact::Message { message_id, .. } => {
                        tx.execute(
                            "INSERT INTO stage_replica_messages VALUES(?1,?2,?3,?4)",
                            params![
                                address.stage_id.to_string(),
                                address.actor_id,
                                message_id,
                                serde_json::to_string(&fact).map_err(|_| StageError::Invalid(
                                    "Cannot encode snapshot message"
                                ))?
                            ],
                        )?;
                    }
                    Fact::TextState {
                        message_id,
                        generation,
                        channel,
                        version,
                        content,
                    } => {
                        let bytes: Vec<u8> = tx.query_row(
                            "SELECT bytes FROM stage_blobs WHERE hash=?1 AND size=?2",
                            params![content.hash, content.size as i64],
                            |r| r.get(0),
                        )?;
                        if super::digest(&bytes) != content.hash || *version > i64::MAX as u64 {
                            return Err(StageError::Conflict("Snapshot text checksum failed"));
                        }
                        let text = String::from_utf8(bytes)
                            .map_err(|_| StageError::Invalid("Snapshot text must be UTF-8"))?;
                        tx.execute(
                            "INSERT INTO stage_replica_text VALUES(?1,?2,?3,?4,?5,?6,?7)",
                            params![
                                address.stage_id.to_string(),
                                address.actor_id,
                                message_id,
                                generation,
                                channel,
                                *version as i64,
                                text
                            ],
                        )?;
                    }
                    _ => {}
                }
            }
        }
        tx.execute("INSERT INTO stage_replicas VALUES(?1,?2,?3,?4,?4,?4,?4) ON CONFLICT(stage,actor) DO UPDATE SET epoch=excluded.epoch,durable=excluded.durable,received=excluded.received,applied=excluded.applied,head=excluded.head",params![address.stage_id.to_string(),address.actor_id,base.epoch.to_string(),base.sequence as i64])?;
        tx.execute("DELETE FROM stage_replica_pages WHERE stage=?1 AND actor=?2 AND (start<?3 OR json_extract(CAST(bytes AS TEXT),'$.base.epoch')<>?4)",params![address.stage_id.to_string(),address.actor_id,base.sequence as i64,base.epoch.to_string()])?;
        tx.execute(
            "INSERT INTO stage_client_events(stage,actor,record) VALUES(?1,?2,?3)",
            params![
                address.stage_id.to_string(),
                address.actor_id,
                "{\"resync\":true}"
            ],
        )?;
        tx.execute(
            "UPDATE stage_replica_snapshot SET complete=1 WHERE stage=?1 AND actor=?2",
            params![address.stage_id.to_string(), address.actor_id],
        )?;
        return Ok(());
    }
}
fn watermarks(tx: &rusqlite::Transaction<'_>, address: &ActorAddress) -> Result<Watermarks> {
    let state:Option<(String,i64,i64,i64,i64)>=tx.query_row("SELECT epoch,durable,received,applied,head FROM stage_replicas WHERE stage=?1 AND actor=?2",params![address.stage_id.to_string(),address.actor_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let Some((epoch, durable, received, applied, head)) = state else {
        let base: String = tx
            .query_row(
                "SELECT base FROM stage_replica_snapshot WHERE stage=?1 AND actor=?2",
                params![address.stage_id.to_string(), address.actor_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(StageError::Unavailable("Actor has not synchronized"))?;
        let base: Cursor = serde_json::from_str(&base)
            .map_err(|_| StageError::Invalid("Invalid snapshot cursor"))?;
        let zero = Cursor {
            epoch: base.epoch,
            sequence: 0,
        };
        return Ok(Watermarks {
            durable_received: zero.clone(),
            contiguous_received: zero.clone(),
            contiguous_applied: zero,
            caught_up: false,
        });
    };
    let epoch =
        Uuid::parse_str(&epoch).map_err(|_| StageError::Invalid("Invalid replica epoch"))?;
    let snapshot_pending: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM stage_replica_snapshot WHERE stage=?1 AND actor=?2 AND complete=0)", params![address.stage_id.to_string(), address.actor_id], |r|r.get(0))?;
    Ok(Watermarks {
        durable_received: Cursor {
            epoch,
            sequence: durable as u64,
        },
        contiguous_received: Cursor {
            epoch,
            sequence: received as u64,
        },
        contiguous_applied: Cursor {
            epoch,
            sequence: applied as u64,
        },
        caught_up: applied == head && !snapshot_pending,
    })
}
fn apply(tx: &rusqlite::Transaction<'_>, address: &ActorAddress) -> Result<()> {
    let state = watermarks(tx, address)?;
    let mut received = state.contiguous_received.sequence;
    loop {
        let next: Option<i64> = tx
            .query_row(
                "SELECT finish FROM stage_replica_pages WHERE stage=?1 AND actor=?2 AND start=?3",
                params![
                    address.stage_id.to_string(),
                    address.actor_id,
                    received as i64
                ],
                |r| r.get(0),
            )
            .optional()?;
        match next {
            Some(next) if next as u64 > received => received = next as u64,
            _ => break,
        }
    }
    tx.execute(
        "UPDATE stage_replicas SET received=?3 WHERE stage=?1 AND actor=?2",
        params![
            address.stage_id.to_string(),
            address.actor_id,
            received as i64
        ],
    )?;
    let mut applied = state.contiguous_applied.sequence;
    loop {
        let bytes: Option<Vec<u8>> = tx
            .query_row(
                "SELECT bytes FROM stage_replica_pages WHERE stage=?1 AND actor=?2 AND start=?3",
                params![
                    address.stage_id.to_string(),
                    address.actor_id,
                    applied as i64
                ],
                |r| r.get(0),
            )
            .optional()?;
        let Some(bytes) = bytes else {
            break;
        };
        let page: Page = serde_json::from_slice(&bytes)
            .map_err(|_| StageError::Invalid("Invalid persisted Actor page"))?;
        if page.base.sequence == applied {
            break;
        }
        let mut ready = true;
        for entry in &page.entries {
            for reference in references(&entry.fact) {
                let available: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM stage_blobs WHERE hash=?1 AND size=?2 AND length(bytes)=size)",
                    params![reference.hash, reference.size as i64],
                    |r| r.get(0),
                )?;
                ready &= available;
            }
        }
        if !ready {
            break;
        }
        for entry in &page.entries {
            client_event(tx, address, &entry.fact, entry.created_at_ms)?;
            match &entry.fact {
                Fact::Message { message_id, .. } => {
                    let fact = serde_json::to_string(&entry.fact)
                        .map_err(|_| StageError::Invalid("Cannot encode replica message"))?;
                    tx.execute("INSERT INTO stage_replica_messages VALUES(?1,?2,?3,?4) ON CONFLICT(stage,actor,message) DO UPDATE SET fact=excluded.fact",params![address.stage_id.to_string(),address.actor_id,message_id,fact])?;
                }
                Fact::TextAppend {
                    message_id,
                    generation,
                    channel,
                    offset,
                    version,
                    text,
                } => {
                    let prior:Option<(i64,String)>=tx.query_row("SELECT version,text FROM stage_replica_text WHERE stage=?1 AND actor=?2 AND message=?3 AND generation=?4 AND channel=?5",params![address.stage_id.to_string(),address.actor_id,message_id,generation,channel],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                    let (previous, current) = prior.unwrap_or((0, String::new()));
                    if *offset != current.len() as u64
                        || *version != previous as u64 + 1
                        || current.len().saturating_add(text.len()) > 32 * 1024 * 1024
                    {
                        return Err(StageError::Conflict("Actor text append requires replay"));
                    }
                    tx.execute("INSERT INTO stage_replica_text VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(stage,actor,message,generation,channel) DO UPDATE SET version=excluded.version,text=excluded.text",params![address.stage_id.to_string(),address.actor_id,message_id,generation,channel,*version as i64,current+text])?;
                }
                _ => {}
            }
        }
        applied = page.base.sequence;
        tx.execute(
            "DELETE FROM stage_replica_pages WHERE stage=?1 AND actor=?2 AND start=?3",
            params![
                address.stage_id.to_string(),
                address.actor_id,
                page.from.sequence as i64
            ],
        )?;
    }
    tx.execute(
        "UPDATE stage_replicas SET applied=?3 WHERE stage=?1 AND actor=?2",
        params![
            address.stage_id.to_string(),
            address.actor_id,
            applied as i64
        ],
    )?;
    Ok(())
}
#[derive(Default)]
pub struct Subscriptions {
    views: BTreeMap<ActorAddress, BTreeMap<Uuid, SubscriptionLevel>>,
}
impl Subscriptions {
    pub fn attach(
        &mut self,
        actor: ActorAddress,
        view: Uuid,
        level: SubscriptionLevel,
    ) -> Result<bool> {
        actor.validate()?;
        if !self.views.contains_key(&actor) && self.views.len() >= 8 {
            return Err(StageError::Capacity("Upstream subscription limit reached"));
        }
        let views = self.views.entry(actor).or_default();
        if !views.contains_key(&view) && views.len() >= 32 {
            return Err(StageError::Capacity("Actor consumer limit reached"));
        }
        let before = views.is_empty();
        views.insert(view, level);
        Ok(before)
    }
    pub fn detach(&mut self, actor: &ActorAddress, view: Uuid) -> bool {
        if let Some(views) = self.views.get_mut(actor) {
            views.remove(&view);
            if views.is_empty() {
                self.views.remove(actor);
                return true;
            }
        }
        false
    }
    pub fn level(&self, actor: &ActorAddress) -> Option<SubscriptionLevel> {
        self.views.get(actor).map(|views| {
            if views
                .values()
                .any(|level| *level == SubscriptionLevel::Detail)
            {
                SubscriptionLevel::Detail
            } else {
                SubscriptionLevel::Summary
            }
        })
    }
    pub fn actors(&self) -> BTreeSet<&ActorAddress> {
        self.views.keys().collect()
    }
}
