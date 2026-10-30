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

mod approval_appeals;
mod contacts;
pub use contacts::{ActorContact, ContactMessage, ContactPage, ContactSort};
#[cfg(test)]
#[path = "store/history_handoff_tests.rs"]
mod history_handoff_tests;
mod review_applications;
mod sudo;
mod tool_provenance;

pub struct ActorStore {
    db: Mutex<Connection>,
    changed: Arc<Notify>,
}
fn task_handoff_db(db: &Connection, actor: &str, input: &str) -> Option<TaskHandoff> {
    let current = message_tx(db, input)
        .ok()
        .filter(|message| message.actor_id == actor)?;
    Some(TaskHandoff::from_message(&current, |id| {
        message_tx(db, id).ok()
    }))
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn delivery_id(actor: &str, input: &str, call: &str) -> String {
    let hash = crate::stage::digest(format!("actor-delivery:{actor}:{input}:{call}").as_bytes());
    let bytes = hex::decode(hash).expect("SHA256 hex");
    Uuid::from_bytes(bytes[..16].try_into().expect("SHA256 prefix")).to_string()
}
fn core_requires_commit(db: &Connection) -> Result<bool> {
    let exists: bool = sql(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='core_rows')",
        [],
        |row| row.get(0),
    ))?;
    if !exists {
        return Ok(false);
    }
    // Missing mode metadata must retain the conservative shared-store contract.
    // Only installing a local Core explicitly opts out of distributed commits.
    let local: bool = sql(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM core_meta WHERE key='local_mode' AND value='1')",
        [],
        |row| row.get(0),
    ))?;
    Ok(!local)
}
fn reply_turn_delivered(db: &Connection, actor: &str, message: &str) -> Result<bool> {
    sql(db.query_row("SELECT EXISTS(SELECT 1 FROM tools WHERE actor_id=?1 AND message_id=?2 AND done=1 AND json_extract(record,'$.name')='actors__reply' AND json_extract(record,'$.error') IS NULL AND json_extract(record,'$.payload.finish_turn')=1)",params![actor,message],|row|row.get(0)))
}
fn turn_yielded(db: &Connection, actor: &str, message: &str) -> Result<bool> {
    sql(db.query_row("SELECT EXISTS(SELECT 1 FROM tools WHERE actor_id=?1 AND message_id=?2 AND done=1 AND json_extract(record,'$.name')='actors__yield' AND json_extract(record,'$.error') IS NULL AND json_extract(record,'$.payload.finish_turn')=1)", params![actor, message], |r|r.get(0)))
}
fn tool_domain_name(name: &str) -> &str {
    name.strip_prefix("d_")
        .filter(|name| {
            name.len() > 13
                && name.as_bytes()[..12].iter().all(u8::is_ascii_hexdigit)
                && name.as_bytes()[12] == b'_'
        })
        .and_then(|name| name.get(13..))
        .unwrap_or(name)
}
fn command_outcome(arguments: &Value, payload: &Value) -> Option<Value> {
    let command = payload
        .get("session")
        .and_then(|session| session.get("command"))
        .and_then(Value::as_str)
        .or_else(|| arguments.get("command").and_then(Value::as_str))?;
    // Bounded source fact only; full commands and other arguments remain in scoped details.
    (command.len() <= 2048).then(|| serde_json::json!({"command": command}))
}
fn sudo_target(name: &str, arguments: &Value) -> Option<AgentToolCall> {
    let name = tool_domain_name(name);
    if !matches!(name, "sudo__execute" | "sudo__execute_as_admin") {
        return None;
    }
    let call = AgentToolCall {
        id: String::new(),
        name: name.into(),
        arguments: arguments.clone(),
    };
    let (_, invocation) =
        crate::sudo_app::SudoApp::request(&call, std::path::PathBuf::new()).ok()?;
    (!tool_domain_name(&invocation.tool).starts_with("sudo__")).then(|| invocation.call(""))
}
fn operation_outcome(outcome: Option<Value>, target: Option<&AgentToolCall>) -> Option<Value> {
    if let Some(target) = target {
        let mut value = outcome.unwrap_or_else(|| serde_json::json!({}));
        value["operation"] = target.name.clone().into();
        Some(value)
    } else {
        outcome
    }
}
fn tool_started_outcome(call: &AgentToolCall) -> Option<Value> {
    let target = sudo_target(&call.name, &call.arguments);
    let source = target.as_ref().unwrap_or(call);
    let outcome = (tool_domain_name(&source.name) == "terminal__terminal_exec")
        .then(|| command_outcome(&source.arguments, &Value::Null))
        .flatten();
    operation_outcome(outcome, target.as_ref())
}
fn tool_outcome(result: &ToolResultRecord) -> Option<Value> {
    let target = sudo_target(&result.name, &result.arguments);
    let (name, arguments) = target
        .as_ref()
        .map(|call| (call.name.as_str(), &call.arguments))
        .unwrap_or((&result.name, &result.arguments));
    operation_outcome(
        source_outcome(name, arguments, &result.payload, result.error.is_some()),
        target.as_ref(),
    )
}
fn source_outcome(name: &str, arguments: &Value, value: &Value, failed: bool) -> Option<Value> {
    if failed {
        return (tool_domain_name(name) == "terminal__terminal_exec")
            .then(|| command_outcome(arguments, value))
            .flatten();
    }
    let payload = value.as_object()?;
    if payload
        .get("approval")
        .and_then(|value| value.get("decision"))
        .and_then(Value::as_str)
        .is_some_and(|decision| matches!(decision, "denied" | "application_required" | "approved"))
    {
        return Some(serde_json::json!({"approval": payload["approval"]}));
    }
    let domain_name = tool_domain_name(name);
    if domain_name
        .split_once("__")
        .is_some_and(|(app, _)| crate::mcp::is_mcp_id(app))
    {
        return payload
            .get("app")
            .cloned()
            .map(|app| serde_json::json!({"app":app}));
    }
    if domain_name == "terminal__terminal_exec" {
        return command_outcome(arguments, value);
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
        "actors__create"
        | "admin_actors__stop"
        | "actors__import_conversation"
        | "actors__message"
        | "actors__reply"
        | "admin_actors__archive"
        | "admin_actors__unarchive" => &[
            "id",
            "actor_id",
            "message_id",
            "name",
            "conversation_url",
            "stage_id",
            "status",
            "kind",
            "reply_to",
            "cancelled_count",
        ],
        "files__write_file" | "files__edit_file" | "files__copy_file" => {
            &["path", "created", "byte_count", "sha256"]
        }
        _ => return None,
    };
    let mut output = serde_json::Map::new();
    if payload.get("sent_message").is_some() {
        // Body stays in authorized result detail, never the incremental stream.
        output.insert("sent_message".into(), true.into());
    }
    for key in keys {
        if let Some(value) = payload.get(*key) {
            output.insert((*key).into(), value.clone());
        }
    }
    if matches!(
        domain_name,
        "actors__create"
            | "admin_actors__stop"
            | "actors__import_conversation"
            | "actors__message"
            | "admin_actors__archive"
            | "admin_actors__unarchive"
    ) {
        // Older Core results retained a host-local URL, but their explicit
        // device argument still identifies the original host after replication.
        let stage = payload
            .get("stage_id")
            .or_else(|| arguments.get("stage_id"))
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
    read_actor(tx, id)
}
fn read_actor(db: &Connection, id: &str) -> Result<ActorRecord> {
    let record = sql(db
        .query_row("SELECT record FROM actors WHERE id=?1", [id], |r| r.get(0))
        .optional())?
    .ok_or_else(|| missing("Actor"))?;
    account_actor(db, decode(record)?)
}
fn account_actor_ids_db(
    db: &Connection,
    sender: &str,
) -> Result<std::collections::HashSet<String>> {
    let bound: bool = sql(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='stage_account_actors')",
        [],
        |row| row.get(0),
    ))?;
    let query = if bound {
        "WITH RECURSIVE ownership(actor,account) AS (
            SELECT actor,account FROM stage_account_actors
            UNION ALL SELECT a.id,o.account FROM actors a JOIN ownership o ON a.parent_id=o.actor
            WHERE NOT EXISTS(SELECT 1 FROM stage_account_actors explicit WHERE explicit.actor=a.id)
        ) SELECT a.id FROM actors a LEFT JOIN ownership o ON o.actor=a.id
        WHERE o.account IS (SELECT account FROM ownership WHERE actor=?1)"
    } else {
        "SELECT id FROM actors WHERE ?1 IS NOT NULL"
    };
    let mut statement = sql(db.prepare(query))?;
    sql(sql(statement.query_map([sender], |row| row.get::<_, String>(0)))?.collect())
}
fn validate_new_actor_name_db(db: &Connection, parent: &str, name: &str) -> Result<()> {
    let name = name.trim().to_lowercase();
    if name.is_empty() {
        return Err(ActorError::Invalid("Actor name must not be empty".into()).into());
    }
    let mut names =
        sql(db.prepare("SELECT json_extract(record, '$.config.name') FROM actors WHERE id=?1"))?;
    for id in account_actor_ids_db(db, parent)? {
        let existing: String = sql(names.query_row([id], |row| row.get(0)))?;
        if existing.trim().to_lowercase() == name {
            return Err(ActorError::Conflict(
                "Actor name is already in use. List existing Actors and choose a different name"
                    .into(),
            )
            .into());
        }
    }
    Ok(())
}
fn account_actor(_db: &Connection, actor: ActorRecord) -> Result<ActorRecord> {
    Ok(actor)
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
fn save_resource(
    tx: &Transaction<'_>,
    resource: StoredResource,
    message_id: Option<&str>,
) -> Result<()> {
    let mut data = serde_json::to_value(&resource).into_diagnostic()?;
    let used:i64=sql(tx.query_row("SELECT COALESCE(SUM(json_extract(record,'$.size_bytes')),0) FROM resources WHERE NOT(actor_id=?1 AND id=?2)",params![resource.actor_id,resource.id],|r|r.get(0)))?;
    if (used as u64).saturating_add(resource.size_bytes) > 512 * 1024 * 1024 {
        return Err(ActorError::Conflict("Account resource storage budget reached".into()).into());
    }
    data["path"] = serde_json::to_value(&resource.path).into_diagnostic()?;
    data["message_id"] = serde_json::to_value(message_id).into_diagnostic()?;
    sql(tx.execute("INSERT INTO resources(actor_id,id,record) VALUES(?1,?2,?3) ON CONFLICT(actor_id,id) DO UPDATE SET record=excluded.record",params![resource.actor_id,resource.id,encode(&data)?]))?;
    Ok(())
}
fn message_tx(tx: &Connection, id: &str) -> Result<MessageRecord> {
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
    if actor_tx(tx, &message.actor_id)?.status == ActorStatus::Archived {
        return Err(ActorError::Conflict(
            "Actor is archived; unarchive it before sending messages".into(),
        )
        .into());
    }
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

// Synchronous callers retain transaction and cancellation semantics. On a
// multithreaded Tokio runtime, hand the worker's scheduler to a replacement
// BEFORE waiting for SQLite's mutex, and retain that boundary through commit.
// Wrapping only writes leaves authorization/readers able to exhaust every worker.
#[track_caller]
fn database_io<T>(operation: impl FnOnce() -> T) -> T {
    let caller = std::panic::Location::caller();
    let execute = || {
        let began = std::time::Instant::now();
        let result = operation();
        if began.elapsed() >= std::time::Duration::from_millis(250) {
            tracing::debug!(
                file = caller.file(),
                line = caller.line(),
                elapsed_ms = began.elapsed().as_millis(),
                "Slow Actor database operation"
            );
        }
        result
    };
    if tokio::runtime::Handle::try_current()
        .is_ok_and(|handle| handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
    {
        tokio::task::block_in_place(execute)
    } else {
        execute()
    }
}

impl ActorStore {
    /// Stage delivery shares the Actor database, including transaction failure semantics.
    #[track_caller]
    pub(crate) fn stage_transaction<T>(
        &self,
        apply: impl FnOnce(&Transaction<'_>) -> crate::stage::Result<T>,
    ) -> crate::stage::Result<T> {
        self.stage_transaction_with_notify(apply, true)
    }
    /// Consensus heartbeats are infrastructure writes, not Actor changes.
    /// Publishing them to Actor wakes creates a heartbeat/write feedback loop.
    #[track_caller]
    pub(crate) fn coordination_transaction<T>(
        &self,
        apply: impl FnOnce(&Transaction<'_>) -> crate::stage::Result<T>,
    ) -> crate::stage::Result<T> {
        self.stage_transaction_with_notify(apply, false)
    }
    #[track_caller]
    fn stage_transaction_with_notify<T>(
        &self,
        apply: impl FnOnce(&Transaction<'_>) -> crate::stage::Result<T>,
        notify: bool,
    ) -> crate::stage::Result<T> {
        database_io(|| {
            let mut db = self.db.lock();
            let before = db.total_changes();
            let tx = db.transaction()?;
            let value = apply(&tx)?;
            tx.commit()?;
            if notify && db.total_changes() != before {
                self.changed.notify_waiters();
            }
            Ok(value)
        })
    }
    pub fn open(path: &Path) -> Result<Self> {
        database_io(|| {
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
          CREATE INDEX IF NOT EXISTS pending_inputs ON messages(id) WHERE status IN ('\"queued\"','\"running\"');
          CREATE TABLE IF NOT EXISTS history(seq INTEGER PRIMARY KEY AUTOINCREMENT,actor_id TEXT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,message_id TEXT NOT NULL,record TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS history_actor ON history(actor_id,seq);
          CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,actor_id TEXT NOT NULL,created_at INTEGER NOT NULL,record TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS events_actor ON events(actor_id,seq);
          CREATE INDEX IF NOT EXISTS events_message ON events(actor_id,json_extract(record,'$.message_id'),seq);
          CREATE TABLE IF NOT EXISTS tools(id TEXT PRIMARY KEY,actor_id TEXT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,message_id TEXT NOT NULL,call_id TEXT NOT NULL,record TEXT NOT NULL,done INTEGER NOT NULL,UNIQUE(message_id,call_id));
          CREATE TABLE IF NOT EXISTS stage_local_tool_results(id TEXT PRIMARY KEY,record TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS resources(actor_id TEXT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,id TEXT NOT NULL,record TEXT NOT NULL,PRIMARY KEY(actor_id,id));
          CREATE TABLE IF NOT EXISTS idempotency(actor_id TEXT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,principal TEXT NOT NULL,key TEXT NOT NULL,message_id TEXT NOT NULL,PRIMARY KEY(actor_id,principal,key));
          CREATE TABLE IF NOT EXISTS approvals(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,record TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS approval_message ON approvals(message_id,seq);"))?;
            crate::persistence::set_private_file_permissions(path).into_diagnostic()?;
            let tx = sql(db.transaction())?;
            super::migrations::retire_guest_execution(&tx, now()).into_diagnostic()?;
            super::migrations::remove_actor_capability_lists(&tx).into_diagnostic()?;
            crate::sandbox::scopes::initialize(&tx)?;
            crate::stage::blob::initialize(&tx).map_err(|error| miette::miette!("{error}"))?;
            crate::stage::journal::initialize(&tx).map_err(|error| miette::miette!("{error}"))?;
            crate::stage::sync::initialize(&tx).map_err(|error| miette::miette!("{error}"))?;
            crate::stage::delivery::initialize(&tx).map_err(|error| miette::miette!("{error}"))?;
            sql(tx.commit())?;
            Ok(Self {
                db: Mutex::new(db),
                changed: Arc::new(Notify::new()),
            })
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
        database_io(|| read_actor(&self.db.lock(), id))
    }
    pub fn actors(&self) -> Result<Vec<ActorRecord>> {
        database_io(|| {
            let db = self.db.lock();
            let mut stmt = sql(db.prepare("SELECT record FROM actors ORDER BY depth,id"))?;
            let rows = sql(stmt.query_map([], |r| r.get::<_, String>(0)))?;
            rows.map(|r| account_actor(&db, decode(sql(r)?)?)).collect()
        })
    }
    pub(crate) fn actor_page(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ActorSummary>> {
        database_io(|| {
            let db = self.db.lock();
            let mut stmt =
                sql(db.prepare("SELECT record FROM actors WHERE id>?1 ORDER BY id LIMIT ?2"))?;
            let rows = sql(stmt.query_map(
                params![after.unwrap_or(""), limit.clamp(1, 101) as i64],
                |r| r.get::<_, String>(0),
            ))?;
            rows.map(|r| Ok(decode::<ActorRecord>(sql(r)?)?.summary()))
                .collect()
        })
    }
    pub(crate) fn actor_models(&self) -> Result<Vec<String>> {
        database_io(|| {
            let db = self.db.lock();
            let mut stmt = sql(db.prepare("SELECT DISTINCT json_extract(record,'$.config.model') FROM actors WHERE json_extract(record,'$.config.model') IS NOT NULL"))?;
            sql(stmt.query_map([], |r| r.get(0)))?
                .collect::<rusqlite::Result<_>>()
                .into_diagnostic()
        })
    }
    pub(crate) fn set_model_preference(
        &self,
        id: &str,
        model: Option<String>,
    ) -> Result<ActorRecord> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut actor = actor_tx(&tx, id)?;
            actor.config.model = model;
            actor.updated_at_ms = now();
            save_actor(&tx, &actor)?;
            self.commit(tx)?;
            Ok(actor)
        })
    }
    /// Only removed model preferences change; identities, grants and history stay
    /// intact. Null selects the account default, or waits for model setup.
    pub(crate) fn reset_model_preferences(&self, removed: &[String]) -> Result<()> {
        database_io(|| {
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
        })
    }
    #[cfg(test)]
    pub(crate) fn actor_count(&self) -> Result<i64> {
        database_io(|| {
            sql(self
                .db
                .lock()
                .query_row("SELECT COUNT(*) FROM actors", [], |r| r.get(0)))
        })
    }
    /// Every queued entry activates a worker: an immediate message whose target is
    /// still running is inserted at that target's boundary, otherwise it is claimed.
    /// Query the mailbox index without decoding records.
    pub(crate) fn has_queued_input(&self, actor: &str) -> Result<bool> {
        database_io(|| {
            sql(self.db.lock().query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE actor_id=?1 AND status='\"queued\"')",
                [actor],
                |r| r.get(0),
            ))
        })
    }
    pub(crate) fn queued_actor_ids(&self) -> Result<Vec<String>> {
        database_io(|| {
            let db = self.db.lock();
            let mut stmt = sql(db.prepare(
            "SELECT DISTINCT actor_id FROM messages WHERE status='\"queued\"' ORDER BY actor_id",
        ))?;
            sql(stmt.query_map([], |r| r.get(0)))?
                .collect::<rusqlite::Result<_>>()
                .into_diagnostic()
        })
    }
    pub fn save_actor(&self, actor: &ActorRecord) -> Result<()> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            save_actor(&tx, actor)?;
            self.commit(tx)
        })
    }
    pub(crate) fn account_actor_ids(
        &self,
        sender: &str,
    ) -> Result<std::collections::HashSet<String>> {
        database_io(|| account_actor_ids_db(&self.db.lock(), sender))
    }
    pub(crate) fn validate_new_actor_name(&self, parent: &str, name: &str) -> Result<()> {
        database_io(|| validate_new_actor_name_db(&self.db.lock(), parent, name))
    }
    pub(crate) fn save_created_actor(
        &self,
        actor: &ActorRecord,
        resource: Option<StoredResource>,
        initial_scopes: &[SandboxScope],
        source_message: Option<&str>,
    ) -> Result<()> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            if let Some(source) = source_message {
                let input = message_tx(&tx, source)?;
                if actor.parent_id.as_deref() != Some(input.actor_id.as_str())
                    || input.status != MessageStatus::Running
                    || input.cancel_requested
                {
                    return Err(ActorError::Forbidden(
                        "Actor creation authority is no longer valid".into(),
                    )
                    .into());
                }
            }
            if initial_scopes
                .iter()
                .any(|scope| scope.active_filesystem().is_none())
            {
                return Err(
                    ActorError::Forbidden("Initial authorization target changed".into()).into(),
                );
            }
            let parent = actor
                .parent_id
                .as_deref()
                .ok_or_else(|| ActorError::Invalid("Created Actor requires a parent".into()))?;
            validate_new_actor_name_db(&tx, parent, &actor.config.name)?;
            save_actor(&tx, actor)?;
            if let Some(resource) = resource {
                save_resource(&tx, resource, None)?;
            }
            if let Some(parent) = &actor.parent_id {
                crate::stage::ledger::inherit_account_ownership(&tx, parent, &actor.id)
                    .map_err(|error| miette::miette!("{error}"))?;
            }
            crate::sandbox::scopes::remember(&tx, initial_scopes)?;
            self.commit(tx)
        })
    }
    pub fn set_status(&self, id: &str, status: ActorStatus) -> Result<()> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut actor = actor_tx(&tx, id)?;
            // Worker shutdown/recovery cannot undo an explicit archive.
            if actor.status == ActorStatus::Archived {
                return Ok(());
            }
            if actor.status != status {
                actor.status = status;
                actor.updated_at_ms = now();
                save_actor(&tx, &actor)?;
            }
            self.commit(tx)
        })
    }
    pub(crate) fn set_archived(&self, id: &str, archived: bool) -> Result<ActorRecord> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut actor = actor_tx(&tx, id)?;
            if id == crate::stage::shared::ACTOR_ID {
                return Err(ActorError::Conflict("Core cannot be archived".into()).into());
            }
            if archived {
                // Archiving discards queued inbox inputs (user, result and receipt
                // messages alike) so they never run after an unarchive. Running
                // inputs are left to finish; only queued rows are removed.
                sql(tx.execute(
                    "DELETE FROM messages WHERE actor_id=?1 AND status='\"queued\"'",
                    [id],
                ))?;
            }
            let status = if archived {
                ActorStatus::Archived
            } else {
                ActorStatus::Idle
            };
            if archived != (actor.status == ActorStatus::Archived) {
                actor.status = status;
                actor.updated_at_ms = now().max(actor.updated_at_ms + 1);
                save_actor(&tx, &actor)?;
            }
            self.commit(tx)?;
            Ok(actor)
        })
    }
    pub(crate) fn update_token_estimate(
        &self,
        id: &str,
        binding: &str,
        baseline: crate::context_budget::TokenEstimateBaseline,
    ) -> Result<()> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut actor = actor_tx(&tx, id)?;
            actor.context.model_binding = binding.to_owned();
            actor.context.token_estimate = baseline;
            save_actor(&tx, &actor)?;
            self.commit(tx)
        })
    }
    /// Optional, runtime-owned task lookup; never infer links from message prose.
    pub(crate) fn task_handoff(&self, actor: &str, input: &str) -> Option<TaskHandoff> {
        database_io(|| {
            let saved = self
                .actor(actor)?
                .context
                .anchor
                .filter(|anchor| anchor.message_id == input)
                .and_then(|anchor| anchor.handoff);
            let db = self.db.lock();
            Ok::<_, miette::Report>(saved.or_else(|| task_handoff_db(&db, actor, input)))
        })
        .ok()
        .flatten()
    }
    pub(crate) fn reset_model_context(
        &self,
        id: &str,
        message: &str,
        checkpoint: &str,
    ) -> Result<AgentMessage> {
        if checkpoint.trim().is_empty() {
            return Err(miette::miette!("Context handoff note is empty"));
        }
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut actor = actor_tx(&tx, id)?;
            let mut handoff = task_handoff_db(&tx, id, message).unwrap_or_else(|| TaskHandoff {
                task_message_id: message.to_owned(),
                checkpoint: None,
                related_message_ids: vec![],
            });
            handoff.checkpoint = Some(checkpoint.to_owned());
            let anchor = HistoryAnchor {
                message_id: message.to_owned(),
                reset_id: Uuid::new_v4(),
                handoff: Some(handoff),
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
        })
    }
    pub(crate) fn record_model_usage(
        &self,
        id: &str,
        model: &super::ModelUsage,
        delta: crate::core::TokenUsage,
        window: Option<i64>,
    ) -> Result<()> {
        database_io(|| {
            if delta.is_zero() {
                return Ok(());
            }
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut actor = actor_tx(&tx, id)?;
            actor.context.usage.append_last_usage(delta.clone());
            if window.is_some() {
                actor.context.usage.model_context_window = window;
            }
            let models = &mut actor.context.model_usage;
            let index = models
                .iter()
                .position(|entry| {
                    entry.model == model.model
                        && entry.provider == model.provider
                        && entry.model_id == model.model_id
                })
                .unwrap_or_else(|| {
                    models.push(model.clone());
                    models.len() - 1
                });
            models[index].usage.append_last_usage(delta);
            models[index].usage.model_context_window = window;
            save_actor(&tx, &actor)?;
            self.commit(tx)
        })
    }
    pub(crate) fn context_start(&self, id: &str) -> Result<i64> {
        database_io(|| {
            let actor = self.actor(id)?;
            let Some(anchor) = actor.context.anchor.filter(|anchor| {
                anchor
                    .handoff
                    .as_ref()
                    .and_then(|note| note.checkpoint.as_ref())
                    .is_some()
            }) else {
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
        })
    }
    /// Bounded locators for direct same-Actor Progress/Result inputs only. New
    /// Requests are independent even if related; this is not a complete event tree.
    pub(crate) fn related_task_inputs(
        &self,
        actor: &str,
        task: &str,
        after: i64,
        limit: usize,
    ) -> Result<Page<Value>> {
        database_io(|| {
            let db = self.db.lock();
            let limit = limit.clamp(1, 32);
            let mut statement = sql(db.prepare("SELECT seq,record FROM messages WHERE actor_id=?1 AND seq>?2 AND id<>?3 AND json_extract(record,'$.kind') IN ('progress','result') AND json_extract(record,'$.in_reply_to')=?3 ORDER BY seq LIMIT ?4"))?;
            let records = sql(statement
                .query_map(params![actor, after, task, (limit + 1) as i64], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                }))?
            .map(|row| {
                let (seq, record) = sql(row)?;
                Ok((seq, decode::<MessageRecord>(record)?))
            })
            .collect::<Result<Vec<_>>>()?;
            let next_cursor = (records.len() > limit).then(|| records[limit - 1].0);
            let items = records.into_iter().take(limit).map(|(seq, input)| serde_json::json!({"sequence":seq,"message_id":input.id,"kind":input.kind})).collect();
            Ok(Page { items, next_cursor })
        })
    }

    /// Pagination bounds I/O, never the number of retained messages or steps.
    pub(crate) fn archive_page(
        &self,
        actor: &str,
        after: i64,
        message: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(i64, String, AgentMessage)>> {
        Ok(self
            .history_page(actor, after, message, limit)?
            .into_iter()
            .take_while(|(seq, _, _)| {
                actor != crate::stage::shared::ACTOR_ID || *seq < 4503599627370496
            })
            .collect())
    }
    pub(crate) fn history_page(
        &self,
        actor: &str,
        after: i64,
        message: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(i64, String, AgentMessage)>> {
        database_io(|| {
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
        })
    }
    pub fn enqueue(&self, message: MessageRecord, key: Option<&str>) -> Result<MessageRecord> {
        database_io(|| {
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
        })
    }
    pub fn claim_next(&self, actor_id: &str) -> Result<Option<MessageRecord>> {
        self.claim_input(actor_id, None)
    }
    /// Publish a resumed input even when it predates the client's history page.
    /// This advances no effects and does not change its authority or start time.
    pub(crate) fn resume_input(&self, actor: &str, id: &str) -> Result<()> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut input = message_tx(&tx, id)?;
            if input.actor_id != actor
                || input.status != MessageStatus::Running
                || input.cancel_requested
            {
                return Err(ActorError::Forbidden("Input cannot resume".into()).into());
            }
            input.updated_at_ms = now().max(input.updated_at_ms + 1);
            save_message(&tx, &input)?;
            self.commit(tx)
        })
    }
    /// Claim at a safe boundary of this Actor's current execution. Other open
    /// requests retain their authority and completion state while suspended.
    pub(crate) fn claim_incoming(
        &self,
        actor_id: &str,
        active: &str,
    ) -> Result<Option<MessageRecord>> {
        self.claim_input(actor_id, Some(active))
    }
    fn claim_input(&self, actor_id: &str, active: Option<&str>) -> Result<Option<MessageRecord>> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut actor = actor_tx(&tx, actor_id)?;
            if actor.status == ActorStatus::Archived {
                return Ok(None);
            }
            if let Some(active) = active {
                let current = message_tx(&tx, active)?;
                if current.actor_id != actor_id
                    || current.status != MessageStatus::Running
                    || current.cancel_requested
                {
                    return Ok(None);
                }
            } else if actor.status != ActorStatus::Idle {
                return Ok(None);
            }
            // A replica/lifecycle transition can leave Actor status idle while its
            // interrupted input is still recorded as running. Recovery must repair
            // that input before the serial inbox claims any later request.
            if active.is_none() && sql(tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE actor_id=?1 AND status='\"running\"')",
                [actor_id],
                |r| r.get::<_, bool>(0),
            ))? {
                return Ok(None);
            }
            let shared = actor_id == crate::stage::shared::ACTOR_ID
            && sql(tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='core_order')", [], |r| r.get::<_,bool>(0)))?;
            let query = if shared {
                "SELECT m.id FROM messages m LEFT JOIN core_order o ON o.entity='messages' AND o.remote_key=m.id WHERE m.actor_id=?1 AND m.status='\"queued\"' ORDER BY o.ordinal IS NULL,o.ordinal,m.seq LIMIT 1"
            } else {
                "SELECT id FROM messages WHERE actor_id=?1 AND status='\"queued\"' ORDER BY seq LIMIT 1"
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
        })
    }
    pub fn message(&self, id: &str) -> Result<MessageRecord> {
        database_io(|| {
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
        })
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
        database_io(|| {
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
                return Err(ActorError::Conflict(
                    "The input changed or has already started".into(),
                )
                .into());
            }
            if withdraw {
                message.status = MessageStatus::Cancelled;
                message.cancel_requested = true;
                message.error = Some("Queued input withdrawn before execution".into());
                message.finished_at_ms = Some(now());
            } else {
                if content.len() > 64 * 1024 || content.trim().is_empty() && attachments.is_empty()
                {
                    return Err(ActorError::Invalid("Invalid queued input".into()).into());
                }
                message.content = content;
                message.attachments = attachments;
            }
            message.updated_at_ms = now().max(message.updated_at_ms + 1);
            save_message(&tx, &message)?;
            self.commit(tx)?;
            Ok(message)
        })
    }
    pub fn clear_queued_users(&self, actor: &str) -> Result<usize> {
        database_io(|| {
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
        })
    }
    /// Read a bounded page of inputs not yet received by the Actor.
    pub fn queued_messages(
        &self,
        actor: &str,
        after: Option<i64>,
        limit: usize,
    ) -> Result<Page<MessageRecord>> {
        database_io(|| {
            let db = self.db.lock();
            let limit = limit.clamp(1, 100);
            let shared = actor == crate::stage::shared::ACTOR_ID && sql(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='core_order')",[],|r|r.get::<_,bool>(0)))?;
            let query = if shared {
                "SELECT m.record,COALESCE(o.ordinal,4503599627370496+m.seq) AS position FROM messages m LEFT JOIN core_order o ON o.entity='messages' AND o.remote_key=m.id WHERE m.actor_id=?1 AND m.status='\"queued\"' AND json_extract(m.record,'$.kind')!='delivery' AND position>?2 ORDER BY position LIMIT ?3"
            } else {
                "SELECT record,seq FROM messages WHERE actor_id=?1 AND status='\"queued\"' AND json_extract(record,'$.kind')!='delivery' AND seq>?2 ORDER BY seq LIMIT ?3"
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
        })
    }
    pub fn running_message(&self, actor: &str) -> Result<Option<MessageRecord>> {
        database_io(|| {
            let db = self.db.lock();
            let value:Option<String>=sql(db.query_row("SELECT record FROM messages WHERE actor_id=?1 AND status='\"running\"' ORDER BY seq DESC LIMIT 1",[actor],|r|r.get(0)).optional())?;
            value.map(decode).transpose()
        })
    }
    pub fn message_events(
        &self,
        actor: &str,
        message: &str,
        after: i64,
        limit: usize,
    ) -> Result<Page<EventRecord>> {
        database_io(|| {
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
        })
    }
    pub fn current_message(&self, actor_id: &str) -> Result<Option<MessageRecord>> {
        database_io(|| {
            let row=sql(self.db.lock().query_row("SELECT record FROM messages WHERE actor_id=?1 AND status='\"running\"' ORDER BY seq DESC LIMIT 1",[actor_id],|r|r.get::<_,String>(0)).optional())?;
            row.map(decode).transpose()
        })
    }
    pub fn messages(
        &self,
        id: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Page<MessageRecord>> {
        database_io(|| {
            let limit = limit.clamp(1, 100);
            let db = self.db.lock();
            let shared = id == crate::stage::shared::ACTOR_ID
            && sql(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='core_order')", [], |r| r.get::<_,bool>(0)))?;
            let query = if shared {
                "SELECT m.record,COALESCE(o.ordinal,4503599627370496+m.seq) AS position FROM messages m LEFT JOIN core_order o ON o.entity='messages' AND o.remote_key=m.id WHERE m.actor_id=?1 AND (json_extract(m.record,'$.kind')!='delivery' OR EXISTS(SELECT 1 FROM core_meta WHERE key='local_mode' AND value='1') OR EXISTS(SELECT 1 FROM core_rows r WHERE r.entity='messages' AND r.remote_key=m.id)) AND position<?2 ORDER BY position DESC LIMIT ?3"
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
        })
    }
    pub fn search_messages(
        &self,
        actor_id: &str,
        query: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Page<MessageRecord>> {
        database_io(|| {
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
            let mut items: Vec<MessageRecord> =
                rows.map(|r| decode(sql(r)?)).collect::<Result<_>>()?;
            let next_cursor = if items.len() > limit {
                items.pop();
                items.last().map(|message| message.seq)
            } else {
                None
            };
            items.reverse();
            Ok(Page { items, next_cursor })
        })
    }
    pub fn append_history(
        &self,
        actor_id: &str,
        message_id: &str,
        messages: &[AgentMessage],
    ) -> Result<()> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            for message in messages {
                sql(tx.execute(
                    "INSERT INTO history(actor_id,message_id,record) VALUES(?1,?2,?3)",
                    params![actor_id, message_id, encode(message)?],
                ))?;
            }
            self.commit(tx)
        })
    }
    pub(crate) fn receive_notice(
        &self,
        source: crate::stage::StageId,
        record: &EventRecord,
    ) -> Result<()> {
        database_io(|| {
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
                    *approval_id = qualify(approval_id)
                }
                ActorEvent::ApprovalResolved { approval_id, .. } => {
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
        })
    }
    pub(crate) fn has_history_for_message(&self, actor: &str, message: &str) -> Result<bool> {
        database_io(|| {
            sql(self.db.lock().query_row(
                "SELECT EXISTS(SELECT 1 FROM history WHERE actor_id=?1 AND message_id=?2)",
                params![actor, message],
                |r| r.get(0),
            ))
        })
    }
    pub fn history(&self, actor_id: &str, limit: usize) -> Result<Vec<AgentMessage>> {
        database_io(|| {
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
        })
    }
    pub fn append_event(&self, actor_id: &str, value: ActorEvent) -> Result<i64> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let seq = event(&tx, actor_id, value)?;
            self.commit(tx)?;
            Ok(seq)
        })
    }
    pub fn events(
        &self,
        after: i64,
        actor: Option<&str>,
        catalog: bool,
        limit: usize,
    ) -> Result<Vec<EventRecord>> {
        database_io(|| {
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
        })
    }
    pub fn event_cursor(&self) -> Result<i64> {
        database_io(|| {
            sql(self
                .db
                .lock()
                .query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| r.get(0)))
        })
    }

    pub fn create_approval(&self, mut approval: ApprovalRecord) -> Result<ApprovalRecord> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let input = message_tx(&tx, &approval.message_id)?;
            if input.status != MessageStatus::Running || input.cancel_requested {
                return Err(
                    ActorError::Conflict("Approval request is no longer active".into()).into(),
                );
            }
            if approval.sudo.is_some() {
                if approval.requested != crate::sandbox::SandboxScopeRequest::default()
                    || !approval.scopes.is_empty()
                    || approval.execution_claim.is_some()
                {
                    return Err(ActorError::Invalid(
                        "Sudo reviews exactly one invocation and cannot remember filesystem scopes"
                            .into(),
                    )
                    .into());
                }
                let existing: Option<String> = sql(tx.query_row(
                    "SELECT record FROM approvals WHERE message_id=?1 AND json_extract(record,'$.call.id')=?2 AND json_type(record,'$.sudo')='object' LIMIT 1",
                    params![approval.message_id,approval.call.id],|row|row.get(0)).optional())?;
                if let Some(existing) = existing {
                    let existing: ApprovalRecord = decode(existing)?;
                    if existing.sudo != approval.sudo || existing.call != approval.call {
                        return Err(ActorError::Conflict(
                            "Sudo incident target is immutable".into(),
                        )
                        .into());
                    }
                    return Ok(existing);
                }
            }
            if approval.sudo.is_none()
                && let Some(existing) = review_applications::sandbox_application(
                    &tx,
                    &approval.message_id,
                    &approval.call.id,
                    &approval.requested,
                )?
            {
                return Ok(existing);
            }
            approval.previous_denials = review_applications::overlapping_denials(
                &tx,
                &approval.message_id,
                &approval.call.id,
                &approval.scopes,
            )?;
            if !approval.previous_denials.is_empty() {
                approval.decision = ApprovalDecision::ApplicationRequired;
                approval.reason = "The requested scope overlaps an immutable refusal for this source operation. Automatic review is not repeated; apply to Core for exactly this scope.".into();
            }
            sql(tx.execute(
                "INSERT INTO approvals(id,message_id,record) VALUES(?1,?2,'')",
                params![approval.id, approval.message_id],
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
            if approval.decision == ApprovalDecision::Pending {
                event(
                    &tx,
                    &message_tx(&tx, &approval.message_id)?.actor_id,
                    ActorEvent::ApprovalRequested {
                        approval_id: approval.id.clone(),
                        message_id: approval.message_id.clone(),
                        call_id: approval.call.id.clone(),
                        name: approval.call.name.clone(),
                    },
                )?;
            }
            self.commit(tx)?;
            Ok(approval)
        })
    }
    pub fn approval(&self, id: &str) -> Result<ApprovalRecord> {
        database_io(|| {
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
        })
    }
    pub(crate) fn native_approval(&self, id: &str) -> Result<bool> {
        database_io(|| {
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
        })
    }
    pub fn approvals(
        &self,
        actor: Option<&str>,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Page<ApprovalRecord>> {
        self.approval_page(actor, before, limit)
    }
    fn approval_page(
        &self,
        actor: Option<&str>,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Page<ApprovalRecord>> {
        database_io(|| {
            let limit = limit.clamp(1, 100);
            let db = self.db.lock();
            let mut stmt=sql(db.prepare("SELECT record FROM approvals WHERE (?1 IS NULL OR message_id IN (SELECT id FROM messages WHERE actor_id=?1)) AND seq<?2 ORDER BY seq DESC LIMIT ?3"))?;
            let rows = sql(stmt.query_map(
                params![actor, before.unwrap_or(i64::MAX), (limit + 1) as i64],
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
        })
    }
    pub fn resolve_approval(
        &self,
        id: &str,
        decision: ApprovalDecision,
        reason: &str,
    ) -> Result<ApprovalRecord> {
        database_io(|| {
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
                return Err(
                    ActorError::Conflict("Approval request is no longer active".into()).into(),
                );
            }
            approval.decision = decision;
            if decision == ApprovalDecision::Approved {
                crate::sandbox::scopes::remember(&tx, &approval.scopes)?;
            }
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
                &message_tx(&tx, &approval.message_id)?.actor_id,
                ActorEvent::ApprovalResolved {
                    approval_id: approval.id.clone(),
                    message_id: approval.message_id.clone(),
                    decision,
                    reason: approval.reason.clone(),
                },
            )?;
            self.commit(tx)?;
            Ok(approval)
        })
    }
    pub fn tool_started(
        &self,
        actor_id: &str,
        message_id: &str,
        call: &AgentToolCall,
    ) -> Result<String> {
        self.tool_started_with_authority(actor_id, message_id, call, false, true)
    }
    pub(crate) fn tool_started_with_authority(
        &self,
        actor_id: &str,
        message_id: &str,
        call: &AgentToolCall,
        read_only: bool,
        native: bool,
    ) -> Result<String> {
        database_io(|| {
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
                output_schema: None,
                model_content: String::new(),
                summary: String::new(),
                error: None,
                started_at_ms: Some(now()),
                finished_at_ms: None,
            };
            sql(tx.execute("INSERT INTO tools(id,actor_id,message_id,call_id,record,done) VALUES(?1,?2,?3,?4,?5,0)",params![result.id,actor_id,message_id,call.id,encode(&result)?]))?;
            if native {
                tool_provenance::insert(&tx, &result, read_only, None)?;
            }
            event(
                &tx,
                actor_id,
                ActorEvent::ToolStarted {
                    message_id: message_id.into(),
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    outcome: tool_started_outcome(call),
                },
            )?;
            self.commit(tx)?;
            Ok(result.id)
        })
    }
    pub(crate) fn terminal_output(
        &self,
        actor: &str,
        message: &str,
        call: &str,
        session: &crate::terminal_app::TerminalSessionState,
        delta: &str,
    ) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let input = message_tx(&tx, message)?;
            if input.actor_id != actor
                || input.status != MessageStatus::Running
                || input.cancel_requested
            {
                return Err(ActorError::Forbidden(
                    "Terminal output belongs to an inactive input".into(),
                )
                .into());
            }
            let record: Option<String> = sql(tx.query_row(
                "SELECT record FROM tools WHERE actor_id=?1 AND message_id=?2 AND call_id=?3 AND done=0",
                params![actor,message,call], |row| row.get(0),
            ).optional())?;
            let Some(record) = record else {
                return Ok(());
            };
            let mut result: ToolResultRecord = decode(record)?;
            let mut output = result
                .payload
                .get("captured_output")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            output.push_str(delta);
            let mut start = output.len().saturating_sub(64 * 1024);
            while !output.is_char_boundary(start) {
                start += 1;
            }
            output.drain(..start);
            result.payload = serde_json::json!({"session":session,"captured_output":output,"running":session.status=="running"});
            result.output_schema =
                Some(schemars::schema_for!(crate::terminal_app::TerminalToolResult).to_value());
            sql(tx.execute(
                "UPDATE tools SET record=?2 WHERE id=?1 AND done=0",
                params![result.id, encode(&result)?],
            ))?;
            event(
                &tx,
                actor,
                ActorEvent::ToolUpdated {
                    message_id: message.into(),
                    call_id: call.into(),
                    result_id: result.id,
                },
            )?;
            self.commit(tx)
        })
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
        self.tool_finished_with_reference(
            actor_id,
            message_id,
            call,
            payload,
            model_content,
            summary,
            error,
            None,
            None,
        )
    }
    /// Only the common App-result adapter can certify a completed native result.
    /// Recovery and copied observations use tool_finished and cannot certify one.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn tool_finished_with_reference(
        &self,
        actor_id: &str,
        message_id: &str,
        call: &AgentToolCall,
        payload: Value,
        model_content: &str,
        summary: &str,
        error: Option<&str>,
        native_result_id: Option<&str>,
        output_schema: Option<Value>,
    ) -> Result<String> {
        database_io(|| {
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
                    output_schema: None,
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
            result.output_schema = output_schema;
            result.model_content = model_content.into();
            result.summary = summary.chars().take(1000).collect();
            result.error = error.map(str::to_owned);
            result.finished_at_ms = Some(now());
            sql(tx.execute("INSERT INTO tools(id,actor_id,message_id,call_id,record,done) VALUES(?1,?2,?3,?4,?5,1) ON CONFLICT(message_id,call_id) DO UPDATE SET record=excluded.record,done=1",params![result.id,actor_id,message_id,call.id,encode(&result)?]))?;
            if let Some(id) = native_result_id {
                tool_provenance::complete(&tx, &result, id)?;
            }
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
                    outcome: tool_outcome(&result),
                },
            )?;
            self.commit(tx)?;
            Ok(result.id)
        })
    }
    pub fn reply_turn_delivered(&self, actor: &str, message: &str) -> Result<bool> {
        database_io(|| reply_turn_delivered(&self.db.lock(), actor, message))
    }
    pub(crate) fn turn_yielded(&self, actor: &str, message: &str) -> Result<bool> {
        database_io(|| turn_yielded(&self.db.lock(), actor, message))
    }
    pub fn tool_result(&self, id: &str) -> Result<ToolResultRecord> {
        database_io(|| {
            decode(
                sql(self
                    .db
                    .lock()
                    .query_row("SELECT record FROM tools WHERE id=?1", [id], |r| r.get(0))
                    .optional())?
                .ok_or_else(|| missing("Tool result"))?,
            )
        })
    }

    pub fn tool_summaries(
        &self,
        actor: &str,
        message: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Page<ToolResultSummary>> {
        database_io(|| {
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
        })
    }
    pub fn delivery_receipt(
        &self,
        actor: &str,
        input: &str,
        call: &str,
    ) -> Result<Option<MessageRecord>> {
        database_io(|| {
            let db = self.db.lock();
            let shared = core_requires_commit(&db)?;
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
        })
    }
    /// Publish a committed account delivery. Repeating recovery cannot emit a
    /// duplicate event or enqueue the same remote delivery twice.
    pub(crate) fn publish_delivery(&self, id: &str) -> Result<()> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let message = message_tx(&tx, id)?;
            if message.actor_id != crate::stage::shared::ACTOR_ID
                || message.kind != MessageKind::Delivery
            {
                return Ok(());
            }
            let shared = core_requires_commit(&tx)?;
            if shared {
                let committed:bool=sql(tx.query_row("SELECT EXISTS(SELECT 1 FROM core_rows WHERE entity='messages' AND remote_key=?1)",[id],|r|r.get(0)))?;
                if !committed {
                    return Err(ActorError::Unavailable("Delivery has not committed".into()).into());
                }
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
        })
    }
    /// Idempotent, origin-bound outward delivery. It never consumes or finishes
    /// the originating input, and a collaborator update is its own later input.
    pub fn deliver(
        &self,
        actor: &str,
        input: &str,
        call: &str,
        content: &str,
        finish_turn: bool,
    ) -> Result<MessageRecord> {
        database_io(|| {
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
                return Err(
                    ActorError::Forbidden("Delivery input is no longer active".into()).into(),
                );
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
                delivery.kind = if finish_turn {
                    MessageKind::Result
                } else {
                    MessageKind::Progress
                };
                delivery.in_reply_to = Some(original.id);
                delivery.content = if finish_turn {
                    format!("Result for delegated request {}:\n{}", source.id, content)
                } else {
                    format!(
                        "Progress for delegated request {} (still running):\n{}",
                        source.id, content
                    )
                };
                delivery.reply = None;
                delivery.status = MessageStatus::Queued;
                delivery.finished_at_ms = None;
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
        })
    }

    pub fn finish_message(
        &self,
        id: &str,
        status: MessageStatus,
        reply: Option<&str>,
        error: Option<&str>,
    ) -> Result<Option<MessageRecord>> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut message = message_tx(&tx, id)?;
            // Explicit Core deliveries are independent conversation messages. A
            // later final model answer closes the input without duplicating them.
            let explicit: bool = sql(tx.query_row("SELECT EXISTS(SELECT 1 FROM messages m WHERE actor_id=?1 AND json_extract(record,'$.kind')='delivery' AND json_extract(record,'$.in_reply_to')=?2 AND EXISTS(SELECT 1 FROM events e WHERE e.actor_id=m.actor_id AND json_extract(e.record,'$.type')='message_added' AND json_extract(e.record,'$.message_id')=m.id))", params![message.actor_id,id], |r|r.get(0)))?;
            let yielded =
                status == MessageStatus::Completed && turn_yielded(&tx, &message.actor_id, id)?;
            let replied = status == MessageStatus::Completed
                && reply_turn_delivered(&tx, &message.actor_id, id)?;
            let reply = if replied {
                Some("")
            } else if yielded {
                // Recovery can reach here with a committed empty assistant boundary.
                // The successful yield observation, not replayed model text, decides
                // whether this input has an outward result.
                None
            } else if message.actor_id == crate::stage::shared::ACTOR_ID
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
            // Immediate messages still queued here keep waiting: without a live
            // target they are claimed and run exactly like any other inbox message.
            let pending = {
                let mut stmt=sql(tx.prepare("SELECT record FROM approvals WHERE message_id=?1 AND json_extract(record,'$.decision') IN ('pending','application_required')"))?;
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
                    &message_tx(&tx, &approval.message_id)?.actor_id,
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
                if actor.status != ActorStatus::Archived {
                    let running: bool = sql(tx.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE actor_id=?1 AND status='\"running\"')", [&message.actor_id], |row| row.get(0)))?;
                    actor.status = if running {
                        ActorStatus::Running
                    } else {
                        ActorStatus::Idle
                    };
                    actor.updated_at_ms = now();
                    save_actor(&tx, &actor)?;
                }
            }
            // A deliberate yield closes only this inbox input. No placeholder result
            // may wake the requester or escape as an empty remote delivery.
            let routed = if (yielded
                || (replied && matches!(message.reply_to, ReplyTarget::Actor { .. })))
                && status == MessageStatus::Completed
            {
                None
            } else if let ReplyTarget::Actor {
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
                                let mut record =
                                    serde_json::to_value(&resource).into_diagnostic()?;
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
        })
    }
    pub fn request_cancel(&self, id: &str) -> Result<MessageRecord> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut message = message_tx(&tx, id)?;
            message.cancel_requested = true;
            message.updated_at_ms = now();
            save_message(&tx, &message)?;
            self.commit(tx)?;
            Ok(message)
        })
    }
    pub(crate) fn request_cancel_actor(&self, actor: &str) -> Result<Vec<String>> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            actor_tx(&tx, actor)?;
            let ids = {
                let mut query = sql(tx.prepare("SELECT id FROM messages WHERE actor_id=?1 AND status IN ('\"queued\"','\"running\"') ORDER BY seq"))?;
                sql(query.query_map([actor], |row| row.get::<_, String>(0)))?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .into_diagnostic()?
            };
            for id in &ids {
                let mut message = message_tx(&tx, id)?;
                message.cancel_requested = true;
                message.updated_at_ms = now();
                save_message(&tx, &message)?;
            }
            self.commit(tx)?;
            Ok(ids)
        })
    }

    pub(crate) fn settle_cancelled_inputs(&self, ids: &[String]) -> Result<()> {
        for id in ids {
            let message = self.message(id)?;
            if message.cancel_requested
                && matches!(
                    message.status,
                    MessageStatus::Running | MessageStatus::Queued
                )
            {
                self.interrupt_message(
                    id,
                    "Actor stopped; completed effects were not rolled back",
                )?;
            }
        }
        Ok(())
    }
    pub fn register_resource(&self, resource: StoredResource) -> Result<()> {
        self.register_resource_for_message(resource, None)
    }
    pub fn register_resource_for_message(
        &self,
        resource: StoredResource,
        message_id: Option<&str>,
    ) -> Result<()> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            save_resource(&tx, resource, message_id)?;
            self.commit(tx)
        })
    }
    pub fn resource(&self, actor_id: &str, id: &str) -> Result<StoredResource> {
        database_io(|| {
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
        })
    }
    /// A process crash never replays an operation whose outcome may be unknown.
    pub fn recover(&self) -> Result<()> {
        database_io(|| {
            let pending = {
                let db = self.db.lock();
                let mut stmt =
                    sql(db.prepare("SELECT id FROM messages WHERE status='\"running\"'"))?;
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
                let mut stmt = sql(db.prepare(
                    "SELECT id FROM actors WHERE json_extract(record,'$.status')='running'",
                ))?;
                sql(stmt.query_map([], |r| r.get::<_, String>(0)))?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .into_diagnostic()?
            };
            for id in active {
                self.set_status(&id, ActorStatus::Idle)?;
            }
            Ok(())
        })
    }
    /// Used after an individual worker has terminated, including forced stops.
    /// It never replays effects or interrupts another inbox.
    pub fn interrupt_actor(&self, actor_id: &str, reason: &str) -> Result<()> {
        while let Some(message) = self.current_message(actor_id)? {
            self.interrupt_message(&message.id, reason)?;
        }
        self.set_status(actor_id, ActorStatus::Idle)
    }
    fn interrupt_message(&self, id: &str, reason: &str) -> Result<()> {
        database_io(|| {
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
        })
    }
}

