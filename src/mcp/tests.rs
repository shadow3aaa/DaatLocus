use super::*;
use crate::{
    app::{ActorCallContext, App, AppManager, AppToolExecutionContext},
    model_protocol::AgentToolCall,
};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn installations_and_tools_survive_reopen_and_growth_beyond_former_quotas() {
    let server = server().await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("apps.json");
    let registry = Registry::open(path.clone()).unwrap();
    let source = Source::Http {
        url: server.url.clone(),
        bearer_token: None,
    };
    let tools: Vec<_> = (0..257).map(|index| Tool {
        name: format!("remote-{index}"), local_name: format!("tool_{index}"), description: String::new(),
        input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
    }).collect();
    let mut state = super::State::default();
    for index in 0..65 {
        let id = format!("mcp_{index:032x}");
        state.installations.insert(
            id.clone(),
            Installation {
                id,
                name: format!("App {index}"),
                description: String::new(),
                revision: 1,
                enabled: true,
                source: source.clone(),
                tools: tools.clone(),
            },
        );
    }
    state.revision = 1;
    registry.persist(state).unwrap();
    let reopened = Registry::open(path.clone()).unwrap();
    assert_eq!(reopened.actor_entries("actor").len(), 65);
    let first = reopened.list_public(None, None, None, 64);
    let rest = reopened.list_public(first.last().unwrap()["id"].as_str(), None, None, 64);
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0]["tool_count"], 257);
    let added = reopened
        .install(
            "Another App".into(),
            String::new(),
            source,
            &context(directory.path()),
        )
        .await
        .unwrap();
    assert_eq!(reopened.get(&added.id).unwrap().tools.len(), 1);
    assert_eq!(Registry::open(path).unwrap().entries().len(), 66);
}

#[test]
fn installed_connection_public_view_hides_query_fragment_and_credentials() {
    let source = Source::Http {
        url: "https://example.invalid/mcp?target=private-query#private-fragment".into(),
        bearer_token: Some("private-token".into()),
    };
    let public = source.public();
    assert_eq!(public["url"], "https://example.invalid/mcp");
    assert_eq!(public["credential_configured"], true);
    let text = public.to_string();
    for private in ["private-query", "private-fragment", "private-token"] {
        assert!(!text.contains(private));
        assert!(serde_json::to_string(&source).unwrap().contains(private));
    }
}

