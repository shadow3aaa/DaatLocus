//! Native GUI tools for Android and macOS.
#![cfg_attr(not(any(target_os = "android", target_os = "macos")), allow(dead_code))]
#[cfg(any(target_os = "android", target_os = "macos", test))]
use crate::schema_utils::model_schema_for;
use crate::{
    actors::ActorSystem,
    app::{App, AppId, AppToolExecutionContext, AppToolExecutionResult, AppToolSpec},
    model_protocol::AgentToolCall,
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
#[cfg(any(target_os = "android", target_os = "macos", test))]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WindowArgs {
    window_id: String,
}
#[model_schema]
#[cfg(any(target_os = "android", target_os = "macos", test))]
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
struct NativeObserveArgs {
    window_id: String,
    region: Option<ImageRegion>,
    scale: Option<u32>,
}
#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ObserveArgs {
    window_id: String,
    /// Null captures a fresh image; a returned state_id re-views that saved source.
    state_id: Option<String>,
    /// Original screenshot pixels, not coordinates of a previous crop.
    region: Option<crate::image_input::ImageRegion>,
    /// Bounds <=1568 per side / 1150000 pixels; null uses a low-resolution overview.
    resolution: Option<crate::image_input::ImageResolution>,
}

#[model_schema]
#[cfg(any(target_os = "android", target_os = "macos", test))]
#[derive(Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum WindowAdjustment {
    Resize,
    Move,
    Fit,
    Restore,
}
#[model_schema]
#[cfg(any(target_os = "android", target_os = "macos", test))]
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
    /// One key or one chord, for example `CTRL+N`, `ALT+F4`, or `ENTER`.
    /// Modifiers must be included in this single string.
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
#[cfg(any(target_os = "android", target_os = "macos", test))]
#[derive(Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Placement {
    Original,
    Center,
    LeftHalf,
    RightHalf,
}
#[model_schema]
#[cfg(any(target_os = "android", target_os = "macos", test))]
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
        Vec::new()
    }
    #[cfg(any(target_os = "android", target_os = "macos", test))]
    fn base_specifications() -> Vec<AppToolSpec> {
        let tool = |name: &str, description: &str, input_schema| AppToolSpec {
            name: name.into(),
            description: description.into(),
            input_schema,
        };
        vec![
            tool(
                "list_apps",
                "List a bounded page of available applications.",
                model_schema_for::<ListArgs>(),
            ),
            tool(
                "list_windows",
                "List visible application windows.",
                model_schema_for::<ListArgs>(),
            ),
            tool(
                "launch_app",
                "Launch a discovered application and observe its new window.",
                model_schema_for::<AppArgs>(),
            ),
            tool(
                "open_window",
                "Acquire a listed window and observe it.",
                model_schema_for::<WindowArgs>(),
            ),
            tool(
                "get_window_state",
                "Observe an acquired window or re-view a saved image.",
                model_schema_for::<ObserveArgs>(),
            ),
            tool(
                "adjust_window",
                "Adjust the size or position of an acquired window.",
                model_schema_for::<AdjustArgs>(),
            ),
            tool(
                "click",
                "Click an image-relative point in the acquired window.",
                model_schema_for::<ClickArgs>(),
            ),
            tool(
                "type_text",
                "Type literal Unicode into the selected editable control.",
                model_schema_for::<TextArgs>(),
            ),
            tool(
                "press_key",
                "Send a supported key or shortcut to the selected window.",
                model_schema_for::<KeyArgs>(),
            ),
            tool(
                "scroll",
                "Scroll at an image-relative point.",
                model_schema_for::<ScrollArgs>(),
            ),
            tool(
                "drag",
                "Drag along an image-relative path.",
                model_schema_for::<DragArgs>(),
            ),
            tool(
                "give_window",
                "Hand an acquired window back to the user.",
                model_schema_for::<GiveArgs>(),
            ),
            tool(
                "release_window",
                "Release an acquired window without closing its application.",
                model_schema_for::<WindowArgs>(),
            ),
            tool(
                "close_window",
                "Close an acquired window without terminating its application.",
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
    fn set_vision_support(&mut self, supported: bool) {
        self.supports_vision = supported;
    }
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
        #[cfg(target_os = "android")]
        {
            self.android.can_passivate()
        }
        #[cfg(target_os = "macos")]
        {
            self.macos.can_passivate()
        }
        #[cfg(not(any(target_os = "android", target_os = "macos")))]
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
        if !self.supports_vision && !matches!(call.name.as_str(), "list_apps" | "list_windows") {
            return Err(miette!(
                "ComputerUse observation and input require a vision-capable model"
            ));
        }
        let mut region = None;
        let mut resolution = None;
        let native_call = if call.name == "get_window_state" {
            let args: ObserveArgs = parse(&call.arguments)?;
            region = args.region;
            resolution = args.resolution;
            if let Some(resolution) = resolution {
                resolution.validate()?;
            }
            if let Some(reference) = args.state_id {
                let (observation, image) = crate::gui_image::reframe(
                    &self.system,
                    &self.actor_id,
                    "computer",
                    &args.window_id,
                    &reference,
                    region,
                    resolution,
                )?;
                let payload = json!({"status":"observed","observation":observation});
                let mut result = AppToolExecutionResult::new(
                    "Saved window image",
                    payload.clone(),
                    Some(payload.to_string()),
                )
                .with_output_schema::<crate::app::output::GuiOutput>();
                result.model_images.push(image);
                return Ok(result);
            }
            AgentToolCall {
                arguments: json!({"window_id":args.window_id,"region":null,"scale":1}),
                ..call.clone()
            }
        } else {
            crate::gui_image::map_call(
                &self.system,
                &self.actor_id,
                "computer",
                "window_id",
                call,
                true,
            )?
            .0
        };
        let call = &native_call;
        #[cfg(target_os = "android")]
        {
            let reply = self.android.call(&self.actor_id, call, context).await?;
            self.publish_reply(
                reply.summary,
                reply.payload,
                reply.failure,
                reply.image,
                region,
                resolution,
            )
        }
        #[cfg(target_os = "macos")]
        {
            let reply = self
                .macos
                .call(&self.actor_id, &self.system, call, context)
                .await?;
            self.publish_reply(
                reply.summary,
                reply.payload,
                reply.failure,
                reply.image,
                region,
                resolution,
            )
        }
        #[cfg(not(any(target_os = "android", target_os = "macos")))]
        {
            let _ = (&self.system, call, region, resolution);
            Err(miette!("ComputerUse requires a supported native backend"))
        }
    }
    async fn shutdown(&mut self) -> Result<()> {
        #[cfg(target_os = "android")]
        self.android.shutdown(&self.actor_id).await?;
        #[cfg(target_os = "macos")]
        self.macos.shutdown(&self.actor_id, &self.system).await?;
        Ok(())
    }
}

impl ComputerUseApp {
    #[cfg(any(target_os = "android", target_os = "macos"))]
    fn publish_reply(
        &self,
        summary: String,
        payload: Value,
        failure: Option<String>,
        image: Option<Vec<u8>>,
        region: Option<crate::image_input::ImageRegion>,
        resolution: Option<crate::image_input::ImageResolution>,
    ) -> Result<AppToolExecutionResult> {
        let mut result = AppToolExecutionResult::new(summary, payload, None)
            .with_output_schema::<crate::app::output::GuiOutput>();
        result.failure = failure;
        if let Some(bytes) = image {
            match crate::gui_image::publish(
                &self.system,
                &self.actor_id,
                "computer",
                result.payload["observation"].clone(),
                &bytes,
                region,
                resolution,
            ) {
                Ok((observation, image)) => {
                    result.payload["observation"] = observation;
                    result.model_images.push(image);
                    result.model_content = Some(result.payload.to_string());
                }
                Err(_) => {
                    result.payload["status"] = json!("outcome_unknown");
                    result.payload["observation_error"] = json!(
                        "Screenshot preparation failed; observe fresh before continuing, do not replay the operation"
                    );
                    result.failure = Some(
                        "ComputerUse image unavailable after native operation; do not replay"
                            .into(),
                    );
                }
            }
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

#[cfg(test)]
mod tests;
