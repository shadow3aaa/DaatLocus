//! Account-wide review stays on the current Core worker. Device-local grants
//! use bounded native control messages, without invoking device Actor Apps.
use super::*;
use crate::{
    actors::{
        ApprovalDecision, ApprovalRecord,
        approval::{ReviewFailure, ReviewLogContext, ReviewVerdict},
    },
    app::ActorCallContext,
};

impl Controller {
    pub(crate) async fn submit_approval_application(
        &self,
        caller: &ActorCallContext,
        stage: StageId,
        source: crate::actors::runtime_permissions::ApplicationSource,
        application: crate::actors::ReviewApplication,
    ) -> Result<ApprovalRecord> {
        let runtime = self.runtime()?;
        runtime
            .host
            .system
            .record_for_tool(shared::ACTOR_ID, caller)
            .map_err(|_| StageError::Forbidden("Core request is no longer active"))?;
        if !self.execution_permitted() {
            return Err(StageError::Forbidden("Core worker changed"));
        }
        match self
            .request(
                stage,
                Request::SubmitApproval {
                    epoch: self.epoch(),
                    message: caller.message_id.clone(),
                    source,
                    application,
                },
            )
            .await?
        {
            Response::ApprovalSubmitted { record } => Ok(*record),
            _ => Err(StageError::Invalid("Invalid approval application response")),
        }
    }

    pub(super) async fn handle_approval_application(
        &self,
        peer: StageId,
        epoch: u64,
        message: &str,
        source: crate::actors::runtime_permissions::ApplicationSource,
        application: crate::actors::ReviewApplication,
    ) -> Result<Response> {
        self.require_worker(peer, epoch).await?;
        let runtime = self.runtime()?;
        let system = &runtime.host.system;
        let actor = system
            .core_device_actor()
            .map_err(|_| StageError::Unavailable("Device policy unavailable"))?;
        let input = system
            .store()
            .message(message)
            .map_err(|_| StageError::Forbidden("Approval request is unavailable"))?;
        let context = system
            .execution_context_with_policy(&actor, &input, Some(actor.config.clone()))
            .map_err(|_| StageError::Forbidden("Approval authority is unavailable"))?;
        let control = ActorControl::default();
        let future = crate::actors::runtime_permissions::submit(
            system,
            &actor,
            &input,
            &control,
            &context,
            source,
            application,
        );
        tokio::pin!(future);
        loop {
            tokio::select! {
                result = &mut future => return result.map(|record|Response::ApprovalSubmitted {record:Box::new(record)})
                    .map_err(|error|crate::actors::approval::rejection(&error).map(StageError::ApprovalDenied)
                        .unwrap_or(StageError::Forbidden("Approval application could not be completed"))),
                () = tokio::time::sleep(Duration::from_millis(25)) => {
                    if !self.fence.lock().permits(peer,epoch,Instant::now()) {
                        control.stop.store(true,Ordering::Release);
                    }
                }
            }
        }
    }

    pub(crate) async fn review_approval(
        &self,
        facts: String,
    ) -> Result<std::result::Result<ReviewVerdict, ReviewFailure>> {
        let (leader, epoch) = {
            let fence = self.fence.lock();
            let leader = fence.leader(Instant::now()).ok_or_else(|| {
                tracing::warn!(
                    failure = "core_worker_unavailable",
                    "Automatic review RPC failed: Core worker is unavailable for approval"
                );
                StageError::Unavailable("Core worker is unavailable for approval")
            })?;
            (leader, fence.epoch())
        };
        let response = self
            .request(leader, Request::ReviewApproval { epoch, facts })
            .await
            .map_err(|error| {
                tracing::warn!(
                    leader = %leader,
                    failure = "rpc_failed",
                    error = %error,
                    "Automatic review RPC failed"
                );
                error
            })?;
        match response {
            Response::ApprovalReview { verdict } => {
                if let Err(failure) = &verdict {
                    tracing::warn!(
                        leader = %leader,
                        failure = failure.code(),
                        "Automatic review model request failed on the Core worker"
                    );
                }
                Ok(verdict)
            }
            _ => {
                tracing::warn!(
                    leader = %leader,
                    failure = "invalid_response",
                    "Automatic review RPC returned an invalid response"
                );
                Err(StageError::Invalid("Invalid approval review response"))
            }
        }
    }

    pub(super) async fn handle_approval_review(
        &self,
        epoch: u64,
        facts: String,
    ) -> Result<Response> {
        let runtime = self.runtime()?;
        self.require_worker(runtime.host.local, epoch).await?;
        let future = crate::actors::approval::review_on_core(
            &runtime.host.system,
            facts,
            ReviewLogContext::default(),
        );
        tokio::pin!(future);
        loop {
            tokio::select! {
                verdict = &mut future => {
                    tracing::debug!(stage = %runtime.host.local, "Automatic review request completed");
                    return Ok(Response::ApprovalReview{verdict});
                }
                () = tokio::time::sleep(Duration::from_millis(250)) => {
                    if !self.fence.lock().permits(runtime.host.local,epoch,Instant::now()) {
                        tracing::warn!(
                            stage = %runtime.host.local,
                            failure = "core_worker_changed",
                            "Automatic review aborted: Core worker changed"
                        );
                        return Err(StageError::Forbidden("Core worker changed during review"));
                    }
                }
            }
        }
    }

    pub(crate) async fn resolve_approval_appeal(
        &self,
        caller: &ActorCallContext,
        stage: StageId,
        approval_id: String,
        decision: ApprovalDecision,
        reason: String,
    ) -> Result<ApprovalRecord> {
        let runtime = self.runtime()?;
        let Response::ApprovalAppeal { record } = self
            .request(
                stage,
                Request::ApprovalAppeal {
                    epoch: self.epoch(),
                    approval_id,
                },
            )
            .await?
        else {
            return Err(StageError::Invalid("Invalid source appeal response"));
        };
        let authorization = runtime
            .host
            .system
            .prepare_appeal_authorization(caller, &record, decision, &reason)
            .map_err(|_| StageError::Forbidden("Appeal requires current account Core authority"))?;
        if !self.execution_permitted() {
            return Err(StageError::Forbidden(
                "Core worker changed before appeal resolution",
            ));
        }
        match self
            .request(
                stage,
                Request::ApplyApprovalAppeal {
                    epoch: self.epoch(),
                    authorization,
                },
            )
            .await?
        {
            Response::ApprovalAppeal { record } => Ok(*record),
            _ => Err(StageError::Invalid("Invalid appeal resolution response")),
        }
    }
}