#[derive(Default)]
struct Fake {
    calls: AtomicUsize,
    requests: parking_lot::Mutex<Vec<Value>>,
}
pub(crate) struct Server {
    pub(crate) url: String,
    state: Arc<Fake>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn rpc(State(state): State<Arc<Fake>>, Json(request): Json<Value>) -> Response {
    let id = &request["id"];
    let result = match request["method"].as_str().unwrap_or_default() {
        "initialize" => {
            json!({"protocolVersion":request["params"]["protocolVersion"],"capabilities":{"tools":{}},"serverInfo":{"name":"local-test","version":"1"}})
        }
        "tools/list" => {
            json!({"tools":[{"name":"lookup.with space","description":"Look up a record","inputSchema":{"type":"object","properties":{"value":{"oneOf":[{"type":"integer"},{"type":"string"}]},"tag":{"type":["string","null"]}},"required":["value"],"additionalProperties":true}}]})
        }
        "tools/call" => {
            state.calls.fetch_add(1, Ordering::SeqCst);
            state.requests.lock().push(request["params"].clone());
            if request["params"]["arguments"]["value"] == "disconnect" {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            if request["params"]["arguments"]["value"] == "resource" {
                return Json(json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"resource","resource":{"uri":"memory://record","mimeType":"text/plain","text":"original resource bytes"}}],"isError":false}})).into_response();
            }
            if request["params"]["arguments"]["value"] == "failure" {
                return Json(json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":"record was rejected"}],"isError":true}})).into_response();
            }
            json!({"content":[{"type":"text","text":"original MCP result"}],"structuredContent":{"received":request["params"]["arguments"]},"isError":false})
        }
        _ if id.is_null() => return StatusCode::ACCEPTED.into_response(),
        _ => {
            return Json(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"unsupported"}}),
            )
            .into_response();
        }
    };
    Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
}
pub(crate) async fn server() -> Server {
    let state = Arc::new(Fake::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new()
        .route("/mcp", post(rpc))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Server {
        url: format!("http://{addr}/mcp"),
        state,
        task,
    }
}
fn context(directory: &std::path::Path) -> AppToolExecutionContext {
    AppToolExecutionContext {
        restart_native: false,
        access_broker: None,
        execution_cwd: directory.into(),
        sandbox_policy: crate::sandbox::RuntimeSandboxPolicy::disabled(),
        tool_output_max_tokens: 4096,
        turn_epoch: 1,
        actor_call: Some(ActorCallContext {
            remote_read_only: false,
            actor_id: "test-actor".into(),
            message_id: "test-request".into(),
            device_policy: None,
        }),
    }
}
fn call(name: &str, arguments: Value) -> AgentToolCall {
    AgentToolCall {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.into(),
        arguments,
    }
}
async fn actor_system(path: &std::path::Path) -> crate::actors::ActorSystem {
    crate::actors::ActorSystem::open_account_gated(
        path.join("state"),
        path.join("workspace"),
        crate::config::Config::default(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn standard_http_install_call_and_close_preserve_mcp_schema_and_credentials() {
    let server = server().await;
    let directory = tempfile::tempdir().unwrap();
    let context = context(directory.path());
    let path = directory.path().join("apps.json");
    let registry = Registry::open(path.clone()).unwrap();
    let entry = registry
        .install(
            "Example".into(),
            "A local test server".into(),
            Source::Http {
                url: server.url.clone(),
                bearer_token: Some("private-test-credential".into()),
            },
            &context,
        )
        .await
        .unwrap();
    assert!(
        !entry
            .public(true)
            .to_string()
            .contains("private-test-credential")
    );
    let reopened = Registry::open(path).unwrap();
    assert_eq!(
        reopened.get(&entry.id).unwrap().tools[0].input_schema,
        entry.tools[0].input_schema
    );
    let mut app = McpApp::new(
        entry.clone(),
        registry.clone(),
        actor_system(directory.path()).await,
    );
    let tool = app.tool_specs().remove(0);
    assert!(format!("d_abcdef123456_{}__{}", app.id(), tool.name).len() <= 64);
    crate::schema_utils::validate_model_facing_schema(&tool.input_schema).unwrap();
    let original = json!({"value":7,"dynamic":{"preserved":true}});
    let invocation = call(&tool.name, json!({"arguments_json":original.to_string()}));
    let outcome = app.execute_tool(&invocation, &context).await.unwrap();
    assert_eq!(
        outcome.model_content.as_deref(),
        Some("original MCP result")
    );
    assert_eq!(server.state.requests.lock()[0]["name"], "lookup.with space");
    assert_eq!(server.state.requests.lock()[0]["arguments"], original);
    assert!(
        app.execute_tool(&call(&tool.name, json!({"arguments_json":"{}"})), &context)
            .await
            .is_err()
    );
    registry
        .set_enabled(&entry.id, entry.revision, false)
        .await
        .unwrap();
    assert!(app.execute_tool(&invocation, &context).await.is_err());
    assert!(
        registry
            .set_enabled(&entry.id, entry.revision, true)
            .await
            .is_err()
    );
    assert_eq!(server.state.calls.load(Ordering::SeqCst), 1);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_failed_remote_effect_is_not_replayed_by_the_transport() {
    let server = server().await;
    let directory = tempfile::tempdir().unwrap();
    let context = context(directory.path());
    let registry = Registry::open(directory.path().join("apps.json")).unwrap();
    let entry = registry
        .install(
            "Example".into(),
            "A local test server".into(),
            Source::Http {
                url: server.url.clone(),
                bearer_token: None,
            },
            &context,
        )
        .await
        .unwrap();
    let mut app = McpApp::new(entry, registry, actor_system(directory.path()).await);
    let tool = app.tool_specs().remove(0);
    assert!(
        app.execute_tool(
            &call(
                &tool.name,
                json!({"arguments_json":"{\"value\":\"disconnect\"}"})
            ),
            &context
        )
        .await
        .is_err()
    );
    assert_eq!(server.state.calls.load(Ordering::SeqCst), 1);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn refresh_preserves_other_app_instances_and_queries_are_exempt() {
    use async_trait::async_trait;
    struct Counter(usize);
    #[async_trait]
    impl App for Counter {
        fn id(&self) -> crate::app::AppId {
            crate::app::AppId::new("counter").unwrap()
        }
        fn tool_specs(&self) -> Vec<crate::app::AppToolSpec> {
            vec![crate::app::AppToolSpec {
                name: "next".into(),
                description: "Count".into(),
                input_schema: crate::schema_utils::model_schema_for::<crate::app::EmptyAppArgs>(),
            }]
        }
        async fn execute_tool(
            &mut self,
            _: &AgentToolCall,
            _: &AppToolExecutionContext,
        ) -> Result<crate::app::AppToolExecutionResult> {
            self.0 += 1;
            Ok(crate::app::AppToolExecutionResult::new(
                "count",
                json!(self.0),
                None,
            ))
        }
    }
    let server = server().await;
    let directory = tempfile::tempdir().unwrap();
    let context = context(directory.path());
    let registry = Registry::open(directory.path().join("apps.json")).unwrap();
    let mut apps = AppManager::new(vec![
        Box::new(Counter(0)),
        Box::new(AppsManager::new(registry.clone(), "test-actor".into())),
    ])
    .unwrap();
    let id = crate::app::AppId::new("counter").unwrap();
    let next = call("counter__next", json!({}));
    assert_eq!(
        apps.execute_tool_for_app(&id, &next, &context)
            .await
            .unwrap()
            .payload,
        1
    );
    let entry = registry
        .install(
            "Example".into(),
            "A local test server".into(),
            Source::Http {
                url: server.url.clone(),
                bearer_token: None,
            },
            &context,
        )
        .await
        .unwrap();
    apps.reconcile_managed(vec![Box::new(McpApp::new(
        entry,
        registry.clone(),
        actor_system(directory.path()).await,
    ))])
    .await
    .unwrap();
    assert_eq!(
        apps.execute_tool_for_app(&id, &next, &context)
            .await
            .unwrap()
            .payload,
        2
    );
    let manager = crate::app::AppId::new("apps_manager").unwrap();
    apps.execute_tool_for_app(
        &manager,
        &call("apps_manager__refresh", json!({})),
        &context,
    )
    .await
    .unwrap();
    assert!(registry.take_refresh("test-actor"));
    assert_eq!(server.state.calls.load(Ordering::SeqCst), 0);
    apps.shutdown().await.unwrap();
}
#[tokio::test]
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
async fn stdio_uses_host_access_and_preserves_administrator_authority_and_process_cleanup() {
    struct FixtureEnvironment(Option<std::ffi::OsString>);
    impl Drop for FixtureEnvironment {
        fn drop(&mut self) {
            unsafe {
                match &self.0 {
                    Some(previous) => std::env::set_var("DL_MCP_AMBIENT_SECRET", previous),
                    None => std::env::remove_var("DL_MCP_AMBIENT_SECRET"),
                }
            }
        }
    }
    let _environment = FixtureEnvironment(std::env::var_os("DL_MCP_AMBIENT_SECRET"));
    unsafe {
        std::env::set_var("DL_MCP_AMBIENT_SECRET", "ambient-fixture");
    }
    let directory = tempfile::tempdir().unwrap();
    let system = actor_system(directory.path()).await;
    let mut context = system.owner_app_context().unwrap();
    context.actor_call.as_mut().unwrap().actor_id = "test-actor".into();
    let workspace = system.device_workspace();
    std::fs::create_dir_all(workspace).unwrap();
    let source_file = workspace.join("fixture.rs");
    std::fs::write(
        &source_file,
        include_str!("../../tests/support/mcp_stdio.rs"),
    )
    .unwrap();
    let tools = directory.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let executable = tools.join("mcp-fixture.exe");
    let compiled =
        std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .args(["--edition=2024", "--crate-name=mcp_stdio_fixture"])
            .arg(&source_file)
            .arg("-o")
            .arg(&executable)
            .output()
            .expect("MCP process tests require the Rust toolchain");
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let public = workspace.join("public.txt");
    std::fs::write(&public, "public").unwrap();
    let secret = system.infrastructure_dir().join("private-test.txt");
    std::fs::write(&secret, "private").unwrap();
    // MCP must start and access OS-user files even when the Actor's device
    // sandbox denies its executable and the target. No native backend is needed.
    context.sandbox_policy.filesystem.deny_read_paths = vec![tools.clone(), secret.clone()];
    context.sandbox_policy.filesystem.deny_write_paths = vec![secret.clone()];
    let registry = Registry::open(directory.path().join("apps.json")).unwrap();
    let source = Source::Stdio {
        command: executable.to_string_lossy().into_owned(),
        arguments: vec![
            public.to_string_lossy().into_owned(),
            secret.to_string_lossy().into_owned(),
            workspace.join("server.pid").to_string_lossy().into_owned(),
        ],
        environment: BTreeMap::from([("DL_MCP_FIXTURE_TOKEN".into(), "explicit".into())]),
    };
    let entry = registry
        .install("Local".into(), String::new(), source, &context)
        .await
        .unwrap();
    let preflight_pid: u32 = std::fs::read_to_string(workspace.join("server.pid"))
        .unwrap()
        .parse()
        .unwrap();
    let preflight_descendant: u32 = std::fs::read_to_string(workspace.join("server.child.pid"))
        .unwrap()
        .parse()
        .unwrap();
    let mut app = McpApp::new(entry, registry, system.clone());
    let name = app.tool_specs()[0].name.clone();
    let allowed = app
        .execute_tool(&call(&name, json!({"value":true})), &context)
        .await
        .unwrap();
    assert_eq!(
        allowed.model_content.as_deref(),
        Some("explicit|ambient-fixture|public")
    );
    let outside = app
        .execute_tool(&call(&name, json!({"value":false})), &context)
        .await
        .unwrap();
    assert_eq!(
        outside.model_content.as_deref(),
        Some("explicit|ambient-fixture|private|write-allowed")
    );
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "bad");
    assert!(
        outside.sandbox.is_none(),
        "MCP must not fabricate sandbox reports"
    );
    let running_pid: u32 = std::fs::read_to_string(workspace.join("server.pid"))
        .unwrap()
        .parse()
        .unwrap();
    #[cfg(windows)]
    assert_host_integrity(running_pid);
    let mut host = context.clone();
    host.sandbox_policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
    let result = app
        .execute_tool(&call(&name, json!({"value":true})), &host)
        .await
        .unwrap();
    assert_eq!(
        result.model_content.as_deref(),
        Some("explicit|ambient-fixture|public")
    );
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "bad");
    let host_pid: u32 = std::fs::read_to_string(workspace.join("server.pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        host_pid, running_pid,
        "sandbox changes must reuse the connection"
    );
    let running_descendant: u32 = std::fs::read_to_string(workspace.join("server.child.pid"))
        .unwrap()
        .parse()
        .unwrap();
    let mut readonly = context.clone();
    readonly.actor_call.as_mut().unwrap().remote_read_only = true;
    std::fs::write(&secret, "private").unwrap();
    assert!(
        app.execute_tool(&call(&name, json!({"value":false})), &readonly)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "private");
    let broker = crate::sudo_host::tests::local_client();
    let mut administrator = host.clone();
    administrator.sandbox_policy.administrator = Some(broker.clone());
    assert!(
        app.execute_tool(&call(&name, json!({"value":true})), &administrator)
            .await
            .is_err(),
        "changing administrator authority requires explicit restart"
    );
    administrator.restart_native = true;
    let result = app
        .execute_tool(&call(&name, json!({"value":true})), &administrator)
        .await
        .unwrap();
    assert_eq!(
        result.model_content.as_deref(),
        Some("explicit|ambient-fixture|public")
    );
    let administrator_pid: u32 = std::fs::read_to_string(workspace.join("server.pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(administrator_pid, host_pid);
    let administrator_descendant: u32 = std::fs::read_to_string(workspace.join("server.child.pid"))
        .unwrap()
        .parse()
        .unwrap();
    app.prepare_turn(&context, true).await.unwrap();
    assert!(
        app.execute_tool(&call(&name, json!({"value":false})), &context)
            .await
            .is_err(),
        "ordinary requests cannot borrow a reviewed administrator connection"
    );
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "private");
    app.shutdown().await.unwrap();
    broker.close();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let mut processes = sysinfo::System::new();
            processes.refresh_processes(
                sysinfo::ProcessesToUpdate::Some(&[
                    sysinfo::Pid::from_u32(preflight_pid),
                    sysinfo::Pid::from_u32(running_pid),
                    sysinfo::Pid::from_u32(administrator_pid),
                    sysinfo::Pid::from_u32(preflight_descendant),
                    sysinfo::Pid::from_u32(running_descendant),
                    sysinfo::Pid::from_u32(administrator_descendant),
                ]),
                true,
            );
            if processes
                .processes()
                .values()
                .all(|process| process.status() == sysinfo::ProcessStatus::Zombie)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("MCP processes must exit on preflight close and App shutdown");
    system.shutdown().await;
}

#[cfg(windows)]
fn assert_host_integrity(pid: u32) {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Security::{
            GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation,
            TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel, TokenIsAppContainer,
        },
        System::Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    };
    unsafe fn identity(process: windows_sys::Win32::Foundation::HANDLE) -> (u32, u32) {
        unsafe {
            let mut token = std::ptr::null_mut();
            assert_ne!(OpenProcessToken(process, TOKEN_QUERY, &mut token), 0);
            let token = OwnedHandle::from_raw_handle(token);
            let mut app_container = 0u32;
            let mut needed = 0;
            assert_ne!(
                GetTokenInformation(
                    token.as_raw_handle(),
                    TokenIsAppContainer,
                    (&mut app_container as *mut u32).cast(),
                    4,
                    &mut needed
                ),
                0
            );
            let mut label = [0usize; 16];
            assert_ne!(
                GetTokenInformation(
                    token.as_raw_handle(),
                    TokenIntegrityLevel,
                    label.as_mut_ptr().cast(),
                    std::mem::size_of_val(&label) as u32,
                    &mut needed
                ),
                0
            );
            let sid = (*label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid;
            let count = *GetSidSubAuthorityCount(sid);
            (
                app_container,
                *GetSidSubAuthority(sid, u32::from(count - 1)),
            )
        }
    }
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        assert!(!process.is_null(), "MCP must still be running");
        let process = OwnedHandle::from_raw_handle(process);
        let (app_container, integrity) = identity(process.as_raw_handle());
        assert_eq!(app_container, 0, "MCP must not use an AppContainer token");
        assert_eq!(
            integrity,
            identity(GetCurrentProcess()).1,
            "MCP must inherit ordinary host integrity without a Low downgrade"
        );
    }
}

#[tokio::test]
async fn remote_errors_keep_original_content_and_embedded_resources_use_actor_ledger() {
    let server = server().await;
    let directory = tempfile::tempdir().unwrap();
    let system = actor_system(directory.path()).await;
    system.seed_test_actor("test-actor");
    let registry = Registry::open(directory.path().join("apps.json")).unwrap();
    let context = context(directory.path());
    let entry = registry
        .install(
            "Example".into(),
            String::new(),
            Source::Http {
                url: server.url.clone(),
                bearer_token: None,
            },
            &context,
        )
        .await
        .unwrap();
    let mut app = McpApp::new(entry, registry, system.clone());
    let name = app.tool_specs()[0].name.clone();
    let failure = app
        .execute_tool(
            &call(&name, json!({"arguments_json":"{\"value\":\"failure\"}"})),
            &context,
        )
        .await
        .unwrap();
    assert!(failure.failure.is_some());
    assert_eq!(
        failure.model_content.as_deref(),
        Some("record was rejected")
    );
    let resource = app
        .execute_tool(
            &call(&name, json!({"arguments_json":"{\"value\":\"resource\"}"})),
            &context,
        )
        .await
        .unwrap();
    let id = resource.payload["resources"][0]["id"].as_str().unwrap();
    let descriptor = crate::resources::descriptor(&system, "test-actor", id).unwrap();
    assert_eq!(
        crate::resources::bytes(&system, "test-actor", &descriptor).unwrap(),
        b"original resource bytes"
    );
    assert!(crate::resources::descriptor(&system, "core", id).is_err());
    app.shutdown().await.unwrap();
    system.shutdown().await;
}
