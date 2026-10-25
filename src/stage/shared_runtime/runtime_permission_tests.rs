// Included in shared_runtime::tests: exercises the real device invoke adapter.
struct ScopeFixture(Arc<std::sync::atomic::AtomicUsize>, bool);
#[async_trait::async_trait]
impl crate::app::App for ScopeFixture {
    fn id(&self) -> crate::app::AppId {
        crate::app::AppId::new("fixture").unwrap()
    }
    fn tool_specs(&self) -> Vec<crate::app::AppToolSpec> {
        vec![crate::app::AppToolSpec {
            name: "inspect".into(),
            description: "Fixture".into(),
            input_schema: crate::schema_utils::model_schema_for::<crate::app::EmptyAppArgs>(),
        }]
    }
    fn review_requirement(
        &self,
        _: &AgentToolCall,
        _: &AppToolExecutionContext,
    ) -> miette::Result<Option<crate::actors::AppReview>> {
        Ok(Some(crate::actors::AppReview::new(
            "fixture/source-review/v1",
            "Review the source fixture effect",
            json!({"effect":"inspect"}),
        )))
    }
    async fn execute_tool(
        &mut self,
        _: &AgentToolCall,
        _: &AppToolExecutionContext,
    ) -> miette::Result<AppToolExecutionResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if self.1 {
            return Err(crate::actors::approval::ApprovalDenied(
                crate::actors::approval::ApprovalRejection {
                    approval_id: "source-denied-approval".into(),
                    stage_id: "source-stage".into(),
                    reason: "Owner declined the exact scope".into(),
                    mode: crate::actors::ApprovalMode::Ask,
                },
            )
            .into());
        }
        Ok(AppToolExecutionResult::new(
            "Fixture",
            json!({"output":"fixture child: Permission denied","exit_code":0,"running":false}),
            None,
        ))
    }
}

