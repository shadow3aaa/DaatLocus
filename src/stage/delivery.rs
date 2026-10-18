//! Durable lineage and independent replies; execution stays in the Actor kernel.
use super::{
    ActorAddress, Result, StageError, StageId, ledger,
    protocol::{BlobRef, Body, Envelope, InvocationState, Origin, RemoteCeiling, invocation_id},
};
use crate::actors::{
    ActorRecord, DelegationGrant, MessageKind, MessageRecord, MessageSource, MessageStatus,
    RemoteAuthority, store::ActorStore,
};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Clone, Serialize, Deserialize)]
struct Sent {
    target: ActorAddress,
    local_actor: String,
    request_id: Option<String>,
    grant: DelegationGrant,
}
pub(crate) fn initialize(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_sent(invocation TEXT PRIMARY KEY,peer TEXT NOT NULL,record TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS stage_ready_deliveries(message TEXT PRIMARY KEY,peer TEXT NOT NULL,record BLOB NOT NULL,created INTEGER NOT NULL,acknowledged INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS stage_delivery_receipts(peer TEXT NOT NULL,message TEXT NOT NULL,digest TEXT NOT NULL,local_message TEXT,expires INTEGER NOT NULL,confirmed INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(peer,message));
        CREATE TABLE IF NOT EXISTS stage_send_keys(actor TEXT NOT NULL,principal TEXT NOT NULL,key TEXT NOT NULL,digest TEXT NOT NULL,frame BLOB NOT NULL,PRIMARY KEY(actor,principal,key));")?;
    Ok(())
}
fn json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|_| StageError::Invalid("Cannot encode delivery state"))
}
fn parse<T: serde::de::DeserializeOwned>(value: String) -> Result<T> {
    serde_json::from_str(&value).map_err(|_| StageError::Invalid("Invalid delivery state"))
}
fn sent(tx: &Transaction<'_>, peer: StageId, id: Uuid) -> Result<Sent> {
    let record: String = tx
        .query_row(
            "SELECT record FROM stage_sent WHERE peer=?1 AND invocation=?2",
            params![peer.to_string(), id.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(StageError::Forbidden("Reply has no originating request"))?;
    parse(record)
}
pub(crate) fn reply_authority(
    store: &ActorStore,
    peer: StageId,
    invocation: Uuid,
    actor: &str,
) -> Result<()> {
    store.stage_transaction(|tx| {
        ledger::peer(tx, peer, chrono::Utc::now().timestamp_millis())?;
        let source = sent(tx, peer, invocation)?;
        if source.request_id.is_none() || source.local_actor != actor {
            return Err(StageError::Forbidden(
                "Reply authority does not match requester",
            ));
        }
        Ok(())
    })
}
pub struct Deliveries<'a> {
    store: &'a ActorStore,
    local: StageId,
}
impl<'a> Deliveries<'a> {
    pub fn new(store: &'a ActorStore, local: StageId) -> Self {
        Self { store, local }
    }
    pub(crate) fn queue(
        &self,
        source: &ActorRecord,
        request: Option<&MessageRecord>,
        target: ActorAddress,
        input: (String, Vec<crate::resources::Resource>),
        now: i64,
        key: &str,
    ) -> Result<Envelope> {
        let (text, attachments) = input;
        target.validate()?;
        if target.stage_id == self.local
            || (text.trim().is_empty() && attachments.is_empty())
            || text.len() > 64 * 1024
        {
            return Err(StageError::Invalid("Invalid remote Actor input"));
        }
        for attachment in &attachments {
            attachment.validate()?;
        }
        let id = invocation_id(now)?;
        let reference = BlobRef {
            hash: super::digest(text.as_bytes()),
            size: text.len() as u64,
            version: 1,
        };
        let origin = if let Some(request) = request {
            Origin::Actor {
                actor: ActorAddress {
                    stage_id: self.local,
                    actor_id: source.id.clone(),
                },
                request_id: Uuid::parse_str(&request.id)
                    .map_err(|_| StageError::Invalid("Invalid originating request"))?,
                ceiling: RemoteCeiling {
                    read_only: source.remote_read_only,
                    approval_mode: source.config.permissions.approval_mode,
                },
            }
        } else {
            Origin::User
        };
        let mut permissions = source.config.permissions.clone();
        if request.is_none_or(|message| message.remote_authority.is_none()) {
            let remembered = self
                .store
                .filesystem_approval_scopes(&source.id)
                .map_err(|_| StageError::Invalid("Invalid remembered filesystem scope"))?;
            crate::actors::grants::extend_permissions(&mut permissions, &remembered);
        }
        let record = Sent {
            target: target.clone(),
            local_actor: source.id.clone(),
            request_id: request.map(|message| message.id.clone()),
            grant: DelegationGrant {
                read_only: source.remote_read_only,
                permissions,
            },
        };
        self.store.stage_transaction(|tx| {
            let peer = ledger::peer(tx, target.stage_id, now)?;
            if key.is_empty()||key.len()>512{return Err(StageError::Invalid("Invalid remote send key"));}
            let principal=request.map_or("user".to_string(),|request|format!("actor:{}",request.id));
            let input_hash=super::digest(json(&(target.clone(),&text,&attachments))?.as_bytes());
            let previous:Option<(String,Vec<u8>)>=tx.query_row("SELECT digest,frame FROM stage_send_keys WHERE actor=?1 AND principal=?2 AND key=?3",params![source.id,principal,key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((previous,bytes))=previous{if previous!=input_hash{return Err(StageError::Conflict("Remote send key changed input"));}return Envelope::decode(&bytes);}
            super::blob::put(tx, &source.id, text.as_bytes())?;
            let frame = Envelope {
                version: 1,
                source: self.local,
                target: target.stage_id,
                generation: peer.generation,
                message_id: Uuid::new_v4(),
                invocation_id: Some(id),
                in_reply_to: None,
                created_at_ms: now,
                expires_at_ms: now.saturating_add(super::protocol::MAX_INVOCATION_LIFETIME_MS),
                body: Body::Invoke {
                    origin,
                    target,
                    content: reference,
                    attachments,
                },
            };
            ledger::enqueue_tx(tx, &frame, now, ledger::DEFAULT_BACKLOG_BYTES)?;
            tx.execute(
                "INSERT INTO stage_sent VALUES(?1,?2,?3)",
                params![id.to_string(), frame.target.to_string(), json(&record)?],
            )?;
            tx.execute("INSERT INTO stage_send_keys VALUES(?1,?2,?3,?4,?5)",params![source.id,principal,key,input_hash,frame.encode()?])?;
            Ok(frame)
        })
    }
    pub fn ready(&self, peer: StageId, now: i64, limit: usize) -> Result<Vec<Envelope>> {
        self.store.stage_transaction(|tx|{ledger::peer(tx,peer,now)?;let rows=tx.prepare("SELECT record FROM stage_ready_deliveries WHERE peer=?1 AND acknowledged=0 ORDER BY created,message LIMIT ?2")?.query_map(params![peer.to_string(),limit.clamp(1,32) as i64],|r|r.get::<_,Vec<u8>>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;rows.iter().map(|bytes|Envelope::decode(bytes)).collect()})
    }
    pub fn acknowledge(&self, peer: StageId, message: Uuid, now: i64) -> Result<()> {
        self.store.stage_transaction(|tx| {
            ledger::peer(tx, peer, now)?;
            tx.execute(
                "UPDATE stage_ready_deliveries SET acknowledged=1 WHERE peer=?1 AND message=?2",
                params![peer.to_string(), message.to_string()],
            )?;
            tx.execute(
                "UPDATE stage_outbox SET received=1,completed=1 WHERE peer=?1 AND message=?2",
                params![peer.to_string(), message.to_string()],
            )?;
            Ok(())
        })
    }
    pub fn expected_reply(&self, peer: StageId, frame: &Envelope, now: i64) -> Result<BlobRef> {
        self.store.stage_transaction(|tx| {
            let current = ledger::peer(tx, peer, now)?;
            frame.validate(peer, self.local, current.generation, now)?;
            let Body::Delivery {
                invocation,
                actor,
                content,
                ..
            } = &frame.body
            else {
                return Err(StageError::Invalid("Expected a delivery"));
            };
            let source = sent(tx, peer, *invocation)?;
            if source.target != *actor {
                return Err(StageError::Forbidden(
                    "Delivery does not match requested Actor",
                ));
            }
            Ok(content.clone())
        })
    }
    pub fn accept_reply(
        &self,
        peer: StageId,
        frame: &Envelope,
        text: String,
        now: i64,
    ) -> Result<Option<String>> {
        let reference = self.expected_reply(peer, frame, now)?;
        if text.len() as u64 != reference.size || super::digest(text.as_bytes()) != reference.hash {
            return Err(StageError::Conflict("Reply checksum failed"));
        }
        self.store.stage_transaction(|tx|{
            let Body::Delivery{invocation,actor,..}=&frame.body else{return Err(StageError::Invalid("Expected a delivery"));};
            if let Body::Delivery { initial: true, invocation, state, content, .. } = &frame.body { super::ledger::record_result(tx, peer, *invocation, state.clone(), Some(content.clone()), now)?; }
            let hash=frame.invocation_digest()?;
            let previous:Option<(String,Option<String>)>=tx.query_row("SELECT digest,local_message FROM stage_delivery_receipts WHERE peer=?1 AND message=?2",params![peer.to_string(),frame.message_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((previous,message))=previous{if previous!=hash{return Err(StageError::Conflict("Delivery retry changed content"));}return Ok(message);}
            let source=sent(tx,peer,*invocation)?;
            let message=if let Some(request)=&source.request_id{
                let data:String=tx.query_row("SELECT record FROM messages WHERE id=?1 AND actor_id=?2",params![request,source.local_actor],|r|r.get(0)).optional()?.ok_or(StageError::Forbidden("Originating request is unavailable"))?;
                let original:MessageRecord=parse(data)?;
                let mut grant=source.grant;
                if matches!(frame.body,Body::Delivery{state:InvocationState::ResultUnknown,..}) {grant.read_only=true;grant.permissions.writable_roots.clear();}
                let input=MessageRecord{seq:0,id:Uuid::new_v4().to_string(),actor_id:source.local_actor.clone(),source:MessageSource::RemoteActor{stage_id:peer,actor_id:actor.actor_id.clone(),invocation_id:*invocation},reply_to:original.reply_to,kind:if matches!(frame.body,Body::Delivery{state:InvocationState::Running,..}){MessageKind::Progress}else{MessageKind::Result},in_reply_to:Some(original.id),delegation_grant:Some(grant),remote_authority:Some(RemoteAuthority{peer,grant_actor:source.local_actor,reply_for:Some(*invocation)}),content:text,attachments:vec![],status:MessageStatus::Queued,cancel_requested:false,started_at_ms:None,finished_at_ms:None,final_generation:None,reply:None,error:None,created_at_ms:now,updated_at_ms:now};
                Some(crate::actors::store::insert_message(tx,input).map_err(|_|StageError::Unavailable("Reply inbox could not be persisted"))?.id)
            }else{None};
            tx.execute("INSERT INTO stage_delivery_receipts(peer,message,digest,local_message,expires) VALUES(?1,?2,?3,?4,?5)",params![peer.to_string(),frame.message_id.to_string(),hash,message,frame.expires_at_ms])?;Ok(message)
        })
    }
}
/// Commit a reference with the Actor's completion. A full transfer cache cannot
/// erase an accepted result or cause the Actor to execute its effects again.
pub(crate) fn finished(tx: &Transaction<'_>, message: &MessageRecord) -> Result<()> {
    use crate::actors::ReplyTarget;
    let ReplyTarget::Remote {
        stage_id,
        invocation_id: invocation,
        ..
    } = message.reply_to
    else {
        return Ok(());
    };
    if !matches!(
        message.status,
        MessageStatus::Completed
            | MessageStatus::Failed
            | MessageStatus::Cancelled
            | MessageStatus::Interrupted
    ) {
        return Ok(());
    }
    let Some(text) = message.reply.as_deref().or(message.error.as_deref()) else {
        return Ok(());
    };
    let local: String =
        tx.query_row("SELECT value FROM stage_meta WHERE key='local'", [], |r| {
            r.get(0)
        })?;
    let local = StageId(
        Uuid::parse_str(&local).map_err(|_| StageError::Invalid("Invalid Stage identity"))?,
    );
    let created = message.finished_at_ms.unwrap_or(message.updated_at_ms);
    let existing:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM stage_ready_deliveries WHERE json_extract(CAST(record AS TEXT),'$.in_reply_to')=?1)",[message.id.as_str()],|r|r.get(0))?;
    if existing {
        return Ok(());
    }
    let key = super::digest(format!("delivery:{}:{:?}", message.id, message.status).as_bytes());
    let bytes = hex::decode(key).map_err(|_| StageError::Invalid("Invalid delivery identifier"))?;
    let mut raw = [0u8; 16];
    raw.copy_from_slice(&bytes[..16]);
    let delivery = invocation_id(created)?;
    let reference = BlobRef {
        hash: super::digest(text.as_bytes()),
        size: text.len() as u64,
        version: 1,
    };
    reference.validate()?;
    tx.execute(
        "INSERT OR IGNORE INTO stage_content_sources(actor,hash,message,field) VALUES(?1,?2,?3,?4)",
        params![
            message.actor_id,
            reference.hash,
            message.id,
            if message.reply.is_some() {
                "reply"
            } else {
                "error"
            }
        ],
    )?;
    let frame = Envelope {
        version: 1,
        source: local,
        target: stage_id,
        generation: 0,
        message_id: Uuid::from_bytes(raw),
        invocation_id: Some(invocation),
        in_reply_to: Uuid::parse_str(&message.id).ok(),
        created_at_ms: created,
        expires_at_ms: created.saturating_add(super::protocol::MAX_INVOCATION_LIFETIME_MS),
        body: Body::Delivery {
            invocation,
            delivery,
            actor: ActorAddress {
                stage_id: local,
                actor_id: message.actor_id.clone(),
            },
            state: if message.kind == MessageKind::Delivery {
                InvocationState::Running
            } else {
                match message.status {
                    MessageStatus::Completed => InvocationState::Completed,
                    MessageStatus::Cancelled => InvocationState::Cancelled,
                    MessageStatus::Interrupted => InvocationState::ResultUnknown,
                    _ => InvocationState::Failed,
                }
            },
            content: reference,
            initial: message.kind == MessageKind::Request,
        },
    };
    tx.execute("INSERT OR IGNORE INTO stage_ready_deliveries(message,peer,record,created) VALUES(?1,?2,?3,?4)",params![frame.message_id.to_string(),stage_id.to_string(),frame.encode()?,created])?;
    Ok(())
}
