//! Explicit local branches when the account authority is unavailable. A branch
//! never rewrites Raft membership or pretends that a minority is a quorum.
use super::{
    Result, StageError, StageId,
    shared::{ACTOR_ID, Head},
};
use crate::actors::{MessageRecord, Page, store::ActorStore};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Branch {
    pub id: String,
    pub device: StageId,
    pub base: Head,
    pub epoch: u64,
    pub created_at_ms: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub branch: Option<Branch>,
    pub head: Head,
    pub epoch: u64,
    pub frozen: bool,
    #[serde(default)]
    pub archives_pending: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Archive {
    pub id: String,
    pub device: StageId,
    pub head: Head,
    pub created_at_ms: i64,
    pub messages: usize,
}
pub fn initialize(store: &ActorStore) -> Result<()> {
    store.stage_transaction(|tx| {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS core_recovery_archives(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,device TEXT NOT NULL,head TEXT UNIQUE NOT NULL,created INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS core_recovery_rows(archive TEXT NOT NULL,entity TEXT NOT NULL,key TEXT NOT NULL,ordinal INTEGER NOT NULL,record BLOB NOT NULL,PRIMARY KEY(archive,entity,key));
            CREATE TABLE IF NOT EXISTS core_recovery_merged(archive TEXT PRIMARY KEY);")?;
        Ok(())
    })
}
pub fn archives_pending(store: &ActorStore) -> Result<bool> {
    store.stage_transaction(|tx| Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM core_recovery_archives a WHERE NOT EXISTS(SELECT 1 FROM core_recovery_merged m WHERE m.archive=a.id))", [], |r| r.get(0))?))
}
pub fn mark_archives_merged(store: &ActorStore) -> Result<()> {
    store.stage_transaction(|tx| {
        tx.execute(
            "INSERT OR IGNORE INTO core_recovery_merged SELECT id FROM core_recovery_archives",
            [],
        )?;
        Ok(())
    })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchivedChange {
    pub archive: String,
    pub change: super::shared_store::Change,
}
pub fn archived_page(
    store: &ActorStore,
    after: Option<i64>,
) -> Result<(Vec<ArchivedChange>, Option<i64>)> {
    if after.is_some_and(|after| after < 0) {
        return Err(StageError::Invalid("Invalid archived record cursor"));
    }
    store.stage_transaction(|tx| {
        let rows = tx.prepare("SELECT r.rowid,r.entity,r.key,r.record,r.archive FROM core_recovery_rows r WHERE r.rowid>?1 AND r.entity IN ('messages','tools','resources') AND NOT EXISTS(SELECT 1 FROM core_recovery_merged m WHERE m.archive=r.archive) ORDER BY r.rowid LIMIT 25")?
            .query_map([after.unwrap_or(0)], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Vec<u8>>(3)?,r.get::<_,String>(4)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let next = (rows.len()>24).then(|| rows[23].0);
        let mut entries=vec![];
        for (ordinal,kind,key,bytes,archive) in rows.into_iter().take(24) {
            let entity=match kind.as_str() { "messages"=>super::shared_store::Entity::Message,"tools"=>super::shared_store::Entity::Tool,_=>super::shared_store::Entity::Resource };
            entries.push(ArchivedChange {archive,change:super::shared_store::Change {entity,key,revision:1,ordinal:ordinal as u64,content:super::blob::put(tx,ACTOR_ID,&bytes)?.content}});
        }
        Ok((entries,next))
    })
}

pub(crate) struct RetainedRow {
    pub branch: String,
    pub entity: super::shared_store::Entity,
    pub key: String,
    pub bytes: Vec<u8>,
}
/// Merge source conversation facts, never model transcripts, approvals or effect
/// intents. Started work from another branch is retained without being queued.
pub(crate) fn merge_conversation(
    store: &ActorStore,
    branches: &[(StageId, Vec<RetainedRow>)],
) -> Result<bool> {
    use super::shared_store::Entity;
    use crate::actors::{MessageStatus, ToolResultRecord};
    // Each archive is a separate version of the same message IDs. Never let an
    // older archive's ID mapping attach observations to a newer outcome.
    let mut groups = std::collections::BTreeMap::new();
    for (device, rows) in branches {
        for row in rows {
            groups
                .entry((*device, row.branch.as_str()))
                .or_insert_with(Vec::new)
                .push(row);
        }
    }
    store.stage_transaction(|tx| {
        let mut changed = false;
        for rows in groups.values() {
            let mut tools = vec![];
            for row in rows.iter().filter(|row| row.entity == Entity::Tool) {
                let value: serde_json::Value = serde_json::from_slice(&row.bytes).map_err(|_| StageError::Invalid("Invalid retained observation"))?;
                let tool: ToolResultRecord = serde_json::from_value(value["record"].clone()).map_err(|_| StageError::Invalid("Invalid retained observation"))?;
                let done = value["done"].as_i64().filter(|done| matches!(done, 0 | 1)).ok_or(StageError::Invalid("Invalid retained observation status"))?;
                if tool.actor_id != ACTOR_ID || tool.id != row.key { return Err(StageError::Forbidden("Retained observation belongs to another Actor")); }
                tools.push((tool, done));
            }
            let mut ids = std::collections::HashMap::<String, String>::new();
            let mut messages = vec![];
            for row in rows.iter().filter(|row| row.entity == Entity::Message) {
                let mut message: MessageRecord = serde_json::from_slice(&row.bytes).map_err(|_| StageError::Invalid("Invalid retained conversation"))?;
                if message.actor_id != ACTOR_ID || message.id != row.key { return Err(StageError::Forbidden("Retained message belongs to another Actor")); }
                if message.status == MessageStatus::Queued && message.started_at_ms.is_none() { continue; }
                let original = message.id.clone();
                let existing: Option<String> = tx.query_row("SELECT record FROM messages WHERE id=?1 AND actor_id='core'", [&original], |r| r.get(0)).optional()?;
                if let Some(existing) = existing {
                    let existing: MessageRecord = serde_json::from_str(&existing).map_err(|_| StageError::Invalid("Invalid existing conversation"))?;
                    let same_input = existing.content == message.content && existing.attachments == message.attachments && existing.source == message.source && existing.reply_to == message.reply_to && existing.kind == message.kind && existing.in_reply_to == message.in_reply_to;
                    let incoming = tools.iter().filter(|(tool, _)| tool.message_id == original).collect::<Vec<_>>();
                    let mut conflicting = false;
                    let mut additional = false;
                    for (tool, done) in &incoming {
                        let current: Option<(String, i64)> = tx.query_row("SELECT record,done FROM tools WHERE message_id=?1 AND call_id=?2", params![original, tool.call_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
                        if let Some((data, current_done)) = current {
                            let current: ToolResultRecord = serde_json::from_str(&data).map_err(|_| StageError::Invalid("Invalid existing observation"))?;
                            conflicting |= observation_facts(&current)? != observation_facts(tool)? || current_done != *done;
                        } else { additional = true; }
                    }
                    let same_outcome = existing.reply == message.reply && existing.error == message.error;
                    // An older incomplete snapshot adds no source fact if the
                    // selected version already contains all its observations.
                    if same_input && !conflicting && (same_outcome || (message.reply.is_none() && message.error.is_none() && !additional)) {
                        ids.insert(original, message.id);
                        continue;
                    }
                    let mut facts = incoming.iter().map(|(tool, done)| Ok((tool.call_id.as_str(), observation_facts(tool)?, *done))).collect::<Result<Vec<_>>>()?;
                    facts.sort_by_key(|(call, _, _)| *call);
                    let digest = super::digest(&serde_json::to_vec(&(original.as_str(), &message.source, &message.reply_to, &message.kind, &message.in_reply_to, &message.content, &message.attachments, &message.reply, &message.error, facts)).map_err(|_| StageError::Invalid("Cannot identify retained outcome"))?);
                    message.id = format!("recovered-{digest}");
                }
                ids.insert(original, message.id.clone());
                if matches!(message.status, MessageStatus::Queued | MessageStatus::Running | MessageStatus::Interrupted) {
                    message.status = MessageStatus::Cancelled;
                    message.cancel_requested = true;
                    message.finished_at_ms = Some(message.updated_at_ms);
                }
                messages.push(message);
            }
            messages.sort_by_key(|message| (message.created_at_ms, message.id.clone()));
            for mut message in messages {
                if let Some(parent) = message.in_reply_to.as_mut() && let Some(mapped) = ids.get(parent) { *parent = mapped.clone(); }
                let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1)", [&message.id], |r| r.get(0))?;
                if exists { continue; }
                tx.execute("INSERT INTO messages(id,actor_id,status,record) VALUES(?1,'core',?2,'')", params![message.id, serde_json::to_string(&message.status).map_err(|_| StageError::Invalid("Invalid retained status"))?])?;
                message.seq = tx.last_insert_rowid();
                tx.execute("UPDATE messages SET record=?2 WHERE id=?1", params![message.id, serde_json::to_string(&message).map_err(|_| StageError::Invalid("Invalid retained conversation"))?])?;
                changed = true;
            }
            for (mut tool, done) in tools {
                let Some(mapped) = ids.get(&tool.message_id) else { continue; };
                let active: Option<String> = tx.query_row("SELECT status FROM messages WHERE id=?1 AND actor_id='core'", [mapped], |r| r.get(0)).optional()?;
                if active.as_deref().is_none_or(|status| matches!(status, "\"queued\"" | "\"running\"" | "\"interrupted\"")) { continue; }
                let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM tools WHERE message_id=?1 AND call_id=?2)", params![mapped, tool.call_id], |r| r.get(0))?;
                if exists { continue; }
                let collision: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM tools WHERE id=?1)", [&tool.id], |r| r.get(0))?;
                if tool.message_id != *mapped || collision { tool.id = format!("recovered-{}", super::digest(format!("{mapped}/{}", tool.call_id).as_bytes())); }
                tool.message_id = mapped.clone();
                changed |= tx.execute("INSERT OR IGNORE INTO tools(id,actor_id,message_id,call_id,record,done) VALUES(?1,'core',?2,?3,?4,?5)", params![tool.id, tool.message_id, tool.call_id, serde_json::to_string(&tool).map_err(|_| StageError::Invalid("Invalid retained observation"))?, done])? > 0;
            }
        }
        if changed {
            let event = crate::actors::ActorEvent::ConversationRestored { archive_id: uuid::Uuid::new_v4().to_string() };
            tx.execute("INSERT INTO events(actor_id,created_at,record) VALUES('core',?1,?2)", params![chrono::Utc::now().timestamp_millis(), serde_json::to_string(&event).map_err(|_| StageError::Invalid("Invalid recovery event"))?])?;
        }
        Ok(changed)
    })
}
fn observation_facts(tool: &crate::actors::ToolResultRecord) -> Result<serde_json::Value> {
    let mut value =
        serde_json::to_value(tool).map_err(|_| StageError::Invalid("Invalid observation"))?;
    if let Some(object) = value.as_object_mut() {
        for key in [
            "id",
            "actor_id",
            "message_id",
            "started_at_ms",
            "finished_at_ms",
        ] {
            object.remove(key);
        }
    }
    Ok(value)
}

pub fn branch(store: &ActorStore) -> Result<Option<Branch>> {
    store.stage_transaction(|tx| {
        let data: Option<String> = tx
            .query_row(
                "SELECT value FROM core_meta WHERE key='recovery_branch'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|data| {
            serde_json::from_str(&data).map_err(|_| StageError::Invalid("Invalid recovery branch"))
        })
        .transpose()
    })
}
pub fn save_branch(store: &ActorStore, branch: &Branch) -> Result<()> {
    branch.base.validate()?;
    store.stage_transaction(|tx| {
        tx.execute("INSERT INTO core_meta VALUES('recovery_branch',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [serde_json::to_string(branch).map_err(|_| StageError::Invalid("Invalid recovery branch"))?])?;
        Ok(())
    })
}
pub fn clear_branch(store: &ActorStore) -> Result<()> {
    store.stage_transaction(|tx| {
        tx.execute("DELETE FROM core_meta WHERE key='recovery_branch'", [])?;
        Ok(())
    })
}
pub fn synchronized(store: &ActorStore) -> Result<bool> {
    store.stage_transaction(|tx| {
        Ok(tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM core_meta WHERE key='synchronized' AND value='1')",
            [],
            |r| r.get(0),
        )?)
    })
}
pub fn mark_synchronized(store: &ActorStore) -> Result<()> {
    store.stage_transaction(|tx| { tx.execute("INSERT INTO core_meta VALUES('synchronized','1') ON CONFLICT(key) DO UPDATE SET value='1' WHERE value!='1'", [])?; Ok(()) })
}
pub(crate) fn archive(tx: &Transaction<'_>, device: StageId, head: &Head) -> Result<String> {
    let encoded =
        serde_json::to_string(head).map_err(|_| StageError::Invalid("Invalid recovery head"))?;
    if let Some(id) = tx
        .query_row(
            "SELECT id FROM core_recovery_archives WHERE head=?1",
            [&encoded],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    let used: i64 = tx.query_row("SELECT (SELECT COALESCE(SUM(length(record)),0) FROM core_recovery_rows)+(SELECT COALESCE(SUM(length(record)),0) FROM core_rows)", [], |r| r.get(0))?;
    if used > 128 * 1024 * 1024 {
        return Err(StageError::Capacity("Recovery archive budget reached"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO core_recovery_archives(id,device,head,created) VALUES(?1,?2,?3,?4)",
        params![
            id,
            device.to_string(),
            encoded,
            chrono::Utc::now().timestamp_millis()
        ],
    )?;
    tx.execute("INSERT INTO core_recovery_rows SELECT ?1,r.entity,r.remote_key,o.ordinal,r.record FROM core_rows r JOIN core_order o ON o.entity=r.entity AND o.remote_key=r.remote_key", [&id])?;
    // Local authority metadata is retained privately, never in native snapshots
    // or client observations. Only inputs which never began are candidates for
    // delivery to the restored conversation.
    tx.execute("INSERT INTO core_recovery_rows SELECT ?1,'pending_input',id,seq,CAST(record AS BLOB) FROM messages WHERE actor_id='core' AND status='\"queued\"' AND json_extract(record,'$.started_at_ms') IS NULL", [&id])?;
    Ok(id)
}
pub(crate) fn restore_unstarted(tx: &Transaction<'_>, archive: &str) -> Result<()> {
    let records = tx.prepare("SELECT record FROM core_recovery_rows WHERE archive=?1 AND entity='pending_input' ORDER BY ordinal")?
        .query_map([archive], |r|r.get::<_,Vec<u8>>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
    for record in records {
        let mut message: MessageRecord = serde_json::from_slice(&record)
            .map_err(|_| StageError::Invalid("Invalid retained input"))?;
        if message.status != crate::actors::MessageStatus::Queued || message.started_at_ms.is_some()
        {
            continue;
        }
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1)",
            [&message.id],
            |r| r.get(0),
        )?;
        if exists {
            continue;
        }
        tx.execute(
            "INSERT INTO messages(id,actor_id,status,record) VALUES(?1,'core','\"queued\"',?2)",
            params![
                message.id,
                std::str::from_utf8(&record)
                    .map_err(|_| StageError::Invalid("Invalid retained input"))?
            ],
        )?;
        message.seq = tx.last_insert_rowid();
        let mut original: serde_json::Value = serde_json::from_slice(&record)
            .map_err(|_| StageError::Invalid("Invalid retained input"))?;
        original["seq"] = message.seq.into();
        tx.execute(
            "UPDATE messages SET record=?2 WHERE id=?1",
            params![
                message.id,
                serde_json::to_string(&original)
                    .map_err(|_| StageError::Invalid("Invalid retained input"))?
            ],
        )?;
        let event = crate::actors::ActorEvent::MessageAdded {
            message_id: message.id,
        };
        tx.execute(
            "INSERT INTO events(actor_id,created_at,record) VALUES('core',?1,?2)",
            params![
                chrono::Utc::now().timestamp_millis(),
                serde_json::to_string(&event)
                    .map_err(|_| StageError::Invalid("Invalid retained input event"))?
            ],
        )?;
    }
    Ok(())
}
pub fn archives(store: &ActorStore, after: Option<i64>, limit: usize) -> Result<Page<Archive>> {
    if limit == 0 || limit > 100 || after.is_some_and(|after| after < 0) {
        return Err(StageError::Invalid("Invalid recovery archive page"));
    }
    store.stage_transaction(|tx| {
        let rows = tx.prepare("SELECT seq,id,device,head,created,(SELECT COUNT(*) FROM core_recovery_rows WHERE archive=a.id AND entity='messages') FROM core_recovery_archives a WHERE seq>?1 ORDER BY seq LIMIT ?2")?
            .query_map(params![after.unwrap_or(0), limit as i64 + 1], |r| Ok((r.get::<_,i64>(0)?, r.get::<_,String>(1)?, r.get::<_,String>(2)?, r.get::<_,String>(3)?, r.get::<_,i64>(4)?, r.get::<_,i64>(5)?)))?
            .collect::<std::result::Result<Vec<_>,_>>()?;
        let next_cursor = (rows.len() > limit).then(|| rows[limit-1].0);
        let items = rows.into_iter().take(limit).map(|(_,id,device,head,created_at_ms,messages)| {
            Ok(Archive {id, device: StageId(uuid::Uuid::parse_str(&device).map_err(|_| StageError::Invalid("Invalid archived device"))?), head: serde_json::from_str(&head).map_err(|_| StageError::Invalid("Invalid archived head"))?, created_at_ms, messages: messages as usize})
        }).collect::<Result<Vec<_>>>()?;
        Ok(Page {items, next_cursor})
    })
}
pub fn messages(
    store: &ActorStore,
    archive: &str,
    after: Option<i64>,
    limit: usize,
) -> Result<Page<MessageRecord>> {
    if uuid::Uuid::parse_str(archive).is_err()
        || limit == 0
        || limit > 100
        || after.is_some_and(|after| after < 0)
    {
        return Err(StageError::Invalid("Invalid archived message page"));
    }
    store.stage_transaction(|tx| {
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM core_recovery_archives WHERE id=?1)", [archive], |r| r.get(0))?;
        if !exists { return Err(StageError::Invalid("Recovery archive does not exist")); }
        let rows = tx.prepare("SELECT ordinal,record FROM core_recovery_rows WHERE archive=?1 AND entity='messages' AND ordinal>?2 ORDER BY ordinal LIMIT ?3")?
            .query_map(params![archive,after.unwrap_or(0),limit as i64+1], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,Vec<u8>>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let next_cursor = (rows.len()>limit).then(|| rows[limit-1].0);
        let items = rows.into_iter().take(limit).map(|(ordinal,record)| {
            let mut message: MessageRecord = serde_json::from_slice(&record).map_err(|_| StageError::Invalid("Invalid archived message"))?;
            message.actor_id = ACTOR_ID.into(); message.seq = ordinal; Ok(message)
        }).collect::<Result<Vec<_>>>()?;
        Ok(Page {items,next_cursor})
    })
}
pub fn archive_count(store: &ActorStore) -> Result<usize> {
    store.stage_transaction(|tx| {
        Ok(
            tx.query_row::<i64, _, _>("SELECT COUNT(*) FROM core_recovery_archives", [], |r| {
                r.get(0)
            })? as usize,
        )
    })
}
