//! Claim one reviewed privileged invocation before effects; never replay it.
use super::*;
impl ActorStore {
    pub(crate) fn sudo_application_for_call(
        &self,
        message: &str,
        call: &str,
    ) -> Result<Option<ApprovalRecord>> {
        database_io(|| {
            let db = self.db.lock();
            let mut query = sql(db.prepare("SELECT record FROM approvals WHERE message_id=?1 AND json_extract(record,'$.call.id')=?2 AND json_type(record,'$.sudo')='object' ORDER BY seq LIMIT 1"))?;
            sql(query
                .query_row(params![message, call], |row| row.get::<_, String>(0))
                .optional())?
            .map(decode)
            .transpose()
        })
    }
    pub(crate) fn claim_sudo(
        &self,
        id: &str,
        input: &str,
        call: &str,
        invocation: &crate::sudo_app::Invocation,
    ) -> Result<()> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut record: ApprovalRecord = decode(sql(tx.query_row(
                "SELECT record FROM approvals WHERE id=?1",
                [id],
                |row| row.get(0),
            ))?)?;
            let original = message_tx(&tx, &record.message_id)?;
            let current = message_tx(&tx, input)?;
            let approved = record
                .appeal
                .as_ref()
                .map_or(record.decision, |appeal| appeal.decision)
                == ApprovalDecision::Approved;
            if !approved
                || record.sudo.as_ref() != Some(invocation)
                || record.execution_claim.is_some()
                || current.actor_id != original.actor_id
                || current.status != MessageStatus::Running
                || current.cancel_requested
                || original.cancel_requested
                || matches!(
                    original.status,
                    MessageStatus::Cancelled | MessageStatus::Interrupted | MessageStatus::Failed
                )
                || current.remote_authority.is_some()
                || original.remote_authority.is_some()
                || actor_tx(&tx, &current.actor_id)?.status == ActorStatus::Archived
                || crate::actors::permissions::canonical_path(&invocation.cwd)? != invocation.cwd
            {
                return Err(ActorError::Forbidden(
                    "Sudo approval is not valid for this invocation or has already been consumed"
                        .into(),
                )
                .into());
            }
            let native: bool = sql(tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM stage_local_approvals WHERE id=?1)",
                [id],
                |row| row.get(0),
            ))?;
            if !native {
                return Err(ActorError::Forbidden(
                    "Sudo approval belongs to another execution device".into(),
                )
                .into());
            }
            record.execution_claim = Some(SudoExecutionClaim {
                message_id: input.into(),
                call_id: call.into(),
            });
            sql(tx.execute(
                "UPDATE approvals SET record=?1 WHERE id=?2",
                params![encode(&record)?, id],
            ))?;
            self.commit(tx)
        })
    }
}
