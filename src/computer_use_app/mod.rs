//! Window-directed desktop tools. Native state lives on one dedicated OS thread.
#![cfg_attr(not(windows), allow(dead_code))]
use crate::{
    actors::ActorSystem,
    app::{App, AppId, AppToolExecutionContext, AppToolExecutionResult, AppToolSpec},
    model_protocol::AgentToolCall,
    schema_utils::model_schema_for,
};
use async_trait::async_trait;
use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[cfg(any(target_os = "android", test))]
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub mod android;
#[cfg(target_os = "macos")]
mod macos;
pub mod preview;
pub(crate) mod settings;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::Workspace;

/// Internal launcher entry point, before service configuration or credentials are read.
#[cfg(windows)]
pub fn launch_from_clean_process(app: &str, display: &str, bounds: [i32; 4]) -> Result<()> {
    windows::launch_from_clean_process(app, display, bounds)
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    query: Option<String>,
    app_id: Option<String>,
    cursor: Option<String>,
    limit: u32,
}
#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AppArgs {
    app_id: String,
}
#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WindowArgs {
    window_id: String,
}
#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StateArgs {
    window_id: String,
    state_id: String,
}
#[model_schema]
#[derive(Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImageRegion {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}
#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ObserveArgs {
    window_id: String,
    region: Option<ImageRegion>,
    scale: Option<u32>,
}
#[model_schema]
#[derive(Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum WindowAdjustment {
    Resize,
    Move,
    Fit,
    Restore,
}
#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AdjustArgs {
    window_id: String,
    /// Null permits recovery when no current image is available.
    state_id: Option<String>,
    action: WindowAdjustment,
    width: Option<i32>,
    height: Option<i32>,
    x: Option<i32>,
    y: Option<i32>,
}

#[derive(Clone, Copy)]
struct ObservedView {
    region: ImageRegion,
    scale: u32,
}
impl ObservedView {
    fn new(
        width: i32,
        height: i32,
        region: Option<ImageRegion>,
        scale: Option<u32>,
    ) -> Result<Self> {
        let region = region.unwrap_or(ImageRegion {
            x: 0,
            y: 0,
            width,
            height,
        });
        let scale = scale.unwrap_or(1);
        if !(1..=4).contains(&scale)
            || region.x < 0
            || region.y < 0
            || region.width <= 0
            || region.height <= 0
            || region
                .x
                .checked_add(region.width)
                .is_none_or(|right| right > width)
            || region
                .y
                .checked_add(region.height)
                .is_none_or(|bottom| bottom > height)
        {
            return Err(miette!(
                "view region must fit the complete window and scale must be 1..=4"
            ));
        }
        if (i64::from(region.width) * i64::from(region.height))
            .checked_mul(i64::from(scale).pow(2))
            .is_none_or(|pixels| pixels > 8_000_000)
        {
            return Err(miette!(
                "image is too large; select a smaller region or lower scale"
            ));
        }
        Ok(Self { region, scale })
    }
    fn width(self) -> i32 {
        self.region.width * self.scale as i32
    }
    fn height(self) -> i32 {
        self.region.height * self.scale as i32
    }
    fn window_point(self, point: ImagePoint) -> Result<ImagePoint> {
        if point.x < 0 || point.y < 0 || point.x >= self.width() || point.y >= self.height() {
            return Err(miette!("point is outside the returned image"));
        }
        Ok(ImagePoint {
            x: self.region.x + point.x / self.scale as i32,
            y: self.region.y + point.y / self.scale as i32,
        })
    }
}
#[model_schema]
#[derive(Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Button {
    Left,
    Right,
    Middle,
}
#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ClickArgs {
    window_id: String,
    state_id: String,
    x: i32,
    y: i32,
    button: Button,
    click_count: u32,
}
#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TextArgs {
    window_id: String,
    state_id: String,
    text: String,
}
#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct KeyArgs {
    window_id: String,
    state_id: String,
    key: String,
}
#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ScrollArgs {
    window_id: String,
    state_id: String,
    x: i32,
    y: i32,
    delta_x: i32,
    delta_y: i32,
}
#[model_schema]
#[derive(Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImagePoint {
    x: i32,
    y: i32,
}
#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DragArgs {
    window_id: String,
    state_id: String,
    path: Vec<ImagePoint>,
}
#[model_schema]
#[derive(Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Placement {
    Original,
    Center,
    LeftHalf,
    RightHalf,
}
#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GiveArgs {
    window_id: String,
    state_id: String,
    placement: Placement,
}

