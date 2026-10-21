//! The account Actor exposes one portable Actors App. Device execution tools
//! belong to hosted workers and cannot be invoked through this interface.
use super::{
    StageId,
    core_wire::{Request, Response},
    shared,
    shared_runtime::Controller,
};
use crate::{
    actor_app::ActorApp,
    actors::{ActorRecord, ActorSystem},
    app::{
        App, AppDocs, AppId, AppManager, AppToolExecutionContext, AppToolExecutionResult,
        AppToolSpec, EmptyAppArgs,
    },
    model_protocol::AgentToolCall,
    schema_utils::model_schema_for,
};
use async_trait::async_trait;
use daat_locus_macros::model_schema;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;

#[model_schema]
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    stage_id: String,
    name: String,
    workspace: Option<String>,
    instructions: Option<String>,
    model: Option<String>,
}
#[model_schema]
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImportArgs {
    stage_id: String,
    resource_id: String,
    name: String,
    instructions: String,
}
#[model_schema]
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    stage_id: String,
    cursor: Option<usize>,
    limit: Option<usize>,
}
#[model_schema]
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendArgs {
    stage_id: String,
    actor_id: String,
    content: String,
}
#[model_schema]
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RequestArgs {
    stage_id: String,
    message_id: String,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SteerArgs {
    stage_id: String,
    message_id: String,
    content: String,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ArchiveArgs {
    stage_id: String,
    actor_id: String,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResolveAppealArgs {
    stage_id: String,
    approval_id: String,
    decision: crate::actor_app::AppealDecision,
    reason: String,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AppealArgs {
    stage_id: String,
    approval_id: String,
    explanation: String,
}

struct AccountActors {
    controller: Arc<Controller>,
    local: ActorApp,
    actor: ActorRecord,
    system: ActorSystem,
}
#[async_trait]
impl App for AccountActors {
    fn can_passivate(&self) -> bool {
        true
    }
    fn id(&self) -> AppId {
        AppId::new("actors").expect("Actors App")
    }
    fn docs(&self) -> AppDocs {
        AppDocs::from_markdown(concat!(
            include_str!("../app_instructions/core.md"),
            "\n\n",
            include_str!("../app_instructions/orchestration.md")
        ))
    }
    fn delegates_approval(&self, call: &AgentToolCall) -> bool {
        matches!(
            call.name.as_str(),
            "list"
                | "create"
                | "import_conversation"
                | "send"
                | "steer"
                | "status"
                | "cancel"
                | "archive"
                | "unarchive"
        )
    }
    fn tool_specs(&self) -> Vec<AppToolSpec> {
        let mut specs = self.local.tool_specs();
        for spec in &mut specs {
            spec.input_schema = match spec.name.as_str() {
                "list" => model_schema_for::<ListArgs>(),
                "create" => model_schema_for::<CreateArgs>(),
                "import_conversation" => model_schema_for::<ImportArgs>(),
                "send" => model_schema_for::<SendArgs>(),
                "steer" => model_schema_for::<SteerArgs>(),
                "status" | "cancel" => model_schema_for::<RequestArgs>(),
                "archive" | "unarchive" => model_schema_for::<ArchiveArgs>(),
                "resolve_approval_appeal" => model_schema_for::<ResolveAppealArgs>(),
                "appeal_approval" => model_schema_for::<AppealArgs>(),
                _ => continue,
            };
            spec.description = spec
                .description
                .replace(
                    "Null stage_id selects the current device.",
                    "Use the explicit stage_id returned by devices.",
                )
                .replace(
                    "Null stage_id selects the current device;",
                    "Use an explicit stage_id;",
                );
        }
        specs.push(AppToolSpec {name:"devices".into(), description:"List authorized account devices and actual connectivity. Select an explicit stage_id for hosted Actor operations. Offline targets are never silently moved.".into(),input_schema:model_schema_for::<EmptyAppArgs>()});
        specs
    }
    fn before_tool_call(
        &self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> miette::Result<()> {
        let caller = context
            .actor_call
            .as_ref()
            .ok_or_else(|| miette::miette!("Missing Core input"))?;
        if caller.actor_id != shared::ACTOR_ID
            || !self.controller.execution_permitted()
            || !self.tool_specs().iter().any(|spec| spec.name == call.name)
        {
            return Err(miette::miette!(
                "Core tool or execution epoch is no longer authorized"
            ));
        }
        self.system.record_for_tool(shared::ACTOR_ID, caller)?;
        self.local.check_import_operation(call, context)?;
        Ok(())
    }
    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> miette::Result<AppToolExecutionResult> {
        self.before_tool_call(call, context)?;
        if call.name == "devices" {
            return Ok(AppToolExecutionResult::new(
                "Account devices",
                self.controller
                    .actor_devices()
                    .map_err(|error| miette::miette!("{error}"))?,
                None,
            ));
        }
        if matches!(
            call.name.as_str(),
            "read_history"
                | "read_resource"
                | "deliver"
                | "yield"
                | "appeal_approval"
                | "resolve_approval_appeal"
        ) {
            return self.local.execute_tool(call, context).await;
        }
        let stage: StageId = serde_json::from_value(
            call.arguments
                .get("stage_id")
                .cloned()
                .ok_or_else(|| miette::miette!("Select an explicit device"))?,
        )
        .map_err(|_| miette::miette!("Invalid target device"))?;
        let Response::Catalog { apps } = self
            .controller
            .request(
                stage,
                Request::Catalog {
                    epoch: self.controller.epoch(),
                },
            )
            .await
            .map_err(|error| miette::miette!("{error}"))?
        else {
            return Err(miette::miette!("Device Actor capabilities unavailable"));
        };
        let installation = apps
            .into_iter()
            .find(|app| {
                app.stage_id == stage
                    && app.app == "actors"
                    && app.tools.iter().any(|tool| tool.name == call.name)
            })
            .ok_or_else(|| miette::miette!("Device does not permit this Actor operation"))?;
        let mut routed = call.clone();
        routed.arguments["stage_id"] = serde_json::Value::Null;
        let caller = context.actor_call.as_ref().expect("validated input");
        match self
            .controller
            .request(
                stage,
                Request::Invoke {
                    epoch: self.controller.epoch(),
                    message: caller.message_id.clone(),
                    app: "actors".into(),
                    revision: installation.revision,
                    call: routed,
                    identity_read_only: self.actor.remote_read_only,
                },
            )
            .await
            .map_err(miette::Report::new)?
        {
            Response::ToolResult {
                result: Some(mut result),
                uncertain: false,
            } => {
                self.controller
                    .import_images(stage, &result.model_images)
                    .await
                    .map_err(|error| miette::miette!("{error}"))?;
                if let Some(payload) = result.payload.as_object_mut() {
                    payload.insert("stage_id".into(), json!(stage));
                }
                Ok(result)
            }
            _ => Err(miette::miette!(
                "Actor operation result is unknown; inspect its original device before proposing another action"
            )),
        }
    }
}
pub fn manager(
    controller: Arc<Controller>,
    actor: ActorRecord,
    system: ActorSystem,
) -> miette::Result<AppManager> {
    AppManager::new(vec![Box::new(AccountActors {
        controller,
        local: ActorApp::new(shared::ACTOR_ID.into(), system.clone()),
        actor,
        system,
    })])
}
