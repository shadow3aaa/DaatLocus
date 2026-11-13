use super::*;

struct NoModel;
impl ModelFactory for NoModel {
    fn build(
        &self,
        _: &str,
        _: &crate::config::Config,
    ) -> Result<Arc<dyn crate::core::ModelProvider>> {
        panic!("No model is needed to create a session")
    }
}

#[tokio::test]
async fn a_fresh_profile_has_no_preset_actor_and_creates_its_first_session() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let system = ActorSystem::open_paused_with_factory(
        temp.path().join("state"),
        workspace,
        crate::config::Config::default(),
        Arc::new(NoModel),
    )
    .await
    .unwrap();
    assert!(system.store().actors().unwrap().is_empty());
    let created = system
        .create_session(None, None, Some("first"))
        .await
        .unwrap();
    assert_ne!(created.id, "core");
    assert_eq!(system.actor(&created.id).unwrap().role, ActorRole::Session);
    assert!(system.actor("core").is_err());
    system.shutdown().await;
}

fn record(id: &str, session: &str, role: ActorRole) -> ActorRecord {
    ActorRecord {
        remote_read_only: false,
        id: id.into(),
        session_id: session.into(),
        role,
        title: None,
        parent_id: (id != session).then(|| session.into()),
        depth: u32::from(id != session),
        config: ActorConfig {
            name: id.into(),
            workspace: ".".into(),
            apps: vec!["actors".into()],
            model: None,
            instructions: String::new(),
        },
        context: Default::default(),
        status: ActorStatus::Idle,
        created_at_ms: 1,
        updated_at_ms: 1,
    }
}
fn message(actor: &str, sender: Option<&str>, content: &str) -> MessageRecord {
    MessageRecord {
        seq: 0,
        id: Uuid::new_v4().to_string(),
        actor_id: actor.into(),
        source: sender.map_or(MessageSource::User, |s| MessageSource::Actor {
            actor_id: s.into(),
        }),
        reply_to: ReplyTarget::User,
        kind: MessageKind::Request,
        in_reply_to: None,
        remote_authority: None,
        content: content.into(),
        attachments: vec![],
        skills: vec![],
        status: MessageStatus::Queued,
        cancel_requested: false,
        started_at_ms: None,
        finished_at_ms: None,
        final_generation: None,
        reply: None,
        error: None,
        created_at_ms: 2,
        updated_at_ms: 2,
    }
}

#[test]
fn session_scope_and_archive_fence_apply_inside_the_enqueue_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let store = ActorStore::open(&temp.path().join("actors.sqlite3")).unwrap();
    for actor in [
        record("a", "a", ActorRole::Session),
        record("b", "b", ActorRole::Session),
        record("worker", "a", ActorRole::Worker),
    ] {
        store.save_actor(&actor).unwrap();
    }
    assert_eq!(
        store.account_actor_ids("worker").unwrap(),
        std::collections::HashSet::from(["a".into(), "worker".into()])
    );
    assert!(
        store
            .enqueue(message("b", Some("worker"), "cross-session"), None)
            .is_err()
    );
    store
        .enqueue(message("worker", Some("a"), "work"), None)
        .unwrap();
    store.set_session_archived("a", true).unwrap();
    assert!(store.claim_next("worker").unwrap().is_none());
    assert!(
        store
            .enqueue(message("worker", Some("a"), "late"), None)
            .is_err()
    );
    assert_eq!(
        store.session_summary("a").unwrap().status,
        ActorStatus::Archived
    );
    assert!(store.delete_session("b").is_err());
    store.delete_session("a").unwrap();
    assert!(store.actor("worker").is_err());
    assert!(store.actor("b").is_ok());
    assert!(
        store
            .events(0, None, true, 100)
            .unwrap()
            .iter()
            .any(|e| matches!(&e.event,ActorEvent::SessionDeleted{session_id} if session_id=="a"))
    );
}

#[test]
fn session_titles_preserve_unicode_boundaries_and_explicit_renames() {
    let temp = tempfile::tempdir().unwrap();
    let store = ActorStore::open(&temp.path().join("actors.sqlite3")).unwrap();
    store
        .save_actor(&record("a", "a", ActorRole::Session))
        .unwrap();
    store
        .enqueue(
            message("a", None, &format!("{}\nsecond line", "界".repeat(90))),
            None,
        )
        .unwrap();
    assert_eq!(
        store.session_summary("a").unwrap().title,
        Some("界".repeat(80))
    );
    store
        .patch_session("a", Some(Some("  renamed  ".into())), None)
        .unwrap();
    store.enqueue(message("a", None, "later"), None).unwrap();
    assert_eq!(
        store.session_summary("a").unwrap().title.as_deref(),
        Some("renamed")
    );
    store
        .patch_session("a", Some(Some("  ".into())), None)
        .unwrap();
    assert_eq!(store.session_summary("a").unwrap().title, None);
}