pub struct ComputerUseApp {
    actor_id: String,
    system: ActorSystem,
    supports_vision: bool,
    #[cfg(windows)]
    client: Option<windows::Client>,
    #[cfg(target_os = "android")]
    android: android::Client,
    #[cfg(target_os = "macos")]
    macos: macos::Client,
}

impl ComputerUseApp {
    pub fn new(actor_id: String, system: ActorSystem, supports_vision: bool) -> Self {
        Self {
            actor_id,
            system,
            supports_vision,
            #[cfg(windows)]
            client: None,
            #[cfg(target_os = "android")]
            android: android::Client::new(),
            #[cfg(target_os = "macos")]
            macos: macos::Client::new(),
        }
    }
    fn specifications() -> Vec<AppToolSpec> {
        #[cfg(target_os = "android")]
        return android::specifications();
        #[cfg(target_os = "macos")]
        return macos::specifications();
        #[cfg(not(any(target_os = "android", target_os = "macos")))]
        Self::windows_specifications()
    }
    fn windows_specifications() -> Vec<AppToolSpec> {
        let tool = |name: &str, description: &str, input_schema| AppToolSpec {
            name: name.into(),
            description: description.into(),
            input_schema,
        };
        vec![
            tool(
                "list_apps",
                "List a bounded page of discovered Windows apps. Use only returned app IDs.",
                model_schema_for::<ListArgs>(),
            ),
            tool(
                "list_windows",
                "List a bounded page of actual windows, with opaque IDs and ownership. Listing does not take a window.",
                model_schema_for::<ListArgs>(),
            ),
            tool(
                "launch_app",
                "Prepare virtual-screen space before launching a discovered app, passing its initial position/size or Shell monitor hint. A unique, correlated new window confirmed on that screen is taken and returned as an image. Apps can override hints or reuse existing windows; unconfirmed startup returns outcome_unknown, never ownership of an existing user window. Do not replay launch.",
                model_schema_for::<AppArgs>(),
            ),
            tool(
                "open_window",
                "Take this returned background window onto the device's shared virtual screen and return its screenshot. Placement prefers free space, then cascades overlapping windows without moving other windows. Never select a user's working window merely because its app name matches.",
                model_schema_for::<WindowArgs>(),
            ),
            tool(
                "get_window_state",
                "Observe the complete taken window, including its title bar. Null region/scale returns the full window at native resolution. A region uses full-window pixel coordinates; scale 1..4 magnifies it. Input coordinates always refer to the returned image; the host handles crop/scale conversion. After outcome_unknown, timeout or capture failure, observe before deciding whether another input is needed.",
                model_schema_for::<ObserveArgs>(),
            ),
            tool(
                "adjust_window",
                "Resize, move, fit or restore a taken window without activating it. Resize requires width/height; move requires x/y in virtual-screen pixels; fit uses the full screen work area; restore keeps its reserved size. Other parameters must be null. Null state_id allows recovery after capture failure. Returns a fresh full-window image and revokes earlier image coordinates and the selected input receiver.",
                model_schema_for::<AdjustArgs>(),
            ),
            tool(
                "click",
                "Click an image-relative point in a taken window. Selects its actual input receiver and returns a fresh image/state. Unsupported mouse buttons fail without input.",
                model_schema_for::<ClickArgs>(),
            ),
            tool(
                "type_text",
                "Insert literal Unicode into the last explicitly selected input receiver. Use press_key for control keys. Returns a fresh image/state.",
                model_schema_for::<TextArgs>(),
            ),
            tool(
                "press_key",
                "Send a supported key or Ctrl/Shift chord to the selected receiver. Read supported actions; no foreground or system-input fallback.",
                model_schema_for::<KeyArgs>(),
            ),
            tool(
                "scroll",
                "Scroll at an image-relative point in wheel notches; positive Y is down. Unsupported axes fail before input. Returns a fresh image/state.",
                model_schema_for::<ScrollArgs>(),
            ),
            tool(
                "drag",
                "Drag a left-button path in image pixels. Two points define a straight line; the host interpolates movement. Existing user capture is refused. Returns a fresh image/state.",
                model_schema_for::<DragArgs>(),
            ),
            tool(
                "give_window",
                "Give a taken window and its controlled same-thread family to the user: revoke input and old IDs, remove hooks, then place on a physical screen. The Agent cannot operate handed-off windows until a new explicit take.",
                model_schema_for::<GiveArgs>(),
            ),
            tool(
                "release_window",
                "Release this taken window and its controlled family back to original physical positions, revoking all old input/window references. Does not close the app.",
                model_schema_for::<WindowArgs>(),
            ),
            tool(
                "close_window",
                "Request closing this taken window, not its process. Unsaved changes can produce a modal; observe the result before continuing.",
                model_schema_for::<StateArgs>(),
            ),
        ]
    }
    fn check_context(context: &AppToolExecutionContext) -> Result<()> {
        if context
            .actor_call
            .as_ref()
            .is_some_and(|call| call.remote_read_only)
        {
            return Err(miette!(
                "read-only remote authority cannot access the interactive desktop"
            ));
        }
        // ComputerUse's reviewed desktop capability explicitly covers host GUI
        // access outside the project filesystem sandbox. Do not modify that
        // sandbox: Files/Terminal and other Apps retain their original policy.
        Ok(())
    }
}

