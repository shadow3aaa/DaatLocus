//! Atomic, source-owned appeals. Denied calls are never replayed here.
use super::*;

/// The runtime control envelope, delivered verbatim when inserted at a boundary.
pub(crate) const APPEAL_NOTICE_TYPE: &str = "approval_appeal_decision";

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
                || original.actor_id != actor
                || !approval.requires_core_review()
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
                "requested_access":approval.requested,
                "unapproved_scopes":approval.scopes, "denial_reason":approval.reason,
                "sudo_invocation":approval.sudo,
                "previous_denials":approval.previous_denials,
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
                decision_message_id: None,
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

    pub(crate) fn resolve_approval_appeal(
        &self,
        authorization: &ApprovalAppealAuthorization,
    ) -> Result<ApprovalRecord> {
        database_io(|| {
            let ApprovalAppealAuthorization {
                approval_id: id,
                decision,
                reason,
                decision_message_id: decision_input,
                ..
            } = authorization;
            let decision = *decision;
            if !matches!(
                decision,
                ApprovalDecision::Approved | ApprovalDecision::Denied
            ) || reason.trim().is_empty()
                || reason.len() > 8192
                || decision_input.is_empty()
                || decision_input.len() > 128
                || authorization
                    .owner_message_id
                    .as_ref()
                    .is_some_and(|id| id != decision_input)
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
                if appeal.decision == decision {
                    return Ok(approval);
                }
                return Err(ActorError::Conflict("Appeal has already been resolved".into()).into());
            }
            let original = message_tx(&tx, &approval.message_id)?;
            let request = message_tx(&tx, &appeal.request_message_id)?;
            if !approval.requires_core_review()
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
                || actor_tx(&tx, &original.actor_id)?.status == ActorStatus::Archived
            {
                return Err(
                    ActorError::Conflict("Appeal authority is no longer valid".into()).into(),
                );
            }
            admission(&tx, &original.actor_id)?;
            if decision == ApprovalDecision::Approved {
                crate::sandbox::scopes::remember(&tx, &approval.scopes)?;
            }
            let notification_id = Uuid::new_v4().to_string();
            let now = now();
            insert_message(&tx, MessageRecord {
                seq:0, id:notification_id.clone(), actor_id:original.actor_id.clone(),
                source:MessageSource::Actor{actor_id:core.into()}, reply_to:original.reply_to.clone(),
                kind:MessageKind::Request, in_reply_to:Some(approval.message_id.clone()),
                remote_authority:None,
                content:serde_json::json!({"type":APPEAL_NOTICE_TYPE, "approval_id":id,
                    "decision":decision, "reason":reason,
                    "instruction":"No operation was executed. If approved and still needed, recheck the target and retry the original operation."
                    }).to_string(),
                attachments:vec![], status:MessageStatus::Queued, cancel_requested:false,
                started_at_ms:None, finished_at_ms:None, final_generation:None, reply:None,error:None,
                created_at_ms:now, updated_at_ms:now,
            })?;
            appeal.decision = decision;
            appeal.owner_message_id = authorization.owner_message_id.clone();
            appeal.decision_message_id = Some(decision_input.into());
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
