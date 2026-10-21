use super::*;

impl ActorStore {
    /// Bind explanatory metadata once, without accepting replacement App arguments or scopes.
    pub(crate) fn submit_review_application(
        &self,
        actor: &str,
        message: &str,
        id: &str,
        application: &ReviewApplication,
    ) -> Result<ApprovalRecord> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut record: ApprovalRecord = decode(
                sql(tx
                    .query_row("SELECT record FROM approvals WHERE id=?1", [id], |row| {
                        row.get(0)
                    })
                    .optional())?
                .ok_or_else(|| missing("Approval"))?,
            )?;
            let input = message_tx(&tx, message)?;
            if record.actor_id != actor
                || record.message_id != message
                || input.actor_id != actor
                || input.status != MessageStatus::Running
                || input.cancel_requested
            {
                return Err(ActorError::Forbidden(
                    "Approval reference is outside the active Actor request".into(),
                )
                .into());
            }
            if let Some(previous) = &record.application {
                if previous != application {
                    return Err(ActorError::Conflict(
                        "Submitted approval application is immutable".into(),
                    )
                    .into());
                }
                if record.decision == ApprovalDecision::Pending {
                    return Err(ActorError::Conflict(
                        "This approval review is already in progress".into(),
                    )
                    .into());
                }
                return Ok(record);
            }
            if record.decision != ApprovalDecision::ApplicationRequired {
                return Err(
                    ActorError::Conflict("Approval is not awaiting an application".into()).into(),
                );
            }
            record.application = Some(application.clone());
            record.decision = ApprovalDecision::Pending;
            record.reason = "Permission review requested".into();
            sql(tx.execute(
                "UPDATE approvals SET record=?1 WHERE id=?2",
                params![encode(&record)?, id],
            ))?;
            event(
                &tx,
                actor,
                ActorEvent::ApprovalRequested {
                    approval_id: record.id.clone(),
                    message_id: record.message_id.clone(),
                    call_id: record.call.id.clone(),
                    name: record.call.name.clone(),
                },
            )?;
            self.commit(tx)?;
            Ok(record)
        })
    }
}