#[async_trait]
impl App for ComputerUseApp {
    fn id(&self) -> AppId {
        AppId::new(if cfg!(target_os = "android") {
            "phone_use"
        } else {
            "computer_use"
        })
        .expect("static App identifier")
    }
    fn tool_specs(&self) -> Vec<AppToolSpec> {
        Self::specifications()
    }
    fn can_passivate(&self) -> bool {
        #[cfg(windows)]
        {
            self.client
                .as_ref()
                .is_none_or(|client| !client.has_resources())
        }
        #[cfg(target_os = "android")]
        {
            self.android.can_passivate()
        }
        #[cfg(target_os = "macos")]
        {
            self.macos.can_passivate()
        }
        #[cfg(not(any(windows, target_os = "android", target_os = "macos")))]
        {
            true
        }
    }
    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        Self::check_context(context)?;
        if context
            .actor_call
            .as_ref()
            .is_some_and(|call| call.actor_id != self.actor_id)
        {
            return Err(miette!("ComputerUse belongs to another Actor"));
        }
        if !self.supports_vision
            && !matches!(
                call.name.as_str(),
                "list_apps" | "list_windows" | "release_window" | "give_window"
            )
        {
            return Err(miette!(
                "ComputerUse observation and input require a vision-capable model"
            ));
        }
        #[cfg(windows)]
        {
            if self.client.is_none() {
                self.client = Some(windows::Client::start(
                    self.actor_id.clone(),
                    self.system.computer_workspace()?,
                )?);
            }
            let reply = self
                .client
                .as_ref()
                .expect("initialized client")
                .call(call.clone(), context.clone())
                .await?;
            self.publish_reply(reply.summary, reply.payload, reply.failure, reply.image)
        }
        #[cfg(target_os = "android")]
        {
            let reply = self.android.call(&self.actor_id, call, context).await?;
            self.publish_reply(reply.summary, reply.payload, reply.failure, reply.image)
        }
        #[cfg(target_os = "macos")]
        {
            let reply = self
                .macos
                .call(&self.actor_id, &self.system, call, context)
                .await?;
            self.publish_reply(reply.summary, reply.payload, reply.failure, reply.image)
        }
        #[cfg(not(any(windows, target_os = "android", target_os = "macos")))]
        {
            let _ = (&self.system, call);
            Err(miette!("ComputerUse requires a supported native backend"))
        }
    }
    async fn shutdown(&mut self) -> Result<()> {
        #[cfg(windows)]
        if let Some(client) = self.client.take() {
            client.shutdown().await?;
        }
        #[cfg(target_os = "android")]
        self.android.shutdown(&self.actor_id).await?;
        #[cfg(target_os = "macos")]
        self.macos.shutdown(&self.actor_id, &self.system).await?;
        Ok(())
    }
}

