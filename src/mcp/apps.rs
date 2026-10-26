use super::*;
use crate::{
    app::{App, AppId, AppToolExecutionResult, AppToolSpec, EmptyAppArgs},
    model_protocol::AgentToolCall,
    schema_utils::{model_schema_for, validate_model_facing_schema},
};
use async_trait::async_trait;
use daat_locus_macros::model_schema;
use schemars::JsonSchema;
use std::sync::atomic::Ordering;

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InspectArgs {
    id: String,
    tool: Option<String>,
}
#[model_schema]
#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TransportKind {
    Http,
    Stdio,
}
#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InstallArgs {
    name: String,
    description: Option<String>,
    transport: TransportKind,
    url: Option<String>,
    command: Option<String>,
    arguments: Vec<String>,
}
impl InstallArgs {
    fn source(&self) -> Result<Source> {
        let source = match self.transport {
            TransportKind::Http if self.command.is_none() && self.arguments.is_empty() => {
                Source::Http {
                    url: self
                        .url
                        .clone()
                        .ok_or_else(|| miette!("MCP URL is required"))?,
                    bearer_token: None,
                }
            }
            TransportKind::Stdio if self.url.is_none() => Source::Stdio {
                command: self
                    .command
                    .clone()
                    .ok_or_else(|| miette!("MCP command is required"))?,
                arguments: self.arguments.clone(),
                environment: Default::default(),
            },
            _ => {
                return Err(miette!(
                    "Choose http or stdio and provide only its connection fields"
                ));
            }
        };
        source.validate()?;
        Ok(source)
    }
}
#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ChangeArgs {
    id: String,
    expected_revision: u64,
}
#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct JsonArguments {
    arguments_json: String,
}

