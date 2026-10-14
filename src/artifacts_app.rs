//! Durable Actor-scoped artifact publication.
use async_trait::async_trait;
use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    actors::{ActorSystem, ToolPermissionRequest},
    app::{App, AppDocs, AppId, AppToolExecutionContext, AppToolExecutionResult, AppToolSpec},
    artifacts::{ArtifactKind, ArtifactRequest, store_artifact},
    model_protocol::AgentToolCall,
    schema_utils::model_schema_for,
};

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Image,
    Svg,
    Html,
    Url,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PresentArgs {
    path: Option<String>,
    content: Option<String>,
    url: Option<String>,
    kind: Option<Kind>,
    title: Option<String>,
    artifact_id: Option<String>,
    description: Option<String>,
}

pub struct ArtifactsApp {
    actor_id: String,
    system: ActorSystem,
}

impl ArtifactsApp {
    pub fn new(actor_id: String, system: ActorSystem) -> Self {
        Self { actor_id, system }
    }
}

#[async_trait]
impl App for ArtifactsApp {
    fn can_passivate(&self) -> bool {
        true
    }
    fn id(&self) -> AppId {
        AppId::new("artifacts").expect("built-in App id")
    }
    fn docs(&self) -> AppDocs {
        AppDocs::from_markdown(include_str!("app_instructions/artifacts.md"))
    }
    fn tool_specs(&self) -> Vec<AppToolSpec> {
        vec![AppToolSpec { name: "present".into(), description: "Store and authorize an inline artifact for this Actor. Reuse the returned artifact_id for a new version.".into(), input_schema: model_schema_for::<PresentArgs>() }]
    }
    fn permission_request(
        &self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<ToolPermissionRequest> {
        let args: PresentArgs = serde_json::from_value(call.arguments.clone())
            .map_err(|_| miette!("invalid artifact arguments"))?;
        let readable_roots = if let Some(path) = args.path {
            let actual = crate::file_io::resolve_file_path(&context.execution_cwd, &path)?;
            if context.sandbox_policy.is_path_readable(&actual) {
                Vec::new()
            } else {
                vec![actual]
            }
        } else {
            Vec::new()
        };
        Ok(ToolPermissionRequest {
            operation_details: serde_json::Value::Null,
            readable_roots,
            writable_roots: Vec::new(),
        })
    }
    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        if call.name != "present" {
            return Err(miette!("unknown Artifacts tool"));
        }
        let caller = context
            .actor_call
            .as_ref()
            .ok_or_else(|| miette!("Actor call context is required"))?;
        if caller.actor_id != self.actor_id {
            return Err(miette!("Actor identity mismatch"));
        }
        self.system.record_for_tool(&self.actor_id, caller)?;
        let args: PresentArgs = serde_json::from_value(call.arguments.clone())
            .map_err(|_| miette!("invalid artifact arguments"))?;
        if let Some(url) = &args.url {
            crate::browser_app::validate_browser_url(url)?;
        }
        let prefix = format!("{}:", self.actor_id);
        let logical_id = args
            .artifact_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let artifact_id = if logical_id.starts_with(&prefix) {
            logical_id
        } else {
            format!("{prefix}{logical_id}")
        };
        let request = ArtifactRequest {
            path: args.path,
            content: args.content,
            url: args.url,
            kind: args.kind.map(|kind| match kind {
                Kind::Image => ArtifactKind::Image,
                Kind::Svg => ArtifactKind::Svg,
                Kind::Html => ArtifactKind::Html,
                Kind::Url => ArtifactKind::Url,
            }),
            title: args.title,
            artifact_id: Some(artifact_id),
            description: args.description,
        };
        let mut reference =
            store_artifact(&request, &context.execution_cwd, &context.sandbox_policy)?;
        self.system.register_artifact(&self.actor_id, &reference)?;
        reference.local_path = None;
        if let Some(file_reference) = reference.uri.strip_prefix("/artifacts/") {
            let name = file_reference
                .split('?')
                .next()
                .ok_or_else(|| miette!("invalid artifact reference"))?;
            reference.uri = format!("/v1/actors/{}/resources/{name}", self.actor_id);
        }
        // Storage paths are trusted infrastructure details, not tool-visible file grants.
        let mut payload = serde_json::to_value(&reference)
            .map_err(|err| miette!("artifact result serialization failed: {err}"))?;
        if let Some(object) = payload.as_object_mut() {
            object.remove("local_path");
        }
        Ok(AppToolExecutionResult::new(
            format!("presented {}", reference.title),
            payload,
            None,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn artifact_schema_is_portable_and_all_fields_required() {
        let schema = model_schema_for::<PresentArgs>();
        crate::schema_utils::validate_model_facing_schema(&schema).unwrap();
        assert_eq!(schema["required"].as_array().unwrap().len(), 7);
        assert_eq!(schema["additionalProperties"], false);
    }
}
