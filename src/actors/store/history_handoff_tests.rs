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
    store
        .reset_model_context("worker", "task-B", "Budget recovery checkpoint")
        .unwrap();
    let first = store.actor("worker").unwrap().context.anchor.unwrap();
    assert_eq!(first.handoff.as_ref().unwrap().task_message_id, "task-B");
    assert_eq!(
        first.handoff.as_ref().unwrap().checkpoint.as_deref(),
        Some("Budget recovery checkpoint")
    );
    assert_eq!(
        store
            .task_handoff("worker", "task-B")
            .unwrap()
            .checkpoint
            .as_deref(),
        Some("Budget recovery checkpoint")
    );
    assert!(
        store
            .task_handoff("worker", "progress-A")
            .unwrap()
            .checkpoint
            .is_none()
    );
    let first_start = store.context_start("worker").unwrap();
    store
        .reset_model_context("worker", "task-B", "Budget recovery checkpoint")
        .unwrap();
    let repeated = store.actor("worker").unwrap().context.anchor.unwrap();
    assert_ne!(first.reset_id, repeated.reset_id);
    assert_eq!(first.handoff, repeated.handoff);
    assert!(store.context_start("worker").unwrap() > first_start);
    store
        .reset_model_context("worker", "progress-A", "Queue investigation checkpoint")
        .unwrap();
    let note = store
        .actor("worker")
        .unwrap()
        .context
        .anchor
        .unwrap()
        .handoff
        .unwrap();
    assert_eq!(note.task_message_id, "task-A");
    assert_eq!(
        note.checkpoint.as_deref(),
        Some("Queue investigation checkpoint")
    );
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
fn task_handoff_anchor_without_checkpoint_rebuilds_from_durable_history() {
    let store = memory_store();
    let original = AgentMessage::user("Preserve the original task and observations.");
    history(&store, "task", original.clone());
    let mut actor = store.actor("worker").unwrap();
    actor.context.anchor = Some(HistoryAnchor {
        message_id: "task".into(),
        reset_id: Uuid::new_v4(),
        handoff: None,
    });
    store.save_actor(&actor).unwrap();
    assert_eq!(store.context_start("worker").unwrap(), 0);
    assert!(store.history_page("worker", 0, None, 10).unwrap()[0].2 == original);
    let recovery = store
        .reset_model_context(
            "worker",
            "task",
            "Original task retained; inspect its observations next.",
        )
        .unwrap();
    let start = store.context_start("worker").unwrap();
    assert!(start > 0);
    assert!(store.history_page("worker", start - 1, None, 10).unwrap()[0].2 == recovery);
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
    assert!(
        new_task.checkpoint.is_none(),
        "a new request must not borrow another task's checkpoint"
    );
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
    let AgentMessage::User { content } = recovery else {
        panic!()
    };
    let text = content.as_text();
    let note: crate::actors::context::TaskHandoff =
        serde_json::from_str(&text[text.find('{').unwrap()..=text.rfind('}').unwrap()]).unwrap();
    assert_eq!(note.task_message_id, "root");
    assert!(note.related_message_ids.iter().any(|id| id == "result-6"));
}
