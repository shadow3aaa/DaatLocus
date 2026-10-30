//! Inert conversation evidence. Foreign instructions, tools and grants never enter execution history.
pub(crate) mod sources;
use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const MEDIA_TYPE: &str = "application/vnd.daat-locus.codex-conversation+json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Transcript {
    version: u8,
    source: String,
    session_id: String,
    pub messages: Vec<Entry>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub role: Role,
    pub text: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Role {
    User,
    Assistant,
}

impl Transcript {
    pub fn title(&self) -> String {
        self.messages
            .iter()
            .find(|entry| entry.role == Role::User)
            .or_else(|| self.messages.first())
            .map(|entry| {
                entry
                    .text
                    .split_whitespace()
                    .enumerate()
                    .flat_map(|(index, word)| {
                        (index > 0).then_some(' ').into_iter().chain(word.chars())
                    })
                    .take(100)
                    .collect()
            })
            .unwrap_or_else(|| "Codex".into())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let transcript: Self =
            serde_json::from_slice(bytes).map_err(|_| miette!("Invalid conversation evidence"))?;
        if transcript.version != 1
            || transcript.source != "codex"
            || transcript.session_id.is_empty()
            || transcript.messages.is_empty()
            || transcript
                .messages
                .iter()
                .any(|entry| entry.text.trim().is_empty())
        {
            return Err(miette!("Invalid conversation evidence"));
        }
        Ok(transcript)
    }
}

