//! Persistent device authority must survive the retirement of Actor-owned grants.
use daat_locus::actors::store::ActorStore;
use rusqlite::params;
use serde_json::json;

#[tokio::test]
async fn actors_with_different_workspaces_execute_files_against_the_same_device_scope() {
    use daat_locus::{
        actors::{
            ActorSystem, MessageKind, MessageRecord, MessageSource, MessageStatus, ReplyTarget,
        },
        app::App,
        config::Config,
        files_app::FilesApp,
        model_protocol::AgentToolCall,
    };
    let target = std::env::current_dir().unwrap().join("target");
    std::fs::create_dir_all(&target).unwrap();
    let root = tempfile::Builder::new()
        .prefix("device-sandbox-")
        .tempdir_in(target)
        .unwrap();
    let workspace = root.path().join("workspace");
    let ungranted = root.path().join("different-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&ungranted).unwrap();
    let shared = workspace.join("shared.txt");
    let outside = ungranted.join("outside.txt");
    std::fs::write(&shared, "shared device content\n").unwrap();
    std::fs::write(&outside, "readable without a workspace grant\n").unwrap();
    let system = ActorSystem::open(
        root.path().join("data"),
        workspace.clone(),
        Config::default(),
    )
    .await
    .unwrap();
    let core = system.actor("core").unwrap();
    let mut contexts = vec![];
    for (id, directory) in [("a", &workspace), ("b", &ungranted)] {
        let mut actor = core.clone();
        actor.id = id.into();
        actor.parent_id = Some("core".into());
        actor.config.name = id.into();
        actor.config.workspace = std::fs::canonicalize(directory).unwrap();
        actor.config.apps = vec!["files".into()];
        system.store().save_actor(&actor).unwrap();
        let input = MessageRecord {
            seq: 0,
            id: format!("input-{id}"),
            actor_id: id.into(),
            source: MessageSource::User,
            reply_to: ReplyTarget::User,
            kind: MessageKind::Request,
            in_reply_to: None,
            remote_authority: None,
            content: "Read the shared device file".into(),
            attachments: vec![],
            status: MessageStatus::Queued,
            cancel_requested: false,
            started_at_ms: None,
            finished_at_ms: None,
            final_generation: None,
            reply: None,
            error: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        system.store().enqueue(input, None).unwrap();
        let message = system.store().claim_next(id).unwrap().unwrap();
        contexts.push(system.execution_context(&actor, &message).unwrap());
    }
    assert_ne!(contexts[0].execution_cwd, contexts[1].execution_cwd);
    assert_eq!(
        contexts[0].sandbox_policy.native,
        contexts[1].sandbox_policy.native
    );
    let mut files = FilesApp::new(root.path().join("images"), false);
    for context in &contexts {
        let read = |path: &std::path::Path| AgentToolCall {
            id: "read".into(),
            name: "read_file".into(),
            arguments: json!({"path":path,"start_line":null,"line_count":null}),
        };
        let result = files.execute_tool(&read(&shared), context).await.unwrap();
        assert_eq!(result.payload["content"], "shared device content\n");
        let read_outside = files.execute_tool(&read(&outside), context).await.unwrap();
        assert_eq!(
            read_outside.payload["content"],
            "readable without a workspace grant\n"
        );
        let error = files
            .execute_tool(&AgentToolCall {
                id: "ungranted-write".into(), name: "write_file".into(),
                arguments: json!({"path":ungranted.join("new.txt"),"content":"forbidden","expected_sha256":null}),
            }, context)
            .await
            .unwrap_err();
        let denial = error
            .downcast_ref::<daat_locus::sandbox::AccessDenied>()
            .expect("actual typed sandbox denial");
        assert_eq!(denial.record.access, daat_locus::sandbox::Access::Write);
        assert_eq!(
            std::fs::canonicalize(&denial.record.path).unwrap(),
            std::fs::canonicalize(&ungranted).unwrap()
        );
        assert!(!ungranted.join("new.txt").exists());
    }
    system.shutdown().await;
}

#[test]
fn upgrade_consolidates_filesystem_authority_without_retaining_actor_permissions() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("actors.sqlite3");
    let workspace = root.path().join("workspace");
    let other_workspace = root.path().join("other-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&other_workspace).unwrap();
    // Legacy Actor configuration persisted canonical roots, including macOS's
    // /private prefix. An alias is not proof that an old grant can be rebound.
    let workspace = std::fs::canonicalize(workspace).unwrap();
    let other_workspace = std::fs::canonicalize(other_workspace).unwrap();
    {
        let db = rusqlite::Connection::open(&database).unwrap();
        db.execute_batch("CREATE TABLE actors(id TEXT PRIMARY KEY,parent_id TEXT,depth INTEGER NOT NULL,record TEXT NOT NULL);
            CREATE TABLE actor_approval_scopes(actor_id TEXT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,fingerprint TEXT NOT NULL,record TEXT NOT NULL,PRIMARY KEY(actor_id,fingerprint));").unwrap();
        for (id, directory) in [("a", &workspace), ("b", &other_workspace)] {
            let actor = json!({"id":id,"parent_id":null,"depth":0,"config":{
                "name":id,"workspace":directory,"apps":["actors"],"model":null,"instructions":"",
                "permissions":{"approval_mode":"ask","readable_roots":[directory],"writable_roots":[directory]}},
                "context":daat_locus::actors::AgentContext::default(),"status":"idle","created_at_ms":1,"updated_at_ms":1});
            db.execute(
                "INSERT INTO actors VALUES(?1,NULL,0,?2)",
                params![id, actor.to_string()],
            )
            .unwrap();
        }
        db.execute_batch("CREATE TABLE messages(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,actor_id TEXT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,status TEXT NOT NULL,record TEXT NOT NULL);
            CREATE TABLE approvals(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,actor_id TEXT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,record TEXT NOT NULL);").unwrap();
        let message = json!({"seq":1,"id":"original-input","actor_id":"a","source":{"kind":"user"},
            "reply_to":{"kind":"user"},"kind":"request","in_reply_to":null,
            "remote_authority":{"peer":"00000000-0000-4000-8000-000000000001","grant_actor":"a","reply_for":null},
            "delegation_grant":{"permissions":{"read_only":true}},
            "content":"original request","attachments":[],"status":"completed","cancel_requested":false,
            "reply":null,"error":null,"created_at_ms":1,"updated_at_ms":1});
        db.execute("INSERT INTO messages(id,actor_id,status,record) VALUES('original-input','a','\"completed\"',?1)", [message.to_string()]).unwrap();
        let approval = json!({"seq":1,"id":"legacy-review","actor_id":"a","message_id":"original-input",
            "call":{"id":"source-call","name":"files__write_file","arguments":{"path":workspace.join("future.txt")}},
            "requested":{"readable_roots":[],"writable_roots":[workspace],"operation_details":{"retired":"facts"}},
            "scopes":[{"kind":"filesystem","path":workspace,"write":true,"recursive":true}],
            "decision":"pending","reason":"waiting","created_at_ms":1,"resolved_at_ms":null,
            "mode":"ask","review":{"policy":"legacy-action-fingerprint"}});
        db.execute("INSERT INTO approvals(id,actor_id,message_id,record) VALUES('legacy-review','a','original-input',?1)", [approval.to_string()]).unwrap();
        for (actor, key, scope) in [
            (
                "a",
                "parent",
                json!({"kind":"filesystem","path":workspace,"write":true,"recursive":true}),
            ),
            (
                "b",
                "covered",
                json!({"kind":"filesystem","path":workspace.join("future/file"),"write":false,"recursive":false}),
            ),
            (
                "b",
                "retired-action",
                json!({"kind":"app","namespace":"fixture","policy":"old","action":"old"}),
            ),
        ] {
            db.execute(
                "INSERT INTO actor_approval_scopes VALUES(?1,?2,?3)",
                params![actor, key, scope.to_string()],
            )
            .unwrap();
        }
    }
    let store = ActorStore::open(&database).unwrap();
    let authority = store
        .message("original-input")
        .unwrap()
        .remote_authority
        .unwrap();
    assert!(
        authority.read_only,
        "legacy readonly constraints must survive permission migration"
    );
    assert_eq!(authority.origin_actor, "a");
    let migrated = store.approval("legacy-review").unwrap();
    assert_eq!(migrated.source_actor(&store).unwrap(), "a");
    assert_eq!(
        migrated.decision,
        daat_locus::actors::ApprovalDecision::Cancelled
    );
    assert_eq!(migrated.call.id, "source-call");
    let serialized = serde_json::to_value(&migrated).unwrap();
    assert!(serialized.get("actor_id").is_none());
    assert!(serialized["requested"].get("operation_details").is_none());
    let scopes = store.sandbox_scopes().unwrap();
    assert_eq!(scopes.len(), 2);
    for directory in [&workspace, &other_workspace] {
        let canonical = std::fs::canonicalize(directory).unwrap();
        assert!(
            scopes
                .iter()
                .any(|scope| scope.path == canonical && scope.write && scope.recursive)
        );
    }
    for id in ["a", "b"] {
        let actor = store.actor(id).unwrap();
        let serialized = serde_json::to_value(actor).unwrap();
        assert!(serialized["config"].get("permissions").is_none());
    }
    drop(store);
    {
        let db = rusqlite::Connection::open(&database).unwrap();
        assert!(
            !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='actor_approval_scopes')",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap()
        );
        assert!(
            !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('approvals') WHERE name='actor_id')",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap()
        );
        // Actor cleanup cannot cascade into the device's authority set.
        db.execute_batch("PRAGMA foreign_keys=ON; DELETE FROM actors;")
            .unwrap();
    }
    assert_eq!(
        ActorStore::open(&database)
            .unwrap()
            .sandbox_scopes()
            .unwrap(),
        scopes
    );
}

/// Exercise the real Files boundary with a controllable asynchronous reviewer.
struct BoundaryReview {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: std::sync::atomic::AtomicUsize,
    policy: daat_locus::sandbox::RuntimeSandboxPolicy,
    approve: bool,
}
#[async_trait::async_trait]
impl daat_locus::sandbox::access::SandboxAccessBroker for BoundaryReview {
    async fn authorize(
        &self,
        denial: daat_locus::sandbox::DenialRecord,
    ) -> miette::Result<daat_locus::sandbox::RuntimeSandboxPolicy> {
        assert_eq!(
            denial.source,
            daat_locus::sandbox::DenialSource::PolicyPreflight
        );
        assert_eq!(denial.access, daat_locus::sandbox::Access::Write);
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.started.notify_one();
        self.release.notified().await;
        if self.approve {
            Ok(self.policy.clone())
        } else {
            Err(miette::miette!("automatic review refused scope"))
        }
    }
}

#[tokio::test]
async fn suspended_filesystem_write_has_no_effect_until_review_and_does_not_replay() {
    use daat_locus::{
        app::{App, AppToolExecutionContext},
        files_app::FilesApp,
        model_protocol::AgentToolCall,
        sandbox::{
            FileSystemSandboxPolicy, RuntimeSandboxPolicy, SandboxPolicy,
            StrongFilesystemSandboxMode, WritableRoot,
        },
    };
    use std::sync::{Arc, atomic::Ordering};
    for (approve, cancel) in [(true, false), (false, false), (true, true)] {
        let target_dir = std::env::current_dir().unwrap().join("target");
        std::fs::create_dir_all(&target_dir).unwrap();
        let root = tempfile::Builder::new()
            .prefix("suspended-boundary-")
            .tempdir_in(target_dir)
            .unwrap();
        let file = root.path().join("report.txt");
        // Original bytes must survive both refusal and cancellation.
        std::fs::write(&file, "original").unwrap();
        let policy = RuntimeSandboxPolicy {
            administrator: None,
            builtin_resources: vec![],
            native: SandboxPolicy {
                filesystem: FileSystemSandboxPolicy {
                    full_disk_read: false,
                    full_disk_write: false,
                    deny_all_writes: false,
                    readable_roots: vec![root.path().to_owned()],
                    writable_roots: vec![],
                    deny_read_paths: vec![],
                    deny_write_paths: vec![],
                    bound_roots: vec![],
                },
                strong_filesystem: StrongFilesystemSandboxMode::Required,
            },
        };
        let mut granted = policy.clone();
        granted.filesystem.writable_roots.push(WritableRoot {
            root: root.path().to_owned(),
            read_only_subpaths: vec![],
        });
        let review = Arc::new(BoundaryReview {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            calls: std::sync::atomic::AtomicUsize::new(0),
            policy: granted,
            approve,
        });
        // File freshness forces the successful operation to use the original
        // bytes, and would reject replay after the first successful write.
        let expected = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"original"));
        let context = AppToolExecutionContext {
            execution_cwd: root.path().to_owned(),
            sandbox_policy: policy,
            tool_output_max_tokens: 1024,
            turn_epoch: 0,
            actor_call: None,
            restart_native: false,
            access_broker: Some(review.clone()),
        };
        let call = AgentToolCall {
            id: "one-write".into(),
            name: "write_file".into(),
            arguments: json!({"path":file,"content":"approved once","expected_sha256":expected}),
        };
        let mut files = FilesApp::new(root.path().join("images"), false);
        let mut operation = Box::pin(files.execute_tool(&call, &context));
        tokio::select! {
            _ = review.started.notified() => {},
            value = &mut operation => panic!("operation returned before review: {value:?}"),
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");
        assert_eq!(review.calls.load(Ordering::Relaxed), 1);
        if cancel {
            drop(operation);
        } else {
            review.release.notify_one();
            let result = operation.await;
            assert_eq!(result.is_ok(), approve, "{result:?}");
        }
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            if approve && !cancel {
                "approved once"
            } else {
                "original"
            }
        );
        assert_eq!(review.calls.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn legacy_device_actor_lists_become_restrictive_device_authority_without_cross_account_access() {
    use daat_locus::{
        actors::{ActorConfig, ActorRecord, ActorStatus},
        stage::{
            identity::DeviceIdentity,
            ledger::{DeviceAuthority, StageLedger},
        },
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device.sqlite3");
    let store = ActorStore::open(&path).unwrap();
    let mut local = DeviceIdentity::generate().unwrap();
    let remote = DeviceIdentity::generate().unwrap();
    let root = local.initialize_group(1000, 900_000).unwrap();
    let enrollment = local
        .authorize(
            &root,
            &remote.join_request(1000).unwrap(),
            1000,
            800_000,
            false,
        )
        .unwrap();
    for id in ["root", "child", "foreign"] {
        store
            .save_actor(&ActorRecord {
                remote_read_only: false,
                context: Default::default(),
                id: id.into(),
                parent_id: (id != "root").then(|| "root".into()),
                depth: u32::from(id != "root"),
                config: ActorConfig {
                    name: id.into(),
                    workspace: directory.path().into(),
                    apps: vec!["files".into()],
                    model: None,
                    instructions: String::new(),
                },
                status: ActorStatus::Idle,
                created_at_ms: 1000,
                updated_at_ms: 1000,
            })
            .unwrap();
    }
    let ledger = StageLedger::initialize(&store, local.profile().stage_id, &root.root).unwrap();
    let peer = ledger
        .import_peer(enrollment.clone(), DeviceAuthority::default(), 1001)
        .unwrap();
    let account = &root.root.group_id;
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE IF NOT EXISTS stage_account_actors(actor TEXT PRIMARY KEY REFERENCES actors(id),account TEXT NOT NULL);").unwrap();
    for (actor, owner) in [
        ("root", account.as_str()),
        ("child", account.as_str()),
        ("foreign", "other-account"),
    ] {
        db.execute(
            "INSERT OR REPLACE INTO stage_account_actors VALUES(?1,?2)",
            params![actor, owner],
        )
        .unwrap();
    }
    let mut old = serde_json::to_value(peer).unwrap();
    old.as_object_mut().unwrap().remove("authority");
    old["grants"] = json!([
        {"actor_id":"root","read":true,"converse":true,"delegate":true,"read_only":false},
        {"actor_id":"child","read":true,"converse":true,"delegate":false,"read_only":true}
    ]);
    db.execute(
        "UPDATE stage_peers SET record=?1 WHERE id=?2",
        params![old.to_string(), remote.profile().stage_id.to_string()],
    )
    .unwrap();
    drop(db);
    drop(store);
    let store = ActorStore::open(&path).unwrap();
    let ledger = StageLedger::initialize(&store, local.profile().stage_id, &root.root).unwrap();
    let id = remote.profile().stage_id;
    let expected = DeviceAuthority {
        read: true,
        converse: true,
        delegate: false,
        read_only: true,
    };
    for actor in ["root", "child"] {
        assert_eq!(ledger.authorize_actor(id, actor, 2000).unwrap(), expected);
    }
    assert!(ledger.authorize_actor(id, "foreign", 2000).is_err());
    let peer = ledger.peer(id, 2000).unwrap();
    let serialized = serde_json::to_value(peer).unwrap();
    assert!(serialized.get("grants").is_none());
    assert!(serialized["authority"].get("actor_id").is_none());
    // Verify migration is durable and idempotent, not a deserialization alias.
    StageLedger::initialize(&store, local.profile().stage_id, &root.root).unwrap();
    assert_eq!(ledger.peer(id, 2000).unwrap().authority, expected);
    let db = rusqlite::Connection::open(&path).unwrap();
    let persisted: String = db
        .query_row(
            "SELECT record FROM stage_peers WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&persisted)
            .unwrap()
            .get("grants")
            .is_none()
    );
    ledger.revoke(id).unwrap();
    assert!(ledger.authorize_actor(id, "root", 2000).is_err());
}

#[tokio::test]
async fn recursive_directory_creation_waits_at_the_first_missing_component() {
    use daat_locus::{
        app::AppToolExecutionContext,
        sandbox::{
            FileSystemSandboxPolicy, RuntimeSandboxPolicy, SandboxPolicy,
            StrongFilesystemSandboxMode, WritableRoot,
        },
    };
    use std::sync::{Arc, atomic::Ordering};
    let target = std::env::current_dir().unwrap().join("target");
    std::fs::create_dir_all(&target).unwrap();
    let root = tempfile::Builder::new()
        .prefix("new-workspace-")
        .tempdir_in(target)
        .unwrap();
    let directory = root.path().join("new/nested/workspace");
    let policy = RuntimeSandboxPolicy {
        administrator: None,
        builtin_resources: vec![],
        native: SandboxPolicy {
            filesystem: FileSystemSandboxPolicy {
                full_disk_read: false,
                full_disk_write: false,
                deny_all_writes: false,
                readable_roots: vec![root.path().to_owned()],
                writable_roots: vec![],
                deny_read_paths: vec![],
                deny_write_paths: vec![],
                bound_roots: vec![],
            },
            strong_filesystem: StrongFilesystemSandboxMode::Required,
        },
    };
    let mut granted = policy.clone();
    granted.filesystem.writable_roots.push(WritableRoot {
        root: root.path().join("new"),
        read_only_subpaths: vec![],
    });
    let review = Arc::new(BoundaryReview {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: std::sync::atomic::AtomicUsize::new(0),
        policy: granted,
        approve: true,
    });
    let context = AppToolExecutionContext {
        execution_cwd: root.path().to_owned(),
        sandbox_policy: policy,
        tool_output_max_tokens: 1024,
        turn_epoch: 0,
        actor_call: None,
        restart_native: false,
        access_broker: Some(review.clone()),
    };
    let files = context.files();
    let mut operation = Box::pin(files.ensure_directory(&directory));
    tokio::select! {
        _ = review.started.notified() => {},
        value = &mut operation => panic!("directory creation did not suspend: {value:?}"),
    }
    assert!(
        !root.path().join("new").exists(),
        "review must precede every directory effect"
    );
    review.release.notify_one();
    let applied = operation.await.unwrap();
    assert!(directory.is_dir());
    assert!(applied.is_path_writable(&directory));
    assert!(!applied.is_path_writable(&root.path().join("unrequested")));
    assert_eq!(review.calls.load(Ordering::Relaxed), 1);
}
