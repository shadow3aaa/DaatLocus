//! Dense authoritative Actor cursors over the existing event log, without a
//! second copy of event text. Global local-client cursors remain independent.
use super::{
    Result, StageError, StageId, ledger,
    protocol::{BlobRef, Cursor, SubscriptionLevel},
};
use crate::actors::{
    ActorEvent, MessageRecord, MessageSource, MessageStatus, ReplyTarget, store::ActorStore,
};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fact {
    Activity {
        event: ActorEvent,
    },
    TextAppend {
        message_id: String,
        generation: u32,
        channel: String,
        offset: u64,
        version: u64,
        text: String,
    },
    TextState {
        message_id: String,
        generation: u32,
        channel: String,
        version: u64,
        content: BlobRef,
    },
    Message {
        metadata: Box<MessageRecord>,
        message_id: String,
        source: MessageSource,
        reply_to: ReplyTarget,
        status: MessageStatus,
        content: BlobRef,
        reply: Option<BlobRef>,
        error: Option<BlobRef>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub cursor: Cursor,
    pub created_at_ms: i64,
    pub fact: Fact,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub from: Cursor,
    pub level: SubscriptionLevel,
    pub base: Cursor,
    pub head: Cursor,
    pub retained_from: u64,
    pub entries: Vec<Entry>,
    pub caught_up: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapshotPage {
    pub token: Uuid,
    pub base: Cursor,
    pub page: u32,
    pub entries: Vec<Fact>,
    pub last: bool,
}

pub(crate) fn initialize(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS actor_journal_meta(key TEXT PRIMARY KEY,value INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS actor_event_heads(actor TEXT PRIMARY KEY,epoch TEXT NOT NULL,ordinal INTEGER NOT NULL,retained INTEGER NOT NULL DEFAULT 0,deleted INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS actor_event_index(event INTEGER PRIMARY KEY REFERENCES events(seq) ON DELETE CASCADE,actor TEXT NOT NULL,ordinal INTEGER NOT NULL,offset INTEGER,version INTEGER,projection TEXT,UNIQUE(actor,ordinal));
        CREATE TABLE IF NOT EXISTS actor_text_heads(actor TEXT NOT NULL,message TEXT NOT NULL,generation INTEGER NOT NULL,channel TEXT NOT NULL,offset INTEGER NOT NULL,version INTEGER NOT NULL,PRIMARY KEY(actor,message,generation,channel));
        CREATE TABLE IF NOT EXISTS stage_snapshot_pages(peer TEXT NOT NULL,actor TEXT NOT NULL,token TEXT NOT NULL,page INTEGER NOT NULL,base TEXT NOT NULL,entries TEXT NOT NULL,last INTEGER NOT NULL,expires INTEGER NOT NULL,PRIMARY KEY(token,page));")?;
    let columns = tx
        .prepare("PRAGMA table_info(actor_event_index)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !columns.iter().any(|column| column == "projection") {
        tx.execute(
            "ALTER TABLE actor_event_index ADD COLUMN projection TEXT",
            [],
        )?;
    }
    // Interrupted migrations roll back with the original store-opening transaction.
    let mut after = tx
        .query_row(
            "SELECT value FROM actor_journal_meta WHERE key='indexed'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0);
    loop {
        let rows=tx.prepare("SELECT seq,actor_id,record FROM events WHERE seq>?1 AND seq NOT IN(SELECT event FROM actor_event_index) ORDER BY seq LIMIT 100")?
            .query_map([after],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?
            .collect::<std::result::Result<Vec<_>,_>>()?;
        if rows.is_empty() {
            break;
        }
        for (id, actor, data) in rows {
            after = id;
            let event: ActorEvent = serde_json::from_str(&data)
                .map_err(|_| StageError::Invalid("Invalid Actor event"))?;
            record(tx, id, &actor, &event)?;
        }
    }
    let through: i64 = tx.query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |row| {
        row.get(0)
    })?;
    tx.execute("INSERT INTO actor_journal_meta VALUES('indexed',?1) ON CONFLICT(key) DO UPDATE SET value=MAX(value,excluded.value)",[through])?;
    Ok(())
}
pub(crate) fn record(tx: &Transaction<'_>, id: i64, actor: &str, event: &ActorEvent) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO actor_event_heads(actor,epoch,ordinal) VALUES(?1,?2,0)",
        params![actor, Uuid::new_v4().to_string()],
    )?;
    let ordinal: i64 = tx.query_row(
        "SELECT ordinal FROM actor_event_heads WHERE actor=?1",
        [actor],
        |r| r.get(0),
    )?;
    let next = ordinal
        .checked_add(1)
        .ok_or(StageError::Capacity("Actor event sequence exhausted"))?;
    let (offset, version) = if let ActorEvent::GenerationDelta {
        message_id,
        generation,
        channel,
        text,
    } = event
    {
        let prior:Option<(i64,i64)>=tx.query_row("SELECT offset,version FROM actor_text_heads WHERE actor=?1 AND message=?2 AND generation=?3 AND channel=?4",params![actor,message_id,generation,channel],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let (offset, version) = prior.unwrap_or((0, 0));
        let end = offset
            .checked_add(text.len() as i64)
            .ok_or(StageError::Capacity("Actor text offset exhausted"))?;
        let version = version
            .checked_add(1)
            .ok_or(StageError::Capacity("Actor text version exhausted"))?;
        tx.execute("INSERT INTO actor_text_heads VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(actor,message,generation,channel) DO UPDATE SET offset=excluded.offset,version=excluded.version",params![actor,message_id,generation,channel,end,version])?;
        (Some(offset), Some(version))
    } else {
        (None, None)
    };
    let projection = if let ActorEvent::MessageAdded { message_id }
    | ActorEvent::MessageUpdated { message_id, .. } = event
    {
        match message_fact(tx, message_id) {
            Ok(fact) => Some(
                serde_json::to_string(&fact)
                    .map_err(|_| StageError::Invalid("Cannot encode message reference"))?,
            ),
            Err(StageError::Database(rusqlite::Error::QueryReturnedNoRows)) => None,
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    tx.execute(
        "INSERT INTO actor_event_index(event,actor,ordinal,offset,version,projection) VALUES(?1,?2,?3,?4,?5,?6)",
        params![id, actor, next, offset, version,projection],
    )?;
    tx.execute("UPDATE actor_event_heads SET ordinal=?2,deleted=CASE WHEN ?3=1 THEN 1 ELSE deleted END WHERE actor=?1",params![actor,next,matches!(event,ActorEvent::ActorDeleted{..}) as i32])?;
    tx.execute("INSERT INTO actor_journal_meta VALUES('indexed',?1) ON CONFLICT(key) DO UPDATE SET value=MAX(value,excluded.value)",[id])?;
    Ok(())
}
fn head(tx: &Transaction<'_>, actor: &str) -> Result<(Cursor, u64)> {
    let (epoch, sequence, retained): (String, i64, i64) = tx.query_row(
        "SELECT epoch,ordinal,retained FROM actor_event_heads WHERE actor=?1",
        [actor],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok((
        Cursor {
            epoch: Uuid::parse_str(&epoch)
                .map_err(|_| StageError::Invalid("Invalid Actor epoch"))?,
            sequence: sequence as u64,
        },
        retained as u64,
    ))
}
fn authorize(tx: &Transaction<'_>, id: StageId, actor: &str, now: i64) -> Result<()> {
    let peer = ledger::peer(tx, id, now)?;
    if !ledger::core_read(tx, actor)? && !ledger::grant(tx, &peer, actor)?.read {
        return Err(StageError::Forbidden("Actor is outside device grants"));
    }
    // A retained deletion tombstone is readable with the previous exact grant.
    let known: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM actor_event_heads WHERE actor=?1)",
        [actor],
        |r| r.get(0),
    )?;
    if !known {
        return Err(StageError::Forbidden("Actor is unavailable"));
    }
    Ok(())
}
fn reference(text: &str) -> BlobRef {
    BlobRef {
        hash: super::digest(text.as_bytes()),
        size: text.len() as u64,
        version: 1,
    }
}
fn message_fact(tx: &Transaction<'_>, id: &str) -> Result<Fact> {
    let data: String = tx.query_row("SELECT record FROM messages WHERE id=?1", [id], |r| {
        r.get(0)
    })?;
    let message: MessageRecord =
        serde_json::from_str(&data).map_err(|_| StageError::Invalid("Invalid Actor message"))?;
    for (field, text) in [
        ("content", Some(message.content.as_str())),
        ("reply", message.reply.as_deref()),
        ("error", message.error.as_deref()),
    ] {
        if let Some(text) = text {
            tx.execute("INSERT OR IGNORE INTO stage_content_sources(actor,hash,message,field) VALUES(?1,?2,?3,?4)",params![message.actor_id,reference(text).hash,message.id,field])?;
        }
    }
    let mut metadata = message.clone();
    metadata.content.clear();
    metadata.reply = None;
    metadata.error = None;
    metadata.delegation_grant = None;
    metadata.remote_authority = None;
    Ok(Fact::Message {
        metadata: Box::new(metadata),
        message_id: message.id,
        source: message.source,
        reply_to: message.reply_to,
        status: message.status,
        content: reference(&message.content),
        reply: message.reply.as_deref().map(reference),
        error: message.error.as_deref().map(reference),
    })
}
fn project(
    tx: &Transaction<'_>,
    event: ActorEvent,
    offset: Option<i64>,
    version: Option<i64>,
) -> Result<Fact> {
    Ok(match event {
        ActorEvent::GenerationDelta {
            message_id,
            generation,
            channel,
            text,
        } => Fact::TextAppend {
            message_id,
            generation,
            channel,
            offset: offset.ok_or(StageError::Invalid("Missing text offset"))? as u64,
            version: version.ok_or(StageError::Invalid("Missing text version"))? as u64,
            text,
        },
        ActorEvent::MessageAdded { ref message_id }
        | ActorEvent::MessageUpdated { ref message_id, .. } => match message_fact(tx, message_id) {
            Ok(fact) => fact,
            Err(StageError::Database(rusqlite::Error::QueryReturnedNoRows)) => {
                Fact::Activity { event }
            }
            Err(error) => return Err(error),
        },
        _ => Fact::Activity { event },
    })
}
pub struct Journal<'a> {
    store: &'a ActorStore,
}
impl<'a> Journal<'a> {
    pub fn new(store: &'a ActorStore) -> Self {
        Self { store }
    }
    pub(crate) fn current_cursor(&self, actor: &str) -> Result<Cursor> {
        self.store.stage_transaction(|tx| Ok(head(tx, actor)?.0))
    }
    pub fn page(
        &self,
        peer: StageId,
        actor: &str,
        after: Option<&Cursor>,
        level: SubscriptionLevel,
        now: i64,
        limit: usize,
    ) -> Result<Page> {
        self.store.stage_transaction(|tx| {
            authorize(tx,peer,actor,now)?;let (base,retained)=head(tx,actor)?;
            let sequence=after.map_or(0,|cursor|cursor.sequence);
            if after.is_some_and(|cursor|cursor.epoch!=base.epoch || cursor.sequence<retained || cursor.sequence>base.sequence) || after.is_none() && retained>0 {return Err(StageError::Conflict("Actor snapshot required"));}
            // Both levels advance across the same dense sequence. Summary emits
            // reliable lifecycle facts and compact skip ranges via the page base.
            let rows=tx.prepare("SELECT i.ordinal,e.created_at,e.record,i.offset,i.version,i.projection FROM actor_event_index i JOIN events e ON e.seq=i.event WHERE i.actor=?1 AND i.ordinal>?2 ORDER BY i.ordinal LIMIT ?3")?.query_map(params![actor,sequence as i64,limit.clamp(1,100) as i64],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<i64>>(3)?,r.get::<_,Option<i64>>(4)?,r.get::<_,Option<String>>(5)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
            let mut entries=vec![];let mut through=sequence;let mut encoded_bytes=512usize;
            for (ordinal,created,data,offset,version,projection) in rows {
                let event:ActorEvent=serde_json::from_str(&data).map_err(|_|StageError::Invalid("Invalid Actor event"))?;
                if level==SubscriptionLevel::Summary && matches!(event,ActorEvent::GenerationStarted{..}|ActorEvent::GenerationDelta{..}|ActorEvent::ToolStarted{..}|ActorEvent::ToolFinished{..}) {through=ordinal as u64;continue;}
                let fact=if let Some(projection)=projection {serde_json::from_str(&projection).map_err(|_|StageError::Invalid("Invalid stored message reference"))?}else{project(tx,event,offset,version)?};
                let entry=Entry {cursor:Cursor{epoch:base.epoch,sequence:ordinal as u64},created_at_ms:created,fact};
                let size=serde_json::to_vec(&entry).map_err(|_|StageError::Invalid("Cannot encode Actor event"))?.len();
                if encoded_bytes.saturating_add(size)>48*1024 {if entries.is_empty(){return Err(StageError::Capacity("Actor fact exceeds event budget"));}break;}
                encoded_bytes+=size;through=ordinal as u64;entries.push(entry);
            }
            Ok(Page {from:Cursor{epoch:base.epoch,sequence},level,base:Cursor{epoch:base.epoch,sequence:through},head:base.clone(),retained_from:retained,entries,caught_up:through==base.sequence})
        })
    }
    /// Snapshot metadata and its event boundary are frozen in one read/write
    /// transaction. Pages contain references, never whole conversation bodies.
    pub fn create_snapshot(&self, peer: StageId, actor: &str, now: i64) -> Result<SnapshotPage> {
        self.store.stage_transaction(|tx| {
            authorize(tx, peer, actor, now)?;
            tx.execute("DELETE FROM stage_snapshot_pages WHERE expires<=?1", [now])?;
            let (base, _) = head(tx, actor)?;
            let token = Uuid::new_v4();
            let ids = tx
                .prepare("SELECT id FROM messages WHERE actor_id=?1 ORDER BY seq")?
                .query_map([actor], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut facts=ids.iter().map(|id|message_fact(tx,id)).collect::<Result<Vec<_>>>()?;
            let streams=tx.prepare("SELECT message,generation,channel,offset,version FROM actor_text_heads WHERE actor=?1 AND offset>0 AND message IN(SELECT id FROM messages WHERE actor_id=?1 AND status='\"running\"')")?.query_map([actor],|r|Ok((r.get::<_,String>(0)?,r.get::<_,u32>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
            for (message,generation,channel,offset,version) in streams {
                let mut text=String::new();
                let mut query=tx.prepare("SELECT json_extract(e.record,'$.text') FROM actor_event_index i JOIN events e ON e.seq=i.event WHERE i.actor=?1 AND json_extract(e.record,'$.type')='generation_delta' AND json_extract(e.record,'$.message_id')=?2 AND json_extract(e.record,'$.generation')=?3 AND json_extract(e.record,'$.channel')=?4 ORDER BY i.ordinal")?;
                for chunk in query.query_map(params![actor,message,generation,channel],|r|r.get::<_,String>(0))? {let chunk=chunk?;text.push_str(&chunk);}
                if text.len() as i64!=offset {return Err(StageError::Conflict("Active text requires retained events"));}
                let content=super::blob::put(tx,actor,text.as_bytes())?.content;
                facts.push(Fact::TextState{message_id:message,generation,channel,version:version as u64,content});
            }
            let pages = facts.len().div_ceil(32).max(1);
            for page in 0..pages {
                let data = serde_json::to_string(&facts.iter().skip(page*32).take(32).collect::<Vec<_>>())
                    .map_err(|_| StageError::Invalid("Cannot encode snapshot"))?;
                if data.len() > 48 * 1024 {
                    return Err(StageError::Capacity("Snapshot metadata budget reached"));
                }
                tx.execute(
                    "INSERT INTO stage_snapshot_pages VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        peer.to_string(),
                        actor,
                        token.to_string(),
                        page as i64,
                        serde_json::to_string(&base)
                            .map_err(|_| StageError::Invalid("Cannot encode cursor"))?,
                        data,
                        (page + 1 == pages) as i32,
                        now.saturating_add(5 * 60 * 1000)
                    ],
                )?;
            }
            snapshot_page(tx, peer, actor, token, 0, now)
        })
    }
    pub fn snapshot_page(
        &self,
        peer: StageId,
        actor: &str,
        token: Uuid,
        page: u32,
        now: i64,
    ) -> Result<SnapshotPage> {
        self.store
            .stage_transaction(|tx| snapshot_page(tx, peer, actor, token, page, now))
    }
    /// Explicit log compaction cannot delete durable Actor history or tombstones.
    pub fn compact(&self, actor: &str, through: &Cursor) -> Result<()> {
        self.store.stage_transaction(|tx| {
            let (head,retained)=head(tx,actor)?;
            if head.epoch!=through.epoch || through.sequence>head.sequence || through.sequence<retained {return Err(StageError::Conflict("Invalid compaction boundary"));}
            // Local HTTP cursors still read the global log. Stage compaction
            // retains that log, active text and deletion tombstones.
            tx.execute("DELETE FROM actor_event_index WHERE actor=?1 AND ordinal<=?2 AND event IN(SELECT seq FROM events WHERE json_extract(record,'$.type')<>'actor_deleted' AND NOT(json_extract(record,'$.type')='generation_delta' AND json_extract(record,'$.message_id') IN(SELECT id FROM messages WHERE actor_id=?1 AND status='\"running\"')))",params![actor,through.sequence as i64])?;
            tx.execute("UPDATE actor_event_heads SET retained=?2 WHERE actor=?1",params![actor,through.sequence as i64])?;Ok(())
        })
    }
}
fn snapshot_page(
    tx: &Transaction<'_>,
    peer: StageId,
    actor: &str,
    token: Uuid,
    page: u32,
    now: i64,
) -> Result<SnapshotPage> {
    authorize(tx, peer, actor, now)?;
    let (base,entries,last):(String,String,bool)=tx.query_row("SELECT base,entries,last FROM stage_snapshot_pages WHERE peer=?1 AND actor=?2 AND token=?3 AND page=?4 AND expires>?5",params![peer.to_string(),actor,token.to_string(),page,now],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or(StageError::Forbidden("Snapshot is unavailable for this device"))?;
    Ok(SnapshotPage {
        token,
        page,
        base: serde_json::from_str(&base)
            .map_err(|_| StageError::Invalid("Invalid snapshot cursor"))?,
        entries: serde_json::from_str(&entries)
            .map_err(|_| StageError::Invalid("Invalid snapshot metadata"))?,
        last,
    })
}
