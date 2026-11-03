use super::*;

pub(super) fn finish_message_tx(
    tx: &Transaction<'_>,
    id: &str,
    status: MessageStatus,
    reply: Option<&str>,
    error: Option<&str>,
    suppress_result: bool,
) -> Result<Option<MessageRecord>> {
    let mut message = message_tx(tx, id)?;
    // Explicit Core deliveries are independent conversation messages. A
    // later final model answer closes the input without duplicating them.
    let explicit: bool = sql(tx.query_row("SELECT EXISTS(SELECT 1 FROM messages m WHERE actor_id=?1 AND json_extract(record,'$.kind')='delivery' AND json_extract(record,'$.in_reply_to')=?2 AND EXISTS(SELECT 1 FROM events e WHERE e.actor_id=m.actor_id AND json_extract(e.record,'$.type')='message_added' AND json_extract(e.record,'$.message_id')=m.id))", params![message.actor_id,id], |r|r.get(0)))?;
    let yielded = status == MessageStatus::Completed && turn_yielded(tx, &message.actor_id, id)?;
    let replied =
        status == MessageStatus::Completed && reply_turn_delivered(tx, &message.actor_id, id)?;
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
    // A deliberate yield closes only this inbox input. No placeholder result
    // may wake the requester or escape as an empty remote delivery.
    let routed = if suppress_result
        || (yielded || (replied && matches!(message.reply_to, ReplyTarget::Actor { .. })))
            && status == MessageStatus::Completed
    {
        None
    } else if let ReplyTarget::Actor {
        actor_id,
        request_id,
    } = &message.reply_to
    {
        if let Ok(original) = message_tx(tx, request_id) {
            if original.actor_id == *actor_id && actor_tx(tx, actor_id).is_ok() {
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
                        let Some(actor_directory) = resource.path.parent().and_then(|p| p.parent())
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
                        let destination = delivery.join(actor_id).join("artifacts").join(file_name);
                        std::fs::create_dir_all(destination.parent().unwrap()).into_diagnostic()?;
                        std::fs::copy(&resource.path, &destination).into_diagnostic()?;
                        super::permissions::protect_private_path(&destination)?;
                        let old_uri = resource.uri.clone();
                        resource.actor_id = actor_id.clone();
                        resource.path = destination;
                        resource.uri = format!("/v1/actors/{actor_id}/resources/{}", resource.id);
                        let mut record = serde_json::to_value(&resource).into_diagnostic()?;
                        record["path"] = serde_json::to_value(&resource.path).into_diagnostic()?;
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
                    tx,
                    MessageRecord {
                        skills: Vec::new(),
                        seq: 0,
                        id: result_id,
                        actor_id: actor_id.clone(),
                        source: MessageSource::Actor {
                            actor_id: message.actor_id.clone(),
                        },
                        reply_to: original.reply_to,
                        kind: MessageKind::Result,
                        in_reply_to: Some(original.id.clone()),
                        input_batch: None,
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
    Ok(routed)
}
