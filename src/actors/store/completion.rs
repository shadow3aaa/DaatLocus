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
    let reply = if actor_tx(tx, &message.actor_id)?.role == ActorRole::Session
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
    notify_requester(tx, &message)?;
    Ok(())
}

/// A delegated request that ends without an explicit answer leaves its requester
/// waiting. Queue a runtime notice with the outcome only, never final model text.
fn notify_requester(tx: &Transaction<'_>, message: &MessageRecord) -> Result<()> {
    let ReplyTarget::Actor {
        actor_id: requester,
        request_id,
    } = &message.reply_to
    else {
        return Ok(());
    };
    let MessageSource::Actor { actor_id: sender } = &message.source else {
        return Ok(());
    };
    if message.kind != MessageKind::Request
        || sender != requester
        || is_appeal_notice(&message.content)
    {
        return Ok(());
    }
    let kinds: &[&str] = match message.status {
        MessageStatus::Failed | MessageStatus::Interrupted => &["result"],
        // Core may defer its answer until the user decides.
        MessageStatus::Completed if actor_tx(tx, &message.actor_id)?.role != ActorRole::Session => {
            &["progress", "result"]
        }
        _ => return Ok(()),
    };
    let Ok(requester_record) = actor_tx(tx, requester) else {
        return Ok(());
    };
    let Ok(original) = message_tx(tx, request_id) else {
        return Ok(());
    };
    if sessions::ensure_admission(tx, &requester_record).is_err() || original.actor_id != *requester
    {
        return Ok(());
    }
    // One answer or notice since this input started covers coalesced inputs.
    let since = message.started_at_ms.unwrap_or(message.created_at_ms);
    let answered: bool = sql(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE actor_id=?1
            AND json_extract(record,'$.source.kind')='actor' AND json_extract(record,'$.source.actor_id')=?2
            AND json_extract(record,'$.kind') IN (SELECT value FROM json_each(?3))
            AND json_extract(record,'$.created_at_ms')>=?4)",
        params![requester, message.actor_id, encode(&kinds)?, since],
        |row| row.get(0),
    ))?;
    if answered {
        return Ok(());
    }
    let name = actor_tx(tx, &message.actor_id)?.config.name;
    let error: String = message
        .error
        .as_deref()
        .unwrap_or("unknown error")
        .chars()
        .take(300)
        .collect();
    let content = match message.status {
        MessageStatus::Failed => format!(
            "Runtime notice: {name} could not finish your request ({error}). Read its conversation for completed work before deciding next steps."
        ),
        MessageStatus::Interrupted => format!(
            "Runtime notice: {name}'s work on your request was interrupted ({error}). Completed effects remain and the work was not resumed. Read its conversation before deciding next steps."
        ),
        _ => format!(
            "Runtime notice: {name} finished your request without replying. Read its conversation for the outcome."
        ),
    };
    let at = now();
    insert_message(
        tx,
        MessageRecord {
            skills: Vec::new(),
            seq: 0,
            id: Uuid::new_v4().to_string(),
            actor_id: requester.clone(),
            source: MessageSource::Actor {
                actor_id: message.actor_id.clone(),
            },
            reply_to: original.reply_to,
            kind: MessageKind::Result,
            in_reply_to: Some(original.id),
            remote_authority: original.remote_authority,
            content,
            attachments: vec![],
            status: MessageStatus::Queued,
            cancel_requested: false,
            started_at_ms: None,
            finished_at_ms: None,
            final_generation: None,
            reply: None,
            error: None,
            created_at_ms: at,
            updated_at_ms: at,
        },
    )?;
    Ok(())
}
