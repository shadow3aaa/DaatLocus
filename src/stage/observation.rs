//! Bounded domain reads. Heavy replies travel as authorized immutable content.
use super::{ActorAddress, Result, StageError, blob::ContentStore, ledger::StageLedger};
use crate::actors::ActorSystem;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Query {
    InvocationMessage {
        id: String,
    },
    Queue {
        after: Option<i64>,
        limit: u8,
    },
    Resource {
        id: String,
    },
    Actor,
    Messages {
        before: Option<i64>,
        limit: u8,
    },
    Message {
        id: String,
    },
    Search {
        text: String,
        before: Option<i64>,
        limit: u8,
    },
    Results {
        message: String,
        before: Option<i64>,
        limit: u8,
    },
    Result {
        id: String,
    },
    Activity {
        message: String,
        after: i64,
        limit: u8,
    },
    Approvals {
        before: Option<i64>,
        limit: u8,
    },
    Approval {
        id: String,
    },
}
impl Query {
    pub fn validate(&self) -> Result<()> {
        let (id, limit) = match self {
            Self::Actor => (None, None),
            Self::Messages { limit, .. }
            | Self::Approvals { limit, .. }
            | Self::Queue { limit, .. } => (None, Some(*limit)),
            Self::Message { id }
            | Self::Result { id }
            | Self::Approval { id }
            | Self::Resource { id }
            | Self::InvocationMessage { id } => (Some(id), None),
            Self::Results { message, limit, .. } | Self::Activity { message, limit, .. } => {
                (Some(message), Some(*limit))
            }
            Self::Search { text, limit, .. } => {
                if text.trim().is_empty() || text.len() > 512 {
                    return Err(StageError::Invalid("Invalid search"));
                }
                (None, Some(*limit))
            }
        };
        if id.is_some_and(|id| id.is_empty() || id.len() > 128)
            || limit.is_some_and(|limit| limit == 0 || limit > 32)
        {
            return Err(StageError::Invalid("Invalid bounded observation"));
        }
        Ok(())
    }
}
pub fn read(
    system: &ActorSystem,
    peer: super::StageId,
    actor: &ActorAddress,
    query: &Query,
    now: i64,
) -> Result<Value> {
    query.validate()?;
    let local = system.store().stage_local_id()?;
    if actor.stage_id != local {
        return Err(StageError::Forbidden("Observation targets another host"));
    }
    let grant =
        StageLedger::attach(system.store(), local).authorize_actor(peer, &actor.actor_id, now)?;
    if !grant.read {
        return Err(StageError::Forbidden("Actor observation is not authorized"));
    }
    let source = system
        .actor(&actor.actor_id)
        .map_err(|_| StageError::Forbidden("Actor is unavailable"))?;
    let event_head =
        if actor.actor_id == super::shared::ACTOR_ID && matches!(query, Query::Messages { .. }) {
            Some(super::journal::Journal::new(system.store()).current_cursor(&actor.actor_id)?)
        } else {
            None
        };
    let fail = |_| StageError::Unavailable("Observation is unavailable");
    let mut value=match query {
        Query::InvocationMessage {id}=>{
            let id=uuid::Uuid::parse_str(id).map_err(|_|StageError::Invalid("Invalid invocation identifier"))?;
            let receipt=StageLedger::attach(system.store(),local).result(peer,id,now)?;
            if receipt.actor_id!=actor.actor_id{return Err(StageError::Forbidden("Invocation belongs to another Actor"));}
            serde_json::to_value(system.store().message(&receipt.message_id.ok_or(StageError::Unavailable("Input is not yet received"))?).map_err(fail)?)
        },
        Query::Queue {after,limit}=>{
            let page=system.store().queued_messages(&actor.actor_id,*after,*limit as usize).map_err(fail)?;
            let mut value=serde_json::to_value(page).map_err(|_|StageError::Invalid("Invalid queue page"))?;
            value["active"]=serde_json::to_value(system.store().running_message(&actor.actor_id).map_err(fail)?).map_err(|_|StageError::Invalid("Invalid active input"))?;Ok(value)
        },
        Query::Resource {id}=>serde_json::to_value(crate::resources::export(system,&actor.actor_id,id)?),
        Query::Actor => {
            let manages=StageLedger::attach(system.store(),local).manages_actor(peer,&actor.actor_id,now)?;
            let mut value=json!({"id":source.id,"name":source.config.name,"conversation_url":actor.conversation_url(),"status":source.status,"created_at_ms":source.created_at_ms,"updated_at_ms":source.updated_at_ms,"access":{"view":true,"converse":grant.converse,"manage":manages},"remote":true});
            value["model_ready"] = crate::config_setup::readiness_for_config(&system.config()).is_complete().into();
            if manages{value["config"]=serde_json::to_value(&source.config).map_err(|_|StageError::Invalid("Invalid Actor config"))?;}Ok(value)
        },
        Query::Messages {before,limit} => {
            let page=system.store().messages(&actor.actor_id,*before,*limit as usize).map_err(fail)?;
            let mut value=serde_json::to_value(page).map_err(|_|StageError::Invalid("Invalid message page"))?;
            if before.is_none() { value["active"]=serde_json::to_value(system.store().current_message(&actor.actor_id).map_err(fail)?).map_err(|_|StageError::Invalid("Invalid active input"))?; }
            Ok(value)
        },
        Query::Search {text,before,limit} => serde_json::to_value(system.store().search_messages(&actor.actor_id,text,*before,*limit as usize).map_err(fail)?),
        Query::Message {id} => {let record=system.store().message(id).map_err(fail)?;if record.actor_id!=actor.actor_id{return Err(StageError::Forbidden("Message belongs to another Actor"));} serde_json::to_value(record)},
        Query::Results {message,before,limit} => serde_json::to_value(system.store().tool_summaries(&actor.actor_id,message,*before,*limit as usize).map_err(fail)?),
        Query::Result {id} => {let record=system.store().tool_result(id).map_err(fail)?;if record.actor_id!=actor.actor_id{return Err(StageError::Forbidden("Result belongs to another Actor"));} serde_json::to_value(record)},
        Query::Activity {message,after,limit} => serde_json::to_value(system.store().message_events(&actor.actor_id,message,*after,*limit as usize).map_err(fail)?),
        Query::Approvals {before,limit} => serde_json::to_value(system.store().approvals(Some(&actor.actor_id),*before,*limit as usize).map_err(fail)?),
        Query::Approval {id} => {let record=system.store().approval(id).map_err(fail)?;if record.source_actor(system.store()).map_err(fail)?!=actor.actor_id{return Err(StageError::Forbidden("Application source belongs to another Actor"));}serde_json::to_value(record)},
    }.map_err(|_|StageError::Invalid("Cannot encode observation"))?;
    if let Some(cursor) = event_head {
        value["event_head"] = serde_json::to_value(cursor)
            .map_err(|_| StageError::Invalid("Invalid conversation cursor"))?;
    }
    // The service redactor also removes private tool protocol transcripts and credentials.
    crate::service::redact(&mut value, &system.config());
    Ok(value)
}
pub fn reference(
    system: &ActorSystem,
    peer: super::StageId,
    actor: &ActorAddress,
    query: &Query,
    now: i64,
) -> Result<super::protocol::BlobRef> {
    let value = read(system, peer, actor, query, now)?;
    let bytes =
        serde_json::to_vec(&value).map_err(|_| StageError::Invalid("Cannot encode observation"))?;
    Ok(ContentStore::new(system.store())
        .store_source(&actor.actor_id, &bytes)?
        .content)
}