impl ComputerUseApp {
    #[cfg(any(windows, target_os = "android", target_os = "macos"))]
    fn publish_reply(
        &self,
        summary: String,
        payload: Value,
        failure: Option<String>,
        image: Option<Vec<u8>>,
    ) -> Result<AppToolExecutionResult> {
        let mut result = AppToolExecutionResult::new(summary, payload, None);
        result.failure = failure;
        if let Some(bytes) = image {
            let resource = match crate::resources::publish(
                &self.system,
                &self.actor_id,
                "computer-use.png",
                "image/png",
                &bytes,
            ) {
                Ok(resource) => resource,
                Err(_) => {
                    result.payload["status"] = json!("outcome_unknown");
                    result.payload["observation_error"] = json!(
                        "Screenshot resource publication failed; observe before continuing, do not replay the operation"
                    );
                    result.failure = Some("ComputerUse screenshot resource unavailable after native operation; do not replay".into());
                    return Ok(result);
                }
            };
            result.payload["observation"]["screenshot"] =
                serde_json::to_value(&resource).map_err(|e| miette!("{e}"))?;
            result.payload["observation"]["screenshot_url"] = json!(format!(
                "/v1/actors/{}/resources/{}",
                self.actor_id, resource.id
            ));
            result
                    .model_images
                    .push(crate::model_protocol::AgentContentPart::ResourceImage {
                        resource,
                        description: Some(
                            "Observed window or explicitly selected region. Input coordinates use this returned image; the host maps its crop and scale."
                                .into(),
                        ),
                    });
        }
        Ok(result)
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T> {
    serde_json::from_value(value.clone()).map_err(|e| miette!("invalid ComputerUse arguments: {e}"))
}

fn page(field: &str, rows: Vec<Value>, cursor: Option<&str>, limit: u32) -> Result<Value> {
    let offset = cursor
        .map(str::parse::<usize>)
        .transpose()
        .map_err(|_| miette!("invalid page cursor"))?
        .unwrap_or(0);
    if offset > rows.len() {
        return Err(miette!("page cursor is no longer valid; enumerate again"));
    }
    let end = offset.saturating_add(limit as usize).min(rows.len());
    let mut payload = json!({"next_cursor":if end<rows.len(){Some(end.to_string())}else{None},"status":"observed"});
    payload[field] = json!(&rows[offset..end]);
    Ok(payload)
}

#[cfg(windows)]
fn key(value: &str) -> Result<(u16, daat_locus_virtual_workspace::KeyModifiers)> {
    let (code, shift, control) = parse_key(value)?;
    Ok((
        code,
        daat_locus_virtual_workspace::KeyModifiers { shift, control },
    ))
}
fn parse_key(value: &str) -> Result<(u16, bool, bool)> {
    let parts: Vec<String> = value
        .split('+')
        .map(|s| s.trim().to_ascii_lowercase())
        .collect();
    let (mut shift, mut control, mut key) = (false, false, None);
    for part in parts {
        match part.as_str() {
            "ctrl" | "control" | "control_l" => control = true,
            "shift" | "shift_l" => shift = true,
            "tab" | "alt" | "win" | "meta" | "super" | "command" => {
                return Err(miette!(
                    "this navigation/system key is unsupported; use observed controls"
                ));
            }
            _ => {
                if key.is_some() {
                    return Err(miette!("one non-modifier key is required"));
                }
                key = Some(match part.as_str() {
                    "enter" | "return" => 0x0d,
                    "escape" | "esc" => 0x1b,
                    "backspace" => 0x08,
                    "delete" => 0x2e,
                    "home" => 0x24,
                    "end" => 0x23,
                    "left" => 0x25,
                    "up" => 0x26,
                    "right" => 0x27,
                    "down" => 0x28,
                    "pageup" | "prior" => 0x21,
                    "pagedown" | "next" => 0x22,
                    "space" => 0x20,
                    _ if part.len() == 1 && part.as_bytes()[0].is_ascii_alphanumeric() => {
                        part.as_bytes()[0].to_ascii_uppercase() as u16
                    }
                    _ => return Err(miette!("unsupported key name")),
                });
            }
        }
    }
    Ok((
        key.ok_or_else(|| miette!("a non-modifier key is required"))?,
        shift,
        control,
    ))
}

#[cfg(windows)]
fn trajectory(path: &[ImagePoint]) -> Result<Vec<daat_locus_virtual_workspace::Point>> {
    interpolate(path).map(|path| {
        path.into_iter()
            .map(|p| daat_locus_virtual_workspace::Point { x: p.x, y: p.y })
            .collect()
    })
}
fn interpolate(path: &[ImagePoint]) -> Result<Vec<ImagePoint>> {
    if !(2..=128).contains(&path.len()) {
        return Err(miette!("drag needs 2..=128 path points"));
    }
    if path.len() != 2 {
        return Ok(path.to_vec());
    }
    let (start, end) = (path[0], path[1]);
    Ok((0..=24)
        .map(|step| ImagePoint {
            x: (start.x as i64 + (end.x as i64 - start.x as i64) * step / 24) as i32,
            y: (start.y as i64 + (end.y as i64 - start.y as i64) * step / 24) as i32,
        })
        .collect())
}

#[cfg(windows)]
fn place_in_area(
    original: daat_locus_virtual_workspace::Rect,
    area: daat_locus_virtual_workspace::Rect,
    placement: Placement,
) -> daat_locus_virtual_workspace::Rect {
    let [left, top, right, bottom] = placement_rect(
        [original.left, original.top, original.right, original.bottom],
        [area.left, area.top, area.right, area.bottom],
        placement,
    );
    daat_locus_virtual_workspace::Rect {
        left,
        top,
        right,
        bottom,
    }
}
fn placement_rect(original: [i32; 4], area: [i32; 4], placement: Placement) -> [i32; 4] {
    let [left, top, right, bottom] = area;
    let width = (original[2] as i64 - original[0] as i64)
        .clamp(1, (right as i64 - left as i64).max(1)) as i32;
    let height = (original[3] as i64 - original[1] as i64)
        .clamp(1, (bottom as i64 - top as i64).max(1)) as i32;
    match placement {
        Placement::LeftHalf => [left, top, left + (right - left) / 2, bottom],
        Placement::RightHalf => [left + (right - left) / 2, top, right, bottom],
        Placement::Center => {
            let x = left + (right - left - width) / 2;
            let y = top + (bottom - top - height) / 2;
            [x, y, x + width, y + height]
        }
        Placement::Original => {
            let x = original[0].clamp(left, right - width);
            let y = original[1].clamp(top, bottom - height);
            [x, y, x + width, y + height]
        }
    }
}

#[cfg(test)]
mod tests;