#[cfg(test)]
mod outcome_tests {
    use super::*;

    #[test]
    fn sudo_observations_keep_target_identity_and_domain_facts_without_changing_the_effect_record()
    {
        let arguments = |tool: &str, target: Value| serde_json::json!({"tool":tool,"arguments_json":target.to_string(),"reason":"Authorized maintenance", "restart_native":false,"approval_id":null});
        let mut result = ToolResultRecord {
            id: "result".into(),
            actor_id: "actor".into(),
            message_id: "request".into(),
            call_id: "call".into(),
            name: "sudo__execute_as_admin".into(),
            arguments: arguments(
                "terminal__terminal_exec",
                serde_json::json!({"command":"repair-service","private":"PRIVATE_ARGUMENT"}),
            ),
            payload: serde_json::json!({"session":{"command":"repair-service"},"output":"original stdout","sudo":{"approval_id":"PRIVATE_APPROVAL"}}),
            output_schema: Some(
                schemars::schema_for!(crate::terminal_app::TerminalToolResult).to_value(),
            ),
            model_content: "PRIVATE_PROTOCOL".into(),
            summary: "Repaired service".into(),
            error: None,
            started_at_ms: Some(1),
            finished_at_ms: Some(2),
        };
        let call = AgentToolCall {
            id: "call".into(),
            name: result.name.clone(),
            arguments: result.arguments.clone(),
        };
        let expected =
            serde_json::json!({"operation":"terminal__terminal_exec","command":"repair-service"});
        assert_eq!(tool_started_outcome(&call), Some(expected.clone()));
        assert_eq!(tool_outcome(&result), Some(expected.clone()));
        result.error = Some("Command failed".into());
        assert_eq!(tool_outcome(&result), Some(expected));
        assert_eq!(result.name, "sudo__execute_as_admin");
        assert_eq!(result.payload["output"], "original stdout");
        assert!(result.output_schema.is_some());
        result.error = None;
        result.name = "d_0123456789ab_sudo__execute".into();
        result.arguments = arguments("files__edit_file", serde_json::json!({"path":"file.rs"}));
        result.payload =
            serde_json::json!({"path":"file.rs","change":{"added_lines":2,"removed_lines":1}});
        assert_eq!(
            tool_outcome(&result),
            Some(
                serde_json::json!({"operation":"files__edit_file","path":"file.rs","added_lines":2,"removed_lines":1})
            )
        );
        let nested = arguments("sudo__execute", serde_json::json!({}));
        assert!(sudo_target("sudo__execute", &nested).is_none());
        assert!(
            sudo_target(
                "sudo__execute",
                &serde_json::json!({"arguments_json":"invalid"})
            )
            .is_none()
        );
    }

