//! One transactional store for Actor identity, mailbox, model history and client events.
use super::*;
use crate::model_protocol::{AgentMessage, AgentToolCall};
use miette::{IntoDiagnostic, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{path::Path, sync::Arc};
use tokio::sync::Notify;
use uuid::Uuid;

pub struct ActorStore {
    db: Mutex<Connection>,
    changed: Arc<Notify>,
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn delivery_id(actor: &str, input: &str, call: &str) -> String {
    let hash = crate::stage::digest(format!("actor-delivery:{actor}:{input}:{call}").as_bytes());
    let bytes = hex::decode(hash).expect("SHA256 hex");
    Uuid::from_bytes(bytes[..16].try_into().expect("SHA256 prefix")).to_string()
}
fn tool_outcome(result: &ToolResultRecord) -> Option<Value> {
    if result.error.is_some() {
        return None;
    }
    let payload = result.payload.as_object()?;
    let domain_name = result
        .name
        .strip_prefix("d_")
        .filter(|name| {
            name.len() > 13
                && name.as_bytes()[..12].iter().all(u8::is_ascii_hexdigit)
                && name.as_bytes()[12] == b'_'
        })
        .and_then(|name| name.get(13..))
        .unwrap_or(&result.name);
    if domain_name == "terminal__terminal_exec" {
        let command = payload
            .get("session")
            .and_then(|session| session.get("command"))
            .and_then(Value::as_str)
            .or_else(|| result.arguments.get("command").and_then(Value::as_str))?;
        // Preserve a small source fact; lengthy commands remain in scoped details.
        return (command.len() <= 2048).then(|| serde_json::json!({"command": command}));
    }
    let keys: &[&str] = match domain_name {
        "artifacts__present" => &[
            "artifact_id",
            "uri",
            "title",
            "kind",
            "description",
            "mime_type",
            "byte_len",
        ],
        "actors__create" | "actors__send" => &[
            "id",
            "actor_id",
            "message_id",
            "name",
            "conversation_url",
            "stage_id",
            "status",
        ],
        "files__write_file" | "files__edit_file" | "files__copy_file" => {
            &["path", "created", "byte_count", "sha256"]
        }
        _ => return None,
    };
    let mut output = serde_json::Map::new();
    for key in keys {
        if let Some(value) = payload.get(*key) {
            output.insert((*key).into(), value.clone());
        }
    }
    if matches!(domain_name, "actors__create" | "actors__send") {
        // Older Core results retained a host-local URL, but their explicit
        // device argument still identifies the original host after replication.
        let stage = payload
            .get("stage_id")
            .or_else(|| result.arguments.get("stage_id"))
            .and_then(Value::as_str)
            .and_then(|id| uuid::Uuid::parse_str(id).ok());
        let actor = payload
            .get("actor_id")
            .or_else(|| payload.get("id"))
            .and_then(Value::as_str);
        if let (Some(stage), Some(actor)) = (stage, actor) {
            output.insert("stage_id".into(), stage.to_string().into());
            if payload
                .get("conversation_url")
                .and_then(Value::as_str)
                .is_none_or(|url| url == format!("/actors/{actor}"))
            {
                output.insert(
                    "conversation_url".into(),
                    format!("/stages/{stage}/actors/{actor}").into(),
                );
            }
        }
    }
    if let Some(change) = payload.get("change").and_then(Value::as_object) {
        for key in [
            "added_lines",
            "removed_lines",
            "before_sha256",
            "after_sha256",
        ] {
            if let Some(value) = change.get(key) {
                output.insert(key.into(), value.clone());
            }
        }
    }
    Some(Value::Object(output))
}

fn encode<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).into_diagnostic()
}
fn decode<T: DeserializeOwned>(value: String) -> Result<T> {
    serde_json::from_str(&value).into_diagnostic()
}
fn missing(kind: &str) -> miette::Report {
    ActorError::NotFound(format!("{kind} not found")).into()
}
fn sql<T>(value: rusqlite::Result<T>) -> Result<T> {
    value.into_diagnostic()
}
fn event(tx: &Transaction<'_>, actor_id: &str, value: ActorEvent) -> Result<i64> {
    if let ActorEvent::GenerationDelta {
        message_id,
        generation,
        channel,
        text,
    } = &value
        && text.len() > 8192
    {
        let mut remaining = text.as_str();
        let mut last = 0;
        while !remaining.is_empty() {
            let mut boundary = remaining.len().min(8192);
            while !remaining.is_char_boundary(boundary) {
                boundary -= 1;
            }
            let (chunk, tail) = remaining.split_at(boundary);
            last = event(
                tx,
                actor_id,
                ActorEvent::GenerationDelta {
                    message_id: message_id.clone(),
                    generation: *generation,
                    channel: channel.clone(),
                    text: chunk.into(),
                },
            )?;
            remaining = tail;
        }
        return Ok(last);
    }
    sql(tx.execute(
        "INSERT INTO events(actor_id,created_at,record) VALUES(?1,?2,?3)",
        params![actor_id, now(), encode(&value)?],
    ))?;
    let id = tx.last_insert_rowid();
    crate::stage::journal::record(tx, id, actor_id, &value)
        .map_err(|error| miette::miette!("{error}"))?;
    Ok(id)
}
pub(crate) fn actor_tx(tx: &Transaction<'_>, id: &str) -> Result<ActorRecord> {
    decode(
        sql(tx
            .query_row("SELECT record FROM actors WHERE id=?1", [id], |r| r.get(0))
            .optional())?
        .ok_or_else(|| missing("Actor"))?,
    )
}
fn save_actor(tx: &Transaction<'_>, actor: &ActorRecord) -> Result<()> {
    let mut data = serde_json::to_value(actor).into_diagnostic()?;
    data["depth"] = Value::from(actor.depth);
    sql(tx.execute("INSERT INTO actors(id,parent_id,depth,record) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET parent_id=excluded.parent_id,depth=excluded.depth,record=excluded.record",params![actor.id,actor.parent_id,actor.depth,encode(&data)?]))?;
    event(
        tx,
        &actor.id,
        ActorEvent::ActorChanged {
            actor: actor.summary(),
        },
    )?;
    Ok(())
}
fn message_tx(tx: &Transaction<'_>, id: &str) -> Result<MessageRecord> {
    decode(
        sql(tx
            .query_row("SELECT record FROM messages WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional())?
        .ok_or_else(|| missing("Message"))?,
    )
}
fn encode_message(message: &MessageRecord) -> Result<String> {
    let mut data = serde_json::to_value(message).into_diagnostic()?;
    data["delegation_grant"] = serde_json::to_value(&message.delegation_grant).into_diagnostic()?;
    data["remote_authority"] = serde_json::to_value(&message.remote_authority).into_diagnostic()?;
    encode(&data)
}
fn save_message(tx: &Transaction<'_>, message: &MessageRecord) -> Result<()> {
    sql(tx.execute(
        "UPDATE messages SET status=?1,record=?2 WHERE id=?3",
        params![
            encode(&message.status)?,
            encode_message(message)?,
            message.id
        ],
    ))?;
    crate::stage::ledger::actor_message_state(tx, message)
        .map_err(|error| miette::miette!("{error}"))?;
    crate::stage::delivery::finished(tx, message).map_err(|error| miette::miette!("{error}"))?;
    event(
        tx,
        &message.actor_id,
        ActorEvent::MessageUpdated {
            message_id: message.id.clone(),
            status: message.status,
        },
    )?;
    Ok(())
}
pub(crate) fn insert_message(
    tx: &Transaction<'_>,
    mut message: MessageRecord,
) -> Result<MessageRecord> {
    sql(tx.execute(
        "INSERT INTO messages(id,actor_id,status,record) VALUES(?1,?2,?3,'')",
        params![message.id, message.actor_id, encode(&message.status)?],
    ))?;
    message.seq = tx.last_insert_rowid();
    sql(tx.execute(
        "UPDATE messages SET record=?1 WHERE id=?2",
        params![encode_message(&message)?, message.id],
    ))?;
    if message.actor_id != crate::stage::shared::ACTOR_ID || message.kind != MessageKind::Delivery {
        event(
            tx,
            &message.actor_id,
            ActorEvent::MessageAdded {
                message_id: message.id.clone(),
            },
        )?;
    }
    Ok(message)
}

impl ActorStore {
    /// Stage delivery shares the Actor database, including transaction failure semantics.
    pub(crate) fn stage_transaction<T>(
        &self,
        apply: impl FnOnce(&Transaction<'_>) -> crate::stage::Result<T>,
    ) -> crate::stage::Result<T> {
        self.stage_transaction_with_notify(apply, true)
    }
    /// Consensus heartbeats are infrastructure writes, not Actor changes.
    /// Publishing them to Actor wakes creates a heartbeat/write feedback loop.
    pub(crate) fn coordination_transaction<T>(
        &self,
        apply: impl FnOnce(&Transaction<'_>) -> crate::stage::Result<T>,
    ) -> crate::stage::Result<T> {
        self.stage_transaction_with_notify(apply, false)
    }
    fn stage_transaction_with_notify<T>(
        &self,
        apply: impl FnOnce(&Transaction<'_>) -> crate::stage::Result<T>,
        notify: bool,
    ) -> crate::stage::Result<T> {
        let mut db = self.db.lock();
        let before = db.total_changes();
        let tx = db.transaction()?;
        let value = apply(&tx)?;
        tx.commit()?;
        if notify && db.total_changes() != before {
            self.changed.notify_waiters();
        }
        Ok(value)
    }
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).into_diagnostic()?;
        }
        let mut db = sql(Connection::open(path))?;
        sql(db.busy_timeout(std::time::Duration::from_secs(5)))?;
        sql(db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA journal_size_limit=67108864;
          CREATE TABLE IF NOT EXISTS stage_public_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS actors(id TEXT PRIMARY KEY,parent_id TEXT,depth INTEGER NOT NULL,record TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS messages(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,actor_id TEXT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,status TEXT NOT NULL,record TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS mailbox ON messages(actor_id,status,seq);
          CREATE INDEX IF NOT EXISTS queued_actors ON messages(actor_id) WHERE status='\"queued\"';
          CREATE TABLE IF NOT EXISTS history(seq INTEGER PRIMARY KEY AUTOINCREMENT,actor_id TEXT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,message_id TEXT NOT NULL,record TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS history_actor ON history(actor_id,seq);
          CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,actor_id TEXT NOT NULL,created_at INTEGER NOT NULL,record TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS events_actor ON events(actor_id,seq);
          CREATE INDEX IF NOT EXISTS events_message ON events(actor_id,json_extract(record,'$.message_id'),seq);
          CREATE TABLE IF NOT EXISTS tools(id TEXT PRIMARY KEY,actor_id TEXT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,message_id TEXT NOT NULL,call_id TEXT NOT NULL,record TEXT NOT NULL,done INTEGER NOT NULL,UNIQUE(message_id,call_id));
          CREATE TABLE IF NOT EXISTS resources(actor_id TEXT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,id TEXT NOT NULL,record TEXT NOT NULL,PRIMARY KEY(actor_id,id));
          CREATE TABLE IF NOT EXISTS idempotency(actor_id TEXT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,principal TEXT NOT NULL,key TEXT NOT NULL,message_id TEXT NOT NULL,PRIMARY KEY(actor_id,principal,key));
          CREATE TABLE IF NOT EXISTS approvals(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,actor_id TEXT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,record TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS approval_actor ON approvals(actor_id,seq);"))?;
        crate::persistence::set_private_file_permissions(path).into_diagnostic()?;
        let tx = sql(db.transaction())?;
        super::migrations::retire_guest_execution(&tx, now()).into_diagnostic()?;
        crate::stage::blob::initialize(&tx).map_err(|error| miette::miette!("{error}"))?;
        crate::stage::journal::initialize(&tx).map_err(|error| miette::miette!("{error}"))?;
        crate::stage::sync::initialize(&tx).map_err(|error| miette::miette!("{error}"))?;
        crate::stage::delivery::initialize(&tx).map_err(|error| miette::miette!("{error}"))?;
        sql(tx.commit())?;
        Ok(Self {
            db: Mutex::new(db),
            changed: Arc::new(Notify::new()),
        })
    }
    pub fn changed(&self) -> Arc<Notify> {
        self.changed.clone()
    }
    /// Public routing identity needs no device key and never changes on enrollment.
    pub fn public_stage_id(&self) -> crate::stage::Result<crate::stage::StageId> {
        self.stage_transaction(|tx|{
            let stored:Option<String>=tx.query_row("SELECT value FROM stage_public_meta WHERE key='stage'",[],|r|r.get(0)).optional()?;
            let value=if let Some(value)=stored{value}else{
                let enrolled:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='stage_meta')",[],|r|r.get(0))?;
                let value=if enrolled{tx.query_row("SELECT value FROM stage_meta WHERE key='local'",[],|r|r.get::<_,String>(0)).optional()?}else{None}.unwrap_or_else(||Uuid::new_v4().to_string());
                tx.execute("INSERT INTO stage_public_meta VALUES('stage',?1)",[&value])?;value
            };
            let id=Uuid::parse_str(&value).map_err(|_|crate::stage::StageError::Invalid("Invalid public Stage identity"))?;
            if id.is_nil(){return Err(crate::stage::StageError::Invalid("Invalid public Stage identity"));}Ok(crate::stage::StageId(id))
        })
    }
    fn commit(&self, tx: Transaction<'_>) -> Result<()> {
        sql(tx.commit())?;
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn actor(&self, id: &str) -> Result<ActorRecord> {
        decode(
            sql(self
                .db
                .lock()
                .query_row("SELECT record FROM actors WHERE id=?1", [id], |r| r.get(0))
                .optional())?
            .ok_or_else(|| missing("Actor"))?,
        )
    }
    pub fn actors(&self) -> Result<Vec<ActorRecord>> {
        let db = self.db.lock();
        let mut stmt = sql(db.prepare("SELECT record FROM actors ORDER BY depth,id"))?;
        let rows = sql(stmt.query_map([], |r| r.get::<_, String>(0)))?;
        rows.map(|r| decode(sql(r)?)).collect()
    }
    pub(crate) fn actor_page(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ActorSummary>> {
        let db = self.db.lock();
        let mut stmt =
            sql(db.prepare("SELECT record FROM actors WHERE id>?1 ORDER BY id LIMIT ?2"))?;
        let rows = sql(stmt.query_map(
            params![after.unwrap_or(""), limit.clamp(1, 101) as i64],
            |r| r.get::<_, String>(0),
        ))?;
        rows.map(|r| Ok(decode::<ActorRecord>(sql(r)?)?.summary()))
            .collect()
    }
    pub(crate) fn actor_models(&self) -> Result<Vec<String>> {
        let db = self.db.lock();
        let mut stmt = sql(db.prepare("SELECT DISTINCT json_extract(record,'$.config.model') FROM actors WHERE json_extract(record,'$.config.model') IS NOT NULL"))?;
        sql(stmt.query_map([], |r| r.get(0)))?
            .collect::<rusqlite::Result<_>>()
            .into_diagnostic()
    }
    /// Only removed model preferences change; identities, grants and history stay
    /// intact. Null selects the account default, or waits for model setup.
    pub(crate) fn reset_model_preferences(&self, removed: &[String]) -> Result<()> {
        if removed.is_empty() {
            return Ok(());
        }
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        for model in removed {
            let ids = {
                let mut query = sql(tx.prepare(
                    "SELECT id FROM actors WHERE json_extract(record,'$.config.model')=?1",
                ))?;
                sql(query.query_map([model], |row| row.get::<_, String>(0)))?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .into_diagnostic()?
            };
            for id in ids {
                let mut actor = actor_tx(&tx, &id)?;
                actor.config.model = None;
                actor.updated_at_ms = now();
                save_actor(&tx, &actor)?;
            }
        }
        self.commit(tx)
    }
    pub(crate) fn actor_ids(&self) -> Result<Vec<String>> {
        let db = self.db.lock();
        let mut stmt = sql(db.prepare("SELECT id FROM actors ORDER BY id"))?;
        sql(stmt.query_map([], |r| r.get(0)))?
            .collect::<rusqlite::Result<_>>()
            .into_diagnostic()
    }
    #[cfg(test)]
    pub(crate) fn actor_count(&self) -> Result<i64> {
        sql(self
            .db
            .lock()
            .query_row("SELECT COUNT(*) FROM actors", [], |r| r.get(0)))
    }
    /// Only executable inbox entries activate a worker; steering belongs to an
    /// already running input. Query the mailbox index without decoding records.
    pub(crate) fn has_queued_input(&self, actor: &str) -> Result<bool> {
        sql(self.db.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE actor_id=?1 AND status='\"queued\"' AND json_extract(record,'$.kind')!='steering')",
            [actor], |r| r.get(0),
        ))
    }
    pub(crate) fn queued_actor_ids(&self) -> Result<Vec<String>> {
        let db = self.db.lock();
        let mut stmt = sql(db.prepare(
            "SELECT DISTINCT actor_id FROM messages WHERE status='\"queued\"' AND json_extract(record,'$.kind')!='steering' ORDER BY actor_id",
        ))?;
        sql(stmt.query_map([], |r| r.get(0)))?
            .collect::<rusqlite::Result<_>>()
            .into_diagnostic()
    }
    pub fn save_actor(&self, actor: &ActorRecord) -> Result<()> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        save_actor(&tx, actor)?;
        self.commit(tx)
    }
    pub fn set_status(&self, id: &str, status: ActorStatus) -> Result<()> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let mut actor = actor_tx(&tx, id)?;
        if actor.status != status {
            actor.status = status;
            actor.updated_at_ms = now();
            save_actor(&tx, &actor)?;
        }
        self.commit(tx)
    }
    pub(crate) fn update_token_estimate(
        &self,
        id: &str,
        binding: &str,
        baseline: crate::context_budget::TokenEstimateBaseline,
    ) -> Result<()> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let mut actor = actor_tx(&tx, id)?;
        actor.context.model_binding = binding.to_owned();
        actor.context.token_estimate = baseline;
        save_actor(&tx, &actor)?;
        self.commit(tx)
    }
    pub(crate) fn reset_model_context(&self, id: &str, message: &str) -> Result<AgentMessage> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let mut actor = actor_tx(&tx, id)?;
        let anchor = HistoryAnchor {
            message_id: message.to_owned(),
            reset_id: Uuid::new_v4(),
        };
        let recovery = anchor.recovery_message();
        sql(tx.execute(
            "INSERT INTO history(actor_id,message_id,record) VALUES(?1,?2,?3)",
            params![id, message, encode(&recovery)?],
        ))?;
        actor.context.anchor = Some(anchor);
        save_actor(&tx, &actor)?;
        self.commit(tx)?;
        Ok(recovery)
    }
    pub(crate) fn record_model_usage(
        &self,
        id: &str,
        delta: crate::core::TokenUsage,
        window: Option<i64>,
    ) -> Result<()> {
        if delta.is_zero() {
            return Ok(());
        }
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let mut actor = actor_tx(&tx, id)?;
        actor.context.usage.append_last_usage(delta);
        actor.context.usage.model_context_window = window;
        save_actor(&tx, &actor)?;
        self.commit(tx)
    }
    pub(crate) fn context_start(&self, id: &str) -> Result<i64> {
        let actor = self.actor(id)?;
        let Some(anchor) = actor.context.anchor else {
            return Ok(0);
        };
        let db = self.db.lock();
        let local: i64 = sql(db.query_row(
            "SELECT MIN(seq) FROM history WHERE actor_id=?1 AND message_id=?2 AND record=?3",
            params![id, anchor.message_id, encode(&anchor.recovery_message())?],
            |row| row.get::<_, Option<i64>>(0),
        ))?
        .ok_or_else(|| miette::miette!("Working context anchor is unavailable"))?;
        if id == crate::stage::shared::ACTOR_ID && sql(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='core_order')",[],|row|row.get::<_,bool>(0)))? {
            return sql(db.query_row("SELECT COALESCE((SELECT o.ordinal FROM core_rows r JOIN core_order o ON o.entity=r.entity AND o.remote_key=r.remote_key WHERE r.entity='history' AND r.local_key=CAST(?1 AS TEXT)),4503599627370496+?1)",[local],|row|row.get(0)));
        }
        Ok(local)
    }
    /// Pagination bounds I/O, never the number of retained messages or steps.
    pub(crate) fn history_page(
        &self,
        actor: &str,
        after: i64,
        message: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(i64, String, AgentMessage)>> {
        let db = self.db.lock();
        let shared=actor == crate::stage::shared::ACTOR_ID && sql(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='core_order')",[],|row|row.get::<_,bool>(0)))?;
        let query = if shared {
            "SELECT ordinal,message_id,record FROM (SELECT COALESCE((SELECT o.ordinal FROM core_rows r JOIN core_order o ON o.entity=r.entity AND o.remote_key=r.remote_key WHERE r.entity='history' AND r.local_key=CAST(h.seq AS TEXT)),4503599627370496+h.seq) AS ordinal,h.message_id,h.record FROM history h WHERE h.actor_id=?1) WHERE ordinal>?2 AND (?3 IS NULL OR message_id=?3) ORDER BY ordinal LIMIT ?4"
        } else {
            "SELECT seq,message_id,record FROM history WHERE actor_id=?1 AND seq>?2 AND (?3 IS NULL OR message_id=?3) ORDER BY seq LIMIT ?4"
        };
        let mut stmt = sql(db.prepare(query))?;
        let rows = sql(stmt.query_map(
            params![actor, after, message, limit.clamp(1, 128) as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        ))?;
        rows.map(|row| {
            let (seq, input, record) = sql(row)?;
            Ok((seq, input, decode(record)?))
        })
        .collect()
    }
    pub fn enqueue(&self, message: MessageRecord, key: Option<&str>) -> Result<MessageRecord> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        actor_tx(&tx, &message.actor_id)?;
        let principal = match &message.source {
            MessageSource::User => "owner".to_string(),
            MessageSource::Actor { actor_id } => format!("actor:{actor_id}"),
            MessageSource::RemoteUser { stage_id, .. } => format!("remote-user:{stage_id}"),
            MessageSource::RemoteActor {
                stage_id, actor_id, ..
            } => format!("remote-actor:{stage_id}:{actor_id}"),
        };
        if let Some(key) = key
            && let Some(id)=sql(tx.query_row("SELECT message_id FROM idempotency WHERE actor_id=?1 AND principal=?2 AND key=?3",params![message.actor_id,principal,key],|r|r.get::<_,String>(0)).optional())? {
                let existing=message_tx(&tx,&id)?;
                if existing.content!=message.content || existing.attachments!=message.attachments {return Err(ActorError::Conflict("Idempotency key was used for another message".into()).into());}
                return Ok(existing);
        }
        let pending:i64=sql(tx.query_row("SELECT COUNT(*) FROM messages WHERE actor_id=?1 AND status IN ('\"queued\"','\"running\"')",[&message.actor_id],|r|r.get(0)))?;
        if pending >= 256 {
            return Err(ActorError::Unavailable("Actor mailbox is full".into()).into());
        }
        let result = insert_message(&tx, message)?;
        if let Some(key) = key {
            sql(tx.execute(
                "INSERT INTO idempotency VALUES(?1,?2,?3,?4)",
                params![result.actor_id, principal, key, result.id],
            ))?;
        }
        self.commit(tx)?;
        Ok(result)
    }
    pub fn claim_next(&self, actor_id: &str) -> Result<Option<MessageRecord>> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let mut actor = actor_tx(&tx, actor_id)?;
        if actor.status == ActorStatus::Running {
            return Ok(None);
        }
        // A replica/lifecycle transition can leave Actor status idle while its
        // interrupted input is still recorded as running. Recovery must repair
        // that input before the serial inbox claims any later request.
        if sql(tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE actor_id=?1 AND status='\"running\"')",
            [actor_id],
            |r| r.get::<_, bool>(0),
        ))? {
            return Ok(None);
        }
        let shared = actor_id == crate::stage::shared::ACTOR_ID
            && sql(tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='core_order')", [], |r| r.get::<_,bool>(0)))?;
        let query = if shared {
            "SELECT m.id FROM messages m LEFT JOIN core_order o ON o.entity='messages' AND o.remote_key=m.id WHERE m.actor_id=?1 AND m.status='\"queued\"' AND json_extract(m.record,'$.kind')!='steering' ORDER BY o.ordinal IS NULL,o.ordinal,m.seq LIMIT 1"
        } else {
            "SELECT id FROM messages WHERE actor_id=?1 AND status='\"queued\"' AND json_extract(record,'$.kind')!='steering' ORDER BY seq LIMIT 1"
        };
        let id = sql(tx
            .query_row(query, [actor_id], |r| r.get::<_, String>(0))
            .optional())?;
        let Some(id) = id else { return Ok(None) };
        let mut message = message_tx(&tx, &id)?;
        message.status = MessageStatus::Running;
        message.updated_at_ms = now();
        message.started_at_ms = Some(message.updated_at_ms);
        save_message(&tx, &message)?;
        actor.status = ActorStatus::Running;
        actor.updated_at_ms = now();
        save_actor(&tx, &actor)?;
        self.commit(tx)?;
        Ok(Some(message))
    }
    pub fn message(&self, id: &str) -> Result<MessageRecord> {
        let db = self.db.lock();
        let mut message: MessageRecord = decode(
            sql(db
                .query_row("SELECT record FROM messages WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .optional())?
            .ok_or_else(|| missing("Message"))?,
        )?;
        if message.actor_id == crate::stage::shared::ACTOR_ID
            && sql(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='core_order')", [], |r| r.get::<_,bool>(0)))?
        {
            if message.kind == MessageKind::Delivery && !sql(db.query_row("SELECT EXISTS(SELECT 1 FROM core_rows WHERE entity='messages' AND remote_key=?1)",[id],|r|r.get::<_,bool>(0)))? { return Err(missing("Committed message")); }
            message.seq = sql(db.query_row("SELECT COALESCE((SELECT ordinal FROM core_order WHERE entity='messages' AND remote_key=?1),4503599627370496+(SELECT seq FROM messages WHERE id=?1))", [id], |r| r.get(0)))?;
        }
        Ok(message)
    }
    // Exact source, version and payload are separate transaction inputs, not model controls.
    #[allow(clippy::too_many_arguments)]
    pub fn amend_queued_user(
        &self,
        actor: &str,
        id: &str,
        expected: i64,
        content: String,
        attachments: Vec<Attachment>,
        withdraw: bool,
    ) -> Result<MessageRecord> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let mut message = message_tx(&tx, id)?;
        if message.actor_id != actor
            || !matches!(
                message.source,
                MessageSource::User | MessageSource::RemoteUser { .. }
            )
        {
            return Err(ActorError::Forbidden(
                "This input cannot be changed with the current access".into(),
            )
            .into());
        }
        if message.status != MessageStatus::Queued
            || message.kind != MessageKind::Request
            || message.updated_at_ms != expected
        {
            return Err(
                ActorError::Conflict("The input changed or has already started".into()).into(),
            );
        }
        if withdraw {
            message.status = MessageStatus::Cancelled;
            message.cancel_requested = true;
            message.error = Some("Queued input withdrawn before execution".into());
            message.finished_at_ms = Some(now());
        } else {
            if content.len() > 64 * 1024 || content.trim().is_empty() && attachments.is_empty() {
                return Err(ActorError::Invalid("Invalid queued input".into()).into());
            }
            message.content = content;
            message.attachments = attachments;
        }
        message.updated_at_ms = now().max(message.updated_at_ms + 1);
        save_message(&tx, &message)?;
        self.commit(tx)?;
        Ok(message)
    }
    pub fn clear_queued_users(&self, actor: &str) -> Result<usize> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        actor_tx(&tx, actor)?;
        let records = {
            let mut statement = sql(tx.prepare("SELECT record FROM messages WHERE actor_id=?1 AND status='\"queued\"' AND json_extract(record,'$.source.kind') IN ('user','remote_user')"))?;
            let rows = sql(statement.query_map(params![actor], |row| row.get::<_, String>(0)))?;
            rows.map(|row| decode::<MessageRecord>(sql(row)?))
                .collect::<Result<Vec<_>>>()?
        };
        let count = records.len();
        for mut message in records {
            message.status = MessageStatus::Cancelled;
            message.cancel_requested = true;
            message.error = Some("Queued input withdrawn before execution".into());
            message.updated_at_ms = now().max(message.updated_at_ms + 1);
            message.finished_at_ms = Some(message.updated_at_ms);
            save_message(&tx, &message)?;
        }
        self.commit(tx)?;
        Ok(count)
    }
    /// Bind an immutable queued input to the exact execution selected by the user.
    pub fn steer_queued(
        &self,
        actor: &str,
        id: &str,
        target: &str,
        expected: i64,
    ) -> Result<MessageRecord> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let mut message = message_tx(&tx, id)?;
        let active = message_tx(&tx, target)?;
        if message.actor_id != actor
            || active.actor_id != actor
            || !matches!(
                message.source,
                MessageSource::User | MessageSource::RemoteUser { .. }
            )
        {
            return Err(
                ActorError::Forbidden("This input cannot guide this execution".into()).into(),
            );
        }
        if message.status != MessageStatus::Queued
            || message.kind != MessageKind::Request
            || message.updated_at_ms != expected
            || active.status != MessageStatus::Running
            || active.cancel_requested
        {
            return Err(ActorError::Conflict(
                "The queued input or active execution changed".into(),
            )
            .into());
        }
        message.kind = MessageKind::Steering;
        message.in_reply_to = Some(active.id);
        message.updated_at_ms = now().max(message.updated_at_ms + 1);
        save_message(&tx, &message)?;
        self.commit(tx)?;
        Ok(message)
    }
    pub(crate) fn pending_steering(&self, actor: &str, target: &str) -> Result<Vec<MessageRecord>> {
        let db = self.db.lock();
        let mut statement = sql(db.prepare("SELECT record FROM messages WHERE actor_id=?1 AND status='\"queued\"' AND json_extract(record,'$.kind')='steering' AND json_extract(record,'$.in_reply_to')=?2 ORDER BY seq LIMIT 32"))?;
        let rows = sql(statement.query_map(params![actor, target], |r| r.get::<_, String>(0)))?;
        rows.map(|r| decode(sql(r)?)).collect()
    }
    pub(crate) fn apply_steering(
        &self,
        actor: &str,
        target: &str,
        input: &MessageRecord,
        content: &AgentMessage,
    ) -> Result<bool> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let mut current = message_tx(&tx, &input.id)?;
        let active = message_tx(&tx, target)?;
        if active.actor_id != actor
            || active.status != MessageStatus::Running
            || active.cancel_requested
        {
            return Ok(false);
        }
        if current.actor_id != actor
            || current.kind != MessageKind::Steering
            || current.in_reply_to.as_deref() != Some(target)
            || current.status != MessageStatus::Queued
            || current.updated_at_ms != input.updated_at_ms
        {
            return Ok(false);
        }
        sql(tx.execute(
            "INSERT INTO history(actor_id,message_id,record) VALUES(?1,?2,?3)",
            params![actor, target, encode(content)?],
        ))?;
        current.status = MessageStatus::Completed;
        current.finished_at_ms = Some(now());
        current.updated_at_ms = now().max(current.updated_at_ms + 1);
        save_message(&tx, &current)?;
        self.commit(tx)?;
        Ok(true)
    }
    pub fn queued_messages(
        &self,
        actor: &str,
        after: Option<i64>,
        limit: usize,
    ) -> Result<Page<MessageRecord>> {
        let db = self.db.lock();
        let limit = limit.clamp(1, 100);
        let shared = actor == crate::stage::shared::ACTOR_ID && sql(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='core_order')",[],|r|r.get::<_,bool>(0)))?;
        let query = if shared {
            "SELECT m.record,COALESCE(o.ordinal,4503599627370496+m.seq) AS position FROM messages m LEFT JOIN core_order o ON o.entity='messages' AND o.remote_key=m.id WHERE m.actor_id=?1 AND m.status='\"queued\"' AND json_extract(m.record,'$.source.kind') IN ('user','remote_user') AND position>?2 ORDER BY position LIMIT ?3"
        } else {
            "SELECT record,seq FROM messages WHERE actor_id=?1 AND status='\"queued\"' AND json_extract(record,'$.source.kind') IN ('user','remote_user') AND seq>?2 ORDER BY seq LIMIT ?3"
        };
        let mut statement = sql(db.prepare(query))?;
        let rows = sql(statement.query_map(
            params![actor, after.unwrap_or(0), (limit + 1) as i64],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        ))?;
        let mut items = rows
            .map(|r| {
                let (record, seq) = sql(r)?;
                let mut record: MessageRecord = decode(record)?;
                record.seq = seq;
                Ok(record)
            })
            .collect::<Result<Vec<_>>>()?;
        let next_cursor = if items.len() > limit {
            items.pop();
            items.last().map(|item| item.seq)
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }
    pub fn running_message(&self, actor: &str) -> Result<Option<MessageRecord>> {
        let db = self.db.lock();
        let value:Option<String>=sql(db.query_row("SELECT record FROM messages WHERE actor_id=?1 AND status='\"running\"' ORDER BY seq LIMIT 1",[actor],|r|r.get(0)).optional())?;
        value.map(decode).transpose()
    }
    pub fn message_events(
        &self,
        actor: &str,
        message: &str,
        after: i64,
        limit: usize,
    ) -> Result<Page<EventRecord>> {
        let limit = limit.clamp(1, 100);
        let db = self.db.lock();
        let mut statement = sql(db.prepare("SELECT seq,created_at,record FROM events WHERE actor_id=?1 AND seq>?2 AND json_extract(record,'$.message_id')=?3 ORDER BY seq LIMIT ?4"))?;
        let rows = sql(statement.query_map(
            params![actor, after, message, (limit + 1) as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        ))?;
        let mut items = Vec::new();
        for row in rows {
            let (seq, created_at_ms, record) = sql(row)?;
            items.push(EventRecord {
                seq,
                actor_id: actor.into(),
                created_at_ms,
                event: decode(record)?,
            });
        }
        let next_cursor = if items.len() > limit {
            items.pop();
            items.last().map(|event| event.seq)
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }
    pub fn current_message(&self, actor_id: &str) -> Result<Option<MessageRecord>> {
        let row=sql(self.db.lock().query_row("SELECT record FROM messages WHERE actor_id=?1 AND status='\"running\"' ORDER BY seq LIMIT 1",[actor_id],|r|r.get::<_,String>(0)).optional())?;
        row.map(decode).transpose()
    }
    pub fn messages(
        &self,
        id: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Page<MessageRecord>> {
        let limit = limit.clamp(1, 100);
        let db = self.db.lock();
        let shared = id == crate::stage::shared::ACTOR_ID
            && sql(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='core_order')", [], |r| r.get::<_,bool>(0)))?;
        let query = if shared {
            "SELECT m.record,COALESCE(o.ordinal,4503599627370496+m.seq) AS position FROM messages m LEFT JOIN core_order o ON o.entity='messages' AND o.remote_key=m.id WHERE m.actor_id=?1 AND (json_extract(m.record,'$.kind')!='delivery' OR EXISTS(SELECT 1 FROM core_rows r WHERE r.entity='messages' AND r.remote_key=m.id)) AND position<?2 ORDER BY position DESC LIMIT ?3"
        } else {
            "SELECT record,seq FROM messages WHERE actor_id=?1 AND seq<?2 ORDER BY seq DESC LIMIT ?3"
        };
        let mut stmt = sql(db.prepare(query))?;
        let rows = sql(stmt.query_map(
            params![id, before.unwrap_or(i64::MAX), (limit + 1) as i64],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        ))?;
        let mut items: Vec<MessageRecord> = rows
            .map(|r| {
                let (record, seq) = sql(r)?;
                let mut record: MessageRecord = decode(record)?;
                record.seq = seq;
                Ok(record)
            })
            .collect::<Result<_>>()?;
        let next_cursor = if items.len() > limit {
            items.pop();
            items.last().map(|m| m.seq)
        } else {
            None
        };
        items.reverse();
        Ok(Page { items, next_cursor })
    }
    pub fn search_messages(
        &self,
        actor_id: &str,
        query: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Page<MessageRecord>> {
        let query = query.trim();
        if query.is_empty() || query.len() > 512 {
            return Err(ActorError::Invalid("Search query is invalid".into()).into());
        }
        let limit = limit.clamp(1, 100);
        let db = self.db.lock();
        let mut stmt = sql(db.prepare(
            "SELECT record FROM messages WHERE actor_id=?1 AND seq<?2
             AND (json_extract(record,'$.kind')!='delivery' OR actor_id!='core' OR EXISTS(SELECT 1 FROM events e WHERE e.actor_id='core' AND json_extract(e.record,'$.type')='message_added' AND json_extract(e.record,'$.message_id')=messages.id))
             AND (instr(lower(json_extract(record,'$.content')),lower(?3))>0
               OR instr(lower(COALESCE(json_extract(record,'$.reply'),'')),lower(?3))>0)
             ORDER BY seq DESC LIMIT ?4",
        ))?;
        let rows = sql(stmt.query_map(
            params![
                actor_id,
                before.unwrap_or(i64::MAX),
                query,
                (limit + 1) as i64
            ],
            |row| row.get::<_, String>(0),
        ))?;
        let mut items: Vec<MessageRecord> = rows.map(|r| decode(sql(r)?)).collect::<Result<_>>()?;
        let next_cursor = if items.len() > limit {
            items.pop();
            items.last().map(|message| message.seq)
        } else {
            None
        };
        items.reverse();
        Ok(Page { items, next_cursor })
    }
    pub fn append_history(
        &self,
        actor_id: &str,
        message_id: &str,
        messages: &[AgentMessage],
    ) -> Result<()> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        for message in messages {
            sql(tx.execute(
                "INSERT INTO history(actor_id,message_id,record) VALUES(?1,?2,?3)",
                params![actor_id, message_id, encode(message)?],
            ))?;
        }
        self.commit(tx)
    }
    pub(crate) fn receive_notice(
        &self,
        source: crate::stage::StageId,
        record: &EventRecord,
    ) -> Result<()> {
        // Also reject duplicate Core message notices from older peers. Core is
        // one account conversation, never a separately hosted remote inbox.
        if record.actor_id == crate::stage::shared::ACTOR_ID
            && matches!(
                record.event,
                ActorEvent::MessageAdded { .. } | ActorEvent::MessageUpdated { .. }
            )
        {
            return Ok(());
        }
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        sql(tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_notice_seen(stage TEXT NOT NULL,seq INTEGER NOT NULL,PRIMARY KEY(stage,seq));"))?;
        let seen = sql(tx.execute(
            "INSERT OR IGNORE INTO stage_notice_seen VALUES(?1,?2)",
            params![source.to_string(), record.seq],
        ))?;
        if seen == 0 {
            return Ok(());
        }
        sql(tx.execute(
            "DELETE FROM stage_notice_seen WHERE stage=?1 AND seq<?2",
            params![source.to_string(), record.seq.saturating_sub(4096)],
        ))?;
        let mut fact = record.event.clone();
        let qualify = |id: &str| format!("{source}~{id}");
        match &mut fact {
            ActorEvent::ActorChanged { actor } => {
                actor.id = qualify(&actor.id);
                actor.conversation_url = format!("/stages/{source}/actors/{}", record.actor_id);
            }
            ActorEvent::ActorDeleted { actor_id } => *actor_id = qualify(actor_id),
            ActorEvent::ApprovalRequested { approval_id, .. } => {
                sql(crate::stage::approval_index::remember(
                    &tx,
                    &source.to_string(),
                    approval_id,
                    &record.actor_id,
                    true,
                ))?;
                *approval_id = qualify(approval_id)
            }
            ActorEvent::ApprovalResolved { approval_id, .. } => {
                sql(crate::stage::approval_index::remember(
                    &tx,
                    &source.to_string(),
                    approval_id,
                    &record.actor_id,
                    false,
                ))?;
                *approval_id = qualify(approval_id)
            }
            ActorEvent::MessageUpdated { .. } => {}
            _ => return Ok(()),
        }
        sql(tx.execute(
            "INSERT INTO events(actor_id,created_at,record) VALUES(?1,?2,?3)",
            params![
                qualify(&record.actor_id),
                record.created_at_ms,
                encode(&fact)?
            ],
        ))?;
        self.commit(tx)
    }
    pub(crate) fn has_history_for_message(&self, actor: &str, message: &str) -> Result<bool> {
        sql(self.db.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM history WHERE actor_id=?1 AND message_id=?2)",
            params![actor, message],
            |r| r.get(0),
        ))
    }
    pub fn history(&self, actor_id: &str, limit: usize) -> Result<Vec<AgentMessage>> {
        let db = self.db.lock();
        let mut stmt=sql(db.prepare("SELECT seq,message_id,record FROM history WHERE actor_id=?1 ORDER BY seq DESC LIMIT ?2"))?;
        let rows = sql(
            stmt.query_map(params![actor_id, limit.clamp(1, 4096) as i64], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            }),
        )?;
        let mut rows = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .into_diagnostic()?;
        rows.reverse();
        if let Some((first_seq, first_id, _)) = rows.first() {
            let minimum: i64 = sql(db.query_row(
                "SELECT MIN(seq) FROM history WHERE actor_id=?1 AND message_id=?2",
                params![actor_id, first_id],
                |r| r.get(0),
            ))?;
            if *first_seq != minimum {
                let incomplete = first_id.clone();
                rows.retain(|r| r.1 != incomplete);
            }
        }
        rows.into_iter()
            .map(|(_, _, value)| decode(value))
            .collect()
    }
    pub fn append_event(&self, actor_id: &str, value: ActorEvent) -> Result<i64> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let seq = event(&tx, actor_id, value)?;
        self.commit(tx)?;
        Ok(seq)
    }
    pub fn events(
        &self,
        after: i64,
        actor: Option<&str>,
        catalog: bool,
        limit: usize,
    ) -> Result<Vec<EventRecord>> {
        let db = self.db.lock();
        let mut stmt=sql(db.prepare("SELECT seq,actor_id,created_at,record FROM events WHERE seq>?1 AND (?2 IS NULL OR actor_id=?2) AND (?3=0 OR json_extract(record,'$.type') IN ('actor_changed','actor_deleted','approval_requested','approval_resolved','message_added','message_updated')) ORDER BY seq LIMIT ?4"))?;
        let rows = sql(stmt.query_map(
            params![after, actor, catalog, limit.clamp(1, 100) as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, String>(3)?)),
        ))?;
        let mut events = Vec::new();
        for row in rows {
            let (seq, actor_id, created_at_ms, data) = sql(row)?;
            let value: ActorEvent = decode(data)?;
            if catalog
                && !matches!(
                    &value,
                    ActorEvent::ActorChanged { .. }
                        | ActorEvent::ActorDeleted { .. }
                        | ActorEvent::MessageUpdated { .. }
                        | ActorEvent::MessageAdded { .. }
                )
                && !matches!(
                    &value,
                    ActorEvent::ApprovalRequested { .. } | ActorEvent::ApprovalResolved { .. }
                )
            {
                continue;
            }
            events.push(EventRecord {
                seq,
                actor_id,
                created_at_ms,
                event: value,
            });
        }
        Ok(events)
    }
    pub fn event_cursor(&self) -> Result<i64> {
        sql(self
            .db
            .lock()
            .query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| r.get(0)))
    }

    pub fn create_approval(&self, mut approval: ApprovalRecord) -> Result<ApprovalRecord> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let input = message_tx(&tx, &approval.message_id)?;
        if input.actor_id != approval.actor_id
            || input.status != MessageStatus::Running
            || input.cancel_requested
        {
            return Err(ActorError::Conflict("Approval request is no longer active".into()).into());
        }
        sql(tx.execute(
            "INSERT INTO approvals(id,actor_id,message_id,record) VALUES(?1,?2,?3,'')",
            params![approval.id, approval.actor_id, approval.message_id],
        ))?;
        approval.seq = tx.last_insert_rowid();
        sql(tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS stage_local_approvals(id TEXT PRIMARY KEY);",
        ))?;
        sql(tx.execute(
            "INSERT INTO stage_local_approvals VALUES(?1)",
            [&approval.id],
        ))?;
        sql(tx.execute(
            "UPDATE approvals SET record=?1 WHERE id=?2",
            params![encode(&approval)?, approval.id],
        ))?;
        event(
            &tx,
            &approval.actor_id,
            ActorEvent::ApprovalRequested {
                approval_id: approval.id.clone(),
                message_id: approval.message_id.clone(),
                call_id: approval.call.id.clone(),
                name: approval.call.name.clone(),
            },
        )?;
        self.commit(tx)?;
        Ok(approval)
    }
    pub fn approval(&self, id: &str) -> Result<ApprovalRecord> {
        decode(
            sql(self
                .db
                .lock()
                .query_row("SELECT record FROM approvals WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .optional())?
            .ok_or_else(|| missing("Approval"))?,
        )
    }
    pub(crate) fn native_approval(&self, id: &str) -> Result<bool> {
        let db = self.db.lock();
        let exists: bool = sql(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='stage_local_approvals')",
            [],
            |r| r.get(0),
        ))?;
        if !exists {
            return Ok(false);
        }
        sql(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM stage_local_approvals WHERE id=?1)",
            [id],
            |r| r.get(0),
        ))
    }
    pub fn approvals(
        &self,
        actor: Option<&str>,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Page<ApprovalRecord>> {
        self.approval_page(actor, before, limit, false)
    }
    pub fn pending_approvals(
        &self,
        actor: Option<&str>,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Page<ApprovalRecord>> {
        self.approval_page(actor, before, limit, true)
    }
    fn approval_page(
        &self,
        actor: Option<&str>,
        before: Option<i64>,
        limit: usize,
        awaiting_owner: bool,
    ) -> Result<Page<ApprovalRecord>> {
        let limit = limit.clamp(1, 100);
        let db = self.db.lock();
        let mut stmt=sql(db.prepare("SELECT record FROM approvals WHERE (?1 IS NULL OR actor_id=?1) AND seq<?2 AND (?3=0 OR (json_extract(record,'$.mode')='ask' AND json_extract(record,'$.decision')='pending')) ORDER BY seq DESC LIMIT ?4"))?;
        let rows = sql(stmt.query_map(
            params![
                actor,
                before.unwrap_or(i64::MAX),
                awaiting_owner,
                (limit + 1) as i64
            ],
            |r| r.get::<_, String>(0),
        ))?;
        let mut items: Vec<ApprovalRecord> =
            rows.map(|r| decode(sql(r)?)).collect::<Result<_>>()?;
        let next_cursor = if items.len() > limit {
            items.pop();
            items.last().map(|a| a.seq)
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }
    pub fn resolve_approval(
        &self,
        id: &str,
        decision: ApprovalDecision,
        reason: &str,
    ) -> Result<ApprovalRecord> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let mut approval: ApprovalRecord = decode(
            sql(tx
                .query_row("SELECT record FROM approvals WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .optional())?
            .ok_or_else(|| missing("Approval"))?,
        )?;
        if approval.decision != ApprovalDecision::Pending {
            return Err(ActorError::Conflict("Approval was already resolved".into()).into());
        }
        let message = message_tx(&tx, &approval.message_id)?;
        if decision != ApprovalDecision::Cancelled
            && (message.status != MessageStatus::Running || message.cancel_requested)
        {
            return Err(ActorError::Conflict("Approval request is no longer active".into()).into());
        }
        approval.decision = decision;
        approval.reason = reason.chars().take(1000).collect();
        if decision != ApprovalDecision::Pending {
            approval.resolved_at_ms = Some(now());
        }
        sql(tx.execute(
            "UPDATE approvals SET record=?1 WHERE id=?2",
            params![encode(&approval)?, id],
        ))?;
        event(
            &tx,
            &approval.actor_id,
            ActorEvent::ApprovalResolved {
                approval_id: approval.id.clone(),
                message_id: approval.message_id.clone(),
                decision,
                reason: approval.reason.clone(),
            },
        )?;
        self.commit(tx)?;
        Ok(approval)
    }
    pub fn tool_started(
        &self,
        actor_id: &str,
        message_id: &str,
        call: &AgentToolCall,
    ) -> Result<()> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let result = ToolResultRecord {
            id: Uuid::new_v4().to_string(),
            actor_id: actor_id.into(),
            message_id: message_id.into(),
            call_id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
            payload: Value::Null,
            model_content: String::new(),
            summary: String::new(),
            error: None,
            started_at_ms: Some(now()),
            finished_at_ms: None,
        };
        sql(tx.execute("INSERT INTO tools(id,actor_id,message_id,call_id,record,done) VALUES(?1,?2,?3,?4,?5,0)",params![result.id,actor_id,message_id,call.id,encode(&result)?]))?;
        event(
            &tx,
            actor_id,
            ActorEvent::ToolStarted {
                message_id: message_id.into(),
                call_id: call.id.clone(),
                name: call.name.clone(),
            },
        )?;
        self.commit(tx)
    }
    // A tool observation is committed together with its complete model protocol.
    #[allow(clippy::too_many_arguments)]
    pub fn tool_finished(
        &self,
        actor_id: &str,
        message_id: &str,
        call: &AgentToolCall,
        payload: Value,
        model_content: &str,
        summary: &str,
        error: Option<&str>,
    ) -> Result<String> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let existing = sql(tx
            .query_row(
                "SELECT record FROM tools WHERE message_id=?1 AND call_id=?2",
                params![message_id, call.id],
                |r| r.get::<_, String>(0),
            )
            .optional())?;
        let mut result: ToolResultRecord = if let Some(existing) = existing {
            decode(existing)?
        } else {
            ToolResultRecord {
                id: Uuid::new_v4().to_string(),
                actor_id: actor_id.into(),
                message_id: message_id.into(),
                call_id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                payload: Value::Null,
                model_content: String::new(),
                summary: String::new(),
                error: None,
                started_at_ms: None,
                finished_at_ms: None,
            }
        };
        if result.actor_id != actor_id {
            return Err(
                ActorError::Forbidden("Tool result belongs to another Actor".into()).into(),
            );
        }
        result.payload = payload;
        result.model_content = model_content.into();
        result.summary = summary.chars().take(1000).collect();
        result.error = error.map(str::to_owned);
        result.finished_at_ms = Some(now());
        sql(tx.execute("INSERT INTO tools(id,actor_id,message_id,call_id,record,done) VALUES(?1,?2,?3,?4,?5,1) ON CONFLICT(message_id,call_id) DO UPDATE SET record=excluded.record,done=1",params![result.id,actor_id,message_id,call.id,encode(&result)?]))?;
        let observation = AgentMessage::tool(
            &call.id,
            &call.name,
            if error.is_some() {
                format!("App error: {model_content}")
            } else {
                model_content.to_owned()
            },
        );
        sql(tx.execute(
            "INSERT INTO history(actor_id,message_id,record) VALUES(?1,?2,?3)",
            params![actor_id, message_id, encode(&observation)?],
        ))?;
        event(
            &tx,
            actor_id,
            ActorEvent::ToolFinished {
                message_id: message_id.into(),
                call_id: call.id.clone(),
                name: call.name.clone(),
                result_id: result.id.clone(),
                summary: result.summary.clone(),
                error: result.error.clone(),
                started_at_ms: result.started_at_ms,
                finished_at_ms: result.finished_at_ms,
                outcome: tool_outcome(&result),
            },
        )?;
        self.commit(tx)?;
        Ok(result.id)
    }
    pub fn core_turn_delivered(&self, message: &str) -> Result<bool> {
        sql(self.db.lock().query_row("SELECT EXISTS(SELECT 1 FROM tools WHERE actor_id='core' AND message_id=?1 AND done=1 AND json_extract(record,'$.name')='actors__deliver' AND json_extract(record,'$.error') IS NULL AND json_extract(record,'$.payload.finish_turn')=1)", [message], |r|r.get(0)))
    }
    pub fn tool_result(&self, id: &str) -> Result<ToolResultRecord> {
        decode(
            sql(self
                .db
                .lock()
                .query_row("SELECT record FROM tools WHERE id=?1", [id], |r| r.get(0))
                .optional())?
            .ok_or_else(|| missing("Tool result"))?,
        )
    }

    pub fn tool_summaries(
        &self,
        actor: &str,
        message: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Page<ToolResultSummary>> {
        let limit = limit.clamp(1, 100);
        let db = self.db.lock();
        let mut stmt=sql(db.prepare("SELECT rowid,record FROM tools WHERE actor_id=?1 AND message_id=?2 AND done=1 AND rowid<?3 ORDER BY rowid DESC LIMIT ?4"))?;
        let rows = sql(stmt.query_map(
            params![
                actor,
                message,
                before.unwrap_or(i64::MAX),
                (limit + 1) as i64
            ],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        ))?;
        let mut items = Vec::new();
        for row in rows {
            let (seq, data) = sql(row)?;
            let result: ToolResultRecord = decode(data)?;
            let outcome = tool_outcome(&result);
            items.push(ToolResultSummary {
                seq,
                id: result.id,
                message_id: result.message_id,
                call_id: result.call_id,
                name: result.name,
                summary: result.summary,
                outcome,
                error: result.error,
                started_at_ms: result.started_at_ms,
                finished_at_ms: result.finished_at_ms,
            });
        }
        let next_cursor = if items.len() > limit {
            items.pop();
            items.last().map(|r| r.seq)
        } else {
            None
        };
        items.reverse();
        Ok(Page { items, next_cursor })
    }
    pub fn delivery_receipt(
        &self,
        actor: &str,
        input: &str,
        call: &str,
    ) -> Result<Option<MessageRecord>> {
        let db = self.db.lock();
        let shared: bool = sql(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='core_rows')",
            [],
            |r| r.get(0),
        ))?;
        let data: Option<String> = if actor == crate::stage::shared::ACTOR_ID && shared {
            sql(db.query_row("SELECT CAST(record AS TEXT) FROM core_rows WHERE entity='messages' AND remote_key=?1",[delivery_id(actor,input,call)],|r|r.get(0)).optional())?
        } else {
            sql(db
                .query_row(
                    "SELECT record FROM messages WHERE id=?1",
                    [delivery_id(actor, input, call)],
                    |r| r.get(0),
                )
                .optional())?
        };
        data.map(decode).transpose()
    }
    /// Publish a committed account delivery. Repeating recovery cannot emit a
    /// duplicate event or enqueue the same remote delivery twice.
    pub(crate) fn publish_delivery(&self, id: &str) -> Result<()> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let message = message_tx(&tx, id)?;
        if message.actor_id != crate::stage::shared::ACTOR_ID
            || message.kind != MessageKind::Delivery
        {
            return Ok(());
        }
        let committed: bool = sql(tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM core_rows WHERE entity='messages' AND remote_key=?1)",
            [id],
            |r| r.get(0),
        ))?;
        if !committed {
            return Err(ActorError::Unavailable("Delivery has not committed".into()).into());
        }
        let published:bool=sql(tx.query_row("SELECT EXISTS(SELECT 1 FROM events WHERE actor_id='core' AND json_extract(record,'$.type')='message_added' AND json_extract(record,'$.message_id')=?1)",[id],|r|r.get(0)))?;
        if !published {
            event(
                &tx,
                &message.actor_id,
                ActorEvent::MessageAdded {
                    message_id: id.into(),
                },
            )?;
        }
        crate::stage::delivery::finished(&tx, &message)
            .map_err(|error| miette::miette!("{error}"))?;
        self.commit(tx)
    }
    /// Idempotent, origin-bound outward delivery. It never consumes or finishes
    /// the originating input, and a collaborator update is its own later input.
    pub fn deliver(
        &self,
        actor: &str,
        input: &str,
        call: &str,
        content: &str,
    ) -> Result<MessageRecord> {
        if content.trim().is_empty()
            || content.len() > 64 * 1024
            || call.is_empty()
            || call.len() > 256
        {
            return Err(ActorError::Invalid("Invalid conversation delivery".into()).into());
        }
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let source = message_tx(&tx, input)?;
        if source.actor_id != actor
            || source.status != MessageStatus::Running
            || source.cancel_requested
        {
            return Err(ActorError::Forbidden("Delivery input is no longer active".into()).into());
        }
        let id = delivery_id(actor, input, call);
        let mut delivery = MessageRecord {
            seq: 0,
            id,
            actor_id: actor.into(),
            source: MessageSource::Actor {
                actor_id: actor.into(),
            },
            reply_to: source.reply_to.clone(),
            kind: MessageKind::Delivery,
            in_reply_to: Some(source.id.clone()),
            delegation_grant: None,
            remote_authority: None,
            content: String::new(),
            attachments: vec![],
            status: MessageStatus::Completed,
            cancel_requested: false,
            started_at_ms: None,
            finished_at_ms: Some(now()),
            final_generation: None,
            reply: Some(content.into()),
            error: None,
            created_at_ms: now(),
            updated_at_ms: now(),
        };
        if let ReplyTarget::Actor {
            actor_id,
            request_id,
        } = &source.reply_to
        {
            let original = message_tx(&tx, request_id)?;
            if original.actor_id != *actor_id {
                return Err(ActorError::Forbidden("Delivery origin changed".into()).into());
            }
            delivery.actor_id = actor_id.clone();
            delivery.reply_to = original.reply_to;
            delivery.kind = MessageKind::Progress;
            delivery.in_reply_to = Some(original.id);
            delivery.content = format!(
                "Progress for delegated request {} (still running):\n{}",
                source.id, content
            );
            delivery.reply = None;
            delivery.status = MessageStatus::Queued;
            delivery.finished_at_ms = None;
            delivery.delegation_grant = original.delegation_grant;
            delivery.remote_authority = original.remote_authority;
        }
        if let Ok(existing) = message_tx(&tx, &delivery.id) {
            if existing.actor_id != delivery.actor_id
                || existing.source != delivery.source
                || existing.in_reply_to != delivery.in_reply_to
                || existing.content != delivery.content
                || (delivery.kind == MessageKind::Delivery && existing.reply != delivery.reply)
            {
                return Err(ActorError::Conflict(
                    "Delivery retry changed its content or origin".into(),
                )
                .into());
            }
            return Ok(existing);
        }
        if delivery.status == MessageStatus::Queued {
            let pending:i64=sql(tx.query_row("SELECT count(*) FROM messages WHERE actor_id=?1 AND status IN ('\"queued\"','\"running\"')",[&delivery.actor_id],|r|r.get(0)))?;
            if pending >= 256 {
                return Err(ActorError::Unavailable("Actor mailbox is full".into()).into());
            }
        }
        let delivery = insert_message(&tx, delivery)?;
        if delivery.actor_id != crate::stage::shared::ACTOR_ID
            || delivery.kind != MessageKind::Delivery
        {
            crate::stage::delivery::finished(&tx, &delivery)
                .map_err(|error| miette::miette!("{error}"))?;
        }
        self.commit(tx)?;
        Ok(delivery)
    }

    pub fn finish_message(
        &self,
        id: &str,
        status: MessageStatus,
        reply: Option<&str>,
        error: Option<&str>,
    ) -> Result<Option<MessageRecord>> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let mut message = message_tx(&tx, id)?;
        // Explicit Core deliveries are independent conversation messages. A
        // later final model answer closes the input without duplicating them.
        let explicit: bool = sql(tx.query_row("SELECT EXISTS(SELECT 1 FROM messages m WHERE actor_id=?1 AND json_extract(record,'$.kind')='delivery' AND json_extract(record,'$.in_reply_to')=?2 AND EXISTS(SELECT 1 FROM events e WHERE e.actor_id=m.actor_id AND json_extract(e.record,'$.type')='message_added' AND json_extract(e.record,'$.message_id')=m.id))", params![message.actor_id,id], |r|r.get(0)))?;
        let reply = if message.actor_id == crate::stage::shared::ACTOR_ID
            && explicit
            && status == MessageStatus::Completed
        {
            Some("")
        } else {
            reply
        };
        if !matches!(
            message.status,
            MessageStatus::Running | MessageStatus::Queued
        ) {
            return Ok(None);
        };
        let was_running = message.status == MessageStatus::Running;
        let (status, reply, error) = if message.cancel_requested {
            (
                MessageStatus::Cancelled,
                None,
                Some("Request cancelled; effects may have occurred"),
            )
        } else {
            (status, reply, error)
        };
        message.status = status;
        message.reply = reply.map(str::to_owned);
        message.error = error.map(str::to_owned);
        message.updated_at_ms = now();
        message.finished_at_ms = Some(message.updated_at_ms);
        if status == MessageStatus::Completed && reply.is_some() {
            message.final_generation = sql(tx.query_row("SELECT MAX(json_extract(record,'$.generation')) FROM events WHERE actor_id=?1 AND json_extract(record,'$.message_id')=?2 AND json_extract(record,'$.type')='generation_started'", params![message.actor_id, message.id], |row| row.get(0)))?;
        }
        save_message(&tx, &message)?;
        // A guide arriving after the last model response still becomes normal
        // queued work. Never discard it because its selected execution ended.
        let guides = sql(tx.prepare("SELECT record FROM messages WHERE actor_id=?1 AND status='\"queued\"' AND json_extract(record,'$.kind')='steering' AND json_extract(record,'$.in_reply_to')=?2"))?.query_map(params![message.actor_id,message.id],|r|r.get::<_,String>(0)).map_err(|error|miette::miette!("{error}"))?.collect::<rusqlite::Result<Vec<_>>>().into_diagnostic()?;
        for data in guides {
            let mut guide: MessageRecord = decode(data)?;
            guide.kind = MessageKind::Request;
            guide.in_reply_to = None;
            guide.updated_at_ms = now().max(guide.updated_at_ms + 1);
            save_message(&tx, &guide)?;
        }
        let pending = {
            let mut stmt=sql(tx.prepare("SELECT record FROM approvals WHERE message_id=?1 AND json_extract(record,'$.decision')='pending'"))?;
            let rows = sql(stmt.query_map([id], |r| r.get::<_, String>(0)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .into_diagnostic()?
        };
        for data in pending {
            let mut approval: ApprovalRecord = decode(data)?;
            approval.decision = ApprovalDecision::Cancelled;
            approval.reason = "Request finished or was interrupted before approval".into();
            approval.resolved_at_ms = Some(now());
            sql(tx.execute(
                "UPDATE approvals SET record=?1 WHERE id=?2",
                params![encode(&approval)?, approval.id],
            ))?;
            event(
                &tx,
                &approval.actor_id,
                ActorEvent::ApprovalResolved {
                    approval_id: approval.id,
                    message_id: approval.message_id,
                    decision: ApprovalDecision::Cancelled,
                    reason: approval.reason,
                },
            )?;
        }
        if was_running {
            let mut actor = actor_tx(&tx, &message.actor_id)?;
            actor.status = ActorStatus::Idle;
            actor.updated_at_ms = now();
            save_actor(&tx, &actor)?;
        }
        let routed = if let ReplyTarget::Actor {
            actor_id,
            request_id,
        } = &message.reply_to
        {
            if let Ok(original) = message_tx(&tx, request_id) {
                if original.actor_id == *actor_id && actor_tx(&tx, actor_id).is_ok() {
                    let mut answer = reply
                        .or(error)
                        .unwrap_or("Actor request completed without a reply")
                        .to_owned();
                    let result_id = Uuid::new_v4().to_string();
                    let mut attachments = Vec::new();
                    if status == MessageStatus::Completed {
                        let resources = {
                            let mut stmt=sql(tx.prepare("SELECT record FROM resources WHERE actor_id=?1 AND json_extract(record,'$.message_id')=?2"))?;
                            let rows = sql(stmt
                                .query_map(params![message.actor_id, message.id], |r| {
                                    r.get::<_, String>(0)
                                }))?;
                            rows.collect::<rusqlite::Result<Vec<_>>>()
                                .into_diagnostic()?
                        };
                        for data in resources {
                            let mut resource: StoredResource = decode(data)?;
                            if !answer.contains(&resource.uri) {
                                continue;
                            }
                            // A result transfers the referenced bytes, never another Actor's authority.
                            let Some(actor_directory) =
                                resource.path.parent().and_then(|p| p.parent())
                            else {
                                continue;
                            };
                            if actor_directory
                                .file_name()
                                .is_none_or(|n| n != message.actor_id.as_str())
                            {
                                continue;
                            }
                            let Some(delivery) = actor_directory.parent() else {
                                continue;
                            };
                            let Some(file_name) = resource.path.file_name() else {
                                continue;
                            };
                            let destination =
                                delivery.join(actor_id).join("artifacts").join(file_name);
                            std::fs::create_dir_all(destination.parent().unwrap())
                                .into_diagnostic()?;
                            std::fs::copy(&resource.path, &destination).into_diagnostic()?;
                            super::permissions::protect_private_path(&destination)?;
                            let old_uri = resource.uri.clone();
                            resource.actor_id = actor_id.clone();
                            resource.path = destination;
                            resource.uri =
                                format!("/v1/actors/{actor_id}/resources/{}", resource.id);
                            let mut record = serde_json::to_value(&resource).into_diagnostic()?;
                            record["path"] =
                                serde_json::to_value(&resource.path).into_diagnostic()?;
                            record["message_id"] = Value::String(result_id.clone());
                            sql(tx.execute("INSERT INTO resources(actor_id,id,record) VALUES(?1,?2,?3) ON CONFLICT(actor_id,id) DO UPDATE SET record=excluded.record",params![actor_id,resource.id,encode(&record)?]))?;
                            answer = answer.replace(&old_uri, &resource.uri);
                            attachments.push(Attachment {
                                id: resource.id,
                                name: resource.name,
                                media_type: resource.media_type,
                                size_bytes: resource.size_bytes,
                                uri: resource.uri,
                            });
                        }
                    }
                    answer = format!(
                        "Result for request {} ({}):\n{}",
                        original.id,
                        original.content.chars().take(1500).collect::<String>(),
                        answer
                    );
                    Some(insert_message(
                        &tx,
                        MessageRecord {
                            seq: 0,
                            id: result_id,
                            actor_id: actor_id.clone(),
                            source: MessageSource::Actor {
                                actor_id: message.actor_id.clone(),
                            },
                            reply_to: original.reply_to,
                            kind: MessageKind::Result,
                            in_reply_to: Some(original.id.clone()),
                            delegation_grant: if original.remote_authority.is_some() {
                                original.delegation_grant.clone()
                            } else {
                                None
                            },
                            remote_authority: original.remote_authority.clone(),
                            content: answer,
                            attachments,
                            status: MessageStatus::Queued,
                            cancel_requested: false,
                            final_generation: None,
                            started_at_ms: None,
                            finished_at_ms: None,
                            reply: None,
                            error: None,
                            created_at_ms: now(),
                            updated_at_ms: now(),
                        },
                    )?)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        self.commit(tx)?;
        Ok(routed)
    }
    pub fn request_cancel(&self, id: &str) -> Result<MessageRecord> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let mut message = message_tx(&tx, id)?;
        message.cancel_requested = true;
        message.updated_at_ms = now();
        save_message(&tx, &message)?;
        self.commit(tx)?;
        Ok(message)
    }
    pub fn register_resource(&self, resource: StoredResource) -> Result<()> {
        self.register_resource_for_message(resource, None)
    }
    pub fn register_resource_for_message(
        &self,
        resource: StoredResource,
        message_id: Option<&str>,
    ) -> Result<()> {
        let mut db = self.db.lock();
        let tx = sql(db.transaction())?;
        let mut data = serde_json::to_value(&resource).into_diagnostic()?;
        data["path"] = serde_json::to_value(&resource.path).into_diagnostic()?;
        data["message_id"] = serde_json::to_value(message_id).into_diagnostic()?;
        sql(tx.execute("INSERT INTO resources(actor_id,id,record) VALUES(?1,?2,?3) ON CONFLICT(actor_id,id) DO UPDATE SET record=excluded.record",params![resource.actor_id,resource.id,encode(&data)?]))?;
        self.commit(tx)
    }
    pub fn resource(&self, actor_id: &str, id: &str) -> Result<StoredResource> {
        decode(
            sql(self
                .db
                .lock()
                .query_row(
                    "SELECT record FROM resources WHERE actor_id=?1 AND id=?2",
                    params![actor_id, id],
                    |r| r.get(0),
                )
                .optional())?
            .ok_or_else(|| missing("Resource"))?,
        )
    }
    /// A process crash never replays an operation whose outcome may be unknown.
    pub fn recover(&self) -> Result<()> {
        let pending = {
            let db = self.db.lock();
            let mut stmt = sql(db.prepare("SELECT id FROM messages WHERE status='\"running\"'"))?;
            sql(stmt.query_map([], |r| r.get::<_, String>(0)))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .into_diagnostic()?
        };
        for id in pending {
            self.interrupt_message(
                &id,
                "Service restarted during execution; effects may have occurred",
            )?;
        }
        let active = {
            let db = self.db.lock();
            let mut stmt = sql(
                db.prepare("SELECT id FROM actors WHERE json_extract(record,'$.status')!='idle'")
            )?;
            sql(stmt.query_map([], |r| r.get::<_, String>(0)))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .into_diagnostic()?
        };
        for id in active {
            self.set_status(&id, ActorStatus::Idle)?;
        }
        Ok(())
    }
    /// Used after an individual worker has terminated, including forced stops.
    /// It never replays effects or interrupts another inbox.
    pub fn interrupt_actor(&self, actor_id: &str, reason: &str) -> Result<()> {
        if let Some(message) = self.current_message(actor_id)? {
            self.interrupt_message(&message.id, reason)?;
        }
        self.set_status(actor_id, ActorStatus::Idle)
    }
    fn interrupt_message(&self, id: &str, reason: &str) -> Result<()> {
        let message = self.message(id)?;
        let history: Vec<AgentMessage> = {
            let db = self.db.lock();
            let mut stmt =
                sql(db.prepare("SELECT record FROM history WHERE message_id=?1 ORDER BY seq"))?;
            let rows = sql(stmt.query_map([&id], |r| r.get::<_, String>(0)))?;
            rows.map(|r| decode(sql(r)?)).collect::<Result<_>>()?
        };
        let completed: std::collections::HashSet<_> = history
            .iter()
            .filter_map(|m| {
                if let AgentMessage::Tool { tool_call_id, .. } = m {
                    Some(tool_call_id.clone())
                } else {
                    None
                }
            })
            .collect();
        let missing_tools:Vec<_>=history.iter().flat_map(|m|match m {AgentMessage::AssistantToolCallProtocol{calls,..}=>calls.clone(),_=>vec![]}).filter(|call|!completed.contains(&call.id)).map(|call|AgentMessage::tool(call.id,call.name,"Execution was interrupted. Effects may have occurred. Inspect the App state before deciding whether to retry.")).collect();
        self.append_history(&message.actor_id, &message.id, &missing_tools)?;
        self.finish_message(id, MessageStatus::Interrupted, None, Some(reason))?;
        Ok(())
    }
}

#[cfg(test)]
mod outcome_tests {
    use super::*;

    #[test]
    fn database_capacity_is_not_capped_at_one_gibibyte_on_open_or_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("actors.sqlite3");
        for _ in 0..2 {
            let store = ActorStore::open(&path).unwrap();
            let db = store.db.lock();
            let pages: i64 = db
                .query_row("PRAGMA max_page_count", [], |row| row.get(0))
                .unwrap();
            let page_size: i64 = db
                .query_row("PRAGMA page_size", [], |row| row.get(0))
                .unwrap();
            assert!(pages * page_size > 1024 * 1024 * 1024);
        }
    }

    #[test]
    fn legacy_delegation_outcome_keeps_the_original_device_after_sync() {
        let stage = Uuid::new_v4();
        let mut result = ToolResultRecord {
            id: "result".into(),
            actor_id: "core".into(),
            message_id: "message".into(),
            call_id: "call".into(),
            name: "actors__send".into(),
            arguments: serde_json::json!({"stage_id":stage,"actor_id":"child","content":"private input"}),
            payload: serde_json::json!({"actor_id":"child","conversation_url":"/actors/child"}),
            model_content: "private model transcript".into(),
            summary: "summary".into(),
            error: None,
            started_at_ms: None,
            finished_at_ms: None,
        };
        let expected = format!("/stages/{stage}/actors/child");
        assert_eq!(tool_outcome(&result).unwrap()["conversation_url"], expected);
        result.payload["conversation_url"] = expected.clone().into();
        assert_eq!(tool_outcome(&result).unwrap()["conversation_url"], expected);
        result.arguments["stage_id"] = Value::Null;
        result.payload["conversation_url"] = "/actors/child".into();
        assert_eq!(
            tool_outcome(&result).unwrap()["conversation_url"],
            "/actors/child"
        );
    }

    #[test]
    fn command_activity_carries_only_a_bounded_source_fact() {
        let mut result = ToolResultRecord {
            id: "result".into(),
            actor_id: "actor".into(),
            message_id: "message".into(),
            call_id: "call".into(),
            name: "d_abcdef123456_terminal__terminal_exec".into(),
            arguments: serde_json::json!({"command":"argument command","environment":{"TOKEN":"private"}}),
            payload: serde_json::json!({"session":{"command":"cargo check","cwd":"private directory"},"output":"heavy output"}),
            model_content: "private model transcript".into(),
            summary: "summary".into(),
            error: None,
            started_at_ms: None,
            finished_at_ms: None,
        };
        assert_eq!(
            tool_outcome(&result),
            Some(serde_json::json!({"command":"cargo check"}))
        );
        result.payload["session"]["command"] = Value::String("x".repeat(2049));
        assert!(tool_outcome(&result).is_none());
        result.payload = serde_json::json!({});
        assert_eq!(
            tool_outcome(&result),
            Some(serde_json::json!({"command":"argument command"}))
        );
        result.error = Some("failed".into());
        assert!(tool_outcome(&result).is_none());
    }
}
