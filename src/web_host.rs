//! Device-local native host mailbox. The daemon never owns a GUI event loop.
use daat_locus_client_protocol::web::{
    BrowserCommand, BrowserReply, Capture, Operation, Reply, Request,
};
use miette::{Result, miette};
use parking_lot::Mutex;
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

#[derive(Default)]
pub struct Host {
    connection: Mutex<Option<(Uuid, mpsc::Sender<Pending>)>>,
}
pub struct Pending {
    pub request: Request,
    pub reply: oneshot::Sender<Result<Reply>>,
}
pub struct Attachment {
    host: Arc<Host>,
    id: Uuid,
    pub requests: mpsc::Receiver<Pending>,
}
impl Drop for Attachment {
    fn drop(&mut self) {
        let mut connection = self.host.connection.lock();
        if connection.as_ref().is_some_and(|(id, _)| *id == self.id) {
            connection.take();
        }
    }
}
impl Host {
    pub fn attach(self: &Arc<Self>) -> Result<Attachment> {
        let mut connection = self.connection.lock();
        if connection
            .as_ref()
            .is_some_and(|(_, sender)| !sender.is_closed())
        {
            return Err(miette!("A native Web host is already connected"));
        }
        let (sender, requests) = mpsc::channel(16);
        let id = Uuid::new_v4();
        *connection = Some((id, sender));
        Ok(Attachment {
            host: self.clone(),
            id,
            requests,
        })
    }
    pub fn available(&self) -> bool {
        self.connection
            .lock()
            .as_ref()
            .is_some_and(|(_, sender)| !sender.is_closed())
    }
    pub async fn request(&self, profile: String, operation: Operation) -> Result<Option<Capture>> {
        Ok(self.dispatch(profile, operation).await?.capture)
    }
    pub async fn browser(&self, profile: String, command: BrowserCommand) -> Result<BrowserReply> {
        self.dispatch(profile, Operation::Browser { command })
            .await?
            .browser
            .ok_or_else(|| miette!("Native browser returned no observation"))
    }
    async fn dispatch(&self, profile: String, operation: Operation) -> Result<Reply> {
        let sender = self
            .connection
            .lock()
            .as_ref()
            .map(|(_, sender)| sender.clone())
            .ok_or_else(|| {
                miette!(
                    "Native Web host unavailable; open the native client or use the HTTP source"
                )
            })?;
        let request = Request {
            id: Uuid::new_v4().simple().to_string(),
            profile,
            operation,
        };
        if !request.validate() {
            return Err(miette!("Invalid native Web request"));
        }
        let (reply, response) = oneshot::channel();
        sender
            .try_send(Pending { request, reply })
            .map_err(|_| miette!("Native Web host is busy or disconnected"))?;
        tokio::time::timeout(Duration::from_secs(35), response).await
            .map_err(|_| miette!("Native Web request timed out; its navigation outcome is unknown, do not replay automatically"))?
            .map_err(|_| miette!("Native Web host disconnected; do not replay navigation automatically"))?
    }
}
fn validate_browser_page_url(url: &str) -> Result<()> {
    if url == "about:blank" {
        return Ok(());
    }
    crate::web_app::validate_url(url)
}
pub fn validate_reply(request: &Request, reply: &Reply) -> Result<()> {
    if reply.id != request.id
        || reply.capture.as_ref().is_some_and(|capture| {
            capture.html.len() > daat_locus_client_protocol::web::MAX_CAPTURE_BYTES
        })
    {
        return Err(miette!("Invalid native Web reply"));
    }
    if reply.error.is_some() {
        if reply.capture.is_some() || reply.browser.is_some() {
            return Err(miette!(
                "Native Web reply contains both an error and a capture"
            ));
        }
        return Ok(());
    }
    if let Operation::Browser { command } = &request.operation {
        if reply.capture.is_some() {
            return Err(miette!("Browser reply contained a document result"));
        }
        let browser = reply
            .browser
            .as_ref()
            .ok_or_else(|| miette!("Native browser result missing"))?;
        for page in &browser.pages {
            validate_browser_page_url(&page.url)?;
        }
        match command {
            BrowserCommand::Open { tab, .. }
            | BrowserCommand::Observe { tab }
            | BrowserCommand::Input { tab, .. } => {
                let observation = browser
                    .observation
                    .as_ref()
                    .ok_or_else(|| miette!("Native browser screenshot missing"))?;
                validate_browser_page_url(&observation.page.url)?;
                if &observation.page.tab != tab
                    || observation.width == 0
                    || observation.height == 0
                    || u64::from(observation.width) * u64::from(observation.height) > 8_000_000
                    || observation.png_base64.len() > 10 * 1024 * 1024
                    || observation.state_id != request.id
                    || observation.state_id.len() != 32
                    || !observation.state_id.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(miette!("Invalid native browser observation"));
                }
            }
            _ => {
                if browser.observation.is_some() {
                    return Err(miette!("Unexpected browser screenshot"));
                }
            }
        }
        return Ok(());
    }
    if reply.browser.is_some() {
        return Err(miette!("Unexpected browser result"));
    }
    match (&request.operation, &reply.capture) {
        (Operation::Load { .. }, Some(capture)) => crate::web_app::validate_url(&capture.url),
        (Operation::Close, None) => Ok(()),
        _ => Err(miette!(
            "Native Web reply did not contain the requested result"
        )),
    }
}
pub fn resolve(request: &Request, reply: Reply) -> Result<Reply> {
    validate_reply(request, &reply)?;
    if let Some(error) = &reply.error {
        return Err(miette!("Native Web: {error}"));
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_catalog_accepts_new_tabs_but_rejects_privileged_urls() {
        let request = Request {
            id: "c".repeat(32),
            profile: daat_locus_client_protocol::web::SHARED_BROWSER_PROFILE.into(),
            operation: Operation::Browser {
                command: BrowserCommand::List,
            },
        };
        for (url, valid) in [
            ("about:blank", true),
            ("https://example.com/", true),
            ("file:///secret", false),
            ("tauri://localhost/", false),
            ("about:config", false),
        ] {
            let reply = Reply {
                id: request.id.clone(),
                capture: None,
                error: None,
                browser: Some(BrowserReply {
                    pages: vec![daat_locus_client_protocol::web::BrowserPage {
                        tab: "a".repeat(32),
                        title: String::new(),
                        url: url.into(),
                    }],
                    observation: None,
                }),
            };
            assert_eq!(validate_reply(&request, &reply).is_ok(), valid, "{url}");
        }
    }

    #[tokio::test]
    async fn attachment_is_exclusive_and_dropped_calls_are_not_replayed_after_reconnect() {
        let host = Arc::new(Host::default());
        let mut attached = host.attach().unwrap();
        assert!(host.attach().is_err());
        let caller = host.clone();
        let task =
            tokio::spawn(async move { caller.request("a".repeat(64), Operation::Close).await });
        let pending = attached.requests.recv().await.unwrap();
        task.abort();
        let _ = task.await;
        assert!(pending.reply.is_closed());
        drop(pending);
        drop(attached);
        assert!(
            host.request("a".repeat(64), Operation::Close)
                .await
                .is_err()
        );
        let mut attached = host.attach().unwrap();
        assert!(matches!(
            attached.requests.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let caller = host.clone();
        let task =
            tokio::spawn(async move { caller.request("a".repeat(64), Operation::Close).await });
        let pending = attached.requests.recv().await.unwrap();
        drop(pending);
        assert!(task.await.unwrap().is_err());
    }
}