pub struct AppsManager {
    registry: Arc<Registry>,
    actor: String,
}
impl AppsManager {
    pub fn new(registry: Arc<Registry>, actor: String) -> Self {
        Self { registry, actor }
    }
}
fn result(summary: &str, value: Value) -> AppToolExecutionResult {
    AppToolExecutionResult::new(summary, value.clone(), Some(value.to_string()))
}
fn spec<T: crate::schema_utils::ModelSchema>(name: &str, description: &str) -> AppToolSpec {
    AppToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema: model_schema_for::<T>(),
    }
}
fn args<T: serde::de::DeserializeOwned>(call: &AgentToolCall) -> Result<T> {
    serde_json::from_value(call.arguments.clone()).map_err(|_| miette!("Invalid App arguments"))
}
#[async_trait]
impl App for AppsManager {
    fn id(&self) -> AppId {
        AppId::new("apps_manager").unwrap()
    }
    fn can_passivate(&self) -> bool {
        true
    }
    fn tool_specs(&self) -> Vec<AppToolSpec> {
        vec![
            spec::<EmptyAppArgs>(
                "list",
                "List installed MCP Apps and the device catalog revision. Does not connect to servers.",
            ),
            spec::<InspectArgs>(
                "inspect",
                "Inspect an installed App. Set tool to its original tool name to read the complete original MCP schema; null lists tool names.",
            ),
            spec::<EmptyAppArgs>(
                "refresh",
                "Refresh this Actor's device-provided Apps after the current tool group completes, preserving the current request and existing App resources.",
            ),
            spec::<InstallArgs>(
                "install",
                "Install an MCP App on this device. Connections and native commands use the device sandbox. Never put credentials in the name or command arguments.",
            ),
            spec::<ChangeArgs>(
                "enable",
                "Enable an installed MCP App using the revision returned by list or inspect.",
            ),
            spec::<ChangeArgs>(
                "close",
                "Disable an installed MCP App on this device. Blocks new calls; already started calls may finish. Does not delete history or data.",
            ),
        ]
    }

    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        if context
            .actor_call
            .as_ref()
            .is_none_or(|c| c.actor_id != self.actor)
        {
            return Err(miette!("Actor identity mismatch"));
        }
        if matches!(call.name.as_str(), "install" | "enable" | "close")
            && context
                .actor_call
                .as_ref()
                .is_some_and(|caller| caller.remote_read_only)
        {
            return Err(miette!(
                "Remote read-only authority cannot change device Apps"
            ));
        }
        match call.name.as_str() {
            "install" => {
                args::<InstallArgs>(call)?.source()?;
            }
            "enable" | "close" => {
                let a = args::<ChangeArgs>(call)?;
                if self.registry.get(&a.id)?.revision != a.expected_revision {
                    return Err(miette!("App changed; inspect it again"));
                }
            }
            _ => {}
        }
        match call.name.as_str() {
            "list" => Ok(result(
                "Installed Apps",
                json!({"revision":self.registry.revision(),"apps":self.registry.actor_entries(&self.actor)}),
            )),
            "inspect" => {
                let a = args::<InspectArgs>(call)?;
                let entry = self.registry.get(&a.id)?;
                let value = if let Some(name) = a.tool {
                    json!({"app":entry.public(false),"tool":entry.tools.iter().find(|t|t.name==name).ok_or_else(||miette!("MCP tool not found"))?})
                } else {
                    json!({"app":entry.public(false),"tools":entry.tools.iter().map(|t|json!({"name":t.name,"local_name":t.local_name,"description":t.description})).collect::<Vec<_>>()})
                };
                Ok(result("App details", value))
            }
            "refresh" => {
                self.registry.request_refresh(&self.actor);
                Ok(result(
                    "App refresh scheduled",
                    json!({"status":"scheduled","revision":self.registry.revision(),"boundary":"after_current_tool_group"}),
                ))
            }
            "install" => {
                let a = args::<InstallArgs>(call)?;
                let source = a.source()?;
                let entry = self
                    .registry
                    .install(a.name, a.description.unwrap_or_default(), source, context)
                    .await?;
                self.registry.request_refresh(&self.actor);
                Ok(result("App installed", entry.public(false)))
            }
            "enable" | "close" => {
                let a = args::<ChangeArgs>(call)?;
                let entry = self
                    .registry
                    .set_enabled(&a.id, a.expected_revision, call.name == "enable")
                    .await?;
                self.registry.request_refresh(&self.actor);
                Ok(result("App installation updated", entry.public(false)))
            }
            _ => Err(miette!("Unknown AppsManager tool")),
        }
    }
}

pub struct McpApp {
    system: crate::actors::ActorSystem,
    entry: Installation,
    registry: Arc<Registry>,
    session: Option<transport::Session>,
}

fn direct_model_arguments(schema: &serde_json::Value) -> bool {
    validate_model_facing_schema(schema).is_ok()
}
impl McpApp {
    pub fn new(
        entry: Installation,
        registry: Arc<Registry>,
        system: crate::actors::ActorSystem,
    ) -> Self {
        Self {
            system,
            entry,
            registry,
            session: None,
        }
    }
    fn tool(&self, name: &str) -> Result<&Tool> {
        self.entry
            .tools
            .iter()
            .find(|t| t.local_name == name)
            .ok_or_else(|| miette!("MCP tool not found"))
    }
    fn arguments(&self, call: &AgentToolCall) -> Result<Value> {
        let tool = self.tool(&call.name)?;
        let value = if direct_model_arguments(&tool.input_schema) {
            call.arguments.clone()
        } else {
            let a = args::<JsonArguments>(call)?;
            serde_json::from_str(&a.arguments_json)
                .map_err(|_| miette!("MCP arguments_json must contain valid JSON"))?
        };
        let validator = jsonschema::validator_for(&tool.input_schema)
            .map_err(|_| miette!("MCP schema unavailable"))?;
        if !value.is_object() || !validator.is_valid(&value) {
            return Err(miette!(
                "Arguments do not match the original MCP schema; inspect this tool's schema"
            ));
        }
        Ok(value)
    }
}
#[async_trait]
impl App for McpApp {
    fn id(&self) -> AppId {
        AppId::new(self.entry.id.clone()).unwrap()
    }
    fn managed_revision(&self) -> Option<u64> {
        Some(self.entry.revision)
    }
    fn can_passivate(&self) -> bool {
        self.session.is_none()
    }
    fn tool_specs(&self) -> Vec<AppToolSpec> {
        self.entry.tools.iter().map(|tool| {
            let direct = direct_model_arguments(&tool.input_schema);
            let mut description = format!("{} · {}\n{}", self.entry.name, tool.name, tool.description);
            if !direct {
                description.push_str(&format!("\nPass the original JSON object in arguments_json. Inspect the original schema with apps_manager__inspect: id={}, tool={}", self.entry.id, tool.name));
            }
            AppToolSpec {
                name: tool.local_name.clone(), description,
                input_schema: if direct { tool.input_schema.clone() } else { model_schema_for::<JsonArguments>() },
            }
        }).collect()
    }

