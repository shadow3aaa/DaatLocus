use super::*;

pub(super) fn finish_message_tx(
    tx: &Transaction<'_>,
    id: &str,
    status: MessageStatus,
    reply: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    let mut message = message_tx(tx, id)?;
    // Explicit Core deliveries are independent conversation messages. A
    // later final model answer closes the input without duplicating them.
    let explicit: bool = sql(tx.query_row("SELECT EXISTS(SELECT 1 FROM messages m WHERE actor_id=?1 AND json_extract(record,'$.kind')='delivery' AND json_extract(record,'$.in_reply_to')=?2 AND EXISTS(SELECT 1 FROM events e WHERE e.actor_id=m.actor_id AND json_extract(e.record,'$.type')='message_added' AND json_extract(e.record,'$.message_id')=m.id))", params![message.actor_id,id], |r|r.get(0)))?;
    let reply = reply.filter(|reply| !reply.trim().is_empty());
    let reply = if message.actor_id == crate::stage::shared::ACTOR_ID
        && explicit
        && status == MessageStatus::Completed
    {
        None
    } else {
        reply
    };
    if !matches!(
        message.status,
        MessageStatus::Running | MessageStatus::Queued
    ) {
        return Ok(());
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
    save_message(tx, &message)?;
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
            tx,
            &message_tx(tx, &approval.message_id)?.actor_id,
            ActorEvent::ApprovalResolved {
                approval_id: approval.id,
                message_id: approval.message_id,
                decision: ApprovalDecision::Cancelled,
                reason: approval.reason,
            },
        )?;
    }
    if was_running {
        let mut actor = actor_tx(tx, &message.actor_id)?;
        if actor.status != ActorStatus::Archived {
            let running: bool = sql(tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE actor_id=?1 AND status='\"running\"')",
                [&message.actor_id],
                |row| row.get(0),
            ))?;
            actor.status = if running {
                ActorStatus::Running
            } else {
                ActorStatus::Idle
            };
            actor.updated_at_ms = now();
            save_actor(tx, &actor)?;
        }
    }
    Ok(())
}