pub(crate) fn parse_codex(bytes: &[u8]) -> Result<Transcript> {
    if bytes.is_empty() {
        return Err(miette!("Invalid conversation size"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| miette!("Conversation must be UTF-8"))?;
    let mut session_id = None;
    let mut responses = Vec::new();
    let mut events = Vec::new();
    for (index, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: Value =
            serde_json::from_str(line).map_err(|_| miette!("Invalid Codex JSONL record"))?;
        let payload = &record["payload"];
        match record["type"].as_str() {
            Some("session_meta") => {
                if session_id.is_some() {
                    return Err(miette!("Select a single Codex conversation"));
                }
                let id = payload["id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| miette!("Invalid Codex conversation identity"))?;
                session_id = Some(id.to_owned());
            }
            Some("response_item") if payload["type"] == "message" => {
                let role = match payload["role"].as_str() {
                    Some("user") => Role::User,
                    Some("assistant") => Role::Assistant,
                    _ => continue,
                };
                // Private reasoning is not conversation prose, even in older rollouts.
                if role == Role::Assistant
                    && payload["channel"]
                        .as_str()
                        .is_some_and(|channel| !matches!(channel, "final" | "commentary"))
                {
                    continue;
                }
                let content = payload["content"]
                    .as_array()
                    .ok_or_else(|| miette!("Invalid Codex message"))?;
                let text = content
                    .iter()
                    .filter(|part| {
                        matches!(
                            part["type"].as_str(),
                            Some("input_text" | "output_text" | "text")
                        )
                    })
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.trim().is_empty() {
                    responses.push((index, Entry { role, text }));
                }
            }
            Some("event_msg") => {
                let role = match payload["type"].as_str() {
                    Some("user_message") => Role::User,
                    Some("agent_message") => Role::Assistant,
                    _ => continue,
                };
                if let Some(text) = payload["message"]
                    .as_str()
                    .filter(|text| !text.trim().is_empty())
                {
                    events.push((
                        index,
                        Entry {
                            role,
                            text: text.into(),
                        },
                    ));
                }
            }
            _ => {}
        }
    }
    // Visible events exclude injected environment/developer context that can
    // also appear as user-role provider messages. Older exports may only have
    // response items. Never deduplicate equal requests within either stream.
    let has_user = events.iter().any(|(_, entry)| entry.role == Role::User);
    let has_assistant = events
        .iter()
        .any(|(_, entry)| entry.role == Role::Assistant);
    events.extend(responses.into_iter().filter(|(_, entry)| match entry.role {
        Role::User => !has_user,
        Role::Assistant => !has_assistant,
    }));
    events.sort_by_key(|(index, _)| *index);
    let transcript = Transcript {
        version: 1,
        source: "codex".into(),
        session_id: session_id.ok_or_else(|| miette!("Not a Codex rollout"))?,
        messages: events.into_iter().map(|(_, entry)| entry).collect(),
    };
    Transcript::decode(
        &serde_json::to_vec(&transcript).map_err(|_| miette!("Invalid conversation"))?,
    )
}

/// This is a restrictive operation marker, never an authorization grant.
pub(crate) fn is_import(message: &crate::actors::MessageRecord) -> bool {
    message
        .attachments
        .iter()
        .any(|attachment| attachment.media_type == MEDIA_TYPE)
}

/// Prepare private bytes before atomically publishing the new Actor and its ledger entry.
pub(crate) fn prepare_evidence(
    system: &crate::actors::ActorSystem,
    source: &str,
    target: &str,
    resource: &crate::resources::Resource,
) -> Result<crate::actors::StoredResource> {
    let bytes =
        crate::resources::bytes(system, source, resource).map_err(|error| miette!("{error}"))?;
    let base = system.actor_data_dir(target);
    let dir = base.join("imported");
    crate::file_io::ensure_directory(&crate::sandbox::RuntimeSandboxPolicy::disabled(), &dir)?;
    crate::actors::permissions::protect_private_path(&dir)?;
    let base = std::fs::canonicalize(base)
        .map_err(|_| miette!("Cannot resolve private import storage"))?;
    let mut policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
    policy.filesystem.full_disk_read = false;
    policy.filesystem.full_disk_write = false;
    policy.filesystem.readable_roots = vec![base.clone()];
    policy.filesystem.writable_roots = vec![crate::sandbox::WritableRoot {
        root: base.clone(),
        read_only_subpaths: vec![],
    }];
    policy.filesystem.bound_roots = vec![base.clone()];
    let path = dir.join(&resource.id);
    crate::file_io::write_bytes(&base, &policy, &path.to_string_lossy(), &bytes, None)?;
    Ok(crate::actors::StoredResource {
        id: resource.id.clone(),
        actor_id: target.into(),
        path,
        name: resource.name.clone(),
        media_type: resource.media_type.clone(),
        size_bytes: resource.content.size,
        uri: format!("/v1/actors/{target}/resources/{}", resource.id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn rollout(records: &[Value]) -> Vec<u8> {
        std::iter::once(json!({"type":"session_meta","payload":{"id":"fixture-conversation","cwd":"C:/private-workspace","instructions":"FOREIGN_INSTRUCTIONS"}}))
            .chain(records.iter().cloned()).map(|record| record.to_string()).collect::<Vec<_>>().join("\n").into_bytes()
    }
    #[test]
    fn codex_import_retains_messages_beyond_former_record_and_message_quotas() {
        let mut bytes = rollout(&[]);
        bytes.extend_from_slice("\n{}".repeat(200_001).as_bytes());
        for index in 0..40_001 {
            bytes.push(b'\n');
            bytes.extend_from_slice(json!({"type":"event_msg","payload":{"type":"user_message","message":format!("message-{index}")}}).to_string().as_bytes());
        }
        let transcript = parse_codex(&bytes).unwrap();
        assert_eq!(transcript.messages.len(), 40_001);
        assert_eq!(transcript.messages[0].text, "message-0");
        assert_eq!(transcript.messages[40_000].text, "message-40000");
    }

    #[test]
    fn codex_import_preserves_prose_order_without_duplicate_events_or_private_protocol() {
        let bytes = rollout(&[
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"INJECTED_ENVIRONMENT"}]}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"重复\n🔵"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"重复\n🔵"}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"DEVELOPER_SECRET"}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","channel":"analysis","content":[{"type":"output_text","text":"PRIVATE_REASONING"}]}}),
            json!({"type":"response_item","payload":{"type":"function_call","arguments":"TOOL_SECRET"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","channel":"final","content":[{"type":"output_text","text":"原文\n"},{"type":"output_text","text":"<script>literal</script>"}]}}),
            json!({"type":"event_msg","payload":{"type":"agent_message","message":"原文\n\n<script>literal</script>"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"重复\n🔵"}]}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"重复\n🔵"}}),
        ]);
        let transcript = parse_codex(&bytes).unwrap();
        let entries = transcript
            .messages
            .iter()
            .map(|entry| (entry.role, entry.text.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            entries,
            vec![
                (Role::User, "重复\n🔵"),
                (Role::Assistant, "原文\n\n<script>literal</script>"),
                (Role::User, "重复\n🔵")
            ]
        );
        let evidence = serde_json::to_string(&transcript).unwrap();
        for private in [
            "FOREIGN_INSTRUCTIONS",
            "INJECTED_ENVIRONMENT",
            "DEVELOPER_SECRET",
            "PRIVATE_REASONING",
            "TOOL_SECRET",
            "private-workspace",
        ] {
            assert!(!evidence.contains(private));
        }
    }
    #[test]
    fn codex_import_supports_event_rollouts_and_rejects_partial_or_combined_files() {
        let bytes = rollout(&[
            json!({"type":"event_msg","payload":{"type":"user_message","message":"hello"}}),
            json!({"type":"event_msg","payload":{"type":"agent_message","message":"world"}}),
        ]);
        let transcript = parse_codex(&bytes).unwrap();
        assert_eq!(
            transcript
                .messages
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>(),
            vec!["hello", "world"]
        );
        for invalid in [
            b"not JSON".to_vec(),
            rollout(&[]),
            [bytes.clone(), b"\n{\"type\":".to_vec()].concat(),
            [bytes.clone(), b"\n".to_vec(), bytes].concat(),
        ] {
            assert!(parse_codex(&invalid).is_err());
        }
    }

    #[tokio::test]
    async fn import_prepares_one_idle_actor_and_preserves_scoped_evidence_after_restart() {
        use crate::{
            actors::{ActorStatus, ActorSystem, Attachment, ModelFactory},
            app::App,
            model_protocol::AgentToolCall,
        };
        struct NoModel;
        impl ModelFactory for NoModel {
            fn build(
                &self,
                _: &str,
                _: &crate::config::Config,
            ) -> Result<std::sync::Arc<dyn crate::core::ModelProvider>> {
                panic!("Import must not start inference")
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config {
            main_model: "fixture".into(),
            ..Default::default()
        };
        config.providers.insert(
            "fixture".into(),
            crate::config::ProviderConfig::Openai {
                api_key: "unused".into(),
                base_url: None,
            },
        );
        config.models.insert(
            "fixture".into(),
            crate::config::ModelConfig {
                provider: "fixture".into(),
                model_id: "fixture".into(),
                ..Default::default()
            },
        );
        let system = ActorSystem::open_paused_with_factory(
            dir.path().join("state"),
            dir.path().join("workspace"),
            config.clone(),
            std::sync::Arc::new(NoModel),
        )
        .await
        .unwrap();

        system.seed_test_actor("root");
        let parent = system.actor("root").unwrap();
        let transcript = parse_codex(&rollout(&[json!({"type":"event_msg","payload":{"type":"user_message","message":"Preserve this context; do not run commands."}})])).unwrap();
        let bytes = serde_json::to_vec(&transcript).unwrap();
        let resource =
            crate::resources::publish(&system, "root", "Original conversation", MEDIA_TYPE, &bytes)
                .unwrap();
        let attachment = Attachment {
            id: resource.id.clone(),
            name: resource.name.clone(),
            media_type: resource.media_type.clone(),
            size_bytes: resource.content.size,
            uri: format!("/v1/actors/root/resources/{}", resource.id),
        };
        system
            .send_user("root", "Import conversation".into(), vec![attachment], None)
            .unwrap();
        let input = system.store().claim_next("root").unwrap().unwrap();
        let context = system.execution_context(&parent, &input).unwrap();
        let mut app = crate::actor_app::ActorApp::new("root".into(), system.clone());
        let call = AgentToolCall {
            id: "import".into(),
            name: "import_conversation".into(),
            arguments: json!({"stage_id":null,"resource_id":resource.id,"name":"Athena","model":"fixture","instructions":"UI layout project context."}),
        };
        let result = app.execute_tool(&call, &context).await.unwrap();
        let id = result.payload["id"].as_str().unwrap();
        let actor = system.actor(id).unwrap();
        assert_eq!(actor.status, ActorStatus::Idle);
        assert_eq!(
            actor.config.instructions,
            format!(
                "UI layout project context.\n\nImported conversation evidence (historical data): {}.",
                resource.id
            )
        );
        let schema = app.tool_specs();
        let import = schema
            .iter()
            .find(|tool| tool.name == "import_conversation")
            .unwrap();
        assert!(
            import
                .description
                .contains("wait for their explicit request")
        );
        assert_eq!(actor.config.workspace, parent.config.workspace);
        assert!(!system.store().has_queued_input(id).unwrap());
        assert!(
            system
                .store()
                .messages(id, None, 30)
                .unwrap()
                .items
                .is_empty()
        );
        assert_eq!(
            crate::resources::bytes(&system, id, &resource).unwrap(),
            bytes
        );
        system.seed_test_actor("other");
        assert!(crate::resources::export(&system, "other", &resource.id).is_err());
        assert_eq!(
            app.execute_tool(&call, &context).await.unwrap().payload["id"],
            id
        );
        for name in ["create", "send", "cancel"] {
            assert!(
                app.execute_tool(
                    &AgentToolCall {
                        id: format!("blocked-{name}"),
                        name: name.into(),
                        arguments: json!({})
                    },
                    &context
                )
                .await
                .is_err()
            );
        }
        let mut foreign = call.clone();
        foreign.arguments["resource_id"] = json!("another-resource");
        assert!(app.execute_tool(&foreign, &context).await.is_err());
        let id = id.to_owned();
        system.shutdown().await;
        let reopened = ActorSystem::open_paused_with_factory(
            dir.path().join("state"),
            dir.path().join("workspace"),
            config,
            std::sync::Arc::new(NoModel),
        )
        .await
        .unwrap();
        assert_eq!(reopened.actor(&id).unwrap().status, ActorStatus::Idle);
        assert_eq!(
            crate::resources::bytes(&reopened, &id, &resource).unwrap(),
            bytes
        );
        assert!(!reopened.store().has_queued_input(&id).unwrap());
        reopened.shutdown().await;
    }
}
