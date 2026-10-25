//! Regressions at the actual Actor -> ModelProvider tool-definition boundary.
//! Historical call arguments remain evidence, never a source of new tool schemas.
use super::*;
use crate::model_protocol::AgentToolInputSpec;

#[test]
fn current_compiled_app_manager_definitions_reject_legacy_extra_parameters() {
    let apps = AppManager::new(vec![Box::new(crate::terminal_app::TerminalApp::new())]).unwrap();
    let tools = apps.model_tool_specs().unwrap();
    let schema = schema_of(&tools, "terminal__terminal_exec");
    assert_eq!(
        schema,
        &crate::schema_utils::model_schema_for::<crate::core::TerminalExecArgs>()
    );
    let mut arguments = json!({"command":"not executed","session_id":null,"workdir":null,"yield_time_ms":null,"max_chars":null});
    crate::schema_utils::validate_value_against_schema(&arguments, schema, "terminal").unwrap();
    for field in ["additional_read_roots", "additional_write_roots"] {
        assert!(schema["properties"].get(field).is_none());
        arguments[field] = serde_json::Value::Null;
        assert!(
            crate::schema_utils::validate_value_against_schema(&arguments, schema, "terminal")
                .is_err(),
            "old nullable parameters must be rejected, not silently restored"
        );
        arguments.as_object_mut().unwrap().remove(field);
    }
}

fn schema_of<'a>(tools: &'a [AgentToolSpec], name: &str) -> &'a serde_json::Value {
    match &tools
        .iter()
        .find(|tool| tool.name == name)
        .unwrap()
        .input_spec
    {
        AgentToolInputSpec::JsonSchema { schema } => schema,
        _ => panic!("expected a JSON schema"),
    }
}

#[tokio::test]
async fn retained_actor_restart_and_refresh_never_promote_legacy_history_to_schema() {
    let provider = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "Before restart",
    )]));
    let (directory, system) = runtime(provider).await;
    let input = system
        .send_user("root", "Establish history".into(), vec![], None)
        .unwrap();
    wait_completed(&system, &input.id).await;
    let retained = system.actor("root").unwrap();
    // A completed old call is deliberately seeded, not executed. Reopening or
    // refreshing must not replay its effect or infer definitions from it.
    let marker = retained.config.workspace.join("must-not-be-created.txt");
    let old_call = AgentToolCall {
        id: "legacy-completed-call".into(),
        name: "terminal__terminal_exec".into(),
        arguments: json!({"command":format!("echo replayed > {}", marker.display()),
            "session_id":null,"workdir":null,"yield_time_ms":null,"max_chars":null,
            "additional_read_roots":null,"additional_write_roots":null}),
    };
    system
        .store()
        .append_history(
            "root",
            &input.id,
            &[
                AgentMessage::assistant_tool_call_protocol_with_reasoning(
                    None,
                    None,
                    vec![old_call.clone()],
                ),
                AgentMessage::tool(
                    &old_call.id,
                    &old_call.name,
                    "Historical completion; do not replay",
                ),
            ],
        )
        .unwrap();
    system.shutdown().await;
    drop(system);

    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "refresh-current-definitions".into(),
            name: "apps_manager__refresh".into(),
            arguments: json!({}),
        }]),
        MockReply::Answer("After refresh"),
    ]));
    let system = ActorSystem::open_with_factory(
        directory.path().join("data"),
        directory.path().join("workspace"),
        runtime_config(),
        Arc::new(ReplayFactory(provider.clone())),
    )
    .await
    .unwrap();
    assert_eq!(
        system.actor("root").unwrap().created_at_ms,
        retained.created_at_ms
    );
    let fresh = system
        .create_owned("root", project_config(&system, "new-schema-peer"))
        .unwrap();
    let fresh_tools = builtin_apps::create_apps(&fresh, system.clone(), &system.config())
        .unwrap()
        .model_tool_specs()
        .unwrap();
    let input = system
        .send_user(
            "root",
            "Refresh definitions without recreating this Actor".into(),
            vec![],
            None,
        )
        .unwrap();
    assert_eq!(
        wait_completed(&system, &input.id).await.status,
        MessageStatus::Completed
    );
    {
        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            let schema = schema_of(&request.tools, "terminal__terminal_exec");
            assert_eq!(schema, schema_of(&fresh_tools, "terminal__terminal_exec"));
            for removed in ["additional_read_roots", "additional_write_roots"] {
                assert!(schema["properties"].get(removed).is_none());
                assert!(
                    !schema["required"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|key| key == removed)
                );
            }
            assert!(
                request.messages.iter().any(|row| matches!(row,
                AgentMessage::AssistantToolCallProtocol { calls, .. }
                    if calls.iter().any(|call| call == &old_call))),
                "history must remain original evidence, not be rewritten"
            );
        }
    }
    assert!(
        !marker.exists(),
        "loading history must never replay terminal effects"
    );
    assert!(
        system
            .store()
            .tool_summaries("root", &input.id, None, 10)
            .unwrap()
            .items
            .iter()
            .all(|tool| tool.name != "terminal__terminal_exec")
    );
    system.shutdown().await;
}

