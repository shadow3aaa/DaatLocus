//! Frozen, bounded checkpoint replacement after replay compaction.
use super::{
    Result, StageError, blob,
    protocol::BlobRef,
    shared::{ACTOR_ID, Head},
    shared_store::{Change, Entity},
};
use crate::actors::store::ActorStore;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
const PAGE: usize = 24;
pub(crate) const RANGE_PAGES: usize = 16;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub token: String,
    pub account: String,
    pub head: Head,
    pub page: u32,
    pub last: bool,
    pub entries: Vec<Change>,
}
fn initialize(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS core_snapshots(token TEXT PRIMARY KEY,head TEXT NOT NULL,created INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS core_snapshot_rows(token TEXT NOT NULL,pos INTEGER NOT NULL,change TEXT NOT NULL,record BLOB NOT NULL,PRIMARY KEY(token,pos));
      CREATE TABLE IF NOT EXISTS core_snapshot_incoming(token TEXT NOT NULL,page INTEGER NOT NULL,head TEXT NOT NULL,record TEXT NOT NULL,PRIMARY KEY(token,page));
      CREATE TABLE IF NOT EXISTS core_snapshot_age(token TEXT PRIMARY KEY,touched INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS core_snapshot_content(token TEXT NOT NULL,entity TEXT NOT NULL,key TEXT NOT NULL,record BLOB NOT NULL,PRIMARY KEY(token,entity,key));")?;
    Ok(())
}
fn entity(value: &str) -> Result<Entity> {
    match value {
        "configuration" => Ok(Entity::Configuration),
        "resources" => Ok(Entity::Resource),
        "messages" => Ok(Entity::Message),
        "history" => Ok(Entity::History),
        "tools" => Ok(Entity::Tool),
        "approvals" => Ok(Entity::Approval),
        "events" => Ok(Entity::Event),
        _ => Err(StageError::Invalid("Invalid snapshot entity")),
    }
}
pub fn produce(store: &ActorStore, head: &Head, token: Option<&str>, page: u32) -> Result<Page> {
    produce_at(
        store,
        head,
        token,
        page,
        chrono::Utc::now().timestamp_millis(),
    )
}
pub(crate) fn produce_range(
    store: &ActorStore,
    head: &Head,
    token: Option<&str>,
    page: u32,
) -> Result<Vec<Page>> {
    let mut token = token.map(str::to_owned);
    let mut pages = vec![];
    let mut size = 0u64;
    for offset in 0..RANGE_PAGES as u32 {
        let part = produce(
            store,
            head,
            token.as_deref(),
            page.checked_add(offset)
                .ok_or(StageError::Invalid("Snapshot page overflow"))?,
        )?;
        size = part
            .entries
            .iter()
            .fold(size, |sum, change| sum.saturating_add(change.content.size));
        let last = part.last;
        token = Some(part.token.clone());
        pages.push(part);
        if last || size >= 2 * 1024 * 1024 {
            break;
        }
    }
    Ok(pages)
}
pub(super) fn produce_at(
    store: &ActorStore,
    head: &Head,
    token: Option<&str>,
    page: u32,
    now: i64,
) -> Result<Page> {
    store.stage_transaction(|tx|{
        initialize(tx)?;
        tx.execute("DELETE FROM core_snapshot_rows WHERE token IN (SELECT token FROM core_snapshots WHERE created<?1)",[now-300000])?;
        tx.execute("DELETE FROM core_snapshots WHERE created<?1",[now-300000])?;
        let token=if let Some(token)=token {uuid::Uuid::parse_str(token).map_err(|_|StageError::Invalid("Invalid snapshot token"))?;token.to_owned()} else {
            if page!=0{return Err(StageError::Invalid("Snapshot begins at page zero"));}
            let current:Head=serde_json::from_str(&tx.query_row::<String,_,_>("SELECT value FROM core_meta WHERE key='head'",[],|r|r.get(0))?).map_err(|_|StageError::Invalid("Invalid core head"))?;
            if current!=*head{return Err(StageError::Conflict("Snapshot head changed"));}
            let token=uuid::Uuid::new_v4().to_string();
            tx.execute("INSERT INTO core_snapshots VALUES(?1,?2,?3)",params![token,serde_json::to_string(head).map_err(|_|StageError::Invalid("Invalid snapshot head"))?,now])?;
            // Freeze on disk while holding the transaction, without collecting
            // an account's entire history into a second in-memory copy.
            let mut statement=tx.prepare("SELECT r.entity,r.remote_key,o.ordinal,r.record FROM core_rows r JOIN core_order o ON o.entity=r.entity AND o.remote_key=r.remote_key ORDER BY CASE r.entity WHEN 'resources' THEN 0 WHEN 'configuration' THEN 1 WHEN 'messages' THEN 2 WHEN 'history' THEN 3 WHEN 'tools' THEN 4 WHEN 'approvals' THEN 5 ELSE 6 END,o.ordinal")?;
            let rows=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,Vec<u8>>(3)?)))?;
            for (pos,row) in rows.enumerate(){let (kind,key,ordinal,record)=row?;let change=Change{entity:entity(&kind)?,key,revision:1,ordinal:ordinal as u64,content:BlobRef{hash:super::digest(&record),size:record.len() as u64,version:1}};
                tx.execute("INSERT INTO core_snapshot_rows VALUES(?1,?2,?3,?4)",params![token,pos as i64,serde_json::to_string(&change).map_err(|_|StageError::Invalid("Invalid snapshot row"))?,record])?;}
            token
        };
        let recorded:String=tx.query_row("SELECT head FROM core_snapshots WHERE token=?1",[&token],|r|r.get(0)).optional()?.ok_or(StageError::Unavailable("Snapshot expired"))?;
        if serde_json::from_str::<Head>(&recorded).map_err(|_|StageError::Invalid("Invalid snapshot head"))?!=*head{return Err(StageError::Conflict("Snapshot token targets another head"));}
        // Expire abandoned transfers, not active ones whose total duration
        // exceeds five minutes on a high-latency relay connection.
        tx.execute("UPDATE core_snapshots SET created=?2 WHERE token=?1",params![token,now])?;
        let rows=tx.prepare("SELECT change,record FROM core_snapshot_rows WHERE token=?1 AND pos>=?2 ORDER BY pos LIMIT ?3")?.query_map(params![token,page as i64*PAGE as i64,PAGE as i64+1],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Vec<u8>>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let last=rows.len()<=PAGE;let mut entries=vec![];
        for (change,record) in rows.into_iter().take(PAGE){let change:Change=serde_json::from_str(&change).map_err(|_|StageError::Invalid("Invalid snapshot row"))?;blob::put(tx,ACTOR_ID,&record)?;entries.push(change);}
        let account:String=tx.query_row("SELECT value FROM core_meta WHERE key='account'",[],|r|r.get(0))?;
        Ok(Page{token,account,head:head.clone(),page,last,entries})
    })
}
pub fn receive(store: &ActorStore, page: &Page, account: &str) -> Result<bool> {
    receive_inner(store, page, account, None)
}
pub(crate) fn receive_recovery(
    store: &ActorStore,
    page: &Page,
    account: &str,
    device: super::StageId,
) -> Result<bool> {
    receive_inner(store, page, account, Some(device))
}
fn receive_inner(
    store: &ActorStore,
    page: &Page,
    account: &str,
    recovery: Option<super::StageId>,
) -> Result<bool> {
    if page.account != account
        || page.entries.len() > PAGE
        || uuid::Uuid::parse_str(&page.token).is_err()
    {
        return Err(StageError::Forbidden("Invalid account snapshot"));
    }
    page.head.validate()?;
    store.stage_transaction(|tx|{
        initialize(tx)?;let now=chrono::Utc::now().timestamp_millis();
        tx.execute("DELETE FROM core_snapshot_incoming WHERE token IN (SELECT token FROM core_snapshot_age WHERE touched<?1)",[now-300000])?;
        tx.execute("DELETE FROM core_snapshot_content WHERE token IN (SELECT token FROM core_snapshot_age WHERE touched<?1)",[now-300000])?;
        tx.execute("DELETE FROM core_snapshot_age WHERE touched<?1",[now-300000])?;
        tx.execute("INSERT INTO core_snapshot_age VALUES(?1,?2) ON CONFLICT(token) DO UPDATE SET touched=excluded.touched",params![page.token,now])?;
        let encoded=serde_json::to_string(page).map_err(|_|StageError::Invalid("Invalid snapshot page"))?;
        let previous:Option<String>=tx.query_row("SELECT record FROM core_snapshot_incoming WHERE token=?1 AND page=?2",params![page.token,page.page],|r|r.get(0)).optional()?;
        if previous.as_ref().is_some_and(|old|old!=&encoded){return Err(StageError::Conflict("Snapshot retry changed"));}
        let expected:i64=tx.query_row("SELECT COUNT(*) FROM core_snapshot_incoming WHERE token=?1",[&page.token],|r|r.get(0))?;
        if previous.is_none() && page.page as i64!=expected{return Err(StageError::Conflict("Snapshot page gap"));}
        let mut keys=std::collections::HashSet::new();let mut ordinals=std::collections::HashSet::new();
        for change in &page.entries {
            if change.ordinal==0 || change.ordinal>9_007_199_254_740_991 || !keys.insert((change.entity.name(),change.key.clone())) || !ordinals.insert((change.entity.name(),change.ordinal)){return Err(StageError::Conflict("Duplicate or invalid snapshot order"));}
            let seen:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM core_snapshot_content WHERE token=?1 AND entity=?2 AND key=?3)",params![page.token,change.entity.name(),change.key],|r|r.get(0))?;
            if seen && previous.is_none(){return Err(StageError::Conflict("Snapshot repeats a canonical key"));}
            change.content.validate()?;let bytes:Vec<u8>=tx.query_row("SELECT bytes FROM stage_blobs WHERE hash=?1 AND size=?2",params![change.content.hash,change.content.size as i64],|r|r.get(0))?;
            if super::digest(&bytes)!=change.content.hash{return Err(StageError::Conflict("Snapshot content checksum failed"));}
            tx.execute("INSERT OR IGNORE INTO core_snapshot_content VALUES(?1,?2,?3,?4)",params![page.token,change.entity.name(),change.key,bytes])?;}
        tx.execute("INSERT OR IGNORE INTO core_snapshot_incoming VALUES(?1,?2,?3,?4)",params![page.token,page.page,serde_json::to_string(&page.head).map_err(|_|StageError::Invalid("Invalid snapshot head"))?,encoded])?;
        if !page.last{return Ok(false);}
        let current:Head=serde_json::from_str(&tx.query_row::<String,_,_>("SELECT value FROM core_meta WHERE key='head'",[],|r|r.get(0))?).map_err(|_|StageError::Invalid("Invalid core head"))?;
        if recovery.is_none() && (current.sequence>page.head.sequence || (current.sequence==page.head.sequence && current!=page.head)){return Err(StageError::Conflict("Snapshot cannot roll back committed state"));}
        let pages=tx.prepare("SELECT record FROM core_snapshot_incoming WHERE token=?1 ORDER BY page")?.query_map([&page.token],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        tx.execute("UPDATE core_meta SET value='1' WHERE key='importing'",[])?;
        let archive = if let Some(device) = recovery.filter(|_| current != page.head) {
            let archive = super::recovery::archive(tx, device, &current)?;
            // The original records remain in the private branch archive. Only
            // the active conversation and model window switch to the winner.
            tx.execute_batch("DELETE FROM approvals WHERE message_id IN (SELECT id FROM messages WHERE actor_id='core');DELETE FROM tools WHERE actor_id='core';DELETE FROM history WHERE actor_id='core';DELETE FROM messages WHERE actor_id='core';DELETE FROM events WHERE actor_id='core';DELETE FROM idempotency WHERE actor_id='core';DELETE FROM core_rows;DELETE FROM core_prepared;DELETE FROM core_dirty;")?;
            Some(archive)
        } else { None };
        // Speculative follower captures cannot reserve committed order positions.
        tx.execute("DELETE FROM core_order",[])?;
        for data in pages {let part:Page=serde_json::from_str(&data).map_err(|_|StageError::Invalid("Invalid snapshot page"))?;if part.head!=page.head{return Err(StageError::Conflict("Snapshot head mismatch"));}
            for change in part.entries {let bytes:Vec<u8>=tx.query_row("SELECT record FROM core_snapshot_content WHERE token=?1 AND entity=?2 AND key=?3",params![page.token,change.entity.name(),change.key],|r|r.get(0))?;
                let previous:Option<String>=tx.query_row("SELECT local_key FROM core_rows WHERE entity=?1 AND remote_key=?2",params![change.entity.name(),change.key],|r|r.get(0)).optional()?;
                let local=super::shared_store::apply(tx,&change,&bytes,previous.as_deref())?;
                tx.execute("INSERT INTO core_rows(entity,remote_key,local_key,record,content_hash) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(entity,remote_key) DO UPDATE SET local_key=excluded.local_key,record=excluded.record,content_hash=excluded.content_hash",params![change.entity.name(),change.key,local,bytes,change.content.hash])?;
                tx.execute("INSERT INTO core_order VALUES(?1,?2,?3) ON CONFLICT(entity,remote_key) DO UPDATE SET ordinal=excluded.ordinal",params![change.entity.name(),change.key,change.ordinal as i64])?;
                tx.execute("DELETE FROM core_dirty WHERE entity=?1 AND local_key=?2",params![change.entity.name(),local])?;
            }
        }
        tx.execute("UPDATE core_meta SET value=?1 WHERE key='head'",[serde_json::to_string(&page.head).map_err(|_|StageError::Invalid("Invalid snapshot head"))?])?;
        tx.execute("UPDATE core_meta SET value='0' WHERE key='importing'",[])?;
        tx.execute("DELETE FROM core_snapshot_incoming WHERE token=?1",[&page.token])?;tx.execute("DELETE FROM core_snapshot_content WHERE token=?1",[&page.token])?;
        if let Some(archive_id) = archive {
            super::recovery::restore_unstarted(tx, &archive_id)?;
            let event = crate::actors::ActorEvent::ConversationRestored { archive_id };
            tx.execute("INSERT INTO events(actor_id,created_at,record) VALUES('core',?1,?2)",params![now,serde_json::to_string(&event).map_err(|_|StageError::Invalid("Cannot record conversation recovery"))?])?;
        }
        Ok(true)
    })
}