#[tokio::test]
async fn runtime_shared_invoke_records_native_result_without_replaying_the_effect() {
    let (_directory, runtime, core, context) = running_core().await;
    let system = &runtime.host.system;
    let coordinator = super::super::coordination::Coordinator::new(&runtime)
        .await
        .unwrap();
    *runtime.coordination.write() = Some(coordinator.clone());
    coordinator.bootstrap().await.unwrap();
    // Replace the lightweight fixture's synthetic fence with real quorum state.
    *core.fence.lock() = Fence::default();
    let reply = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let head = shared_store::head(system.store()).unwrap();
            let _ = core
                .coordinate(
                    Operation::Advertise {
                        head: head.clone(),
                        eligible: true,
                        priority: 0,
                    },
                    vec![],
                )
                .await;
            if let Ok(reply) = core.coordinate(Operation::Claim { head }, vec![]).await
                && reply.accepted
            {
                break reply;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    core.epoch.store(reply.epoch, Ordering::Release);
    core.recovered_epoch.store(reply.epoch, Ordering::Release);
    system
        .apply_owned_preferences(
            shared::ACTOR_ID,
            None,
            Some(crate::actors::ApprovalMode::None),
        )
        .await
        .unwrap();
    let actor = system.core_device_actor().unwrap();
    let portable =
        crate::stage::shared_apps::manager(core.clone(), actor.clone(), system.clone()).unwrap();
    let appeal = portable
        .model_tool_specs()
        .unwrap()
        .into_iter()
        .find(|tool| tool.name == "actors__appeal_approval")
        .unwrap();
    let crate::model_protocol::AgentToolInputSpec::JsonSchema { schema } = appeal.input_spec else {
        panic!("permission control must expose a portable schema")
    };
    crate::schema_utils::validate_model_facing_schema(&schema).unwrap();
    for name in [
        "approval_id",
        "tool_result_id",
        "readable_roots",
        "writable_roots",
        "explanation",
    ] {
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == name)
        );
    }
    let revision = super::super::digest(
        &serde_json::to_vec(&json!({"actor":actor.config,"configuration":system.config()}))
            .unwrap(),
    );
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    *core.device_apps.lock().await = Some((
        revision.clone(),
        AppManager::new(vec![Box::new(ScopeFixture(count.clone(), false))]).unwrap(),
    ));
    let input = &context.actor_call.as_ref().unwrap().message_id;
    let call = AgentToolCall {
        id: "shared-native-result".into(),
        name: "inspect".into(),
        arguments: json!({}),
    };
    // Coordinator observation exists before the target executes. It must not be used as native proof.
    let coordinator_id = system
        .store()
        .tool_started_with_authority(shared::ACTOR_ID, input, &call, false, false)
        .unwrap();
    let response = core
        .invoke(
            runtime.host.local,
            reply.epoch,
            input,
            "fixture",
            &revision,
            &call,
            false,
        )
        .await
        .unwrap();
    let Response::ToolResult {
        result: Some(result),
        uncertain: false,
    } = response
    else {
        panic!("device result must be retained")
    };
    let reference = result.permission_reference.unwrap();
    assert_ne!(reference.tool_result_id, coordinator_id);
    assert_eq!(reference.stage_id, runtime.host.local.to_string());
    assert!(reference.approval_id.is_none());
    assert_eq!(
        result.payload,
        json!({"output":"fixture child: Permission denied","exit_code":0,"running":false})
    );
    assert!(
        system
            .store()
            .native_tool_result(&reference.tool_result_id, shared::ACTOR_ID, input)
            .is_ok()
    );
    assert!(
        system
            .store()
            .native_tool_result(&coordinator_id, shared::ACTOR_ID, input)
            .is_err()
    );
    let repeated = core
        .invoke(
            runtime.host.local,
            reply.epoch,
            input,
            "fixture",
            &revision,
            &call,
            false,
        )
        .await
        .unwrap();
    assert!(matches!(
        repeated,
        Response::ToolResult {
            uncertain: false,
            ..
        }
    ));
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "the original immutable effect is returned, never replayed"
    );
    *core.device_apps.lock().await = Some((
        revision.clone(),
        AppManager::new(vec![Box::new(ScopeFixture(count.clone(), true))]).unwrap(),
    ));
    let refused = AgentToolCall {
        id: "shared-refused-result".into(),
        ..call.clone()
    };
    let response = core
        .invoke(
            runtime.host.local,
            reply.epoch,
            input,
            "fixture",
            &revision,
            &refused,
            false,
        )
        .await
        .unwrap();
    let Response::ToolResult {
        result: Some(refused),
        uncertain: false,
    } = response
    else {
        panic!("structured permission decision must be retained")
    };
    assert!(refused.failure.is_none());
    assert!(refused.permission_reference.is_none());
    assert_eq!(refused.payload["approval"]["decision"], "denied");
    assert_eq!(
        refused.payload["approval"]["approval_id"],
        "source-denied-approval"
    );
    // System temp is already readable by native processes. Exercise a real
    // missing scope instead of requesting an approval for an exempt path.
    let extra_root = crate::sandbox::test_support::scoped_directory();
    let extra = extra_root.path().join("extra-scope");
    std::fs::create_dir(&extra).unwrap();
    let submitted = core
        .handle_approval_application(
            runtime.host.local,
            reply.epoch,
            input,
            crate::actors::runtime_permissions::ApplicationSource::ToolResult {
                tool_result_id: reference.tool_result_id,
                readable_roots: vec![extra.clone()],
                writable_roots: vec![],
            },
            crate::actors::ReviewApplication {
                task: "Inspect fixture".into(),
                reason: "Read the exact additional fixture directory".into(),
            },
        )
        .await
        .unwrap();
    let Response::ApprovalSubmitted { record } = submitted else {
        panic!("source device must approve the result reference")
    };
    assert_eq!(record.decision, crate::actors::ApprovalDecision::Approved);
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "scope review never executes either original App call"
    );
    assert!(
        system
            .execution_context_with_policy(
                &actor,
                &system.store().message(input).unwrap(),
                Some(actor.config.clone())
            )
            .unwrap()
            .sandbox_policy
            .is_path_readable(&extra)
    );
    // Losing source authority while Ask is pending must cancel the record and
    // leave the effect unstarted, rather than only dropping the RPC future.
    system
        .apply_owned_preferences(
            shared::ACTOR_ID,
            None,
            Some(crate::actors::ApprovalMode::Ask),
        )
        .await
        .unwrap();
    let actor = system.core_device_actor().unwrap();
    let revision = super::super::digest(
        &serde_json::to_vec(&json!({"actor":actor.config,"configuration":system.config()}))
            .unwrap(),
    );
    *core.device_apps.lock().await = Some((
        revision.clone(),
        AppManager::new(vec![Box::new(ScopeFixture(count.clone(), false))]).unwrap(),
    ));
    let waiting = AgentToolCall {
        id: "wait-before-effect".into(),
        ..call.clone()
    };
    let invoke = core.invoke(
        runtime.host.local,
        reply.epoch,
        input,
        "fixture",
        &revision,
        &waiting,
        false,
    );
    let lose_authority = async {
        let record = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(record) = system
                    .store()
                    .pending_approvals(Some(shared::ACTOR_ID), None, 10)
                    .unwrap()
                    .items
                    .into_iter()
                    .find(|record| record.call.id == waiting.id)
                {
                    break record;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("source invocation must wait for Ask approval");
        *core.fence.lock() = Fence::default();
        record.id
    };
    let (result, id) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(invoke, lose_authority)
    })
    .await
    .expect("source review cancellation must finish");
    assert!(result.is_err());
    assert_eq!(
        system.store().approval(&id).unwrap().decision,
        crate::actors::ApprovalDecision::Cancelled
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "authority loss must not execute the waiting effect"
    );
    coordinator.stop().await;
    system.shutdown().await;
}