    #[test]
    fn terminal_start_fact_preserves_the_command_without_streaming_other_arguments() {
        let arguments = serde_json::json!({"command":"Get-Content 文件.rs | Select-Object -First 54", "environment":{"token":"PRIVATE_SENTINEL"}, "workdir":"PRIVATE_WORKSPACE"});
        let event = ActorEvent::ToolStarted {
            message_id: "request".into(),
            call_id: "call".into(),
            name: "terminal__terminal_exec".into(),
            outcome: command_outcome(&arguments, &Value::Null),
        };
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(
            value["outcome"],
            serde_json::json!({"command":arguments["command"]})
        );
        assert!(!value.to_string().contains("PRIVATE_"));
        assert!(
            command_outcome(
                &serde_json::json!({"command":"x".repeat(2049)}),
                &Value::Null
            )
            .is_none()
        );
        assert!(command_outcome(&serde_json::json!({"command":42}), &Value::Null).is_none());
        assert_eq!(
            tool_domain_name("d_0123456789ab_terminal__terminal_exec"),
            "terminal__terminal_exec"
        );
        let old:ActorEvent=serde_json::from_value(serde_json::json!({"type":"tool_started","message_id":"request","call_id":"call","name":"terminal__terminal_exec"})).unwrap();
        assert!(matches!(old, ActorEvent::ToolStarted { outcome: None, .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn blocked_transaction_and_waiting_reader_leave_control_tasks_runnable() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(ActorStore::open(&directory.path().join("actors.sqlite3")).unwrap());
        let (entered, entering) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::sync_channel(1);
        let writing = store.clone();
        let writer = tokio::spawn(async move {
            writing.stage_transaction(|tx| {
                let _ = entered.send(());
                blocked
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .map_err(|_| {
                        crate::stage::StageError::Unavailable("Storage starved the control task")
                    })?;
                tx.execute(
                    "INSERT INTO stage_public_meta VALUES('control-progress','committed')",
                    [],
                )?;
                Ok(())
            })
        });
        entering.await.unwrap();
        let (reading, read_started) = tokio::sync::oneshot::channel();
        let querying = store.clone();
        let reader = tokio::spawn(async move {
            let _ = reading.send(());
            querying.event_cursor()
        });
        let control = tokio::spawn(async move {
            read_started.await.unwrap();
            // Let the reader reach the held SQLite mutex before releasing it.
            tokio::task::yield_now().await;
            release.send(())
        });
        let write_result = writer.await.unwrap();
        let read_result = reader.await.unwrap();
        let control_result = control.await.unwrap();
        assert!(
            control_result.is_ok(),
            "database work prevented control progress"
        );
        write_result.unwrap();
        assert_eq!(read_result.unwrap(), 0);
        let value: String = store
            .stage_transaction(|tx| {
                Ok(tx.query_row(
                    "SELECT value FROM stage_public_meta WHERE key='control-progress'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(value, "committed");
    }

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
            name: "actors__message".into(),
            arguments: serde_json::json!({"stage_id":stage,"actor_id":"child","content":"private input"}),
            payload: serde_json::json!({"actor_id":"child","conversation_url":"/actors/child"}),
            output_schema: None,
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
            output_schema: None,
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
        assert_eq!(
            tool_outcome(&result),
            Some(serde_json::json!({"command":"argument command"}))
        );
    }
}