/// A stateful App whose discovery changes at a settled tool group, just like
/// tools/list_changed. Refresh must replace the model-facing list, not its App.
struct ChangingDefinitions {
    changed: bool,
    executions: Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait]
impl crate::app::App for ChangingDefinitions {
    fn id(&self) -> crate::app::AppId {
        crate::app::AppId::new("fixture").unwrap()
    }
    fn tool_specs(&self) -> Vec<crate::app::AppToolSpec> {
        let spec = |name: &str, schema| crate::app::AppToolSpec {
            name: name.into(),
            description: format!("Current definition for {name}"),
            input_schema: schema,
        };
        let empty = crate::schema_utils::model_schema_for::<crate::app::EmptyAppArgs>();
        vec![
            spec("change", empty.clone()),
            spec(if self.changed { "new_only" } else { "old_only" }, empty),
            spec(
                "inspect",
                if self.changed {
                    json!({"type":"object","properties":{"current":{"type":"string"}},"required":["current"],"additionalProperties":false})
                } else {
                    json!({"type":"object","properties":{"obsolete":{"type":["string","null"]}},"required":["obsolete"],"additionalProperties":false})
                },
            ),
        ]
    }
    async fn execute_tool(
        &mut self,
        _: &AgentToolCall,
        _: &AppToolExecutionContext,
    ) -> Result<crate::app::AppToolExecutionResult> {
        self.changed = true;
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(crate::app::AppToolExecutionResult::new(
            "changed discovery",
            json!({"changed":true}),
            None,
        ))
    }
}

#[tokio::test]
async fn refresh_next_provider_request_replaces_schema_additions_and_removals_without_recreating_app()
 {
    let (directory, system, provider) = paused_runtime().await;
    provider.replies.lock().extend([
        MockReply::Calls(vec![
            AgentToolCall {
                id: "change-discovery".into(),
                name: "fixture__change".into(),
                arguments: json!({}),
            },
            AgentToolCall {
                id: "refresh-definitions".into(),
                name: "apps_manager__refresh".into(),
                arguments: json!({}),
            },
        ]),
        MockReply::Answer("Refreshed"),
    ]);
    let input = system
        .send_user(
            "root",
            "Refresh changed tool discovery".into(),
            vec![],
            None,
        )
        .unwrap();
    let message = system.store().claim_next("root").unwrap().unwrap();
    assert_eq!(input.id, message.id);
    let actor = system.execution_actor(&message).unwrap();
    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut apps = AppManager::new(vec![
        Box::new(ChangingDefinitions {
            changed: false,
            executions: executions.clone(),
        }),
        Box::new(crate::mcp::AppsManager::new(
            system.app_registry(),
            actor.id.clone(),
        )),
    ])
    .unwrap();
    execute_message(
        &system,
        &actor,
        &message,
        &ActorControl::default(),
        &mut apps,
    )
    .await
    .unwrap();
    {
        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 2);
        assert!(
            schema_of(&requests[0].tools, "fixture__inspect")["properties"]
                .get("obsolete")
                .is_some()
        );
        let refreshed = schema_of(&requests[1].tools, "fixture__inspect");
        assert!(refreshed["properties"].get("current").is_some());
        assert!(refreshed["properties"].get("obsolete").is_none());
        assert!(
            requests[0]
                .tools
                .iter()
                .any(|tool| tool.name == "fixture__old_only")
        );
        assert!(
            !requests[1]
                .tools
                .iter()
                .any(|tool| tool.name == "fixture__old_only")
        );
        assert!(
            requests[1]
                .tools
                .iter()
                .any(|tool| tool.name == "fixture__new_only")
        );
    }
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "refresh must retain live App state and not replay calls"
    );
    assert!(
        !system.app_registry().take_refresh(&actor.id),
        "refresh was consumed at the boundary"
    );
    let _ = directory;
    apps.shutdown().await.unwrap();
    system.shutdown().await;
}
