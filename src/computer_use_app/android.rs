//! The mobile host supplies a narrow native transport; the core never acquires OS privileges.
use super::*;
use std::sync::{Arc, OnceLock};

pub struct Reply {
    pub summary: String,
    pub payload: Value,
    pub failure: Option<String>,
    pub image: Option<Vec<u8>>,
}

pub trait Backend: Send + Sync {
    fn preview_catalog(&self) -> Result<super::preview::Catalog> {
        Ok(super::preview::Catalog {
            supported: true,
            state: super::preview::State::Unavailable,
            displays: vec![],
        })
    }
    fn preview_frame(&self, _id: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn call(&self, request: &str) -> Result<Reply>;
    fn cancel(&self, request_id: &str);
    fn scope(&self) -> Result<Value>;
    fn has_resources(&self, session: &str) -> bool;
    fn reset(&self) -> Result<()>;
}
static BACKEND: OnceLock<Arc<dyn Backend>> = OnceLock::new();
pub fn install_backend(backend: Arc<dyn Backend>) -> Result<()> {
    BACKEND
        .set(backend)
        .map_err(|_| miette!("PhoneUse backend already installed"))
}
fn backend() -> Result<Arc<dyn Backend>> {
    BACKEND
        .get()
        .cloned()
        .ok_or_else(|| miette!("PhoneUse native backend unavailable"))
}
#[cfg(target_os = "android")]
pub(crate) fn preview_catalog() -> Result<super::preview::Catalog> {
    backend()?.preview_catalog()
}
#[cfg(target_os = "android")]
pub(crate) fn preview_frame(id: &str) -> Result<Option<Vec<u8>>> {
    backend()?.preview_frame(id)
}
#[cfg(target_os = "android")]
pub(crate) fn reset_resources() -> Result<()> {
    if let Some(backend) = BACKEND.get() {
        backend.reset()?;
    }
    Ok(())
}
pub(super) fn specifications() -> Vec<AppToolSpec> {
    let mut tools = ComputerUseApp::windows_specifications();
    tools.retain(|tool| {
        !matches!(
            tool.name.as_str(),
            "adjust_window" | "give_window" | "type_text"
        )
    });
    for tool in &mut tools {
        tool.description = match tool.name.as_str() {
            "list_apps" => "List a bounded page of Android launcher apps. Use only returned app IDs.",
            "list_windows" => "List actual windows in your Android virtual display. Listing does not acquire input ownership. Main-screen windows and other Actors' displays are excluded.",
            "launch_app" => "Launch a discovered Android app as a new task in your isolated virtual display. Returns a fresh screenshot when the new task is confirmed. Apps may reuse tasks; outcome_unknown must not be replayed.",
            "open_window" => "Acquire a returned window in your own virtual display and observe it. Does not move or acquire the user's main-screen tasks.",
            "get_window_state" => "Observe the acquired Android window. Null region/scale returns the full view; crop coordinates are window pixels, scale is 1..4. All input coordinates refer to the returned image. After outcome_unknown, cancellation or disconnect, observe before deciding whether another input is needed.",
            "click" => "Tap once with the left button at an image-relative point in the acquired Android window, then observe. Selects the editable receiver at that point when available.",
            "press_key" => "Send a supported Android key to the acquired virtual display. Read supported_actions; no main-display input fallback. Returns a fresh observation.",
            "scroll" => "Scroll vertically at an image-relative point in the acquired window. Positive Y scrolls content down. Returns a fresh observation.",
            "drag" => "Perform one continuous touch gesture along an image-relative path, then observe. Two points define a straight gesture.",
            "release_window" => "Revoke this acquired window's input and observation references. Its task and virtual display remain available; list and explicitly open a fresh reference to reacquire it. Does not close the app.",
            "close_window" => "Remove the acquired Android task from your virtual display. May discard unsaved state. Does not issue force-stop; Android may reclaim the cached process after its last task closes.",
            _ => unreachable!(),
        }.into();
    }
    tools.push(AppToolSpec {
        name: "set_text".into(),
        description: "Replace the complete text of the editable control selected by the last tap, using Android accessibility ACTION_SET_TEXT. Supports Unicode without moving the main-screen IME. This replaces rather than appends text; unsupported controls fail before dispatch. Observe the result.".into(),
        input_schema: model_schema_for::<TextArgs>(),
    });
    tools
}

pub(super) struct Client {
    session: String,
}
impl Client {
    pub fn new() -> Self {
        Self {
            session: uuid::Uuid::new_v4().to_string(),
        }
    }
    #[cfg(target_os = "android")]
    pub fn can_passivate(&self) -> bool {
        backend().is_ok_and(|backend| !backend.has_resources(&self.session))
    }
    #[cfg(target_os = "android")]
    #[cfg(target_os = "android")]
    pub async fn call(
        &self,
        actor: &str,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<Reply> {
        self.request(actor, context.turn_epoch, &call.name, &call.arguments)
            .await
    }
    async fn request(&self, actor: &str, epoch: u64, op: &str, args: &Value) -> Result<Reply> {
        let backend = backend()?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let request = json!({"id":request_id,"actor":actor,"session":self.session,"epoch":epoch.to_string(),"op":op,"args":args}).to_string();
        struct Cancel {
            backend: Arc<dyn Backend>,
            id: String,
            armed: bool,
        }
        impl Drop for Cancel {
            fn drop(&mut self) {
                if self.armed {
                    self.backend.cancel(&self.id);
                }
            }
        }
        let mut cancel = Cancel {
            backend: backend.clone(),
            id: request_id,
            armed: true,
        };
        let reply = tokio::task::spawn_blocking(move || backend.call(&request))
            .await
            .map_err(|e| miette!("PhoneUse outcome unknown: {e}; do not replay"))?;
        cancel.armed = false;
        reply
    }
    #[cfg(target_os = "android")]
    pub async fn shutdown(&self, actor: &str) -> Result<()> {
        self.request(actor, 0, "detach", &json!({})).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    struct Transport {
        started: tokio::sync::Notify,
        finished: std::sync::Condvar,
        cancelled: Mutex<Option<String>>,
        blocking: AtomicBool,
    }
    impl Backend for Transport {
        fn call(&self, request: &str) -> Result<Reply> {
            let request: Value = serde_json::from_str(request).unwrap();
            if self.blocking.load(Ordering::Acquire) {
                self.started.notify_one();
                let cancelled = self.cancelled.lock().unwrap();
                let (cancelled, wait) = self
                    .finished
                    .wait_timeout_while(cancelled, std::time::Duration::from_secs(5), |id| {
                        id.is_none()
                    })
                    .unwrap();
                assert!(
                    !wait.timed_out(),
                    "Cancellation did not reach the in-flight transport"
                );
                assert_eq!(cancelled.as_deref(), request["id"].as_str());
            }
            Ok(Reply {
                summary: String::new(),
                payload: request,
                failure: None,
                image: None,
            })
        }
        fn cancel(&self, id: &str) {
            *self.cancelled.lock().unwrap() = Some(id.into());
            self.finished.notify_all();
        }
        fn scope(&self) -> Result<Value> {
            Ok(json!({"method":"shizuku","uid":2000}))
        }
        fn has_resources(&self, _: &str) -> bool {
            false
        }
        fn reset(&self) -> Result<()> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn cancelled_future_fences_its_native_request_but_success_does_not_cancel() {
        let transport = Arc::new(Transport {
            started: tokio::sync::Notify::new(),
            finished: std::sync::Condvar::new(),
            cancelled: Mutex::new(None),
            blocking: AtomicBool::new(false),
        });
        install_backend(transport.clone()).unwrap();
        let client = Client::new();
        let reply = client
            .request("actor-a", 42, "list_windows", &json!({"limit":10}))
            .await
            .unwrap();
        assert_eq!(reply.payload["actor"], "actor-a");
        assert_eq!(reply.payload["epoch"], "42");
        assert_eq!(reply.payload["args"]["limit"], 10);
        assert!(transport.cancelled.lock().unwrap().is_none());
        transport.blocking.store(true, Ordering::Release);
        let pending =
            tokio::spawn(async move { client.request("actor-a", 43, "click", &json!({})).await });
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            transport.started.notified(),
        )
        .await
        .unwrap();
        pending.abort();
        assert!(pending.await.is_err_and(|error| error.is_cancelled()));
        assert!(transport.cancelled.lock().unwrap().is_some());
    }
}