    async fn prepare_turn(
        &mut self,
        context: &AppToolExecutionContext,
        refresh: bool,
    ) -> Result<()> {
        if self
            .registry
            .check(&self.entry.id, self.entry.revision)
            .is_err()
        {
            return Ok(());
        }
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.ensure_policy(&context.sandbox_policy).is_err())
        {
            // A prior reviewed invocation may own an elevated connection.
            // Background catalog refresh cannot borrow that authority.
            return Ok(());
        }
        if (refresh
            || self
                .session
                .as_ref()
                .is_some_and(|s| s.dirty.load(Ordering::Acquire)))
            && let Some(session) = &mut self.session
        {
            let tools = match session.tools().await {
                Ok(tools) => tools,
                Err(_) => {
                    session.close().await;
                    self.session = None;
                    return Ok(());
                }
            };
            self.registry
                .update_tools(&self.entry.id, self.entry.revision, tools)
                .await?;
            self.entry = self.registry.get(&self.entry.id)?;
        }
        Ok(())
    }
    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        if context
            .actor_call
            .as_ref()
            .is_some_and(|caller| caller.remote_read_only)
        {
            return Err(miette!(
                "Remote read-only authority cannot invoke MCP effects"
            ));
        }
        self.registry.check(&self.entry.id, self.entry.revision)?;
        self.arguments(call)?;
        self.registry.check(&self.entry.id, self.entry.revision)?;
        let name = self.tool(&call.name)?.name.clone();
        let arguments = self.arguments(call)?;
        // Selecting Sudo explicitly requests a new OS execution environment.
        // Reconnect that native session; never assert that an existing process
        // acquired another token/policy, or replay any prior MCP tool effects.
        if context.restart_native
            && context.actor_call.is_some()
            && context.sandbox_policy.is_unrestricted()
            && self
                .session
                .as_ref()
                .is_some_and(|session| session.ensure_policy(&context.sandbox_policy).is_err())
            && let Some(mut session) = self.session.take()
        {
            session.close().await;
        }
        if self.session.is_none() {
            let mut session = transport::Session::connect(&self.entry.source, context).await?;
            let tools = session.tools().await?;
            if serde_json::to_value(&tools).into_diagnostic()?
                != serde_json::to_value(&self.entry.tools).into_diagnostic()?
            {
                self.registry
                    .update_tools(&self.entry.id, self.entry.revision, tools)
                    .await?;
                session.close().await;
                if let Some(caller) = &context.actor_call {
                    self.registry.request_refresh(&caller.actor_id);
                }
                return Err(miette!(
                    "MCP tools changed; refresh Apps before invoking the new definition"
                ));
            }
            self.session = Some(session);
        }
        self.registry.check(&self.entry.id, self.entry.revision)?;
        self.session
            .as_ref()
            .unwrap()
            .ensure_policy(&context.sandbox_policy)?;
        let service = self.session.as_ref().unwrap().service.as_ref().unwrap();
        let params = rmcp::model::CallToolRequestParams::new(name)
            .with_arguments(arguments.as_object().unwrap().clone());
        let outcome = match tokio::time::timeout(
            std::time::Duration::from_secs(120),
            service.call_tool(params),
        )
        .await
        {
            Ok(Ok(outcome)) => outcome,
            outcome => {
                let reason = if outcome.is_err() {
                    "MCP call timed out; effects are uncertain, do not replay"
                } else {
                    "MCP call failed; effects may be uncertain"
                };
                return Err(self
                    .session
                    .as_mut()
                    .unwrap()
                    .with_diagnostics(miette!("{reason}")));
            }
        };
        let mut payload = serde_json::to_value(outcome).into_diagnostic()?;
        let actor = context
            .actor_call
            .as_ref()
            .ok_or_else(|| miette!("Actor context missing"))?
            .actor_id
            .clone();
        let mut images = Vec::new();
        let mut resources = Vec::new();
        if let Some(parts) = payload.get("content").and_then(Value::as_array) {
            use base64::Engine;
            for part in parts {
                let (bytes, media_type) = match part.get("type").and_then(Value::as_str) {
                    Some("image") => {
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(
                                part["data"]
                                    .as_str()
                                    .ok_or_else(|| miette!("Invalid MCP image"))?,
                            )
                            .map_err(|_| miette!("Invalid MCP image encoding"))?;
                        (
                            bytes,
                            part["mimeType"]
                                .as_str()
                                .unwrap_or("application/octet-stream"),
                        )
                    }
                    Some("resource") => {
                        let resource = &part["resource"];
                        let bytes = if let Some(text) = resource["text"].as_str() {
                            text.as_bytes().to_vec()
                        } else if let Some(blob) = resource["blob"].as_str() {
                            base64::engine::general_purpose::STANDARD
                                .decode(blob)
                                .map_err(|_| miette!("Invalid MCP resource encoding"))?
                        } else {
                            continue;
                        };
                        (bytes, resource["mimeType"].as_str().unwrap_or("text/plain"))
                    }
                    _ => continue,
                };
                let resource = crate::resources::publish(
                    &self.system,
                    &actor,
                    "MCP result",
                    media_type,
                    &bytes,
                )
                .map_err(|_| miette!("MCP output could not be saved as an Actor resource"))?;
                resources.push(json!({"id":resource.id,"name":resource.name,"media_type":resource.media_type,"uri":format!("/v1/actors/{actor}/resources/{}",resource.id)}));
                if media_type.starts_with("image/") && media_type != "image/svg+xml" {
                    images.push(crate::model_protocol::AgentContentPart::ResourceImage {
                        resource,
                        description: None,
                    });
                }
            }
        }
        payload["resources"] = json!(resources);
        payload["app"] =
            json!({"id":self.entry.id,"name":self.entry.name,"tool":self.tool(&call.name)?.name});
        let text = payload
            .get("content")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let failed = payload.get("isError") == Some(&json!(true));
        let mut result = AppToolExecutionResult::new(
            if payload.get("isError") == Some(&json!(true)) {
                "MCP tool returned an error"
            } else {
                "MCP tool completed"
            },
            payload.clone(),
            Some(if text.is_empty() {
                payload
                    .get("structuredContent")
                    .unwrap_or(&payload)
                    .to_string()
            } else {
                text
            }),
        );
        result.model_images = images;
        result.sandbox = self
            .session
            .as_mut()
            .and_then(|session| session.take_denial_report());
        if failed {
            result.failure = Some("MCP tool returned an error".into());
        }
        Ok(result)
    }
    async fn shutdown(&mut self) -> Result<()> {
        if let Some(mut session) = self.session.take() {
            session.close().await;
        }
        Ok(())
    }
}
