//! Explicit opt-in contract against the separate, disposable AppKit fixture.
use super::*;
use base64::Engine;

fn expect(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error(message.into()))
    }
}
fn observation(reply: Reply) -> Result<Value> {
    if let Some(error) = reply.failure {
        return Err(Error(error));
    }
    let image = base64::engine::general_purpose::STANDARD
        .decode(
            reply
                .image
                .ok_or_else(|| Error("Missing real screenshot".into()))?,
        )
        .map_err(|_| Error("Invalid PNG encoding".into()))?;
    expect(
        image.len() > 24 && &image[..8] == b"\x89PNG\r\n\x1a\n" && &image[12..16] == b"IHDR",
        "Capture is not a PNG",
    )?;
    let value = reply.payload["observation"].clone();
    let width = u32::from_be_bytes(image[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(image[20..24].try_into().unwrap());
    expect(
        value["width"] == width && value["height"] == height,
        "PNG dimensions disagree with coordinate metadata",
    )?;
    Ok(value)
}
fn control<'a>(view: &'a Value, id: &str) -> Result<&'a Value> {
    view["controls"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["identifier"] == id))
        .ok_or_else(|| Error(format!("Missing native control: {id}")))
}
fn point(view: &Value, id: &str, portion: f64) -> Result<Value> {
    let bounds = &control(view, id)?["bounds"];
    Ok(
        json!({"x":(bounds["x"].as_f64().unwrap()+bounds["width"].as_f64().unwrap()*portion).round() as i64,"y":(bounds["y"].as_f64().unwrap()+bounds["height"].as_f64().unwrap()/2.0).round() as i64}),
    )
}
pub fn run_native_contract(root: &Path) -> Result<Value> {
    expect(
        root.file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("mac-test-")),
        "Native GUI tests require an isolated mac-test-* profile",
    )?;
    let until = std::time::Instant::now() + Duration::from_secs(300);
    loop {
        if permissions(root).is_ok_and(|p| p.ready()) {
            break;
        }
        if std::time::Instant::now() >= until {
            return Err(Error("GUI test prerequisite unavailable: enable GUI operation and grant Accessibility and Screen Recording".into()));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    std::thread::sleep(Duration::from_secs(2));
    let mut host = Host::default();
    let cancelled = Arc::new(Mutex::new(HashMap::new()));
    let session = Uuid::new_v4().to_string();
    let invoke = |host: &mut Host, scope: &str, operation: &str, arguments: Value| {
        host.call(
            root,
            &Request {
                id: Uuid::new_v4().to_string(),
                scope: scope.into(),
                session: session.clone(),
                epoch: 2,
                operation: operation.into(),
                arguments,
            },
            &cancelled,
        )
    };
    let query =
        json!({"app_id":null,"query":"Daat Locus Native Fixture","cursor":null,"limit":100});
    let listed = invoke(&mut host, "test-a", "list_windows", query.clone());
    expect(
        listed.failure.is_none(),
        &format!("Window discovery failed: {:?}", listed.failure),
    )?;
    let windows = listed.payload["windows"]
        .as_array()
        .ok_or_else(|| Error("Invalid window page".into()))?;
    expect(
        windows.len() == 1,
        "Start exactly one disposable Native Fixture window",
    )?;
    let id = windows[0]["window_id"].as_str().unwrap().to_owned();
    let fixture_pid = host.scopes["test-a"].windows[&id].receipt["pid"].clone();
    let fixture_path = host.scopes["test-a"].windows[&id].receipt["path"].clone();
    let desktop = result(native(json!({"op":"desktop_state"}))?)?;
    expect(
        desktop["pid"] != fixture_pid,
        "Fixture must be in the background before testing",
    )?;
    let mut observed = observation(invoke(
        &mut host,
        "test-a",
        "open_window",
        json!({"window_id":id}),
    ))?;
    expect(
        control(&observed, "native-editor")?["value"] == "initial",
        "Fixture was not in its initial state",
    )?;
    let mut input = point(&observed, "native-editor", 0.5)?;
    input["window_id"] = json!(id);
    input["state_id"] = observed["state_id"].clone();
    input["button"] = json!("left");
    input["click_count"] = json!(1);
    let old = observed["state_id"].clone();
    observed = observation(invoke(&mut host, "test-a", "click", input))?;
    let stale = invoke(
        &mut host,
        "test-a",
        "type_text",
        json!({"window_id":id,"state_id":old,"text":"must not appear"}),
    );
    expect(
        stale.failure.is_some() && stale.payload["status"] == "not_started",
        "Stale screenshot reference was accepted",
    )?;
    observed = observation(invoke(
        &mut host,
        "test-a",
        "press_key",
        json!({"window_id":id,"state_id":observed["state_id"],"key":"Super+a"}),
    ))?;
    observed = observation(invoke(
        &mut host,
        "test-a",
        "type_text",
        json!({"window_id":id,"state_id":observed["state_id"],"text":"后台 Unicode 🚀"}),
    ))?;
    expect(
        observed["input_target"]["text"] == "后台 Unicode 🚀",
        "Unicode did not reach the actual editable control",
    )?;
    observed = observation(invoke(
        &mut host,
        "test-a",
        "press_key",
        json!({"window_id":id,"state_id":observed["state_id"],"key":"Backspace"}),
    ))?;
    expect(
        observed["input_target"]["text"] == "后台 Unicode ",
        "Backspace split a Unicode character",
    )?;
    let mut input = point(&observed, "native-increment", 0.5)?;
    input["window_id"] = json!(id);
    input["state_id"] = observed["state_id"].clone();
    input["button"] = json!("left");
    input["click_count"] = json!(1);
    observed = observation(invoke(&mut host, "test-a", "click", input))?;
    expect(
        control(&observed, "native-counter")?["value"] == "Count: 1",
        "AXPress did not invoke the actual button action",
    )?;
    let start = point(&observed, "native-slider", 0.1)?;
    let end = point(&observed, "native-slider", 0.8)?;
    observed = observation(invoke(
        &mut host,
        "test-a",
        "drag",
        json!({"window_id":id,"state_id":observed["state_id"],"path":[start,end]}),
    ))?;
    let value = control(&observed, "native-slider")?["value"]
        .as_f64()
        .unwrap_or(0.0);
    expect(
        value > 70.0 && value < 90.0,
        "Background drag did not change the actual slider",
    )?;
    let mut scroll = point(&observed, "native-scroll", 0.5)?;
    scroll["window_id"] = json!(id);
    scroll["state_id"] = observed["state_id"].clone();
    scroll["delta_x"] = json!(0);
    scroll["delta_y"] = json!(2);
    observed = observation(invoke(&mut host, "test-a", "scroll", scroll))?;
    expect(
        observed["controls"].as_array().is_some_and(|rows| {
            rows.iter().any(|row| {
                row["role"] == "AXScrollBar" && row["value"].as_f64().is_some_and(|v| v > 0.0)
            })
        }),
        "Background scroll did not move the actual scroll bar",
    )?;
    observed = observation(invoke(
        &mut host,
        "test-a",
        "adjust_window",
        json!({"window_id":id,"state_id":observed["state_id"],"action":"resize","width":560,"height":500,"x":null,"y":null}),
    ))?;
    expect(
        observed["window_width"] == 1120 && observed["window_height"] == 1000,
        "Background resize did not update native capture geometry",
    )?;
    let cancelled_id = Uuid::new_v4().to_string();
    cancelled
        .lock()
        .unwrap()
        .insert(cancelled_id.clone(), std::time::Instant::now());
    let denied = host.call(
        root,
        &Request {
            id: cancelled_id,
            scope: "test-a".into(),
            session: session.clone(),
            epoch: 2,
            operation: "close_window".into(),
            arguments: json!({"window_id":id,"state_id":observed["state_id"]}),
        },
        &cancelled,
    );
    expect(
        denied.failure.is_some() && denied.payload["status"] == "not_started",
        "Cancelled operation reached the target",
    )?;
    let other = invoke(&mut host, "test-b", "list_windows", query);
    expect(
        other.payload["windows"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "Another scope discovered an acquired window",
    )?;
    let stale_epoch = host.call(
        root,
        &Request {
            id: Uuid::new_v4().to_string(),
            scope: "test-a".into(),
            session: session.clone(),
            epoch: 1,
            operation: "close_window".into(),
            arguments: json!({"window_id":id,"state_id":observed["state_id"]}),
        },
        &cancelled,
    );
    expect(
        stale_epoch.failure.is_some() && stale_epoch.payload["status"] == "not_started",
        "Stale execution epoch was accepted",
    )?;
    let crop = observation(invoke(
        &mut host,
        "test-a",
        "get_window_state",
        json!({"window_id":id,"region":{"x":0,"y":0,"width":300,"height":200},"scale":2}),
    ))?;
    expect(
        crop["width"] == 600 && crop["height"] == 400,
        "Crop and scale did not match the PNG",
    )?;
    let released = invoke(
        &mut host,
        "test-a",
        "release_window",
        json!({"window_id":id}),
    );
    expect(
        released.failure.is_none() && !released.has_resources,
        "Release failed",
    )?;
    let revoked = invoke(
        &mut host,
        "test-a",
        "get_window_state",
        json!({"window_id":id,"region":null,"scale":null}),
    );
    expect(
        revoked.failure.is_some(),
        "Released reference remained actionable",
    )?;
    configure(root, false)?;
    let disabled = invoke(
        &mut host,
        "test-a",
        "list_windows",
        json!({"query":null,"app_id":null,"cursor":null,"limit":100}),
    );
    configure(root, true)?;
    expect(
        disabled.failure.is_some(),
        "Disabled GUI capability admitted work",
    )?;
    // Close the actual disposable window, then launch its exact discovered app.
    // Poll process exit, never repeat the close or an uncertain launch.
    let listed = invoke(
        &mut host,
        "test-a",
        "list_windows",
        json!({"app_id":null,"query":"Daat Locus Native Fixture","cursor":null,"limit":100}),
    );
    let fresh_id = listed.payload["windows"][0]["window_id"]
        .as_str()
        .ok_or_else(|| Error("Fixture disappeared before close test".into()))?
        .to_owned();
    let fresh = observation(invoke(
        &mut host,
        "test-a",
        "open_window",
        json!({"window_id":fresh_id}),
    ))?;
    let closed = invoke(
        &mut host,
        "test-a",
        "close_window",
        json!({"window_id":fresh_id,"state_id":fresh["state_id"]}),
    );
    expect(
        closed.failure.is_none() && !closed.has_resources,
        "Native close was not confirmed",
    )?;
    let stopped = std::time::Instant::now() + Duration::from_secs(3);
    while unsafe { libc::kill(fixture_pid.as_i64().unwrap() as i32, 0) } == 0 {
        expect(
            std::time::Instant::now() < stopped,
            "Fixture did not exit after closing its last window",
        )?;
        std::thread::sleep(Duration::from_millis(50));
    }
    let apps = invoke(
        &mut host,
        "test-a",
        "list_apps",
        json!({"app_id":null,"query":"Daat Locus Native Fixture","cursor":null,"limit":100}),
    );
    expect(apps.failure.is_none(), "Fixture app discovery failed")?;
    let app_id = host.scopes["test-a"]
        .apps
        .iter()
        .find(|(_, app)| app["path"] == fixture_path)
        .map(|(id, _)| id.clone())
        .ok_or_else(|| Error("Installed fixture app was not discovered".into()))?;
    let launched = observation(invoke(
        &mut host,
        "test-a",
        "launch_app",
        json!({"app_id":app_id}),
    ))?;
    expect(
        control(&launched, "native-editor")?["value"] == "initial",
        "Background launch did not produce a fresh window",
    )?;
    expect(
        control(&launched, "native-counter")?["value"] == "Count: 0",
        "Relaunch did not reset fixture state",
    )?;
    let closed = invoke(
        &mut host,
        "test-a",
        "close_window",
        json!({"window_id":launched["window_id"],"state_id":launched["state_id"]}),
    );
    expect(
        closed.failure.is_none() && !closed.has_resources,
        "Relaunched window close was not confirmed",
    )?;
    let after = result(native(json!({"op":"desktop_state"}))?)?;
    expect(
        after == desktop,
        "Background operations changed the foreground application or physical pointer",
    )?;
    Ok(
        json!({"window_capture":true,"background_editing":true,"unicode_editing":true,"actual_button_action":true,"background_launch":true,"actual_window_close":true,"native_scroll":true,"slider_drag":true,"native_resize":true,"stale_state_rejected":true,"stale_epoch_rejected":true,"cancellation":true,"scope_isolation":true,"png_crop_scale":true,"released_reference_rejected":true,"disabled_admission_rejected":true,"foreground_and_physical_cursor_unchanged":true,"foreground_pid":desktop["pid"]}),
    )
}
