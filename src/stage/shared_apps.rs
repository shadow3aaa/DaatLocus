//! The account Actor composes portable Apps using their unchanged schemas. Device tools
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
    admin_actors_app::AdminActorsApp,
    app::{App, AppId, AppManager, AppToolExecutionContext, AppToolExecutionResult, AppToolSpec},
    model_protocol::AgentToolCall,
};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

struct AccountApp {
    controller: Arc<Controller>,
    local: Box<dyn App>,
    actor: ActorRecord,
    system: ActorSystem,
}
#[async_trait]
impl App for AccountApp {
    fn can_passivate(&self) -> bool {
        true
    }
    fn id(&self) -> AppId {
        self.local.id()
    }
    fn output_schema(&self) -> Option<serde_json::Value> {
        self.local.output_schema()
    }
    fn tool_specs(&self) -> Vec<AppToolSpec> {
        self.local.tool_specs()
    }

    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> miette::Result<AppToolExecutionResult> {
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
        if matches!(
            call.name.as_str(),
            "read_history"
                | "read_resource"
                | "reply"
                | "yield"
                | "appeal_approval"
                | "resolve_approval_appeal"
                | "devices"
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
        if matches!(call.name.as_str(), "status" | "cancel")
            && stage == self.system.store().public_stage_id()?
            && call
                .arguments
                .get("message_id")
                .and_then(|id| id.as_str())
                .and_then(|id| self.system.store().message(id).ok())
                .is_some_and(|input| input.actor_id == shared::ACTOR_ID)
        {
            let mut local = call.clone();
            local.arguments["stage_id"] = serde_json::Value::Null;
            return self.local.execute_tool(&local, context).await;
        }
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
        let app_id = self.local.id().to_string();
        let installation = apps
            .into_iter()
            .find(|app| {
                app.stage_id == stage
                    && app.app == app_id
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
                    app: app_id,
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
    let local: Vec<Box<dyn App>> = vec![
        Box::new(ActorApp::new(shared::ACTOR_ID.into(), system.clone())),
        Box::new(AdminActorsApp::new(system.clone())),
    ];
    AppManager::new(
        local
            .into_iter()
            .map(|local| {
                Box::new(AccountApp {
                    controller: controller.clone(),
                    local,
                    actor: actor.clone(),
                    system: system.clone(),
                }) as Box<dyn App>
            })
            .collect(),
    )
}
