//! Account Actor administration. Installed only on Core; execution revalidates authority.
use crate::{
    actors::ActorSystem,
    app::{
        ActorCallContext, App, AppId, AppToolExecutionContext, AppToolExecutionResult, AppToolSpec,
    },
    model_protocol::AgentToolCall,
    schema_utils::model_schema_for,
};
use async_trait::async_trait;
use daat_locus_macros::model_schema;
use miette::{IntoDiagnostic, Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub struct AdminActorsApp {
    system: ActorSystem,
}
impl AdminActorsApp {
    pub fn new(system: ActorSystem) -> Self {
        Self { system }
    }

    fn caller<'a>(&self, context: &'a AppToolExecutionContext) -> Result<&'a ActorCallContext> {
        let caller = context
            .actor_call
            .as_ref()
            .ok_or_else(|| miette!("Actor call context is required"))?;
        let actor = self.system.record_for_tool(&caller.actor_id, caller)?;
        if actor.role != crate::actors::ActorRole::Session {
            return Err(miette!("Administration requires the session Core"));
        }
        let input = self.system.store().message(&caller.message_id)?;
        if crate::conversation_import::is_import(&input) {
            return Err(miette!("Conversation import cannot administer Actors"));
        }
        Ok(caller)
    }
}
#[model_schema]
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppealDecision {
    Approve,
    Deny,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ActorTargetArgs {
    stage_id: Option<String>,
    actor_id: String,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResolveApprovalAppealArgs {
    stage_id: Option<String>,
    approval_id: String,
    decision: AppealDecision,
    reason: String,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetModelArgs {
    stage_id: Option<String>,
    actor_id: String,
    /// Configured model name; null selects the account main model.
    model: Option<String>,
}
#[async_trait]
impl App for AdminActorsApp {
    fn id(&self) -> AppId {
        AppId::new("admin_actors").expect("static App ID")
    }
    fn can_passivate(&self) -> bool {
        true
    }
    fn output_schema(&self) -> Option<Value> {
        Some(schemars::schema_for!(crate::app::output::ActorOutput).to_value())
    }
    fn tool_specs(&self) -> Vec<AppToolSpec> {
        vec![
            AppToolSpec{name:"stop".into(),description:"Stop a hosted Actor's entire current execution, including buffered messages and running work, and wait for cleanup. Preserve identity, history and completed effects. Core cannot be targeted.".into(),input_schema:model_schema_for::<ActorTargetArgs>()},
            AppToolSpec{name:"archive".into(),description:"Archive a hosted Actor only on an explicit Owner request. Discard queued inputs, leave running work to finish, preserve history and reject new messages. Core cannot be archived.".into(),input_schema:model_schema_for::<ActorTargetArgs>()},
            AppToolSpec{name:"unarchive".into(),description:"Unarchive a hosted Actor only on an explicit Owner request, without replaying discarded inputs.".into(),input_schema:model_schema_for::<ActorTargetArgs>()},
            AppToolSpec{name:"resolve_approval_appeal".into(),description:"Resolve an immutable permission appeal from the user's authorization. Ask when authorization is missing or unclear. Approval remembers the original scopes and notifies the Actor without executing its operation.".into(),input_schema:model_schema_for::<ResolveApprovalAppealArgs>()},
            AppToolSpec{name:"set_model".into(),description:"Change a hosted Actor's model preference. It takes effect before the next inference after the current tool group settles, preserving tasks, history and App state. Cannot change Core's model.".into(),input_schema:model_schema_for::<SetModelArgs>()},
        ]
    }
    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        let caller = self.caller(context)?;
        match call.name.as_str() {
            "resolve_approval_appeal" => {
                let args: ResolveApprovalAppealArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                let decision = match args.decision {
                    AppealDecision::Approve => crate::actors::ApprovalDecision::Approved,
                    AppealDecision::Deny => crate::actors::ApprovalDecision::Denied,
                };
                let target = self.system.approval_stage(args.stage_id.as_deref())?;
                let record = if target
                    != self
                        .system
                        .store()
                        .public_stage_id()
                        .map_err(|e| miette!("{e}"))?
                {
                    use crate::stage::session::{Operation, execute};
                    let value = execute(
                        &self.system,
                        caller,
                        args.stage_id.as_deref(),
                        Operation::AppealRead {
                            id: args.approval_id,
                        },
                    )
                    .await
                    .map_err(|e| miette!("{e}"))?;
                    let record: crate::actors::ApprovalRecord =
                        serde_json::from_value(value).into_diagnostic()?;
                    let appeal = record
                        .appeal
                        .as_ref()
                        .ok_or_else(|| miette!("Approval has no appeal"))?;
                    let notice = self.system.store().message(&appeal.core_message_id)?;
                    let facts: Value = serde_json::from_str(&notice.content).into_diagnostic()?;
                    if notice.actor_id != caller.actor_id
                        || facts["approval_id"] != record.id
                        || facts["stage_id"] != json!(target)
                    {
                        return Err(miette!("Appeal belongs to another session or device"));
                    }
                    let input = self.system.store().message(&caller.message_id)?;
                    if input.remote_authority.is_some() {
                        return Err(miette!("Remote authority cannot resolve appeals"));
                    }
                    let authorization = crate::actors::ApprovalAppealAuthorization {
                        approval_id: record.id.clone(),
                        core_message_id: appeal.core_message_id.clone(),
                        decision_message_id: caller.message_id.clone(),
                        owner_message_id: (input.source == crate::actors::MessageSource::User)
                            .then_some(input.id),
                        scopes_digest: crate::actors::approval::scopes_digest(&record)?,
                        decision,
                        reason: self.system.safe_error(&args.reason),
                    };
                    serde_json::from_value(
                        execute(
                            &self.system,
                            caller,
                            args.stage_id.as_deref(),
                            Operation::AppealApply { authorization },
                        )
                        .await
                        .map_err(|e| miette!("{e}"))?,
                    )
                    .into_diagnostic()?
                } else {
                    self.system.require_local_stage(args.stage_id.as_deref())?;
                    self.system
                        .resolve_appeal_from_core(caller, &args.approval_id, decision, &args.reason)
                        .await?
                };
                Ok(AppToolExecutionResult::new(
                    "Owner appeal decision recorded",
                    json!({"approval_id":record.id,"appeal":record.appeal,"effect_executed":false}),
                    None,
                ))
            }
            "archive" | "unarchive" => {
                let args: ActorTargetArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                if crate::stage::session::target(&self.system, args.stage_id.as_deref())
                    .map_err(|e| miette!("{e}"))?
                    != self
                        .system
                        .store()
                        .public_stage_id()
                        .map_err(|e| miette!("{e}"))?
                {
                    let value = crate::stage::session::execute(
                        &self.system,
                        caller,
                        args.stage_id.as_deref(),
                        crate::stage::session::Operation::Admin {
                            actor: args.actor_id,
                            action: call.name.clone(),
                            model: None,
                        },
                    )
                    .await
                    .map_err(|e| miette!("{e}"))?;
                    return Ok(AppToolExecutionResult::new(
                        "Actor archival updated",
                        value,
                        None,
                    ));
                }
                self.system.require_local_stage(args.stage_id.as_deref())?;
                let actor = self
                    .system
                    .archive_from_core(&args.actor_id, call.name == "archive", caller)
                    .await?;
                Ok(AppToolExecutionResult::new(
                    if call.name == "archive" {
                        "Actor archived"
                    } else {
                        "Actor unarchived"
                    },
                    json!({"actor_id":actor.id,"name":actor.config.name,"status":actor.status,"conversation_url":self.system.conversation_url(&actor.id)?}),
                    None,
                ))
            }
            "stop" => {
                let args: ActorTargetArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                if crate::stage::session::target(&self.system, args.stage_id.as_deref())
                    .map_err(|e| miette!("{e}"))?
                    != self
                        .system
                        .store()
                        .public_stage_id()
                        .map_err(|e| miette!("{e}"))?
                {
                    let value = crate::stage::session::execute(
                        &self.system,
                        caller,
                        args.stage_id.as_deref(),
                        crate::stage::session::Operation::Admin {
                            actor: args.actor_id,
                            action: "stop".into(),
                            model: None,
                        },
                    )
                    .await
                    .map_err(|e| miette!("{e}"))?;
                    return Ok(AppToolExecutionResult::new("Actor stopped", value, None));
                }
                self.system.require_local_stage(args.stage_id.as_deref())?;
                let cancelled_count = self.system.stop_from_core(&args.actor_id, caller).await?;
                Ok(AppToolExecutionResult::new(
                    "Actor stopped",
                    json!({"actor_id":args.actor_id,"cancelled_count":cancelled_count}),
                    None,
                ))
            }

            "set_model" => {
                let args: SetModelArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                if crate::stage::session::target(&self.system, args.stage_id.as_deref())
                    .map_err(|e| miette!("{e}"))?
                    != self
                        .system
                        .store()
                        .public_stage_id()
                        .map_err(|e| miette!("{e}"))?
                {
                    let value = crate::stage::session::execute(
                        &self.system,
                        caller,
                        args.stage_id.as_deref(),
                        crate::stage::session::Operation::Admin {
                            actor: args.actor_id,
                            action: "set_model".into(),
                            model: args.model,
                        },
                    )
                    .await
                    .map_err(|e| miette!("{e}"))?;
                    return Ok(AppToolExecutionResult::new(
                        "Actor model updated",
                        value,
                        None,
                    ));
                }
                self.system.require_local_stage(args.stage_id.as_deref())?;
                let actor = self
                    .system
                    .set_model_from_core(&args.actor_id, args.model, caller)
                    .await?;
                Ok(AppToolExecutionResult::new(
                    "Actor model updated",
                    json!({"actor_id":actor.id,"name":actor.config.name,"model":actor.config.model}),
                    None,
                ))
            }
            _ => Err(miette!("Unknown AdminActors tool")),
        }
    }
}
