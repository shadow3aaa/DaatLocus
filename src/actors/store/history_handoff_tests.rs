use super::*;

// Test the real transactional reset/query methods without filesystem ACLs or
// native processes. The production schema subset and journal are still used.
fn memory_store() -> ActorStore {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE actors(id TEXT PRIMARY KEY,parent_id TEXT,depth INTEGER NOT NULL,record TEXT NOT NULL);
        CREATE TABLE messages(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE,actor_id TEXT,status TEXT,record TEXT);
        CREATE TABLE history(seq INTEGER PRIMARY KEY AUTOINCREMENT,actor_id TEXT,message_id TEXT,record TEXT);
        CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT,actor_id TEXT,created_at INTEGER,record TEXT);").unwrap();
    let tx = db.transaction().unwrap();
    crate::stage::journal::initialize(&tx).unwrap();
    tx.commit().unwrap();
    let actor = ActorRecord {
        remote_read_only: false,
        id: "worker".into(),
        parent_id: None,
        depth: 0,
        config: ActorConfig {
            name: "Worker".into(),
            workspace: ".".into(),
            apps: vec!["actors".into()],
            model: None,
            instructions: String::new(),
            permissions: ActorPermissions::workspace(".".into()),
        },
        context: AgentContext::default(),
        status: ActorStatus::Running,
        created_at_ms: 0,
        updated_at_ms: 0,
    };
    db.execute(
        "INSERT INTO actors VALUES('worker',NULL,0,?1)",
        [encode(&actor).unwrap()],
    )
    .unwrap();
    ActorStore {
        db: Mutex::new(db),
        changed: Arc::new(Notify::new()),
    }
}
fn input(
    store: &ActorStore,
    id: &str,
    kind: MessageKind,
    content: &str,
    parent: Option<&str>,
    actor: &str,
) {
    let message = MessageRecord {
        seq: 0,
        id: id.into(),
        actor_id: actor.into(),
        source: MessageSource::User,
        reply_to: ReplyTarget::User,
        kind,
        in_reply_to: parent.map(str::to_owned),
        delegation_grant: None,
        remote_authority: None,
        content: content.into(),
        attachments: vec![],
        status: MessageStatus::Running,
        cancel_requested: false,
        started_at_ms: None,
        finished_at_ms: None,
        final_generation: None,
        reply: None,
        error: None,
        created_at_ms: 0,
        updated_at_ms: 0,
    };
    store
        .db
        .lock()
        .execute(
            "INSERT INTO messages(id,actor_id,status,record) VALUES(?1,?2,?3,?4)",
            params![
                id,
                actor,
                encode(&message.status).unwrap(),
                encode_message(&message).unwrap()
            ],
        )
        .unwrap();
}
fn history(store: &ActorStore, input: &str, message: AgentMessage) -> i64 {
    let db = store.db.lock();
    db.execute(
        "INSERT INTO history(actor_id,message_id,record) VALUES('worker',?1,?2)",
        params![input, encode(&message).unwrap()],
    )
    .unwrap();
    db.last_insert_rowid()
}

#[test]
fn task_handoff_transaction_repeated_resets_do_not_mix_interleaved_tasks() {
    let store = memory_store();
    input(
        &store,
        "task-A",
        MessageKind::Request,
        "Investigate queue ordering.",
        None,
        "worker",
    );
    input(
        &store,
        "task-B",
        MessageKind::Request,
        "Fix budget recovery.",
        None,
        "worker",
    );
    input(
        &store,
        "progress-A",
        MessageKind::Progress,
        "Task: WRONG UPDATE STATUS",
        Some("task-A"),
        "worker",
    );
    history(&store, "task-A", AgentMessage::user("OLD TASK DATA"));
    history(&store, "task-B", AgentMessage::user("CURRENT TASK DATA"));
    store.reset_model_context("worker", "task-B").unwrap();
    let first = store.actor("worker").unwrap().context.anchor.unwrap();
    assert_eq!(first.handoff.as_ref().unwrap().task_message_id, "task-B");
    assert_eq!(
        first.handoff.as_ref().unwrap().goal.as_deref(),
        Some("Fix budget recovery.")
    );
    let first_start = store.context_start("worker").unwrap();
    store.reset_model_context("worker", "task-B").unwrap();
    let repeated = store.actor("worker").unwrap().context.anchor.unwrap();
    assert_ne!(first.reset_id, repeated.reset_id);
    assert_eq!(first.handoff, repeated.handoff);
    assert!(store.context_start("worker").unwrap() > first_start);
    store.reset_model_context("worker", "progress-A").unwrap();
    let note = store
        .actor("worker")
        .unwrap()
        .context
        .anchor
        .unwrap()
        .handoff
        .unwrap();
    assert_eq!(note.task_message_id, "task-A");
    assert_eq!(note.goal.as_deref(), Some("Investigate queue ordering."));
    assert!(
        !serde_json::to_string(&note)
            .unwrap()
            .contains("WRONG UPDATE")
    );
    assert_eq!(
        store
            .history_page("worker", 0, Some("task-B"), 100)
            .unwrap()
            .len(),
        3,
        "reset must preserve original source and prior recovery rows"
    );
}

