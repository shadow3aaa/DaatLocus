use super::*;
use base64::Engine;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(super) fn specifications() -> Vec<AppToolSpec> {
    let mut tools = ComputerUseApp::windows_specifications();
    for tool in &mut tools {
        tool.description=match tool.name.as_str(){
        "list_apps"=>"List a bounded page of macOS apps. Use only returned app IDs.",
        "list_windows"=>"List actual macOS windows. Does not acquire input ownership. Windows owned by another Actor and Daat Locus are excluded.",
        "launch_app"=>"Launch a discovered app in the background and acquire only a uniquely confirmed new window. Never replay an uncertain launch.",
        "open_window"=>"Acquire an explicitly discovered window for background operation and return its screenshot. Does not raise, activate or move the window.",
        "get_window_state"=>"Observe the acquired window, including when occluded. Null region/scale returns the full image. Crop uses full-image pixels; input coordinates use the returned image. Minimized windows are unavailable.",
        "adjust_window"=>"Adjust an acquired window without activation. Width/height and desktop X/Y use logical points. Null state_id permits recovery after capture failure. Revoke earlier image coordinates.",
        "click"=>"Click a supported control at an image-relative point without moving the user's pointer. A text control selects the editable receiver. Read supported_actions; unsupported surfaces fail without foreground fallback.",
        "type_text"=>"Insert literal Unicode into the explicitly selected native editable receiver and observe the actual value. Unsupported controls fail without using the shared clipboard or foreground keyboard.",
        "press_key"=>"Apply a supported key or editing chord to the selected receiver. Read supported_actions. No global input or foreground fallback.",
        "scroll"=>"Scroll at an image-relative point using an available background-safe native route. Unsupported surfaces fail before input.",
        "drag"=>"Drag along an image-relative path using a background-safe route. Unsupported surfaces fail before input.",
        "give_window"=>"Return ownership to the user and revoke this window's references. Use original placement; the native window remains on its current desktop.",
        "release_window"=>"Release this acquired window and revoke its old references, preserving the native window. Discover and explicitly open a fresh reference to reacquire it.",
        "close_window"=>"Invoke the acquired window's actual close control without terminating its app. Unsaved content may produce a dialog; observe before continuing.",
        _=>unreachable!(),
    }.into();
    }
    tools
}
pub(super) struct Reply {
    pub summary: String,
    pub payload: Value,
    pub failure: Option<String>,
    pub image: Option<Vec<u8>>,
}
pub(super) struct Client {
    session: String,
    live: Arc<AtomicBool>,
}
impl Client {
    pub fn new() -> Self {
        Self {
            session: uuid::Uuid::new_v4().to_string(),
            live: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn can_passivate(&self) -> bool {
        !self.live.load(Ordering::Acquire)
    }
    pub fn review(&self, system: &ActorSystem) -> Result<Option<AppReview>> {
        Ok(Some(AppReview::new(
            "computer_use/macos/v1",
            "Authorize background operation of macOS applications on this device, including window screenshots, launch, native input and window closure. Target apps retain this user's filesystem access. This capability does not change other Apps' sandboxes; read-only remote authority and Daat Locus exclusion remain enforced.",
            json!({"device":system.store().public_stage_id().map_err(|e|miette!("{e}"))?.to_string(),"desktop":"macos-user-session"}),
        )))
    }
    fn transport(system: &ActorSystem) -> Result<daat_locus_client_protocol::computer_use::Client> {
        let root = system.infrastructure_dir();
        let root = root
            .parent()
            .ok_or_else(|| miette!("Profile unavailable"))?;
        let service = std::env::current_exe().map_err(|_| miette!("Native service unavailable"))?;
        let peer = service
            .parent()
            .ok_or_else(|| miette!("Native client unavailable"))?
            .join("daat-locus-app");
        Ok(daat_locus_client_protocol::computer_use::Client::new(
            root, &peer,
        ))
    }
    async fn request(
        &self,
        actor: &str,
        system: &ActorSystem,
        operation: &str,
        arguments: Value,
        epoch: u64,
    ) -> Result<Reply> {
        let transport = Self::transport(system)?;
        let request = daat_locus_client_protocol::computer_use::Request {
            id: uuid::Uuid::new_v4().to_string(),
            scope: actor.into(),
            session: self.session.clone(),
            epoch,
            operation: operation.into(),
            arguments,
        };
        struct Cancel {
            transport: daat_locus_client_protocol::computer_use::Client,
            id: String,
            armed: bool,
        }
        impl Drop for Cancel {
            fn drop(&mut self) {
                if self.armed {
                    let transport = self.transport.clone();
                    let id = self.id.clone();
                    std::thread::spawn(move || transport.cancel(&id));
                }
            }
        }
        let mut guard = Cancel {
            transport: transport.clone(),
            id: request.id.clone(),
            armed: true,
        };
        if matches!(operation, "open_window" | "launch_app") {
            self.live.store(true, Ordering::Release);
        }
        let reply = tokio::task::spawn_blocking(move || transport.call(&request))
            .await
            .map_err(|_| miette!("ComputerUse outcome unknown; do not replay"))?
            .map_err(|e| miette!("{e}"))?;
        guard.armed = false;
        self.live.store(reply.has_resources, Ordering::Release);
        let image = reply
            .image
            .map(|value| {
                base64::engine::general_purpose::STANDARD
                    .decode(value)
                    .map_err(|_| miette!("Invalid native image; observe before continuing"))
            })
            .transpose()?;
        Ok(Reply {
            summary: reply.summary,
            payload: reply.payload,
            failure: reply.failure,
            image,
        })
    }
    pub async fn call(
        &self,
        actor: &str,
        system: &ActorSystem,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<Reply> {
        fn validate<T: serde::de::DeserializeOwned>(args: &Value) -> Result<()> {
            serde_json::from_value::<T>(args.clone())
                .map(|_| ())
                .map_err(|error| miette!("Invalid ComputerUse arguments: {error}"))
        }
        match call.name.as_str() {
            "list_apps" | "list_windows" => validate::<ListArgs>(&call.arguments)?,
            "launch_app" => validate::<AppArgs>(&call.arguments)?,
            "open_window" | "release_window" => validate::<WindowArgs>(&call.arguments)?,
            "get_window_state" => validate::<ObserveArgs>(&call.arguments)?,
            "adjust_window" => validate::<AdjustArgs>(&call.arguments)?,
            "click" => validate::<ClickArgs>(&call.arguments)?,
            "type_text" => validate::<TextArgs>(&call.arguments)?,
            "press_key" => validate::<KeyArgs>(&call.arguments)?,
            "scroll" => validate::<ScrollArgs>(&call.arguments)?,
            "drag" => validate::<DragArgs>(&call.arguments)?,
            "give_window" => validate::<GiveArgs>(&call.arguments)?,
            "close_window" => validate::<StateArgs>(&call.arguments)?,
            _ => return Err(miette!("Unknown ComputerUse operation")),
        }
        self.request(
            actor,
            system,
            &call.name,
            call.arguments.clone(),
            context.turn_epoch,
        )
        .await
    }
    pub async fn shutdown(&self, actor: &str, system: &ActorSystem) -> Result<()> {
        if self.can_passivate() {
            return Ok(());
        }
        if Self::transport(system)?.is_disconnected() {
            self.live.store(false, Ordering::Release);
            return Ok(());
        }
        self.request(actor, system, "detach", json!({}), 0).await?;
        Ok(())
    }
}
