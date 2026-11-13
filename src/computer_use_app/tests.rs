use super::*;

#[test]
fn phone_schemas_support_explicit_text_replacement_without_desktop_window_management() {
    let tools = android::specifications();
    for tool in &tools {
        crate::schema_utils::validate_model_facing_schema(&tool.input_schema).unwrap();
    }
    let replace = tools.iter().find(|tool| tool.name == "set_text").unwrap();
    crate::schema_utils::validate_value_against_schema(
        &json!({"window_id":"window","state_id":"image","text":"中文🙂"}),
        &replace.input_schema,
        "set_text",
    )
    .unwrap();
    assert!(
        crate::schema_utils::validate_value_against_schema(
            &json!({"window_id":"window","text":"中文🙂"}),
            &replace.input_schema,
            "set_text",
        )
        .is_err()
    );
}

#[test]
fn drag_interpolation_preserves_endpoints() {
    let points = interpolate(&[ImagePoint { x: 20, y: 10 }, ImagePoint { x: 100, y: 58 }]).unwrap();
    assert_eq!((points[0].x, points[0].y), (20, 10));
    assert_eq!((points[12].x, points[12].y), (60, 34));
    assert_eq!((points[24].x, points[24].y), (100, 58));
    assert!(interpolate(&[ImagePoint { x: 0, y: 0 }]).is_err());
}

#[test]
fn desktop_exception_preserves_other_app_policy_and_rejects_read_only_authority() {
    let mut context = AppToolExecutionContext {
        restart_native: false,
        access_broker: None,
        execution_cwd: ".".into(),
        sandbox_policy: crate::sandbox::RuntimeSandboxPolicy::disabled(),
        tool_output_max_tokens: 4000,
        turn_epoch: 1,
        actor_call: None,
    };
    ComputerUseApp::check_context(&context).unwrap();
    context.sandbox_policy.filesystem.full_disk_write = false;
    context.sandbox_policy.filesystem.full_disk_read = false;
    context.sandbox_policy.strong_filesystem =
        crate::sandbox::StrongFilesystemSandboxMode::Required;
    let policy = context.sandbox_policy.clone();
    ComputerUseApp::check_context(&context).unwrap();
    assert_eq!(context.sandbox_policy, policy);
    context.actor_call = Some(crate::app::ActorCallContext {
        remote_read_only: true,
        actor_id: "actor".into(),
        message_id: "input".into(),
        device_policy: None,
    });
    assert!(ComputerUseApp::check_context(&context).is_err());
}
