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
        "list_windows"=>"List a bounded page of actual macOS windows with opaque IDs and acquisition status. Listing does not move or acquire a window. Windows owned by another Actor and Daat Locus are excluded.",
        "launch_app"=>"Launch a discovered app without activation, acquire only a uniquely confirmed new window, move it onto the enabled virtual display and return its screenshot. If the app already runs, discover its windows instead. Startup placement is confirmed after launch; never replay an uncertain launch or take an existing user window as its result.",
        "open_window"=>"Acquire this explicitly discovered window, move and fit it onto the device's shared virtual display, and return its screenshot without activating the app or moving the pointer. Never select a user's working window merely because its app name matches. Acquire child dialogs separately.",
        "get_window_state"=>"Observe the acquired window on the virtual display, including when other windows overlap it. Null region/scale returns the full image; region uses full-image pixels and scale 1..4 magnifies it. Input coordinates refer to the returned image; crop and scale conversion is automatic. Minimized windows require restore. After outcome_unknown, cancellation, timeout or capture failure, observe before deciding whether another input is needed.",
        "adjust_window"=>"Resize, move, fit or restore an acquired window on the virtual display without activation. Resize requires width/height; move requires x/y relative to that display's origin, all in logical points. Fit uses the available display area within the image budget; restore unminimizes and relocates if needed. Other parameters must be null. Null state_id permits recovery after capture failure. Returns a fresh image/state and clears earlier coordinates and the selected editable receiver.",
        "click"=>"Click a supported control at an image-relative point without moving the user's pointer. A text control selects the editable receiver. Read supported_actions; unsupported surfaces fail without foreground fallback.",
        "type_text"=>"Insert literal Unicode into the explicitly selected native editable receiver and observe the actual value. Unsupported controls fail without using the shared clipboard or foreground keyboard.",
        "press_key"=>"Apply a supported key or editing chord to the selected receiver. Read supported_actions. No global input or foreground fallback.",
        "scroll"=>"Scroll at an image-relative point using an available background-safe native route. Unsupported surfaces fail before input.",
        "drag"=>"Drag along an image-relative path using a background-safe route. Unsupported surfaces fail before input.",
        "give_window"=>"Hand this acquired window back to the user, revoke input and old references, and place it on a user display using original, center, left_half or right_half placement. Other acquired windows and dialogs need separate handoffs. An unconfirmed return keeps input revoked; finish cleanup with release_window.",
        "release_window"=>"Return this acquired window to its original placement on a user display and revoke its old references. Does not close the app or release separately acquired dialogs. Also finishes an unconfirmed handoff. Discover and explicitly open a fresh reference to reacquire it.",
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
    pub(super) fn transport(
        system: &ActorSystem,
    ) -> Result<daat_locus_client_protocol::computer_use::Client> {
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

pub(super) fn preview_catalog(system: &ActorSystem) -> Result<super::preview::Catalog> {
    let client = Client::transport(system)?;
    if client.is_disconnected() {
        return Ok(super::preview::Catalog {
            supported: true,
            state: super::preview::State::Unavailable,
            displays: vec![],
        });
    }
    let reply = preview_request(client, "preview_catalog", json!({}))?;
    serde_json::from_value(reply.payload).map_err(|_| miette!("Invalid virtual display catalog"))
}
pub(super) fn preview_frame(system: &ActorSystem, id: &str) -> Result<Option<Vec<u8>>> {
    let reply = preview_request(
        Client::transport(system)?,
        "preview_frame",
        json!({"id":id}),
    )?;
    reply
        .image
        .map(|image| {
            base64::engine::general_purpose::STANDARD
                .decode(image)
                .map_err(|_| miette!("Invalid virtual display image"))
        })
        .transpose()
}
fn preview_request(
    client: daat_locus_client_protocol::computer_use::Client,
    operation: &str,
    arguments: Value,
) -> Result<daat_locus_client_protocol::computer_use::Reply> {
    let reply = client
        .call(&daat_locus_client_protocol::computer_use::Request {
            id: uuid::Uuid::new_v4().to_string(),
            scope: "owner-preview".into(),
            session: String::new(),
            epoch: 0,
            operation: operation.into(),
            arguments,
        })
        .map_err(|error| miette!("{error}"))?;
    if let Some(error) = &reply.failure {
        return Err(miette!("{error}"));
    }
    Ok(reply)
}
