use super::*;
use crate::sandbox::SandboxScopeRequest;

impl ActorStore {
    /// Reusing a result reference reuses its immutable application, not another
    /// automatic review. Call IDs are source-event identity, not action hashes.
    pub(crate) fn sandbox_application_for_call(
        &self,
        message: &str,
        call: &str,
        requested: &SandboxScopeRequest,
    ) -> Result<Option<ApprovalRecord>> {
        database_io(|| sandbox_application(&self.db.lock(), message, call, requested))
    }
    /// Bind explanatory metadata once, without accepting replacement App arguments or scopes.
    pub(crate) fn submit_review_application(
        &self,
        actor: &str,
        message: &str,
        id: &str,
        application: &ReviewApplication,
    ) -> Result<(ApprovalRecord, bool)> {
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
            if record.message_id != message
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
                return Ok((record, false));
            }
            if record.decision != ApprovalDecision::ApplicationRequired {
                return Err(
                    ActorError::Conflict("Approval is not awaiting an application".into()).into(),
                );
            }
            record.application = Some(application.clone());
            let claimed = record.previous_denials.is_empty();
            if claimed {
                record.decision = ApprovalDecision::Pending;
                record.reason = "Permission review requested".into();
            }
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
            Ok((record, claimed))
        })
    }
}

/// Source-event lookup shared by optimistic reads and transactional creation.
pub(super) fn sandbox_application(
    db: &Connection,
    message: &str,
    call: &str,
    requested: &SandboxScopeRequest,
) -> Result<Option<ApprovalRecord>> {
    let requested_scopes = requested
        .readable_roots
        .iter()
        .map(|path| SandboxScope::filesystem(path, false))
        .chain(
            requested
                .writable_roots
                .iter()
                .map(|path| SandboxScope::filesystem(path, true)),
        )
        .collect::<Result<Vec<_>>>()?;
    let mut statement = sql(db.prepare("SELECT record FROM approvals WHERE message_id=?1 AND json_extract(record,'$.call.id')=?2 ORDER BY seq"))?;
    let rows = sql(statement.query_map(params![message, call], |row| row.get::<_, String>(0)))?;
    for row in rows {
        let record: ApprovalRecord = decode(sql(row)?)?;
        if record.sudo.is_some() {
            continue;
        }
        if record.requested == *requested
            || record.decision == ApprovalDecision::Approved
                && requested_scopes
                    .iter()
                    .all(|scope| record.scopes.iter().any(|saved| saved.covers(scope)))
        {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

pub(super) fn overlapping_denials(
    db: &Connection,
    message: &str,
    call: &str,
    scopes: &[SandboxScope],
) -> Result<Vec<String>> {
    let mut statement = sql(db.prepare("SELECT record FROM approvals WHERE message_id=?1 AND json_extract(record,'$.call.id')=?2 ORDER BY seq"))?;
    let rows = sql(statement.query_map(params![message, call], |row| row.get::<_, String>(0)))?;
    let mut ids = Vec::new();
    for row in rows {
        let record: ApprovalRecord = decode(sql(row)?)?;
        let core_decision = record.appeal.as_ref().map(|appeal| appeal.decision);
        let refused = core_decision == Some(ApprovalDecision::Denied)
            || record.decision == ApprovalDecision::Denied
                && core_decision != Some(ApprovalDecision::Approved);
        if refused
            && scopes
                .iter()
                .any(|scope| record.scopes.iter().any(|denied| denied.overlaps(scope)))
        {
            ids.push(record.id);
        }
    }
    Ok(ids)
}
