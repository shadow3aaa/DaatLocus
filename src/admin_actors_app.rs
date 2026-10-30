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
        self.system
            .record_for_tool(crate::stage::shared::ACTOR_ID, caller)?;
        let input = self.system.store().message(&caller.message_id)?;
        if crate::conversation_import::is_import(&input) {
            return Err(miette!("Conversation import cannot administer Actors"));
        }
        Ok(caller)
    }
    fn require_local_stage(&self, stage: Option<&str>) -> Result<()> {
        if let Some(stage) = stage {
            let local = self
                .system
                .store()
                .public_stage_id()
                .map_err(|error| miette!("{error}"))?;
            if stage != local.to_string() {
                return Err(miette!("Operation targets another device"));
            }
        }
        Ok(())
    }
    fn approval_stage(&self, stage: Option<&str>) -> Result<crate::stage::StageId> {
        stage.map_or_else(
            || {
                self.system
                    .store()
                    .public_stage_id()
                    .map_err(|error| miette!("{error}"))
            },
            |stage| {
                serde_json::from_value(json!(stage))
                    .map_err(|_| miette!("Invalid approval source device"))
            },
        )
    }
    fn conversation_url(&self, actor: &str) -> Result<String> {
        Ok(crate::stage::ActorAddress {
            stage_id: self
                .system
                .store()
                .public_stage_id()
                .map_err(|error| miette!("{error}"))?,
            actor_id: actor.into(),
        }
        .conversation_url())
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
            AppToolSpec{name:"stop".into(),description:"Stop a hosted Actor's entire current execution, including pending and suspended inputs, and wait for cleanup. Preserve identity, history and completed effects. Core cannot be targeted.".into(),input_schema:model_schema_for::<ActorTargetArgs>()},
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
                let record = if let Some(core) = self.system.shared_core() {
                    core.resolve_approval_appeal(
                        caller,
                        self.approval_stage(args.stage_id.as_deref())?,
                        args.approval_id,
                        decision,
                        args.reason,
                    )
                    .await
                    .map_err(|error| miette!("{error}"))?
                } else {
                    self.require_local_stage(args.stage_id.as_deref())?;
                    self.system
                        .resolve_appeal_from_core(caller, &args.approval_id, decision, &args.reason)
                        .await?
                };
                if record.source_actor(self.system.store())? == crate::stage::shared::ACTOR_ID {
                    self.system.checkpoint_core().await?;
                }
                Ok(AppToolExecutionResult::new(
                    "Owner appeal decision recorded",
                    json!({"approval_id":record.id,"appeal":record.appeal,"effect_executed":false}),
                    None,
                ))
            }
            "archive" | "unarchive" => {
                let args: ActorTargetArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                self.require_local_stage(args.stage_id.as_deref())?;
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
                    json!({"actor_id":actor.id,"name":actor.config.name,"status":actor.status,"conversation_url":self.conversation_url(&actor.id)?}),
                    None,
                ))
            }
            "stop" => {
                let args: ActorTargetArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                self.require_local_stage(args.stage_id.as_deref())?;
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
                self.require_local_stage(args.stage_id.as_deref())?;
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