#[test]
fn existing_profiles_gain_session_ownership_without_losing_history() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actors.sqlite3");
    let store = ActorStore::open(&path).unwrap();
    store
        .save_actor(&record("core", "core", ActorRole::Session))
        .unwrap();
    store
        .save_actor(&record("worker", "core", ActorRole::Worker))
        .unwrap();
    let input = store
        .enqueue(message("core", None, "original input"), None)
        .unwrap();
    store
        .stage_transaction(|tx| {
            tx.execute(
                "UPDATE actors SET record=json_remove(record,'$.session_id','$.role','$.title')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    drop(store);
    let store = ActorStore::open(&path).unwrap();
    assert_eq!(store.actor("core").unwrap().role, ActorRole::Session);
    assert_eq!(store.actor("worker").unwrap().session_id, "core");
    assert_eq!(store.message(&input.id).unwrap().content, "original input");
    assert_eq!(store.session_summary("core").unwrap().title, None);
}

#[test]
fn migrated_core_exposes_local_deliveries_without_a_replication_commit() {
    let temp = tempfile::tempdir().unwrap();
    let store = ActorStore::open(&temp.path().join("actors.sqlite3")).unwrap();
    store
        .save_actor(&record("core", "core", ActorRole::Session))
        .unwrap();
    store
        .save_actor(&record("worker", "core", ActorRole::Worker))
        .unwrap();
    store.stage_transaction(|tx| {
        tx.execute_batch("CREATE TABLE core_order(entity TEXT, remote_key TEXT, ordinal INTEGER);
            CREATE TABLE core_rows(entity TEXT,remote_key TEXT); CREATE TABLE core_meta(key TEXT,value TEXT);")?;
        Ok(())
    }).unwrap();
    let input = store
        .enqueue(message("core", None, "Owner request"), None)
        .unwrap();
    let mut delivery = message("core", Some("worker"), "Finished work");
    delivery.kind = MessageKind::Delivery;
    delivery.status = MessageStatus::Completed;
    let delivered = store.enqueue(delivery, None).unwrap();
    store.publish_delivery(&delivered.id).unwrap();
    let listed = store.messages("core", None, 10).unwrap();
    assert_eq!(
        listed
            .items
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        vec![input.id.as_str(), delivered.id.as_str()]
    );
    assert!(store.events(0,Some("core"),false,100).unwrap().iter().any(|e|
        matches!(&e.event,ActorEvent::MessageAdded { message_id } if message_id==&delivered.id)));
    assert_eq!(store.claim_next("core").unwrap().unwrap().id, input.id);
}

#[test]
fn session_catalog_pages_by_activity_and_keeps_archive_filters_separate() {
    let temp = tempfile::tempdir().unwrap();
    let store = ActorStore::open(&temp.path().join("actors.sqlite3")).unwrap();
    let mut alpha = record("alpha", "alpha", ActorRole::Session);
    alpha.created_at_ms = 10;
    let mut beta = record("beta", "beta", ActorRole::Session);
    beta.created_at_ms = 20;
    store.save_actor(&alpha).unwrap();
    store.save_actor(&beta).unwrap();
    let mut newest = message("alpha", None, "Most recent");
    newest.created_at_ms = 30;
    newest.updated_at_ms = 30;
    store.enqueue(newest, None).unwrap();
    let (first, cursor) = store.session_page(false, None, 1).unwrap();
    assert_eq!(first[0].id, "alpha");
    let (second, end) = store.session_page(false, cursor.as_deref(), 1).unwrap();
    assert_eq!(second[0].id, "beta");
    assert!(end.is_none());
    store.set_session_archived("alpha", true).unwrap();
    assert_eq!(store.session_page(false, None, 10).unwrap().0.len(), 1);
    assert_eq!(store.session_page(true, None, 10).unwrap().0[0].id, "alpha");
}

#[test]
fn unbound_actor_cannot_inherit_access_through_a_shared_session_label() {
    let temp = tempfile::tempdir().unwrap();
    let store = ActorStore::open(&temp.path().join("actors.sqlite3")).unwrap();
    store
        .save_actor(&record("owner", "owner", ActorRole::Session))
        .unwrap();
    let mut stray = record("unbound", "owner", ActorRole::Worker);
    stray.parent_id = None;
    store.save_actor(&stray).unwrap();
    store
        .stage_transaction(|tx| {
            tx.execute_batch(
                "CREATE TABLE stage_account_actors(actor TEXT PRIMARY KEY,account TEXT NOT NULL);
            INSERT INTO stage_account_actors VALUES('owner','account-a');",
            )?;
            Ok(())
        })
        .unwrap();
    assert!(store.account_actor_ids("unbound").unwrap().is_empty());
    assert_eq!(
        store.account_actor_ids("owner").unwrap(),
        std::collections::HashSet::from(["owner".into()])
    );
}
