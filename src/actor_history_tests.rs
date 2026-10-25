use super::*;

fn args(value: serde_json::Value) -> ReadHistoryArgs {
    serde_json::from_value(value).unwrap()
}
fn task() -> crate::actors::TaskHandoff {
    crate::actors::TaskHandoff {
        task_message_id: "source-request".into(),
        goal: Some("Fix recovery.".into()),
        related_message_ids: vec!["result-input".into()],
    }
}

#[test]
fn task_handoff_history_default_is_runtime_task_not_all_or_result_input() {
    // Old serialized argument objects remain deserializable; null is now safe.
    let query = args(json!({"cursor":null,"message_id":null,"limit":5}));
    assert_eq!(
        query
            .selected_message("result-input", Some(&task()))
            .unwrap(),
        Some("source-request".into())
    );
    assert_eq!(
        query.selected_message("new-request", None).unwrap(),
        Some("new-request".into())
    );
    for scope in [serde_json::Value::Null, json!("current")] {
        let query = args(
            json!({"cursor":null,"message_id":null,"scope":scope,"related_cursor":null,"limit":5}),
        );
        assert_eq!(
            query
                .selected_message("result-input", Some(&task()))
                .unwrap(),
            Some("source-request".into())
        );
    }
}

#[test]
fn task_handoff_history_explicit_input_and_explicit_all_are_distinct() {
    let exact =
        args(json!({"cursor":null,"message_id":"result-input","scope":"current","limit":5}));
    assert_eq!(
        exact
            .selected_message("result-input", Some(&task()))
            .unwrap(),
        Some("result-input".into())
    );
    let all = args(json!({"cursor":null,"message_id":null,"scope":"all","limit":5}));
    assert_eq!(
        all.selected_message("result-input", Some(&task())).unwrap(),
        None
    );
    let ambiguous =
        args(json!({"cursor":null,"message_id":"source-request","scope":"all","limit":5}));
    assert!(
        ambiguous
            .selected_message("result-input", Some(&task()))
            .is_err()
    );
}

#[test]
fn task_handoff_history_schema_exposes_safe_scope_and_separate_locator_cursor() {
    let schema = model_schema_for::<ReadHistoryArgs>();
    crate::schema_utils::validate_model_facing_schema(&schema).unwrap();
    for mode in ["current", "all"] {
        assert!(
            schema["properties"]["scope"]["enum"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == mode)
        );
    }
    assert!(schema["properties"].get("related_cursor").is_some());
}
