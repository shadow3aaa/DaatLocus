use super::*;
use crate::app::App;

#[tokio::test]
#[cfg(target_os = "linux")]
#[ignore = "requires an installed native workspace backend and permission to mount directories"]
async fn workspace_native_app_shares_actor_branches_and_restores_the_selected_directory() {
    use crate::file_io::{ReadFileArgs, WriteFileArgs};
    let (directory, system) = fixture().await;
    assert!(
        system.workspace_available().unwrap(),
        "native workspace mount probe failed"
    );
    let input = system
        .send_user("root", "Develop in a shared branch".into(), vec![], None)
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    let original = system.actor("root").unwrap();
    std::fs::write(original.config.workspace.join("code.txt"), b"original\n").unwrap();
    let context = system.execution_context(&original, &input).unwrap();
    let mut app = crate::workspace_app::WorkspaceApp::new("root".into(), system.clone());
    app.execute_tool(
        &crate::model_protocol::AgentToolCall {
            id: "select-feature".into(),
            name: "select".into(),
            arguments: serde_json::json!({"path":null,"branch":"feature","from":null}),
        },
        &context,
    )
    .await
    .unwrap();
    let selected = system.actor("root").unwrap();
    let scope = SandboxScope::filesystem(&selected.config.workspace, true).unwrap();
    system
        .store()
        .stage_transaction(|tx| {
            crate::sandbox::scopes::remember(tx, &[scope])
                .map_err(|_| crate::stage::StageError::Invalid("test workspace grant failed"))
        })
        .unwrap();
    let context = system.execution_context(&selected, &input).unwrap();
    context
        .files()
        .write_file(&WriteFileArgs {
            path: "code.txt".into(),
            content: "branch edit\n".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(original.config.workspace.join("code.txt")).unwrap(),
        b"original\n"
    );
    app.execute_tool(
        &crate::model_protocol::AgentToolCall {
            id: "fork-review".into(),
            name: "select".into(),
            arguments: serde_json::json!({"path":original.config.workspace,"branch":"review","from":null}),
        },
        &context,
    )
    .await
    .unwrap();
    let review = system.actor("root").unwrap();
    assert_ne!(review.config.workspace, selected.config.workspace);
    assert_eq!(
        std::fs::read(review.config.workspace.join("code.txt")).unwrap(),
        b"branch edit\n",
        "forking the same explicit project path uses the current branch"
    );
    app.execute_tool(
        &crate::model_protocol::AgentToolCall {
            id: "return-to-feature".into(),
            name: "select".into(),
            arguments: serde_json::json!({"path":null,"branch":"feature","from":null}),
        },
        &system.execution_context(&review, &input).unwrap(),
    )
    .await
    .unwrap();
    let mut actors = crate::actor_app::ActorApp::new("root".into(), system.clone());
    let created = actors.execute_tool(&crate::model_protocol::AgentToolCall {
        id: "create-branch-peer".into(), name: "create".into(),
        arguments: serde_json::json!({"stage_id":null,"name":"Branch peer","workspace":context.execution_cwd,
            "initial_authorization":null,"instructions":null,"model":"mock"}),
    }, &context).await.unwrap();
    let peer_id = created.payload["id"].as_str().unwrap();
    let peer_input = system
        .message_from_actor("root", &input.id, peer_id, "Inspect shared changes".into())
        .unwrap();
    system.store().claim_next(peer_id).unwrap().unwrap();
    let peer = system.actor(peer_id).unwrap();
    let peer_context = system.execution_context(&peer, &peer_input).unwrap();
    let mut peer_app = crate::workspace_app::WorkspaceApp::new(peer_id.into(), system.clone());
    let current = peer_app
        .execute_tool(
            &crate::model_protocol::AgentToolCall {
                id: "peer-workspace".into(),
                name: "current".into(),
                arguments: serde_json::json!({}),
            },
            &peer_context,
        )
        .await
        .unwrap();
    assert_eq!(current.payload["branch"], "feature");
    assert_eq!(
        std::fs::read(peer_context.execution_cwd.join("code.txt")).unwrap(),
        b"branch edit\n"
    );
    let merged = app
        .execute_tool(
            &crate::model_protocol::AgentToolCall {
                id: "merge-feature".into(),
                name: "merge".into(),
                arguments: serde_json::json!({"path":null,"branch":null}),
            },
            &context,
        )
        .await
        .unwrap();
    assert!(merged.payload["error"].is_null(), "{merged:?}");
    assert_eq!(
        std::fs::read(original.config.workspace.join("code.txt")).unwrap(),
        b"branch edit\n"
    );
    system
        .store()
        .finish_message(
            &peer_input.id,
            MessageStatus::Completed,
            Some("Observed shared changes."),
            None,
        )
        .unwrap();
    system
        .store()
        .finish_message(
            &input.id,
            MessageStatus::Completed,
            Some("Merged branch changes."),
            None,
        )
        .unwrap();
    // Settle the peer's durable correlated result before testing a fresh input
    // after restart; shutdown never discards or replays that result for us.
    let delivery = system.store().claim_next("root").unwrap().unwrap();
    system
        .store()
        .finish_message(&delivery.id, MessageStatus::Completed, None, None)
        .unwrap();
    let config = system.config();
    system.shutdown().await;
    drop((system, app, peer_app, actors));
    let reopened = ActorSystem::open_paused_with_factory(
        directory.path().join("data"),
        directory.path().join("workspace"),
        config,
        Arc::new(NoModels),
    )
    .await
    .unwrap();
    let next = reopened
        .send_user("root", "Continue the existing branch".into(), vec![], None)
        .unwrap();
    reopened.store().claim_next("root").unwrap().unwrap();
    let actor = reopened.actor("root").unwrap();
    let restored = reopened.execution_context(&actor, &next).unwrap();
    let result = restored
        .files()
        .read_file(&ReadFileArgs {
            path: "code.txt".into(),
            start_line: None,
            line_count: None,
        })
        .await
        .unwrap();
    assert_eq!(result.content, "branch edit\n");
    reopened.shutdown().await;
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "requires the installed Dokan 2 driver and permission to mount directories"]
async fn workspace_native_dokan_without_links_registers_no_app_tools_or_prompt() {
    assert!(
        daat_locus_workspace::native::available().unwrap(),
        "Dokan must be installed to exercise the native compatibility probe"
    );
    let (_directory, system) = fixture().await;
    assert!(!system.workspace_available().unwrap());
    let worker = system.actor("root").unwrap();
    let apps = crate::builtin_apps::create_apps(&worker, system.clone(), &system.config()).unwrap();
    assert!(apps.app_ids().iter().all(|id| id.as_str() != "workspace"));
    assert!(
        apps.model_tool_specs()
            .unwrap()
            .iter()
            .all(|tool| !tool.name.starts_with("workspace__"))
    );
    assert!(
        system
            .device_app_names()
            .unwrap()
            .iter()
            .all(|name| name != "workspace")
    );
    let prompt = super::super::super::prompt::SystemPrompt::for_role(&system, false).unwrap();
    assert!(prompt.files.iter().all(|file| file.name != "workspaces.md"));
    // An older service may have persisted this backend's branch as cwd. Do not
    // resume it with incomplete semantics or silently switch away from its edits.
    let input = system
        .send_user("root", "Resume the existing branch".into(), vec![], None)
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    let workspaces = system.workspaces().unwrap();
    std::fs::write(worker.config.workspace.join("code.txt"), b"original\n").unwrap();
    let origin =
        daat_locus_workspace::Directory::open(&worker.config.workspace, Arc::new(|_, _| Ok(())))
            .unwrap();
    let location = workspaces
        .select(origin.clone(), "retained", "default")
        .unwrap();
    std::fs::write(location.directory.join("code.txt"), b"branch edit\n").unwrap();
    system
        .store()
        .select_actor_workspace(
            "root",
            &input.id,
            &worker.config.workspace,
            &location.directory,
        )
        .unwrap();
    workspaces.close();
    let selected = system.actor("root").unwrap();
    assert!(system.execution_context(&selected, &input).is_err());
    assert!(
        daat_locus_workspace::native::open_mount_directory(&location.directory)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        system.actor("root").unwrap().config.workspace,
        location.directory
    );
    assert_eq!(
        workspaces
            .store()
            .open_branch(origin, "retained")
            .unwrap()
            .read("code.txt", 0, 80)
            .unwrap(),
        b"branch edit\n"
    );
    assert_eq!(
        std::fs::read(worker.config.workspace.join("code.txt")).unwrap(),
        b"original\n"
    );
    system.shutdown().await;
}

#[tokio::test]
async fn workspace_merge_checks_device_write_scopes_before_any_native_file_change() {
    use daat_locus_workspace::Directory;
    let (directory, system) = fixture().await;
    *system.inner.workspace_backend.lock() = Some(true);
    let input = system
        .send_user("root", "Inspect a branch".into(), vec![], None)
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    let actor = system.actor("root").unwrap();
    let context = system.execution_context(&actor, &input).unwrap();
    let source = directory.path().join("ungranted-source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("code.txt"), b"original\n").unwrap();
    let manager = system.workspaces().unwrap();
    let origin = Directory::open(
        &source,
        system
            .workspace_authorize(context.actor_call.clone())
            .unwrap(),
    )
    .unwrap();
    let branch = manager
        .store()
        .branch(origin, "feature", "default")
        .unwrap();
    branch.write("code.txt", 0, b"modified").unwrap();
    let mut app = crate::workspace_app::WorkspaceApp::new("root".into(), system.clone());
    let error = app
        .execute_tool(
            &crate::model_protocol::AgentToolCall {
                id: "ungranted-merge".into(),
                name: "merge".into(),
                arguments: serde_json::json!({"path":source,"branch":"feature"}),
            },
            &context,
        )
        .await
        .unwrap_err();
    assert!(
        error.chain().any(|source| source
            .downcast_ref::<crate::sandbox::AccessDenied>()
            .is_some()),
        "merge must retain the typed denial for the common authorization flow: {error:?}"
    );
    assert_eq!(
        std::fs::read(source.join("code.txt")).unwrap(),
        b"original\n"
    );
    assert_eq!(branch.read("code.txt", 0, 80).unwrap(), b"modified\n");
    assert_eq!(branch.changes(0, 10).unwrap().len(), 1);
    system.shutdown().await;
}

#[tokio::test]
async fn workspace_selection_commits_only_the_current_request_and_preserves_other_actor_cwds() {
    let (directory, system) = fixture().await;
    system.seed_test_actor("peer");
    let original = system.actor("root").unwrap();
    let input = system
        .send_user(
            "root",
            "Select another execution directory".into(),
            vec![],
            None,
        )
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    let selected = directory.path().join("selected");
    std::fs::create_dir(&selected).unwrap();
    let selected = selected.canonicalize().unwrap();
    system
        .store()
        .select_actor_workspace("root", &input.id, &original.config.workspace, &selected)
        .unwrap();
    assert_eq!(system.actor("root").unwrap().config.workspace, selected);
    assert_eq!(
        system.actor("peer").unwrap().config.workspace,
        original.config.workspace
    );
    let context = system.execution_context(&original, &input).unwrap();
    assert_eq!(
        context.execution_cwd, selected,
        "every next App invocation uses persisted cwd"
    );
    assert!(
        !context
            .sandbox_policy
            .is_path_writable(&selected.join("ungranted.txt")),
        "cwd never grants write authority"
    );
    assert!(
        system
            .store()
            .select_actor_workspace(
                "root",
                &input.id,
                &original.config.workspace,
                &original.config.workspace
            )
            .is_err()
    );
    system.cancel("root", &input.id).unwrap();
    assert!(
        system
            .store()
            .select_actor_workspace("root", &input.id, &selected, &original.config.workspace)
            .is_err()
    );
    assert_eq!(system.actor("root").unwrap().config.workspace, selected);
    system.shutdown().await;
}

#[tokio::test]
async fn unavailable_workspace_backend_registers_no_app_or_tools() {
    let (_directory, system) = fixture().await;
    // Device capability is the production registry input. This test exercises
    // registry composition independently of the machine's installed drivers.
    *system.inner.workspace_backend.lock() = Some(false);
    let worker = system.actor("root").unwrap();
    let apps = crate::builtin_apps::create_apps(&worker, system.clone(), &system.config()).unwrap();
    assert!(apps.app_ids().iter().all(|id| id.as_str() != "workspace"));
    assert!(
        apps.model_tool_specs()
            .unwrap()
            .iter()
            .all(|tool| !tool.name.starts_with("workspace__"))
    );
    assert!(
        !system
            .device_app_names()
            .unwrap()
            .iter()
            .any(|name| name == "workspace")
    );
    let prompt = super::super::super::prompt::SystemPrompt::for_role(&system, false).unwrap();
    assert!(prompt.files.iter().all(|file| file.name != "workspaces.md"));
    let core = system.core_device_actor().unwrap();
    let apps = crate::builtin_apps::create_apps(&core, system.clone(), &system.config()).unwrap();
    assert!(apps.app_ids().iter().all(|id| id.as_str() != "workspace"));
    system.shutdown().await;
}

#[tokio::test]
async fn workspace_prompt_follows_app_availability_and_excludes_core() {
    use super::super::super::prompt::SystemPrompt;
    let (_directory, system) = fixture().await;
    *system.inner.workspace_backend.lock() = Some(false);
    let unavailable = SystemPrompt::for_role(&system, false).unwrap();
    *system.inner.workspace_backend.lock() = Some(true);
    let available = SystemPrompt::for_role(&system, false).unwrap();
    let guidance = available
        .files
        .iter()
        .filter(|file| file.name == "workspaces.md")
        .collect::<Vec<_>>();
    assert_eq!(guidance.len(), 1);
    assert_eq!(
        guidance[0].content,
        include_str!("../../../prompts/workspaces.md")
    );
    assert_ne!(unavailable.revision(), available.revision());
    let core = SystemPrompt::for_role(&system, true).unwrap();
    assert!(core.files.iter().all(|file| file.name != "workspaces.md"));
    system.shutdown().await;
}

#[tokio::test]
async fn workspace_tool_schemas_and_default_migration_retain_native_files_and_authority() {
    let (directory, system) = fixture().await;
    // Default needs no projection; enable App composition to exercise it on
    // machines without a driver. Native branch I/O has separate opt-in tests.
    *system.inner.workspace_backend.lock() = Some(true);
    let original = system.actor("root").unwrap();
    let input = system
        .send_user("root", "Use a native directory".into(), vec![], None)
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    let context = system.execution_context(&original, &input).unwrap();
    let target = directory.path().join("another-project");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("existing.txt"), "user bytes").unwrap();
    let before = system.store().sandbox_scopes().unwrap();
    let mut app = crate::workspace_app::WorkspaceApp::new("root".into(), system.clone());
    for spec in app.tool_specs() {
        crate::schema_utils::validate_model_facing_schema(&spec.input_schema).unwrap();
    }
    let result = app
        .execute_tool(
            &crate::model_protocol::AgentToolCall {
                id: "select-native".into(),
                name: "select".into(),
                arguments: serde_json::json!({"path":target,"branch":"default","from":null}),
            },
            &context,
        )
        .await
        .unwrap();
    assert_eq!(
        std::path::PathBuf::from(result.payload["directory"].as_str().unwrap()),
        target.canonicalize().unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(target.join("existing.txt")).unwrap(),
        "user bytes"
    );
    assert_eq!(system.store().sandbox_scopes().unwrap(), before);
    let fresh = system.execution_context(&original, &input).unwrap();
    let mut readonly = fresh.clone();
    readonly.actor_call.as_mut().unwrap().remote_read_only = true;
    let result = app.execute_tool(&crate::model_protocol::AgentToolCall {
        id: "readonly-select".into(), name: "select".into(),
        arguments: serde_json::json!({"path":original.config.workspace,"branch":"default","from":null}),
    }, &readonly).await;
    assert!(result.is_err());
    assert_eq!(
        system.actor("root").unwrap().config.workspace,
        target.canonicalize().unwrap()
    );
    system.shutdown().await;
}
