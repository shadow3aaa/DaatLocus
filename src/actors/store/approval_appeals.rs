//! Atomic, source-owned appeals. Denied calls are never replayed here.
use super::*;

fn approval_tx(tx: &Transaction<'_>, id: &str) -> Result<ApprovalRecord> {
    decode(
        sql(tx
            .query_row("SELECT record FROM approvals WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional())?
        .ok_or_else(|| missing("Approval"))?,
    )
}

fn admission(tx: &Transaction<'_>, actor: &str) -> Result<()> {
    let pending: i64 = sql(tx.query_row("SELECT count(*) FROM messages WHERE actor_id=?1 AND status IN ('\"queued\"','\"running\"')", [actor], |row| row.get(0)))?;
    if pending >= 256 {
        return Err(ActorError::Unavailable("Actor mailbox is full".into()).into());
    }
    Ok(())
}

fn position(tx: &Transaction<'_>, message: &MessageRecord) -> Result<i64> {
    if core_requires_commit(tx)? {
        sql(tx.query_row(
            "SELECT ordinal FROM core_order WHERE entity='messages' AND remote_key=?1",
            [&message.id],
            |row| row.get(0),
        ))
    } else {
        Ok(message.seq)
    }
}

impl ActorStore {
    pub(crate) fn appeal_approval(
        &self,
        actor: &str,
        input: &str,
        id: &str,
        explanation: &str,
        stage: crate::stage::StageId,
        core_authorized: bool,
    ) -> Result<ApprovalRecord> {
        database_io(|| {
            if explanation.trim().is_empty() || explanation.len() > 8192 {
                return Err(
                    ActorError::Invalid("An appeal needs a concise explanation".into()).into(),
                );
            }
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut approval = approval_tx(&tx, id)?;
            let original = message_tx(&tx, &approval.message_id)?;
            // A native request from the current Core worker has already checked
            // its input there. No Core worker or current input is needed here.
            let current = if core_authorized && actor == crate::stage::shared::ACTOR_ID {
                original.clone()
            } else {
                message_tx(&tx, input)?
            };
            if current.actor_id != actor
                || (!core_authorized && current.status != MessageStatus::Running)
                || current.cancel_requested
                || approval.actor_id != actor
                || approval.mode != ApprovalMode::Auto
                || approval.decision != ApprovalDecision::Denied
                || current.remote_authority.is_some()
                || original.remote_authority.is_some()
                || original.cancel_requested
                || matches!(
                    original.status,
                    MessageStatus::Cancelled | MessageStatus::Interrupted | MessageStatus::Failed
                )
            {
                return Err(ActorError::Forbidden("Only the originating Actor may appeal an active automatic denial within its own authority".into()).into());
            }
            if let Some(appeal) = &approval.appeal {
                if appeal.explanation != explanation {
                    return Err(ActorError::Conflict(
                        "This denial already has an immutable appeal".into(),
                    )
                    .into());
                }
                return Ok(approval);
            }
            let core = crate::stage::shared::ACTOR_ID;
            admission(&tx, core)?;
            let message_id = Uuid::new_v4().to_string();
            let content = serde_json::json!({
                "type":"approval_appeal", "approval_id":id, "stage_id":stage,
                "actor_id":actor, "request_message_id":approval.message_id,
                "application":approval.application,
                "proposed_call":approval.call,
                "requested_access":approval.requested, "app_review":approval.review,
                "unapproved_scopes":approval.scopes, "denial_reason":approval.reason,
                "explanation":explanation
            })
            .to_string();
            if content.len() > 64 * 1024 {
                return Err(ActorError::Invalid("Appeal facts exceed the message limit; narrow the proposed operation before applying again".into()).into());
            }
            let now = now();
            insert_message(
                &tx,
                MessageRecord {
                    seq: 0,
                    id: message_id.clone(),
                    actor_id: core.into(),
                    source: MessageSource::Actor {
                        actor_id: actor.into(),
                    },
                    reply_to: ReplyTarget::User,
                    kind: MessageKind::Request,
                    in_reply_to: None,
                    delegation_grant: None,
                    remote_authority: None,
                    content,
                    attachments: vec![],
                    status: MessageStatus::Queued,
                    cancel_requested: false,
                    started_at_ms: None,
                    finished_at_ms: None,
                    final_generation: None,
                    reply: None,
                    error: None,
                    created_at_ms: now,
                    updated_at_ms: now,
                },
            )?;
            approval.appeal = Some(ApprovalAppeal {
                core_message_id: message_id,
                request_message_id: current.id,
                explanation: explanation.into(),
                decision: ApprovalDecision::Pending,
                owner_message_id: None,
                notification_id: None,
            });
            sql(tx.execute(
                "UPDATE approvals SET record=?1 WHERE id=?2",
                params![encode(&approval)?, id],
            ))?;
            self.commit(tx)?;
            Ok(approval)
        })
    }

    pub(crate) fn owner_appeal_reply(
        &self,
        owner_input: &str,
        core_message_id: &str,
    ) -> Result<String> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let owner = message_tx(&tx, owner_input)?;
            let core = crate::stage::shared::ACTOR_ID;
            if owner.actor_id != core
                || owner.source != MessageSource::User
                || owner.status != MessageStatus::Running
                || owner.cancel_requested
                || owner.remote_authority.is_some()
                || crate::conversation_import::is_import(&owner)
            {
                return Err(ActorError::Forbidden(
                    "Appeal resolution requires a direct current Owner reply to Core".into(),
                )
                .into());
            }
            let order = if core_requires_commit(&tx)? {
                "EXISTS(SELECT 1 FROM core_order o WHERE o.entity='messages' AND o.remote_key=m.id AND o.ordinal<?2)"
            } else {
                "m.seq<?2"
            };
            let query = format!(
                "SELECT m.id FROM messages m WHERE m.actor_id='core' AND json_extract(m.record,'$.kind')='delivery' AND json_extract(m.record,'$.in_reply_to')=?1 AND json_extract(m.record,'$.reply_to.kind')='user' AND length(trim(json_extract(m.record,'$.reply')))>0 AND {order} AND EXISTS(SELECT 1 FROM events e WHERE e.actor_id='core' AND json_extract(e.record,'$.type')='message_added' AND json_extract(e.record,'$.message_id')=m.id) LIMIT 1"
            );
            sql(tx.query_row(&query,params![core_message_id,position(&tx,&owner)?],|row|row.get(0)).optional())?
                .ok_or_else(|| ActorError::Forbidden("Core must ask the Owner about this appeal before resolving it from a later Owner reply".into()).into())
        })
    }

    pub(crate) fn resolve_approval_appeal(
        &self,
        authorization: &ApprovalAppealAuthorization,
    ) -> Result<ApprovalRecord> {
        database_io(|| {
            let ApprovalAppealAuthorization {
                approval_id: id,
                decision,
                reason,
                owner_message_id: owner_input,
                ..
            } = authorization;
            let decision = *decision;
            if !matches!(
                decision,
                ApprovalDecision::Approved | ApprovalDecision::Denied
            ) || reason.trim().is_empty()
                || reason.len() > 8192
                || owner_input.is_empty()
                || owner_input.len() > 128
                || authorization.question_message_id.is_empty()
                || authorization.question_message_id.len() > 128
            {
                return Err(ActorError::Invalid("Invalid appeal decision".into()).into());
            }
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let core = crate::stage::shared::ACTOR_ID;
            let mut approval = approval_tx(&tx, id)?;
            let mut appeal = approval
                .appeal
                .clone()
                .ok_or_else(|| missing("Approval appeal"))?;
            if appeal.core_message_id != authorization.core_message_id
                || crate::actors::approval::scopes_digest(&approval)? != authorization.scopes_digest
            {
                return Err(ActorError::Forbidden(
                    "Appeal decision changed its immutable target or scope".into(),
                )
                .into());
            }
            if appeal.decision != ApprovalDecision::Pending {
                if appeal.owner_message_id.as_deref() == Some(owner_input.as_str())
                    && appeal.decision == decision
                {
                    return Ok(approval);
                }
                return Err(ActorError::Conflict("Appeal has already been resolved".into()).into());
            }
            let original = message_tx(&tx, &approval.message_id)?;
            let request = message_tx(&tx, &appeal.request_message_id)?;
            if approval.mode != ApprovalMode::Auto
                || approval.decision != ApprovalDecision::Denied
                || original.cancel_requested
                || original.remote_authority.is_some()
                || request.cancel_requested
                || matches!(
                    request.status,
                    MessageStatus::Cancelled | MessageStatus::Interrupted | MessageStatus::Failed
                )
                || matches!(
                    original.status,
                    MessageStatus::Cancelled | MessageStatus::Interrupted | MessageStatus::Failed
                )
                || actor_tx(&tx, &approval.actor_id)?.status == ActorStatus::Archived
            {
                return Err(
                    ActorError::Conflict("Appeal authority is no longer valid".into()).into(),
                );
            }
            admission(&tx, &approval.actor_id)?;
            if decision == ApprovalDecision::Approved {
                super::super::grants::remember(&tx, &approval.actor_id, &approval.scopes)?;
            }
            let notification_id = Uuid::new_v4().to_string();
            let now = now();
            insert_message(&tx, MessageRecord {
                seq:0, id:notification_id.clone(), actor_id:approval.actor_id.clone(),
                source:MessageSource::Actor{actor_id:core.into()}, reply_to:original.reply_to.clone(),
                kind:MessageKind::Progress, in_reply_to:Some(approval.message_id.clone()),
                delegation_grant:original.delegation_grant.clone(), remote_authority:None,
                content:serde_json::json!({"type":"approval_appeal_decision", "approval_id":id,
                    "decision":decision, "reason":reason,
                    "instruction":"Permission memory has been updated if approved. No effect was executed. Recheck current targets and retry only if still needed; do not replay uncertain effects."}).to_string(),
                attachments:vec![], status:MessageStatus::Queued, cancel_requested:false,
                started_at_ms:None, finished_at_ms:None, final_generation:None, reply:None,error:None,
                created_at_ms:now, updated_at_ms:now,
            })?;
            appeal.decision = decision;
            appeal.owner_message_id = Some(owner_input.into());
            appeal.notification_id = Some(notification_id);
            approval.appeal = Some(appeal);
            sql(tx.execute(
                "UPDATE approvals SET record=?1 WHERE id=?2",
                params![encode(&approval)?, id],
            ))?;
            self.commit(tx)?;
            Ok(approval)
        })
    }
}
