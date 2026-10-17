//! Atomic shared-core checkpoints. Only typed Actor records are replicated;
//! account credentials and device filesystem/App grants remain local.
use super::{
    Result, StageError, StageId,
    blob::{self, ContentStore},
    protocol::BlobRef,
    shared::{ACTOR_ID, Head},
};
use crate::actors::{
    ActorEvent, ApprovalRecord, MessageRecord, ToolResultRecord, store::ActorStore,
};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entity {
    Resource,
    Configuration,
    Message,
    History,
    Tool,
    Approval,
    Event,
}
impl Entity {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Resource => "resources",
            Self::Configuration => "configuration",
            Self::Message => "messages",
            Self::History => "history",
            Self::Tool => "tools",
            Self::Approval => "approvals",
            Self::Event => "events",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub entity: Entity,
    pub key: String,
    pub revision: u64,
    pub ordinal: u64,
    pub content: BlobRef,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    pub account_id: String,
    pub leader: StageId,
    pub epoch: u64,
    pub previous: Head,
    pub head: Head,
    pub changes: Vec<Change>,
}
pub fn initialize(store: &ActorStore, account: &str) -> Result<()> {
    store.stage_transaction(|tx| {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS core_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS core_dirty(entity TEXT NOT NULL,local_key TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(entity,local_key));
          CREATE TABLE IF NOT EXISTS core_rows(entity TEXT NOT NULL,remote_key TEXT NOT NULL,local_key TEXT NOT NULL,record BLOB NOT NULL,PRIMARY KEY(entity,remote_key),UNIQUE(entity,local_key));
          CREATE TABLE IF NOT EXISTS core_prepared(sequence INTEGER PRIMARY KEY,batch TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS core_order(entity TEXT NOT NULL,remote_key TEXT NOT NULL,ordinal INTEGER NOT NULL,PRIMARY KEY(entity,remote_key),UNIQUE(entity,ordinal));")?;
        let hash_column:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('core_rows') WHERE name='content_hash')",[],|r|r.get(0))?;
        if !hash_column {tx.execute_batch("ALTER TABLE core_rows ADD COLUMN content_hash TEXT;CREATE INDEX core_content_hash ON core_rows(content_hash);")?;}
        let previous:Option<String>=tx.query_row("SELECT value FROM core_meta WHERE key='account'",[],|r|r.get(0)).optional()?;
        if previous.as_ref().is_some_and(|previous|previous!=account){return Err(StageError::Forbidden("Shared Actor belongs to another account"));}
        tx.execute("INSERT OR IGNORE INTO core_meta VALUES('account',?1)",[account])?;
        tx.execute("INSERT OR IGNORE INTO core_meta VALUES('head',?1)",[encode(&Head::default())?])?;
        tx.execute("INSERT OR IGNORE INTO core_meta VALUES('importing','0')",[])?;
        for (entity,key) in [(Entity::Message,"id"),(Entity::History,"seq"),(Entity::Tool,"id"),(Entity::Approval,"id"),(Entity::Event,"seq")] {
            let table=entity.name();
            for action in ["INSERT","UPDATE"] {
                let sql=format!("CREATE TRIGGER IF NOT EXISTS core_{table}_{action} AFTER {action} ON {table} WHEN NEW.actor_id='core' AND (SELECT value FROM core_meta WHERE key='importing')='0' BEGIN INSERT INTO core_dirty(entity,local_key,revision) VALUES('{table}',CAST(NEW.{key} AS TEXT),1) ON CONFLICT(entity,local_key) DO UPDATE SET revision=revision+1; END;");
                tx.execute_batch(&sql)?;
            }
        }
        tx.execute_batch("CREATE TRIGGER IF NOT EXISTS core_configuration_insert AFTER INSERT ON actors WHEN NEW.id='core' AND (SELECT value FROM core_meta WHERE key='importing')='0' BEGIN INSERT INTO core_dirty VALUES('configuration','core',1) ON CONFLICT(entity,local_key) DO UPDATE SET revision=revision+1; END;
        CREATE TRIGGER IF NOT EXISTS core_configuration_update AFTER UPDATE ON actors WHEN NEW.id='core' AND (SELECT value FROM core_meta WHERE key='importing')='0' BEGIN INSERT INTO core_dirty VALUES('configuration','core',1) ON CONFLICT(entity,local_key) DO UPDATE SET revision=revision+1; END;")?;
        Ok(())
    })
}
fn encode<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|_| StageError::Invalid("Cannot encode shared Actor state"))
}
pub fn head(store: &ActorStore) -> Result<Head> {
    store.stage_transaction(|tx| {
        let value: String =
            tx.query_row("SELECT value FROM core_meta WHERE key='head'", [], |r| {
                r.get(0)
            })?;
        serde_json::from_str(&value)
            .map_err(|_| StageError::Invalid("Invalid shared Actor checkpoint"))
    })
}
fn source(tx: &Transaction<'_>, entity: Entity, key: &str) -> Result<Value> {
    if entity == Entity::Configuration {
        let data: String = tx.query_row("SELECT record FROM actors WHERE id='core'", [], |r| {
            r.get(0)
        })?;
        let actor: crate::actors::ActorRecord = serde_json::from_str(&data)
            .map_err(|_| StageError::Invalid("Invalid core configuration"))?;
        return serde_json::to_value(super::core_wire::Configuration::from_actor(&actor))
            .map_err(|_| StageError::Invalid("Invalid core configuration"));
    }
    let sql = match entity {
        Entity::Resource => "SELECT record FROM stage_resources WHERE actor='core' AND id=?1",
        Entity::Configuration => unreachable!(),
        Entity::History => {
            "SELECT json_object('message_id',message_id,'record',json(record)) FROM history WHERE actor_id='core' AND seq=?1"
        }
        Entity::Event => {
            "SELECT json_object('created_at_ms',created_at,'record',json(record)) FROM events WHERE actor_id='core' AND seq=?1"
        }
        Entity::Tool => {
            "SELECT json_object('done',done,'record',json(record)) FROM tools WHERE actor_id='core' AND id=?1"
        }
        Entity::Message => "SELECT record FROM messages WHERE actor_id='core' AND id=?1",
        Entity::Approval => "SELECT record FROM approvals WHERE actor_id='core' AND id=?1",
    };
    let value: String = tx.query_row(sql, [key], |r| r.get(0))?;
    let mut value: Value = serde_json::from_str(&value)
        .map_err(|_| StageError::Invalid("Invalid Actor source record"))?;
    if entity == Entity::Message || entity == Entity::Approval {
        value["seq"] = 0.into();
    }
    // These are device-local authorization ceilings. Tool hosts enforce their
    // own grants; neither serialized credentials nor host paths are imported.
    if let Some(object) = value.as_object_mut() {
        object.remove("_authority");
        object.remove("_delegation_grant");
        object.remove("remote_authority");
        object.remove("delegation_grant");
    }
    Ok(value)
}
pub fn capture(
    store: &ActorStore,
    account: &str,
    leader: StageId,
    epoch: u64,
) -> Result<Option<Batch>> {
    store.stage_transaction(|tx| {
        let previous:Head=serde_json::from_str(&tx.query_row::<String,_,_>("SELECT value FROM core_meta WHERE key='head'",[],|r|r.get(0))?).map_err(|_|StageError::Invalid("Invalid core head"))?;
        // A lost coordinator response must retry the exact prepared proposal.
        // Dirty rows can advance while publication is in flight; recapturing
        // them would replace the only copy of an already committed head.
        let prepared:Option<String>=tx.query_row("SELECT batch FROM core_prepared WHERE sequence=?1",[previous.sequence as i64+1],|r|r.get(0)).optional()?;
        if let Some(data)=prepared {
            let batch:Batch=serde_json::from_str(&data).map_err(|_|StageError::Invalid("Invalid prepared checkpoint"))?;
            if batch.previous==previous && batch.leader==leader && batch.epoch==epoch {
                validate(&batch,account)?;
                return Ok(Some(batch));
            }
        }
        let rows=tx.prepare("SELECT entity,local_key,revision FROM core_dirty ORDER BY CASE entity WHEN 'resources' THEN 0 WHEN 'messages' THEN 1 WHEN 'history' THEN 2 WHEN 'tools' THEN 3 WHEN 'approvals' THEN 4 ELSE 5 END,CASE WHEN entity='messages' THEN (SELECT seq FROM messages WHERE id=local_key) ELSE CAST(local_key AS INTEGER) END,local_key LIMIT 256")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let mut changes=vec![];
        for (table,key,revision) in rows {
            let entity=match table.as_str(){"resources"=>Entity::Resource,"configuration"=>Entity::Configuration,"messages"=>Entity::Message,"history"=>Entity::History,"tools"=>Entity::Tool,"approvals"=>Entity::Approval,"events"=>Entity::Event,_=>return Err(StageError::Invalid("Invalid checkpoint entity"))};
            let value=source(tx,entity,&key)?;let bytes=serde_json::to_vec(&value).map_err(|_|StageError::Invalid("Cannot encode checkpoint record"))?;
            let previous:Option<(String,Vec<u8>)>=tx.query_row("SELECT remote_key,record FROM core_rows WHERE entity=?1 AND local_key=?2",params![table,key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if previous.as_ref().is_some_and(|(_,old)|old==&bytes){tx.execute("DELETE FROM core_dirty WHERE entity=?1 AND local_key=?2 AND revision=?3",params![table,key,revision])?;continue;}
            let remote_key=previous.map(|(key,_)|key).unwrap_or_else(||match entity{Entity::History|Entity::Event=>format!("{leader}/{key}"),_=>key.clone()});
            let ordinal:Option<i64>=tx.query_row("SELECT ordinal FROM core_order WHERE entity=?1 AND remote_key=?2",params![table,remote_key],|r|r.get(0)).optional()?;
            let ordinal=match ordinal {Some(value)=>value,None=>{let next:i64=tx.query_row("SELECT COALESCE(MAX(ordinal),0)+1 FROM core_order WHERE entity=?1",[table.as_str()],|r|r.get(0))?;tx.execute("INSERT INTO core_order VALUES(?1,?2,?3)",params![table,remote_key,next])?;next}};
            let content=blob::put(tx,ACTOR_ID,&bytes)?.content;
            changes.push(Change{entity,key:remote_key,revision: revision as u64,ordinal:ordinal as u64,content});
        }
        if changes.is_empty(){return Ok(None);}
        let hash=super::digest(&serde_json::to_vec(&json!({"previous":previous,"changes":changes})).map_err(|_|StageError::Invalid("Cannot hash checkpoint"))?);
        let head=Head{sequence:previous.sequence.checked_add(1).ok_or(StageError::Capacity("Checkpoint sequence exhausted"))?,hash};head.validate()?;
        let batch=Batch{account_id:account.into(),leader,epoch,previous,head,changes};
        tx.execute("INSERT INTO core_prepared(sequence,batch) VALUES(?1,?2) ON CONFLICT(sequence) DO UPDATE SET batch=excluded.batch",params![batch.head.sequence as i64,encode(&batch)?])?;
        Ok(Some(batch))
    })
}
pub fn validate(batch: &Batch, account: &str) -> Result<()> {
    batch.previous.validate()?;
    batch.head.validate()?;
    if batch.account_id != account
        || batch.leader.0.is_nil()
        || batch.epoch == 0
        || batch.changes.is_empty()
        || batch.changes.len() > 256
        || batch.head.sequence != batch.previous.sequence + 1
    {
        return Err(StageError::Forbidden("Invalid shared Actor checkpoint"));
    }
    let mut keys = std::collections::HashSet::new();
    for change in &batch.changes {
        change.content.validate()?;
        if change.key.is_empty()
            || change.key.len() > 200
            || !keys.insert((change.entity.name(), &change.key))
        {
            return Err(StageError::Invalid("Invalid checkpoint key"));
        }
    }
    let hash = super::digest(
        &serde_json::to_vec(&json!({"previous":batch.previous,"changes":batch.changes}))
            .map_err(|_| StageError::Invalid("Cannot hash checkpoint"))?,
    );
    if hash != batch.head.hash {
        return Err(StageError::Conflict("Checkpoint checksum failed"));
    }
    Ok(())
}
pub fn prepare(store: &ActorStore, batch: &Batch, account: &str) -> Result<()> {
    validate(batch, account)?;
    let current = head(store)?;
    if current == batch.head {
        return Ok(());
    }
    if current != batch.previous {
        return Err(StageError::Conflict("Shared Actor needs checkpoint replay"));
    }
    for change in &batch.changes {
        ContentStore::new(store).verified_bytes(&change.content)?;
    }
    store.stage_transaction(|tx|{tx.execute("INSERT INTO core_prepared(sequence,batch) VALUES(?1,?2) ON CONFLICT(sequence) DO UPDATE SET batch=excluded.batch",params![batch.head.sequence as i64,encode(batch)?])?;Ok(())})
}

pub fn prepared(store: &ActorStore, head: &Head) -> Result<Option<Batch>> {
    store.stage_transaction(|tx| {
        let data: Option<String> = tx
            .query_row(
                "SELECT batch FROM core_prepared WHERE sequence=?1",
                [head.sequence as i64],
                |r| r.get(0),
            )
            .optional()?;
        let batch = data
            .map(|data| {
                serde_json::from_str::<Batch>(&data)
                    .map_err(|_| StageError::Invalid("Invalid prepared checkpoint"))
            })
            .transpose()?;
        Ok(batch.filter(|batch| &batch.head == head))
    })
}
pub fn pending(store: &ActorStore) -> Result<bool> {
    store.stage_transaction(|tx| {
        Ok(tx.query_row("SELECT EXISTS(SELECT 1 FROM core_dirty)", [], |r| r.get(0))?)
    })
}
pub fn resume(store: &ActorStore) -> Result<()> {
    // Restore committed model/message state before deciding what can resume.
    // Speculative model output is disposable; externally issued calls are not.
    store.stage_transaction(|tx|{
        tx.execute("UPDATE core_meta SET value='1' WHERE key='importing'",[])?;
        tx.execute("DELETE FROM history WHERE actor_id='core' AND CAST(seq AS TEXT) NOT IN (SELECT local_key FROM core_rows WHERE entity='history')",[])?;
        let records=tx.prepare("SELECT entity,remote_key,local_key,record FROM core_rows WHERE entity IN ('messages','history','tools','approvals') ORDER BY CASE entity WHEN 'messages' THEN 0 WHEN 'history' THEN 1 WHEN 'tools' THEN 2 ELSE 3 END")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Vec<u8>>(3)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        for (kind,key,local,bytes) in records {
            let entity=match kind.as_str(){"messages"=>Entity::Message,"history"=>Entity::History,"tools"=>Entity::Tool,"approvals"=>Entity::Approval,_=>return Err(StageError::Invalid("Invalid committed entity"))};
            let change=Change{entity,key,revision:1,ordinal:1,content:BlobRef{hash:super::digest(&bytes),size:bytes.len() as u64,version:1}};
            apply(tx,&change,&bytes,Some(&local))?;
        }
        tx.execute("UPDATE core_meta SET value='0' WHERE key='importing'",[])?;
        tx.execute("DELETE FROM core_dirty",[])?;
        let records=tx.prepare("SELECT record FROM messages WHERE actor_id='core' AND status='\"running\"'")?.query_map([],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        for data in records {
            let mut message:MessageRecord=serde_json::from_str(&data).map_err(|_|StageError::Invalid("Invalid resumable input"))?;
            message.status=crate::actors::MessageStatus::Queued;
            tx.execute("UPDATE messages SET status='\"queued\"',record=?2 WHERE id=?1",params![message.id,encode(&message)?])?;
        }
        // Queued inputs retained from a recovery branch have not entered the
        // chosen checkpoint yet. Keep them pending for delivery/commit rather
        // than clearing their only scheduling marker with speculative output.
        tx.execute("INSERT INTO core_dirty(entity,local_key,revision) SELECT 'messages',m.id,1 FROM messages m WHERE m.actor_id='core' AND m.status='\"queued\"' AND NOT EXISTS(SELECT 1 FROM core_rows r WHERE r.entity='messages' AND r.remote_key=m.id) ON CONFLICT(entity,local_key) DO NOTHING",[])?;
        Ok(())
    })?;
    store
        .set_status(ACTOR_ID, crate::actors::ActorStatus::Idle)
        .map_err(|_| StageError::Unavailable("Cannot restore Actor readiness"))
}
/// A final answer already committed before worker loss is delivered without
/// asking a replacement model to generate it again.
pub fn completed_answer(store: &ActorStore, message: &str) -> Result<Option<String>> {
    store.stage_transaction(|tx| {
        require_message(tx, message)?;
        let data: Option<String> = tx.query_row(
            "SELECT record FROM history WHERE actor_id='core' AND message_id=?1 ORDER BY seq DESC LIMIT 1",
            [message], |r| r.get(0),
        ).optional()?;
        match data.map(|data| serde_json::from_str::<crate::model_protocol::AgentMessage>(&data)).transpose()
            .map_err(|_| StageError::Invalid("Invalid committed model history"))? {
            Some(crate::model_protocol::AgentMessage::Assistant { content, .. }) => Ok(Some(content)),
            _ => Ok(None),
        }
    })
}
pub fn incomplete_calls(
    store: &ActorStore,
    message: &str,
) -> Result<Vec<crate::model_protocol::AgentToolCall>> {
    store.stage_transaction(|tx| {
        require_message(tx, message)?;
        let rows = tx
            .prepare(
                "SELECT record FROM history WHERE actor_id='core' AND message_id=?1 ORDER BY seq",
            )?
            .query_map([message], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let history = rows
            .into_iter()
            .map(|row| {
                serde_json::from_str::<crate::model_protocol::AgentMessage>(&row)
                    .map_err(|_| StageError::Invalid("Invalid resumable context"))
            })
            .collect::<Result<Vec<_>>>()?;
        let completed = history
            .iter()
            .filter_map(|item| {
                if let crate::model_protocol::AgentMessage::Tool { tool_call_id, .. } = item {
                    Some(tool_call_id)
                } else {
                    None
                }
            })
            .collect::<std::collections::HashSet<_>>();
        Ok(history
            .iter()
            .flat_map(|item| {
                if let crate::model_protocol::AgentMessage::AssistantToolCallProtocol {
                    calls,
                    ..
                } = item
                {
                    calls.clone()
                } else {
                    vec![]
                }
            })
            .filter(|call| !completed.contains(&call.id))
            .collect())
    })
}

/// A bounded journal query for a lagging replica. No tool is executed by replay.
pub fn batches(store: &ActorStore, after: &Head, limit: usize) -> Result<Vec<Batch>> {
    if limit == 0 || limit > 8 {
        return Err(StageError::Invalid("Invalid checkpoint page"));
    }
    store.stage_transaction(|tx| {
        let records = tx
            .prepare(
                "SELECT batch FROM core_prepared WHERE sequence>?1 AND sequence<=CAST(json_extract((SELECT value FROM core_meta WHERE key='head'),'$.sequence') AS INTEGER) ORDER BY sequence LIMIT ?2",
            )?
            .query_map(params![after.sequence as i64, limit as i64], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let batches = records
            .into_iter()
            .map(|data| {
                serde_json::from_str::<Batch>(&data)
                    .map_err(|_| StageError::Invalid("Invalid checkpoint journal"))
            })
            .collect::<Result<Vec<_>>>()?;
        if batches
            .first()
            .is_some_and(|batch| batch.previous != *after)
        {
            return Err(StageError::Conflict(
                "Shared Actor requires a state snapshot",
            ));
        }
        Ok(batches)
    })
}
pub fn commit(store: &ActorStore, batch: &Batch, local: StageId) -> Result<()> {
    store.stage_transaction(|tx| {
        let value:String=tx.query_row("SELECT value FROM core_meta WHERE key='account'",[],|r|r.get(0))?;validate(batch,&value)?;
        let current:Head=serde_json::from_str(&tx.query_row::<String,_,_>("SELECT value FROM core_meta WHERE key='head'",[],|r|r.get(0))?).map_err(|_|StageError::Invalid("Invalid checkpoint head"))?;
        if current==batch.head{return Ok(());}
        if current!=batch.previous{return Err(StageError::Conflict("Shared Actor requires replay"));}
        tx.execute("UPDATE core_meta SET value='1' WHERE key='importing'",[])?;
        for change in &batch.changes {
            tx.execute("INSERT INTO core_order VALUES(?1,?2,?3) ON CONFLICT(entity,remote_key) DO UPDATE SET ordinal=excluded.ordinal",params![change.entity.name(),change.key,change.ordinal as i64])?;
            let bytes:Vec<u8>=tx.query_row("SELECT bytes FROM stage_blobs WHERE hash=?1 AND size=?2",params![change.content.hash,change.content.size as i64],|r|r.get(0))?;
            if super::digest(&bytes)!=change.content.hash{return Err(StageError::Conflict("Checkpoint content changed"));}
            let previous:Option<String>=tx.query_row("SELECT local_key FROM core_rows WHERE entity=?1 AND remote_key=?2",params![change.entity.name(),change.key],|r|r.get(0)).optional()?;
            let key=if batch.leader==local {
                previous.clone().unwrap_or_else(||match change.entity{Entity::History|Entity::Event=>change.key.rsplit('/').next().unwrap_or_default().into(),_=>change.key.clone()})
            }else{apply(tx,change,&bytes,previous.as_deref())?};
            tx.execute("INSERT INTO core_rows(entity,remote_key,local_key,record) VALUES(?1,?2,?3,?4) ON CONFLICT(entity,remote_key) DO UPDATE SET record=excluded.record,local_key=excluded.local_key",params![change.entity.name(),change.key,key,bytes])?;
            tx.execute("UPDATE core_rows SET content_hash=?3 WHERE entity=?1 AND remote_key=?2",params![change.entity.name(),change.key,change.content.hash])?;
            tx.execute("DELETE FROM core_dirty WHERE entity=?1 AND local_key=?2 AND revision=?3",params![change.entity.name(),key,change.revision as i64])?;
        }
        tx.execute("UPDATE core_meta SET value=?1 WHERE key='head'",[encode(&batch.head)?])?;
        tx.execute("UPDATE core_meta SET value='0' WHERE key='importing'",[])?;
        tx.execute("DELETE FROM core_prepared WHERE sequence<?1",[batch.head.sequence.saturating_sub(64) as i64])?;
        Ok(())
    })
}
pub(crate) fn apply(
    tx: &Transaction<'_>,
    change: &Change,
    bytes: &[u8],
    previous: Option<&str>,
) -> Result<String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| StageError::Invalid("Invalid shared Actor record"))?;
    let decode = |value| {
        serde_json::from_value::<MessageRecord>(value)
            .map_err(|_| StageError::Invalid("Invalid shared Actor message"))
    };
    match change.entity {
        Entity::Resource => {
            let resource: crate::resources::Resource = serde_json::from_value(value)
                .map_err(|_| StageError::Invalid("Invalid resource checkpoint"))?;
            resource.validate()?;
            if resource.id != change.key {
                return Err(StageError::Forbidden("Resource identifier mismatch"));
            }
            let ready:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM stage_resources WHERE actor='core' AND id=?1 AND record=?2)",params![resource.id,encode(&resource)?],|r|r.get(0))?;
            if !ready {
                return Err(StageError::Unavailable(
                    "Resource bytes have not been installed",
                ));
            }
            Ok(change.key.clone())
        }
        Entity::Configuration => {
            let mut configuration: super::core_wire::Configuration = serde_json::from_value(value)
                .map_err(|_| StageError::Invalid("Invalid shared configuration"))?;
            if change.key != "core"
                || configuration.name.len() > 100
                || configuration.instructions.len() > 64 * 1024
                || !configuration.context.valid()
                || configuration.apps.len() > 16
                || configuration
                    .delegation_apps
                    .as_ref()
                    .is_some_and(|apps| apps.len() > 16)
                || configuration
                    .tools
                    .as_ref()
                    .is_some_and(|tools| tools.len() > 256)
                || configuration
                    .app_access
                    .as_ref()
                    .is_some_and(|apps| apps.len() > 16)
            {
                return Err(StageError::Invalid("Invalid shared configuration"));
            }
            crate::actors::migrations::retire_study_grants(
                &mut configuration.apps,
                &mut configuration.app_access,
                &mut configuration.delegation_apps,
                &mut configuration.tools,
            );
            if configuration.app_access.as_ref().is_some_and(|apps| {
                crate::builtin_apps::validate_app_names(apps, configuration.read_only).is_err()
            }) || crate::builtin_apps::validate_app_names(
                configuration
                    .delegation_apps
                    .as_ref()
                    .unwrap_or(&configuration.apps),
                configuration.read_only,
            )
            .is_err()
            {
                return Err(StageError::Invalid("Invalid shared configuration"));
            }
            let record: String =
                tx.query_row("SELECT record FROM actors WHERE id='core'", [], |r| {
                    r.get(0)
                })?;
            let mut actor: crate::actors::ActorRecord = serde_json::from_str(&record)
                .map_err(|_| StageError::Invalid("Invalid Actor record"))?;
            configuration.apply(&mut actor.config);
            actor.context = configuration.context;
            tx.execute(
                "UPDATE actors SET record=?1 WHERE id='core'",
                [encode(&actor)?],
            )?;
            Ok("core".into())
        }
        Entity::Message => {
            let mut message = decode(value)?;
            if message.actor_id != ACTOR_ID || message.id != change.key {
                return Err(StageError::Forbidden("Checkpoint contains another Actor"));
            }
            let exists: Option<(i64, String)> = tx
                .query_row(
                    "SELECT seq,actor_id FROM messages WHERE id=?1",
                    [&message.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if exists.as_ref().is_some_and(|(_, actor)| actor != ACTOR_ID) {
                return Err(StageError::Forbidden(
                    "Message identifier belongs to another Actor",
                ));
            }
            if let Some((seq, _)) = exists {
                message.seq = seq;
                tx.execute(
                    "UPDATE messages SET status=?2,record=?3 WHERE id=?1",
                    params![message.id, encode(&message.status)?, encode(&message)?],
                )?;
            } else {
                tx.execute(
                    "INSERT INTO messages(id,actor_id,status,record) VALUES(?1,'core',?2,'')",
                    params![message.id, encode(&message.status)?],
                )?;
                message.seq = tx.last_insert_rowid();
                tx.execute(
                    "UPDATE messages SET record=?2 WHERE id=?1",
                    params![message.id, encode(&message)?],
                )?;
            }
            Ok(change.key.clone())
        }
        Entity::History => {
            let message = value
                .get("message_id")
                .and_then(Value::as_str)
                .ok_or(StageError::Invalid("History has no originating message"))?;
            let record: crate::model_protocol::AgentMessage =
                serde_json::from_value(value["record"].clone())
                    .map_err(|_| StageError::Invalid("Invalid model history"))?;
            require_message(tx, message)?;
            if let Some(key) = previous {
                tx.execute(
                    "UPDATE history SET record=?2 WHERE seq=?1 AND actor_id='core'",
                    params![key, encode(&record)?],
                )?;
                Ok(key.into())
            } else {
                tx.execute(
                    "INSERT INTO history(actor_id,message_id,record) VALUES('core',?1,?2)",
                    params![message, encode(&record)?],
                )?;
                Ok(tx.last_insert_rowid().to_string())
            }
        }
        Entity::Event => {
            let record: ActorEvent = serde_json::from_value(value["record"].clone())
                .map_err(|_| StageError::Invalid("Invalid Actor event"))?;
            if let Some(key) = previous {
                return Ok(key.into());
            }
            tx.execute(
                "INSERT INTO events(actor_id,created_at,record) VALUES('core',?1,?2)",
                params![
                    value["created_at_ms"]
                        .as_i64()
                        .ok_or(StageError::Invalid("Event has no time"))?,
                    encode(&record)?
                ],
            )?;
            Ok(tx.last_insert_rowid().to_string())
        }
        Entity::Tool => {
            let record: ToolResultRecord = serde_json::from_value(value["record"].clone())
                .map_err(|_| StageError::Invalid("Invalid tool result"))?;
            if record.actor_id != ACTOR_ID || record.id != change.key {
                return Err(StageError::Forbidden("Tool belongs to another Actor"));
            }
            require_message(tx, &record.message_id)?;
            let done = value["done"]
                .as_i64()
                .ok_or(StageError::Invalid("Tool has no completion state"))?;
            tx.execute("INSERT INTO tools(id,actor_id,message_id,call_id,record,done) VALUES(?1,'core',?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET record=excluded.record,done=excluded.done",params![record.id,record.message_id,record.call_id,encode(&record)?,done])?;
            Ok(change.key.clone())
        }
        Entity::Approval => {
            let mut record: ApprovalRecord = serde_json::from_value(value)
                .map_err(|_| StageError::Invalid("Invalid approval"))?;
            if record.actor_id != ACTOR_ID || record.id != change.key {
                return Err(StageError::Forbidden("Approval belongs to another Actor"));
            }
            require_message(tx, &record.message_id)?;
            let seq: Option<i64> = tx
                .query_row(
                    "SELECT seq FROM approvals WHERE id=?1 AND actor_id='core'",
                    [&record.id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(seq) = seq {
                record.seq = seq;
                tx.execute(
                    "UPDATE approvals SET record=?2 WHERE id=?1",
                    params![record.id, encode(&record)?],
                )?;
            } else {
                tx.execute(
                    "INSERT INTO approvals(id,actor_id,message_id,record) VALUES(?1,'core',?2,'')",
                    params![record.id, record.message_id],
                )?;
                record.seq = tx.last_insert_rowid();
                tx.execute(
                    "UPDATE approvals SET record=?2 WHERE id=?1",
                    params![record.id, encode(&record)?],
                )?;
            }
            Ok(change.key.clone())
        }
    }
}
fn require_message(tx: &Transaction<'_>, id: &str) -> Result<()> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1 AND actor_id='core')",
        [id],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(StageError::Forbidden("Record has no shared Actor input"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn older_checkpoints_drop_retired_study_grants_and_still_reject_unknown_apps() {
        let source_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let source = store(source_dir.path());
        let target = store(target_dir.path());
        let leader = StageId::new();
        let follower = StageId::new();
        let mut actor = source.actor(ACTOR_ID).unwrap();
        actor.config.apps = vec!["actors".into()];
        actor.config.permissions.app_access = Some(vec!["study".into()]);
        actor.config.permissions.delegation_apps = Some(vec!["files".into(), "study".into()]);
        actor.config.permissions.tools =
            Some(vec!["actors__create".into(), "study__create_node".into()]);
        source.save_actor(&actor).unwrap();
        let batch = capture(&source, "account-a", leader, 1).unwrap().unwrap();
        copy(&source, &target, &batch);
        commit(&target, &batch, follower).unwrap();
        commit(&source, &batch, leader).unwrap();
        let imported = target.actor(ACTOR_ID).unwrap();
        assert_eq!(imported.config.permissions.app_access, Some(vec![]));
        assert_eq!(
            imported.config.permissions.delegation_apps,
            Some(vec!["files".into()])
        );
        assert_eq!(
            imported.config.permissions.tools,
            Some(vec!["actors__create".into()])
        );
        actor.config.permissions.delegation_apps = Some(vec!["unknown-app".into()]);
        source.save_actor(&actor).unwrap();
        let invalid = capture(&source, "account-a", leader, 1).unwrap().unwrap();
        copy(&source, &target, &invalid);
        assert!(commit(&target, &invalid, follower).is_err());
        assert_eq!(target.actor(ACTOR_ID).unwrap().config, imported.config);
    }

    #[test]
    fn a_shared_core_failure_never_creates_a_remote_actor_notice() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        let peer = StageId::new();
        let cursor = store.event_cursor().unwrap();
        let mut event = crate::actors::EventRecord {
            seq: 42,
            actor_id: ACTOR_ID.into(),
            created_at_ms: 1,
            event: crate::actors::ActorEvent::MessageUpdated {
                message_id: "failed-input".into(),
                status: MessageStatus::Failed,
            },
        };
        store.receive_notice(peer, &event).unwrap();
        assert_eq!(store.event_cursor().unwrap(), cursor);
        event.actor_id = "hosted-actor".into();
        store.receive_notice(peer, &event).unwrap();
        let events = store.events(cursor, None, true, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].actor_id, format!("{peer}~hosted-actor"));
        store.receive_notice(peer, &event).unwrap();
        assert_eq!(store.events(cursor, None, true, 10).unwrap().len(), 1);
    }
    #[test]
    fn an_idle_actor_record_cannot_bypass_a_running_serial_input() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        let first = store.enqueue(input(), None).unwrap();
        assert_eq!(store.claim_next(ACTOR_ID).unwrap().unwrap().id, first.id);
        store
            .set_status(ACTOR_ID, crate::actors::ActorStatus::Idle)
            .unwrap();
        let second = store.enqueue(input(), None).unwrap();
        assert!(store.claim_next(ACTOR_ID).unwrap().is_none());
        store
            .finish_message(&first.id, MessageStatus::Completed, Some("Done"), None)
            .unwrap();
        assert_eq!(store.claim_next(ACTOR_ID).unwrap().unwrap().id, second.id);
    }
    #[test]
    fn divergent_answers_and_observations_merge_idempotently_without_changing_model_history() {
        use crate::stage::recovery::{RetainedRow, merge_conversation};
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let a = store(a_dir.path());
        let b = store(b_dir.path());
        let owner = StageId::new();
        let other = StageId::new();
        let input = input();
        for (store, answer, arg) in [
            (&a, "Chosen answer", "chosen"),
            (&b, "Offline answer", "offline"),
        ] {
            store.enqueue(input.clone(), None).unwrap();
            store.claim_next(ACTOR_ID).unwrap();
            let call = crate::model_protocol::AgentToolCall {
                id: "same-call".into(),
                name: "actors__create".into(),
                arguments: json!({"name":arg}),
            };
            store.tool_started(ACTOR_ID, &input.id, &call).unwrap();
            store
                .append_history(
                    ACTOR_ID,
                    &input.id,
                    &[crate::model_protocol::AgentMessage::user(
                        "Selected model context",
                    )],
                )
                .unwrap();
            store
                .finish_message(&input.id, MessageStatus::Completed, Some(answer), None)
                .unwrap();
        }
        let batch = capture(&a, "account-a", owner, 1).unwrap().unwrap();
        commit(&a, &batch, owner).unwrap();
        let batch = capture(&b, "account-a", other, 2).unwrap().unwrap();
        commit(&b, &batch, other).unwrap();
        let rows = batch
            .changes
            .iter()
            .filter(|change| matches!(change.entity, Entity::Message | Entity::Tool))
            .map(|change| RetainedRow {
                branch: "first-archive".into(),
                entity: change.entity,
                key: change.key.clone(),
                bytes: ContentStore::new(&b)
                    .verified_bytes(&change.content)
                    .unwrap(),
            })
            .collect::<Vec<_>>();
        let mut branches = vec![(other, rows)];
        let third = branches[0]
            .1
            .iter()
            .map(|row| {
                let mut value: Value = serde_json::from_slice(&row.bytes).unwrap();
                match row.entity {
                    Entity::Message => value["reply"] = json!("Older offline answer"),
                    Entity::Tool => value["record"]["arguments"] = json!({"name":"older-offline"}),
                    _ => unreachable!(),
                }
                RetainedRow {
                    branch: "second-archive".into(),
                    entity: row.entity,
                    key: row.key.clone(),
                    bytes: serde_json::to_vec(&value).unwrap(),
                }
            })
            .collect();
        branches.push((other, third));
        assert!(merge_conversation(&a, &branches).unwrap());
        assert!(!merge_conversation(&a, &branches).unwrap());
        let messages = a.messages(ACTOR_ID, None, 100).unwrap().items;
        assert_eq!(messages.len(), 3);
        for (answer, arg) in [
            ("Chosen answer", "chosen"),
            ("Offline answer", "offline"),
            ("Older offline answer", "older-offline"),
        ] {
            let message = messages
                .iter()
                .find(|message| message.reply.as_deref() == Some(answer))
                .unwrap();
            let tool = a
                .stage_transaction(|tx| {
                    Ok(tx.query_row::<String, _, _>(
                        "SELECT record FROM tools WHERE message_id=?1 AND call_id='same-call'",
                        [&message.id],
                        |r| r.get(0),
                    )?)
                })
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&tool).unwrap()["arguments"]["name"],
                arg
            );
        }
        let history = a.history(ACTOR_ID, 100).unwrap();
        assert_eq!(
            history.len(),
            1,
            "divergent model transcripts must never be concatenated"
        );
        while let Some(batch) = capture(&a, "account-a", owner, 3).unwrap() {
            commit(&a, &batch, owner).unwrap();
        }
        resume(&a).unwrap();
        assert!(a.claim_next(ACTOR_ID).unwrap().is_none());
        assert_eq!(a.messages(ACTOR_ID, None, 100).unwrap().items.len(), 3);
    }
    #[tokio::test]
    async fn repeated_synchronization_confirmation_does_not_create_a_wake_feedback_loop() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        crate::stage::recovery::initialize(&store).unwrap();
        crate::stage::recovery::mark_synchronized(&store).unwrap();
        let changes = store.changed();
        let notified = changes.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        crate::stage::recovery::mark_synchronized(&store).unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), notified)
                .await
                .is_err()
        );
    }
    #[test]
    fn recovery_snapshot_archives_unknown_effects_and_restores_only_unstarted_inputs_with_their_constraints()
     {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let a = store(a_dir.path());
        let b = store(b_dir.path());
        crate::stage::recovery::initialize(&a).unwrap();
        crate::stage::recovery::initialize(&b).unwrap();
        let source = StageId::new();
        let target = StageId::new();
        let winner = a.enqueue(input(), None).unwrap();
        a.claim_next(ACTOR_ID).unwrap();
        a.finish_message(
            &winner.id,
            MessageStatus::Completed,
            Some("Retained winning answer"),
            None,
        )
        .unwrap();
        let batch = capture(&a, "account-a", source, 1).unwrap().unwrap();
        commit(&a, &batch, source).unwrap();
        let unknown = b.enqueue(input(), None).unwrap();
        b.claim_next(ACTOR_ID).unwrap();
        let call = crate::model_protocol::AgentToolCall {
            id: "unknown-effect".into(),
            name: "actors__create".into(),
            arguments: json!({"name":"already-created"}),
        };
        b.tool_started(ACTOR_ID, &unknown.id, &call).unwrap();
        let mut pending = input();
        pending.delegation_grant = Some(crate::actors::DelegationGrant {
            apps: vec!["actors".into()],
            permissions: crate::actors::ActorPermissions::workspace(
                b_dir.path().to_path_buf(),
                true,
            ),
        });
        let pending = b.enqueue(pending, None).unwrap();
        let batch = capture(&b, "account-a", target, 2).unwrap().unwrap();
        commit(&b, &batch, target).unwrap();
        let head = head(&a).unwrap();
        let mut token = None;
        let mut page = 0;
        loop {
            let snapshot =
                crate::stage::shared_snapshot::produce(&a, &head, token.as_deref(), page).unwrap();
            for change in &snapshot.entries {
                let bytes = ContentStore::new(&a)
                    .verified_bytes(&change.content)
                    .unwrap();
                ContentStore::new(&b)
                    .store_source(ACTOR_ID, &bytes)
                    .unwrap();
            }
            if crate::stage::shared_snapshot::receive_recovery(&b, &snapshot, "account-a", target)
                .unwrap()
            {
                break;
            }
            token = Some(snapshot.token);
            page += 1;
        }
        assert!(
            b.message(&unknown.id).is_err(),
            "started work must not be requeued from the losing branch"
        );
        resume(&b).unwrap();
        assert!(
            super::pending(&b).unwrap(),
            "unstarted retained input must still be scheduled after inference recovery"
        );
        let restored = b.message(&pending.id).unwrap();
        assert_eq!(restored.status, MessageStatus::Queued);
        assert!(
            restored.delegation_grant.unwrap().permissions.read_only,
            "private input constraints must survive restoration"
        );
        assert_eq!(
            b.message(&winner.id).unwrap().reply.as_deref(),
            Some("Retained winning answer")
        );
        let archives = crate::stage::recovery::archives(&b, None, 100).unwrap();
        assert_eq!(archives.items.len(), 1);
        let archived =
            crate::stage::recovery::messages(&b, &archives.items[0].id, None, 100).unwrap();
        assert!(archived.items.iter().any(|message|message.id==unknown.id && message.status==MessageStatus::Running));
        let preserved=b.stage_transaction(|tx|Ok(tx.query_row::<bool,_,_>("SELECT EXISTS(SELECT 1 FROM core_recovery_rows WHERE archive=?1 AND entity='tools' AND json_extract(CAST(record AS TEXT),'$.record.call_id')='unknown-effect')",[&archives.items[0].id],|r|r.get(0))?)).unwrap();
        assert!(
            preserved,
            "unknown tool intent must remain inspectable in retained data"
        );
        // Existing installations upgrade their private archives into ordinary
        // conversation facts, including unknown effects, without scheduling them.
        let (entries, next) = crate::stage::recovery::archived_page(&b, None).unwrap();
        assert!(next.is_none());
        let rows = entries
            .into_iter()
            .map(|entry| crate::stage::recovery::RetainedRow {
                branch: entry.archive,
                entity: entry.change.entity,
                key: entry.change.key,
                bytes: ContentStore::new(&b)
                    .verified_bytes(&entry.change.content)
                    .unwrap(),
            })
            .collect();
        let branches = vec![(target, rows)];
        assert!(crate::stage::recovery::merge_conversation(&b, &branches).unwrap());
        assert!(!crate::stage::recovery::merge_conversation(&b, &branches).unwrap());
        assert_eq!(
            b.message(&unknown.id).unwrap().status,
            MessageStatus::Cancelled
        );
        let observed = b
            .stage_transaction(|tx| {
                Ok(tx.query_row::<i64, _, _>(
                    "SELECT done FROM tools WHERE message_id=?1 AND call_id='unknown-effect'",
                    [&unknown.id],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(
            observed, 0,
            "unknown effects must not acquire a fabricated completion"
        );
        while let Some(batch) = capture(&b, "account-a", target, 3).unwrap() {
            commit(&b, &batch, target).unwrap();
        }
        resume(&b).unwrap();
        assert_eq!(
            b.message(&unknown.id).unwrap().status,
            MessageStatus::Cancelled
        );
        assert_eq!(b.claim_next(ACTOR_ID).unwrap().unwrap().id, pending.id);
        crate::stage::recovery::mark_archives_merged(&b).unwrap();
        assert!(!crate::stage::recovery::archives_pending(&b).unwrap());
        assert!(
            crate::stage::recovery::archived_page(&b, None)
                .unwrap()
                .0
                .is_empty()
        );
    }
    use crate::actors::{
        ActorConfig, ActorPermissions, ActorRecord, ActorStatus, MessageKind, MessageSource,
        MessageStatus, ReplyTarget,
    };
    use uuid::Uuid;
    pub(super) fn store(path: &std::path::Path) -> ActorStore {
        let store = ActorStore::open(&path.join("actors.sqlite")).unwrap();
        store
            .save_actor(&ActorRecord {
                context: Default::default(),
                id: ACTOR_ID.into(),
                parent_id: None,
                depth: 0,
                config: ActorConfig {
                    name: "Core".into(),
                    workspace: path.to_path_buf(),
                    apps: vec!["files".into()],
                    model: None,
                    instructions: String::new(),
                    permissions: ActorPermissions::workspace(path.to_path_buf(), false),
                },
                status: ActorStatus::Idle,
                created_at_ms: 1,
                updated_at_ms: 1,
            })
            .unwrap();
        initialize(&store, "account-a").unwrap();
        store
    }
    pub(super) fn input() -> MessageRecord {
        MessageRecord {
            seq: 0,
            id: Uuid::new_v4().to_string(),
            actor_id: ACTOR_ID.into(),
            source: MessageSource::User,
            reply_to: ReplyTarget::User,
            kind: MessageKind::Request,
            in_reply_to: None,
            delegation_grant: None,
            remote_authority: None,
            content: "hello shared Actor".into(),
            attachments: vec![],
            status: MessageStatus::Queued,
            cancel_requested: false,
            started_at_ms: None,
            finished_at_ms: None,
            final_generation: None,
            reply: None,
            error: None,
            created_at_ms: 2,
            updated_at_ms: 2,
        }
    }
    pub(super) fn copy(source: &ActorStore, target: &ActorStore, batch: &Batch) {
        for change in &batch.changes {
            let bytes = ContentStore::new(source)
                .verified_bytes(&change.content)
                .unwrap();
            ContentStore::new(target)
                .store_source(ACTOR_ID, &bytes)
                .unwrap();
        }
    }

    #[test]
    fn explicit_delivery_receipts_require_commit_and_survive_takeover_without_duplication() {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let a = store(a_dir.path());
        let b = store(b_dir.path());
        let leader = StageId::new();
        let input = a.enqueue(input(), None).unwrap();
        a.claim_next(ACTOR_ID).unwrap().unwrap();
        let receipt = a.deliver(ACTOR_ID, &input.id, "ack", "Started").unwrap();
        assert!(
            a.delivery_receipt(ACTOR_ID, &input.id, "ack")
                .unwrap()
                .is_none()
        );
        assert!(a.publish_delivery(&receipt.id).is_err());
        assert!(a.message(&receipt.id).is_err());
        assert_eq!(a.messages(ACTOR_ID, None, 100).unwrap().items.len(), 1);
        assert!(
            a.search_messages(ACTOR_ID, "Started", None, 100)
                .unwrap()
                .items
                .is_empty()
        );

        assert_eq!(
            a.deliver(ACTOR_ID, &input.id, "ack", "Started").unwrap().id,
            receipt.id
        );
        assert!(
            a.deliver(ACTOR_ID, &input.id, "ack", "Changed content")
                .is_err()
        );
        assert_eq!(a.message(&input.id).unwrap().status, MessageStatus::Running);
        let batch = capture(&a, "account-a", leader, 1).unwrap().unwrap();
        assert_eq!(
            a.messages(ACTOR_ID, None, 100).unwrap().items.len(),
            1,
            "preparing is not commitment"
        );
        copy(&a, &b, &batch);
        prepare(&b, &batch, "account-a").unwrap();
        commit(&a, &batch, leader).unwrap();
        commit(&b, &batch, StageId::new()).unwrap();
        assert_eq!(
            a.delivery_receipt(ACTOR_ID, &input.id, "ack")
                .unwrap()
                .unwrap()
                .id,
            receipt.id
        );
        resume(&b).unwrap();
        b.claim_next(ACTOR_ID).unwrap().unwrap();
        assert_eq!(
            b.delivery_receipt(ACTOR_ID, &input.id, "ack")
                .unwrap()
                .unwrap()
                .reply
                .as_deref(),
            Some("Started")
        );
        assert_eq!(
            b.deliver(ACTOR_ID, &input.id, "ack", "Started").unwrap().id,
            receipt.id
        );
        b.publish_delivery(&receipt.id).unwrap();
        b.finish_message(
            &input.id,
            MessageStatus::Completed,
            Some("must not duplicate"),
            None,
        )
        .unwrap();
        assert_eq!(b.message(&input.id).unwrap().reply.as_deref(), Some(""));
        assert_eq!(b.messages(ACTOR_ID, None, 100).unwrap().items.len(), 2);
        assert!(b.deliver(ACTOR_ID, &input.id, "late", "Too late").is_err());
    }
    #[test]
    fn compacted_checkpoint_history_recovers_from_frozen_bounded_pages() {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let a = store(a_dir.path());
        let b = store(b_dir.path());
        let leader = StageId::new();
        for _ in 0..75 {
            a.enqueue(input(), None).unwrap();
            let batch = capture(&a, "account-a", leader, 1).unwrap().unwrap();
            commit(&a, &batch, leader).unwrap();
        }
        assert!(batches(&a, &Head::default(), 8).is_err());
        let expected = head(&a).unwrap();
        let mut token = None;
        let mut number = 0;
        loop {
            let page =
                super::super::shared_snapshot::produce(&a, &expected, token.as_deref(), number)
                    .unwrap();
            assert!(page.entries.len() <= 24);
            for change in &page.entries {
                let bytes = ContentStore::new(&a)
                    .verified_bytes(&change.content)
                    .unwrap();
                ContentStore::new(&b)
                    .store_source(ACTOR_ID, &bytes)
                    .unwrap();
            }
            if super::super::shared_snapshot::receive(&b, &page, "account-a").unwrap() {
                break;
            }
            assert_eq!(head(&b).unwrap(), Head::default());
            token = Some(page.token);
            number += 1;
        }
        assert_eq!(head(&b).unwrap(), expected);
        assert_eq!(b.messages(ACTOR_ID, None, 100).unwrap().items.len(), 75);
        assert!(a_dir.path() != b.actor(ACTOR_ID).unwrap().config.workspace);
    }
    #[test]
    fn takeover_claims_committed_order_instead_of_device_arrival_order() {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let a = store(a_dir.path());
        let b = store(b_dir.path());
        let first = a.enqueue(input(), None).unwrap();
        let second = a.enqueue(input(), None).unwrap();
        // The second input originated on B and exists there before A's first.
        b.enqueue(second.clone(), None).unwrap();
        let leader = StageId::new();
        let batch = capture(&a, "account-a", leader, 1).unwrap().unwrap();
        copy(&a, &b, &batch);
        prepare(&b, &batch, "account-a").unwrap();
        commit(&b, &batch, StageId::new()).unwrap();
        resume(&b).unwrap();
        let page = b.messages(ACTOR_ID, None, 1).unwrap();
        assert_eq!(page.items[0].id, second.id);
        assert_eq!(page.items[0].seq, 2);
        assert_eq!(b.message(&first.id).unwrap().seq, 1);
        assert_eq!(
            b.messages(ACTOR_ID, page.next_cursor, 1).unwrap().items[0].id,
            first.id
        );
        assert_eq!(b.claim_next(ACTOR_ID).unwrap().unwrap().id, first.id);
        b.finish_message(&first.id, MessageStatus::Completed, Some("done"), None)
            .unwrap();
        assert_eq!(b.claim_next(ACTOR_ID).unwrap().unwrap().id, second.id);
    }
    #[test]
    fn lost_commit_ack_retries_the_immutable_batch_and_keeps_later_changes() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        let leader = StageId::new();
        let input = store.enqueue(input(), None).unwrap();
        let first = capture(&store, "account-a", leader, 1).unwrap().unwrap();
        // More work is persisted after the DO accepts the proposal but before
        // the sender receives confirmation.
        store.claim_next(ACTOR_ID).unwrap().unwrap();
        let retry = capture(&store, "account-a", leader, 1).unwrap().unwrap();
        assert_eq!(first.head, retry.head);
        assert!(batches(&store, &Head::default(), 8).unwrap().is_empty());
        commit(&store, &retry, leader).unwrap();
        let next = capture(&store, "account-a", leader, 1).unwrap().unwrap();
        assert_eq!(next.previous, first.head);
        commit(&store, &next, leader).unwrap();
        assert_eq!(
            store.message(&input.id).unwrap().status,
            MessageStatus::Running
        );
        assert!(capture(&store, "account-a", leader, 1).unwrap().is_none());
    }
    #[test]
    fn takeover_recovers_a_committed_answer_without_another_model_request() {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let a = store(a_dir.path());
        let b = store(b_dir.path());
        let leader = StageId::new();
        let input = a.enqueue(input(), None).unwrap();
        a.claim_next(ACTOR_ID).unwrap().unwrap();
        a.append_history(
            ACTOR_ID,
            &input.id,
            &[
                crate::model_protocol::AgentMessage::user("question"),
                crate::model_protocol::AgentMessage::assistant("committed answer"),
            ],
        )
        .unwrap();
        let batch = capture(&a, "account-a", leader, 1).unwrap().unwrap();
        copy(&a, &b, &batch);
        prepare(&b, &batch, "account-a").unwrap();
        commit(&b, &batch, StageId::new()).unwrap();
        resume(&b).unwrap();
        assert_eq!(b.message(&input.id).unwrap().status, MessageStatus::Queued);
        assert_eq!(
            completed_answer(&b, &input.id).unwrap().as_deref(),
            Some("committed answer")
        );
    }
    #[test]
    fn committed_core_context_replicates_without_replaying_tools_or_importing_device_grants() {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let a = store(a_dir.path());
        let b = store(b_dir.path());
        let leader = StageId::new();
        let follower = StageId::new();
        let input = a.enqueue(input(), Some("same-input")).unwrap();
        a.append_history(
            ACTOR_ID,
            &input.id,
            &[crate::model_protocol::AgentMessage::user(
                "hello shared Actor",
            )],
        )
        .unwrap();
        let batch = capture(&a, "account-a", leader, 1).unwrap().unwrap();
        assert_eq!(head(&a).unwrap(), Head::default());
        assert!(prepare(&b, &batch, "account-a").is_err());
        assert!(b.message(&input.id).is_err());
        copy(&a, &b, &batch);
        prepare(&b, &batch, "account-a").unwrap();
        assert!(b.message(&input.id).is_err());
        commit(&a, &batch, leader).unwrap();
        commit(&b, &batch, follower).unwrap();
        commit(&b, &batch, follower).unwrap();
        assert_eq!(head(&a).unwrap(), head(&b).unwrap());
        assert_eq!(b.messages(ACTOR_ID, None, 32).unwrap().items.len(), 1);
        assert_eq!(b.history(ACTOR_ID, 512).unwrap().len(), 1);
        assert_eq!(b.actor(ACTOR_ID).unwrap().config.workspace, b_dir.path());
        assert!(capture(&b, "account-a", follower, 2).unwrap().is_none());
        assert!(initialize(&b, "account-b").is_err());
        let claimed = a.claim_next(ACTOR_ID).unwrap().unwrap();
        let call = crate::model_protocol::AgentToolCall {
            id: "effect-1".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":"once.txt","content":"once"}),
        };
        a.tool_started(ACTOR_ID, &claimed.id, &call).unwrap();
        let next = capture(&a, "account-a", leader, 1).unwrap().unwrap();
        copy(&a, &b, &next);
        prepare(&b, &next, "account-a").unwrap();
        commit(&a, &next, leader).unwrap();
        commit(&b, &next, follower).unwrap();
        assert_eq!(b.message(&input.id).unwrap().status, MessageStatus::Running);
        assert!(!b_dir.path().join("once.txt").exists());
        assert_eq!(
            b.tool_summaries(ACTOR_ID, &input.id, None, 32)
                .unwrap()
                .items
                .len(),
            0
        ); // Started, never executed by replication.
        assert_eq!(batches(&a, &Head::default(), 8).unwrap().len(), 2);
    }
}

#[cfg(test)]
mod context_checkpoint_tests {
    use super::*;
    #[test]
    fn takeover_retains_reset_anchor_and_token_calibration_across_local_sequence_numbers() {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let a = super::tests::store(a_dir.path());
        let b = super::tests::store(b_dir.path());
        let input = a.enqueue(super::tests::input(), None).unwrap();
        a.append_history(
            ACTOR_ID,
            &input.id,
            &[crate::model_protocol::AgentMessage::user(
                "original request",
            )],
        )
        .unwrap();
        let recovery = a.reset_model_context(ACTOR_ID, &input.id).unwrap();
        a.update_token_estimate(
            ACTOR_ID,
            "model-a",
            crate::context_budget::TokenEstimateBaseline {
                estimated_input_tokens: 100,
                observed_input_tokens: Some(130),
            },
        )
        .unwrap();
        let mut other = b.actor(ACTOR_ID).unwrap();
        other.id = "other".into();
        b.save_actor(&other).unwrap();
        b.append_history(
            "other",
            "other-input",
            &[crate::model_protocol::AgentMessage::user(
                "private other Actor",
            )],
        )
        .unwrap();
        let leader = StageId::new();
        let batch = capture(&a, "account-a", leader, 1).unwrap().unwrap();
        super::tests::copy(&a, &b, &batch);
        commit(&a, &batch, leader).unwrap();
        prepare(&b, &batch, "account-a").unwrap();
        commit(&b, &batch, StageId::new()).unwrap();
        assert_eq!(
            a.actor(ACTOR_ID).unwrap().context,
            b.actor(ACTOR_ID).unwrap().context
        );
        assert_eq!(
            a.context_start(ACTOR_ID).unwrap(),
            b.context_start(ACTOR_ID).unwrap()
        );
        assert!(
            b.history_page(ACTOR_ID, b.context_start(ACTOR_ID).unwrap() - 1, None, 10)
                .unwrap()[0]
                .2
                == recovery
        );
        assert!(
            b.history("other", 10).unwrap()[0]
                == crate::model_protocol::AgentMessage::user("private other Actor")
        );
    }
}