#[test]
fn task_handoff_legacy_anchor_lookup_and_note_failure_are_safe() {
    let store = memory_store();
    let old = HistoryAnchor {
        message_id: "old".into(),
        reset_id: Uuid::new_v4(),
        handoff: None,
    };
    let old_seq = history(&store, "old", old.legacy_recovery_message());
    let mut actor = store.actor("worker").unwrap();
    actor.context.anchor = Some(old);
    store.save_actor(&actor).unwrap();
    assert_eq!(store.context_start("worker").unwrap(), old_seq);
    let recovery = store
        .reset_model_context("worker", "unavailable-source")
        .unwrap();
    assert!(
        store
            .actor("worker")
            .unwrap()
            .context
            .anchor
            .unwrap()
            .handoff
            .is_none()
    );
    assert!(
        matches!(recovery,AgentMessage::User {content} if content.as_text().contains("NOTE generation was unavailable or ambiguous"))
    );
    assert!(store.context_start("worker").unwrap() > old_seq);
}

#[test]
fn task_handoff_related_requests_and_self_loops_never_become_old_task_events() {
    let store = memory_store();
    input(
        &store,
        "root",
        MessageKind::Request,
        "Fix the old task.",
        None,
        "worker",
    );
    input(
        &store,
        "new-request",
        MessageKind::Request,
        "Fix a distinct new task.",
        Some("root"),
        "worker",
    );
    input(
        &store,
        "delivery",
        MessageKind::Delivery,
        "outward only",
        Some("root"),
        "worker",
    );
    input(
        &store,
        "result",
        MessageKind::Result,
        "material result",
        Some("root"),
        "worker",
    );
    let page = store.related_task_inputs("worker", "root", 0, 4).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0]["message_id"], "result");
    let new_task = store.task_handoff("worker", "new-request").unwrap();
    assert_eq!(new_task.task_message_id, "new-request");
    assert_eq!(new_task.goal.as_deref(), Some("Fix a distinct new task."));
    input(
        &store,
        "loop",
        MessageKind::Progress,
        "self loop",
        Some("loop"),
        "worker",
    );
    assert!(
        store
            .related_task_inputs("worker", "loop", 0, 4)
            .unwrap()
            .items
            .is_empty()
    );
    input(
        &store,
        "indirect",
        MessageKind::Progress,
        "nested result update",
        Some("result"),
        "worker",
    );
    assert_eq!(
        store
            .related_task_inputs("worker", "root", 0, 4)
            .unwrap()
            .items
            .len(),
        1,
        "locators are direct-only, never pretend to be a complete event tree"
    );
}

#[test]
fn task_handoff_linked_input_pages_are_bounded_isolated_and_preserve_current_result() {
    let store = memory_store();
    input(
        &store,
        "root",
        MessageKind::Request,
        "Fix recovery.",
        None,
        "worker",
    );
    history(
        &store,
        "root",
        AgentMessage::user("Request source has content"),
    );
    input(
        &store,
        "other-task",
        MessageKind::Request,
        "Fix something unrelated.",
        None,
        "worker",
    );
    let mut expected = Vec::new();
    for index in 0..7 {
        let id = format!("result-{index}");
        input(
            &store,
            &id,
            MessageKind::Result,
            "Current task state",
            Some("root"),
            "worker",
        );
        history(
            &store,
            &id,
            AgentMessage::tool(
                format!("call-{index}"),
                "fixture__effect",
                "already executed; do not replay",
            ),
        );
        input(
            &store,
            &format!("unrelated-{index}"),
            MessageKind::Progress,
            "Related message: root",
            Some("other-task"),
            "worker",
        );
        expected.push(id);
    }
    input(
        &store,
        "foreign-result",
        MessageKind::Result,
        "OTHER ACTOR",
        Some("root"),
        "foreign",
    );
    let first = store.related_task_inputs("worker", "root", 0, 4).unwrap();
    assert_eq!(first.items.len(), 4);
    let second = store
        .related_task_inputs("worker", "root", first.next_cursor.unwrap(), 4)
        .unwrap();
    assert_eq!(second.items.len(), 3);
    assert!(second.next_cursor.is_none());
    let ids: Vec<_> = first
        .items
        .iter()
        .chain(&second.items)
        .map(|item| item["message_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, expected);
    let note = store.task_handoff("worker", "result-6").unwrap();
    assert_eq!(note.task_message_id, "root");
    assert!(
        !store
            .history_page("worker", 0, Some("root"), 10)
            .unwrap()
            .is_empty()
    );
    let current = store
        .history_page("worker", 0, Some("result-6"), 10)
        .unwrap();
    assert!(
        matches!(&current[0].2,AgentMessage::Tool{content,..} if content.contains("already executed")),
        "root history is not the whole task: current input must be read even when root is nonempty"
    );
    let recovery = HistoryAnchor {
        message_id: "result-6".into(),
        reset_id: Uuid::new_v4(),
        handoff: Some(note),
    }
    .recovery_message();
    assert!(
        matches!(recovery,AgentMessage::User{content} if content.as_text().contains("MUST read message_id=\"result-6\""))
    );
}
