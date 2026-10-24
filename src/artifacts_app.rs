//! Durable Actor-scoped artifact publication.
use async_trait::async_trait;
use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    actors::{ActorSystem, ToolPermissionRequest},
    app::{App, AppId, AppToolExecutionContext, AppToolExecutionResult, AppToolSpec},
    artifacts::{ArtifactKind, ArtifactRequest, store_artifact, store_resource_artifact},
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
    /// Existing Actor-scoped resource ID or internal URI returned by any App. No filesystem access is needed.
    resource: Option<String>,
    /// Local image/SVG/HTML path, subject to the Actor's file permissions. Publishes a snapshot unaffected by later file changes.
    path: Option<String>,
    /// Inline SVG or HTML.
    content: Option<String>,
    /// External HTTP(S) address, or an existing internal resource URI.
    url: Option<String>,
    kind: Option<Kind>,
    title: Option<String>,
    artifact_id: Option<String>,
    description: Option<String>,
}

impl PresentArgs {
    fn parse(call: &AgentToolCall) -> Result<Self> {
        let args: Self = serde_json::from_value(call.arguments.clone())
            .map_err(|_| miette!("invalid artifact arguments"))?;
        let sources = [
            args.resource.is_some(),
            args.path.is_some(),
            args.content.is_some(),
            args.url.is_some(),
        ];
        if sources.into_iter().filter(|value| *value).count() != 1 {
            return Err(miette!(
                "Provide exactly one of resource, path, content or url"
            ));
        }
        Ok(args)
    }
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
    fn tool_specs(&self) -> Vec<AppToolSpec> {
        vec![AppToolSpec { name: "present".into(), description: "Present an existing resource from any App, an authorized local image/SVG/HTML path, inline SVG/HTML, or an external URL. Set exactly one source. Reuse artifact_id for a new version. Returned internal resource URIs can be used directly in links or image embeds; they remain Actor-scoped references.".into(), input_schema: model_schema_for::<PresentArgs>() }]
    }
    fn permission_request(
        &self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<ToolPermissionRequest> {
        let args = PresentArgs::parse(call)?;
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
        let args = PresentArgs::parse(call)?;
        // Internal resource URIs remain valid locators when supplied in url.
        // They resolve through the ledger, never through a browser HTTP request.
        let resource_source = args.resource.as_deref().or_else(|| {
            args.url
                .as_deref()
                .filter(|url| url.starts_with("/v1/actors/"))
        });
        let resource = resource_source
            .map(|reference| {
                crate::resources::resolve_reference(&self.system, &self.actor_id, reference)
                    .map_err(|error| miette!("{error}"))
            })
            .transpose()?;
        if resource.is_none()
            && let Some(url) = &args.url
        {
            crate::web_app::validate_url(url)?;
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
            url: if resource.is_some() { None } else { args.url },
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
        let mut reference = if let Some(resource) = resource {
            if resource.content.size > crate::artifacts::MAX_ARTIFACT_FILE_BYTES {
                return Err(miette!("Resource exceeds the artifact size limit"));
            }
            let bytes = crate::resources::bytes(&self.system, &self.actor_id, &resource)
                .map_err(|error| miette!("{error}"))?;
            store_resource_artifact(&request, bytes, &resource.name)?
        } else {
            store_artifact(&request, &context.execution_cwd, &context.sandbox_policy)?
        };
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
    use base64::Engine;
    use serde_json::json;

    #[test]
    fn artifact_schema_is_portable_and_all_fields_required() {
        let schema = model_schema_for::<PresentArgs>();
        crate::schema_utils::validate_model_facing_schema(&schema).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("resource"))
        );
    }

    #[tokio::test]
    async fn present_reuses_generic_resources_and_snapshots_authorized_local_images() {
        let temporary = tempfile::tempdir().unwrap();
        let _home =
            crate::daat_locus_paths::DaatLocusHomeOverride::set(temporary.path().join("profile"))
                .await;
        let mut config = crate::config::Config {
            main_model: "test".into(),
            ..Default::default()
        };
        config.models.insert(
            "test".into(),
            crate::config::ModelConfig {
                provider: "test".into(),
                model_id: "test".into(),
                ..Default::default()
            },
        );
        config.providers.insert(
            "test".into(),
            crate::config::ProviderConfig::OpenaiCompatible {
                base_url: "https://example.invalid".into(),
                api_key: "unused-test-key".into(),
            },
        );
        let system = ActorSystem::open_account_gated(
            temporary.path().join("profile/actors"),
            temporary.path().join("workspace"),
            config,
        )
        .await
        .unwrap();
        system.seed_test_actor("image-worker");
        system.seed_test_actor("other-worker");
        let actor = system.actor("image-worker").unwrap();
        system
            .send_user(&actor.id, "Show the image".into(), vec![], None)
            .unwrap();
        let message = system.store().claim_next(&actor.id).unwrap().unwrap();
        let context = system.execution_context(&actor, &message).unwrap();
        let png = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .unwrap();
        let resource =
            crate::resources::publish(&system, &actor.id, "image.png", "image/png", &png).unwrap();
        let foreign =
            crate::resources::publish(&system, "other-worker", "private.png", "image/png", &png)
                .unwrap();
        let local = actor.config.workspace.join("图片.png");
        std::fs::write(&local, &png).unwrap();
        let outside = temporary.path().join("outside.png");
        std::fs::write(&outside, &png).unwrap();
        let call = |source: &str, value: &str| {
            let mut arguments = json!({
                "resource":null,"path":null,"content":null,"url":null,
                "kind":null,"title":"Result image","artifact_id":"result-image","description":null
            });
            arguments[source] = json!(value);
            AgentToolCall {
                id: uuid::Uuid::new_v4().to_string(),
                name: "present".into(),
                arguments,
            }
        };
        let mut app = ArtifactsApp::new(actor.id.clone(), system.clone());
        // The resource can originate from any App. Its private storage path is
        // not passed as a model argument or added to the filesystem policy.
        let stored = system.store().resource(&actor.id, &resource.id).unwrap();
        let mut resource_context = context.clone();
        resource_context.sandbox_policy.filesystem.full_disk_read = false;
        resource_context
            .sandbox_policy
            .filesystem
            .readable_roots
            .clear();
        let mut published = Vec::new();
        for (index, (source, value)) in [
            ("resource", resource.uri(&actor.id)),
            ("resource", resource.id.clone()),
            ("url", resource.uri(&actor.id)),
            ("path", local.to_string_lossy().into_owned()),
        ]
        .into_iter()
        .enumerate()
        {
            let request = call(source, &value);
            let execution = if source == "path" {
                &context
            } else {
                &resource_context
            };
            let permission = app.permission_request(&request, execution).unwrap();
            assert!(permission.readable_roots.is_empty());
            let result = app.execute_tool(&request, execution).await.unwrap();
            assert_eq!(result.payload["kind"], "image");
            assert_eq!(result.payload["mime_type"], "image/png");
            assert_eq!(result.payload["version"], index + 1);
            assert!(result.payload.get("local_path").is_none());
            let uri = result.payload["uri"].as_str().unwrap();
            let delivered = crate::resources::resolve_reference(&system, &actor.id, uri).unwrap();
            assert_eq!(
                crate::resources::bytes(&system, &actor.id, &delivered).unwrap(),
                png
            );
            published.push(delivered);
        }
        // Publishing a local path never follows subsequent changes to the source.
        std::fs::write(&local, b"changed source").unwrap();
        assert_eq!(
            crate::resources::bytes(&system, &actor.id, published.last().unwrap()).unwrap(),
            png
        );
        let outside_call = call("path", &outside.to_string_lossy());
        assert_eq!(
            app.permission_request(&outside_call, &context)
                .unwrap()
                .readable_roots,
            vec![outside.canonicalize().unwrap()]
        );
        assert!(app.execute_tool(&outside_call, &context).await.is_err());
        for reference in [
            foreign.id.clone(),
            foreign.uri("other-worker"),
            "/v1/actors/image-worker/resources/../private.png".into(),
            format!("{}?token=forged", resource.uri(&actor.id)),
        ] {
            assert!(
                app.execute_tool(&call("resource", &reference), &context)
                    .await
                    .is_err()
            );
        }
        let mut ambiguous = call("resource", &resource.id);
        ambiguous.arguments["path"] = json!(local);
        assert!(app.permission_request(&ambiguous, &context).is_err());
        assert!(app.execute_tool(&ambiguous, &context).await.is_err());
        std::fs::write(stored.path, b"tampered resource").unwrap();
        assert!(
            app.execute_tool(&call("resource", &resource.id), &context)
                .await
                .is_err()
        );
        system.shutdown().await;
    }
}
