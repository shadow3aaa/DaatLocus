//! Core guidance is bound atomically to its original delegated request.
use super::*;

impl ActorStore {
    pub(crate) fn enqueue_core_guidance(
        &self,
        input: &str,
        guide: MessageRecord,
    ) -> Result<MessageRecord> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let core = crate::stage::shared::ACTOR_ID;
            let sender = message_tx(&tx, input)?;
            let target = message_tx(
                &tx,
                guide
                    .in_reply_to
                    .as_deref()
                    .ok_or_else(|| missing("Guidance target"))?,
            )?;
            if sender.actor_id != core
                || sender.status != MessageStatus::Running
                || sender.cancel_requested
                || sender.remote_authority.is_some()
                || !matches!(&guide.source, MessageSource::Actor { actor_id } if actor_id == core)
                || !matches!(&guide.reply_to, ReplyTarget::Actor { actor_id, request_id } if actor_id == core && request_id == input)
                || guide.kind != MessageKind::Steering
                || guide.actor_id != target.actor_id
                || target.kind != MessageKind::Request
                || target.actor_id == core
                || !matches!(&target.source, MessageSource::Actor { actor_id } if actor_id == core)
                || !matches!(&target.reply_to, ReplyTarget::Actor { actor_id, .. } if actor_id == core)
            {
                return Err(ActorError::Forbidden(
                    "Guidance does not belong to this Core delegation".into(),
                )
                .into());
            }
            if !matches!(
                target.status,
                MessageStatus::Queued | MessageStatus::Running
            ) || target.cancel_requested
            {
                return Err(ActorError::Conflict("The selected request has finished or was cancelled; inspect its result before sending new work".into()).into());
            }
            let pending: i64 = sql(tx.query_row("SELECT count(*) FROM messages WHERE actor_id=?1 AND status IN ('\"queued\"','\"running\"')", [&guide.actor_id], |row| row.get(0)))?;
            if pending >= 256 {
                return Err(ActorError::Unavailable("Actor mailbox is full".into()).into());
            }
            let guide = insert_message(&tx, guide)?;
            self.commit(tx)?;
            Ok(guide)
        })
    }
}

/// A Core guide never turns into a new task just because its exact target ended.
pub(super) fn not_applied(
    tx: &Transaction<'_>,
    guide: &mut MessageRecord,
) -> Result<MessageRecord> {
    guide.status = MessageStatus::Cancelled;
    guide.finished_at_ms = Some(now());
    guide.updated_at_ms = now().max(guide.updated_at_ms + 1);
    save_message(tx, guide)?;
    let ReplyTarget::Actor {
        actor_id,
        request_id,
    } = &guide.reply_to
    else {
        return Err(ActorError::Invalid("Core guidance has no original requester".into()).into());
    };
    let original = message_tx(tx, request_id)?;
    insert_message(
        tx,
        MessageRecord {
            seq: 0,
            id: Uuid::new_v4().to_string(),
            actor_id: actor_id.clone(),
            source: MessageSource::Actor {
                actor_id: guide.actor_id.clone(),
            },
            reply_to: original.reply_to,
            kind: MessageKind::Result,
            in_reply_to: Some(original.id),
            delegation_grant: original.delegation_grant,
            remote_authority: original.remote_authority,
            content: format!(
                "Guidance {} was not applied: request {} ended before a safe application boundary. No separate task was queued. Inspect the task result and decide whether new work is needed.\nGuidance:\n{}",
                guide.id,
                guide.in_reply_to.as_deref().unwrap_or_default(),
                guide.content
            ),
            attachments: vec![],
            status: MessageStatus::Queued,
            cancel_requested: false,
            started_at_ms: None,
            finished_at_ms: None,
            final_generation: None,
            reply: None,
            error: None,
            created_at_ms: now(),
            updated_at_ms: now(),
        },
    )
}
