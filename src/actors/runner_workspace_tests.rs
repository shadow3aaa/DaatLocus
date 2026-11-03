use super::*;

#[tokio::test]
async fn workspace_migration_redirects_following_calls_in_the_same_protocol_group() {
    let provider = Arc::new(ReplayProvider::new(vec![]));
    let (_directory, system) = runtime(provider.clone()).await;
    // Exercise native default-directory migration independently of any mount
    // dependency. Isolated projections have separate opt-in native tests.
    system.set_workspace_backend_for_test(true);
    let original = system.actor("root").unwrap().config.workspace;
    let selected = original.join("selected");
    std::fs::create_dir(&selected).unwrap();
    provider.replies.lock().extend([
        MockReply::Calls(vec![
            AgentToolCall {
                id: "select-workspace".into(),
                name: "workspace__select".into(),
                arguments: json!({"path":selected,"branch":"default"}),
            },
            AgentToolCall {
                id: "write-in-new-cwd".into(),
                name: "files__write_file".into(),
                arguments: json!({"path":"result.txt","content":"new cwd bytes\n"}),
            },
        ]),
        MockReply::Answer("The file is in the selected directory."),
    ]);
    let input = system
        .send_user(
            "root",
            "Switch workspace and write the result".into(),
            vec![],
            None,
        )
        .unwrap();
    let finished = wait_completed(&system, &input.id).await;
    assert_eq!(finished.status, MessageStatus::Completed, "{finished:?}");
    assert_eq!(
        std::fs::read(selected.join("result.txt")).unwrap(),
        b"new cwd bytes\n"
    );
    assert!(!original.join("result.txt").exists());
    assert_eq!(
        system.actor("root").unwrap().config.workspace,
        selected.canonicalize().unwrap()
    );
    let tools = system
        .store()
        .tool_summaries("root", &input.id, None, 10)
        .unwrap();
    assert_eq!(tools.items.len(), 2);
    assert!(
        tools.items.iter().all(|tool| tool.error.is_none()),
        "{:?}",
        tools.items
    );
    system.shutdown().await;
}
