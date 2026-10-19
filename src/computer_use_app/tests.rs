use super::*;

#[test]
fn observed_views_map_full_images_and_magnified_crops_to_window_pixels() {
    let full = ObservedView::new(800, 600, None, None).unwrap();
    assert_eq!((full.width(), full.height()), (800, 600));
    let point = full.window_point(ImagePoint { x: 799, y: 599 }).unwrap();
    assert_eq!((point.x, point.y), (799, 599));
    let crop = ObservedView::new(
        800,
        600,
        Some(ImageRegion {
            x: 120,
            y: 40,
            width: 200,
            height: 100,
        }),
        Some(3),
    )
    .unwrap();
    assert_eq!((crop.width(), crop.height()), (600, 300));
    for (input, expected) in [
        ((0, 0), (120, 40)),
        ((5, 8), (121, 42)),
        ((599, 299), (319, 139)),
    ] {
        let point = crop
            .window_point(ImagePoint {
                x: input.0,
                y: input.1,
            })
            .unwrap();
        assert_eq!((point.x, point.y), expected);
    }
    for (x, y) in [(-1, 0), (0, -1), (600, 0), (0, 300)] {
        assert!(crop.window_point(ImagePoint { x, y }).is_err());
    }
}

#[test]
fn views_refuse_partial_crops_oversized_images_and_coordinate_overflow() {
    for region in [
        ImageRegion {
            x: -1,
            y: 0,
            width: 10,
            height: 10,
        },
        ImageRegion {
            x: 790,
            y: 0,
            width: 11,
            height: 10,
        },
        ImageRegion {
            x: 0,
            y: 590,
            width: 10,
            height: 11,
        },
        ImageRegion {
            x: 0,
            y: 0,
            width: 0,
            height: 10,
        },
        ImageRegion {
            x: i32::MAX,
            y: 0,
            width: 1,
            height: 10,
        },
    ] {
        assert!(ObservedView::new(800, 600, Some(region), None).is_err());
    }
    for scale in [0, 5, u32::MAX] {
        assert!(ObservedView::new(800, 600, None, Some(scale)).is_err());
    }
    assert!(ObservedView::new(1920, 1080, None, Some(4)).is_err());
    assert!(ObservedView::new(i32::MAX, i32::MAX, None, Some(4)).is_err());
}

#[test]
#[cfg(not(target_os = "android"))]
fn all_computer_schemas_are_portable_and_handoff_is_explicit() {
    let tools = ComputerUseApp::specifications();
    for tool in &tools {
        crate::schema_utils::validate_model_facing_schema(&tool.input_schema).unwrap();
    }
    let give = tools.iter().find(|t| t.name == "give_window").unwrap();
    crate::schema_utils::validate_value_against_schema(
        &json!({"window_id":"returned-window","state_id":"latest-image","placement":"right_half"}),
        &give.input_schema,
        "give_window",
    )
    .unwrap();
    assert!(
        crate::schema_utils::validate_value_against_schema(
            &json!({"window_id":"returned-window","placement":"right_half"}),
            &give.input_schema,
            "give_window",
        )
        .is_err()
    );
    assert!(!tools.iter().any(|t| t.name == "activate_window"));
}

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
    for name in [
        "type_text",
        "adjust_window",
        "give_window",
        "activate_window",
    ] {
        assert!(!tools.iter().any(|tool| tool.name == name));
    }
}

#[test]
fn physical_handoff_uses_work_area_and_clamps_removed_display_positions() {
    assert_eq!(
        placement_rect(
            [2200, 100, 3000, 700],
            [0, 0, 1920, 1040],
            Placement::Original
        ),
        [1120, 100, 1920, 700]
    );
    assert_eq!(
        placement_rect([0, 0, 900, 600], [-1920, 0, 0, 1040], Placement::RightHalf),
        [-960, 0, 0, 1040]
    );
    assert_eq!(
        placement_rect([0, 0, 4000, 3000], [0, 0, 1920, 1040], Placement::Center),
        [0, 0, 1920, 1040]
    );
}

#[test]
fn key_parser_rejects_unsupported_system_or_accelerator_routing() {
    assert_eq!(parse_key("Control_L+Shift+a").unwrap(), (0x41, true, true));
    assert_eq!(parse_key("Return").unwrap(), (0x0d, false, false));
    for key in ["Win+r", "Alt+F4", "Tab", "Ctrl", "a+b"] {
        assert!(parse_key(key).is_err(), "{key}");
    }
}

#[test]
fn drag_interpolation_preserves_negative_coordinates_and_endpoints() {
    let points =
        interpolate(&[ImagePoint { x: -20, y: 10 }, ImagePoint { x: 100, y: 58 }]).unwrap();
    assert_eq!((points[0].x, points[0].y), (-20, 10));
    assert_eq!((points[12].x, points[12].y), (40, 34));
    assert_eq!((points[24].x, points[24].y), (100, 58));
    assert!(interpolate(&[ImagePoint { x: 0, y: 0 }]).is_err());
}

#[test]
fn desktop_exception_preserves_other_app_policy_and_rejects_read_only_authority() {
    let mut context = AppToolExecutionContext {
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
        private_roots: vec![],
        device_policy: None,
    });
    assert!(ComputerUseApp::check_context(&context).is_err());
}
