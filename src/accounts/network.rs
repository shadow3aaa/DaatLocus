//! Password-authenticated device login. Discovery supplies candidates, never trust.
use super::{CredentialBundle, login_error::LoginError, normalize_name};
use crate::stage::{
    Result, StageError, StageId,
    identity::{DeviceProfile, Enrollment, JoinRequest, verify_join_request},
    runtime::Runtime,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::{sync::Notify, task::JoinHandle};
use zeroize::Zeroizing;
const MAX_FRAME: usize = 48 * 1024;
pub fn lookup(name: &str) -> String {
    hex::encode(Sha256::digest(format!("DaatLocus.AccountLookup.v1/{name}")))
}
#[derive(Clone)]
pub struct Candidate {
    pub profile: DeviceProfile,
    pub account_id: String,
    pub name_hash: String,
    pub label: String,
    pub relay: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceLogin {
    pub name: String,
    pub account_id: String,
    pub request: String,
    pub device: JoinRequest,
    pub origin: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub version: u16,
    pub account_id: String,
    pub credential_binding: String,
    pub source: StageId,
    pub target: StageId,
    pub device_binding: String,
    pub transport_binding: String,
    pub nonce: uuid::Uuid,
    pub origin: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Packet {
    Start { login: DeviceLogin },
    Challenge { response: String, context: String },
    Finish { message: String },
    Accepted { payload: String },
    Rejected,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountImport {
    pub source: Enrollment,
    pub enrollment: Enrollment,
    pub credentials: CredentialBundle,
    pub label: String,
}
fn decode(value: &str) -> Result<Vec<u8>> {
    if value.len() > MAX_FRAME {
        return Err(StageError::Capacity("Account packet too large"));
    }
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| StageError::Invalid("Invalid account encoding"))
}
pub struct Attempt {
    opaque: daat_locus_auth::PendingLogin,
    credentials: CredentialBundle,
    device: JoinRequest,
}
impl Attempt {
    pub fn start(runtime: &Runtime, login: DeviceLogin, binding: String) -> Result<(Self, Packet)> {
        let credentials = runtime
            .host
            .accounts
            .current()
            .map_err(|_| StageError::Unavailable("Account unavailable"))?
            .ok_or(StageError::Forbidden("Account unavailable"))?;
        let own = runtime.host.enrollment()?;
        if login.account_id != credentials.account_id || own.root.group_id != credentials.account_id
        {
            return Err(StageError::Forbidden("Account mismatch"));
        }
        verify_join_request(&login.device, chrono::Utc::now().timestamp_millis())?;
        if login.device.device.stage_id == runtime.host.local || login.origin.len() > 256 {
            return Err(StageError::Forbidden("Invalid login source"));
        }
        let name =
            normalize_name(&login.name).map_err(|_| StageError::Invalid("Invalid account name"))?;
        let context = Context {
            version: 1,
            account_id: credentials.account_id.clone(),
            credential_binding: credentials.binding(),
            source: runtime.host.local,
            target: login.device.device.stage_id,
            device_binding: crate::stage::digest(
                &serde_json::to_vec(&login.device)
                    .map_err(|_| StageError::Invalid("Invalid device request"))?,
            ),
            transport_binding: binding,
            nonce: uuid::Uuid::new_v4(),
            origin: login.origin,
        };
        let context = serde_json::to_vec(&context)
            .map_err(|_| StageError::Invalid("Invalid login context"))?;
        let opaque = daat_locus_auth::PendingLogin::start(
            &credentials
                .setup()
                .map_err(|_| StageError::Invalid("Invalid account setup"))?,
            if name == credentials.name {
                Some(
                    credentials
                        .password_file()
                        .map_err(|_| StageError::Invalid("Invalid account record"))?,
                )
            } else {
                None
            },
            &credentials.identifier(),
            &decode(&login.request)?,
            context.clone(),
        )
        .map_err(|_| StageError::Forbidden("Account login failed"))?;
        let challenge = Packet::Challenge {
            response: URL_SAFE_NO_PAD.encode(opaque.response()),
            context: URL_SAFE_NO_PAD.encode(context),
        };
        Ok((
            Self {
                opaque,
                credentials,
                device: login.device,
            },
            challenge,
        ))
    }
    pub fn finish(self, runtime: &Runtime, message: &str) -> Result<Packet> {
        if !runtime
            .host
            .accounts
            .authorizes(&self.credentials.account_id, &self.credentials.binding())
        {
            return Err(StageError::Forbidden("Account changed"));
        }
        let channel = self
            .opaque
            .authenticate(&decode(message)?)
            .map_err(|_| StageError::Forbidden("Account login failed"))?;
        let enrollment = runtime.host.enroll_authenticated_device(&self.device)?;
        let source = runtime.host.enrollment()?;
        let payload = AccountImport {
            source,
            enrollment,
            credentials: self.credentials,
            label: runtime.host.settings().label,
        };
        let bytes = Zeroizing::new(
            serde_json::to_vec(&payload)
                .map_err(|_| StageError::Invalid("Invalid private account record"))?,
        );
        let encrypted = channel
            .seal(&bytes)
            .map_err(|_| StageError::Invalid("Cannot seal account record"))?;
        runtime.want(self.device.device.stage_id);
        runtime.relay_changed();
        Ok(Packet::Accepted {
            payload: URL_SAFE_NO_PAD.encode(encrypted),
        })
    }
}
pub struct RemoteLogin {
    pub response: String,
    pub context: Vec<u8>,
    pub candidate: Candidate,
    pub device: JoinRequest,
    transport: Box<super::relay::Client>,
}
impl RemoteLogin {
    pub async fn connect(
        runtime: &Runtime,
        candidate: Candidate,
        name: String,
        request: String,
        origin: String,
    ) -> Result<Self> {
        let device = runtime
            .host
            .identity()?
            .join_request(chrono::Utc::now().timestamp_millis())?;
        let endpoint = &candidate.relay;
        let identity = runtime.host.identity()?;
        let (transport, response, context, binding) = super::relay::Client::login(
            endpoint,
            &identity,
            &candidate,
            name,
            request,
            &device,
            origin.clone(),
        )
        .await?;
        Self::validate_context(runtime, &candidate, &device, &origin, &binding, &context)?;
        Ok(Self {
            response,
            context,
            candidate,
            device,
            transport: Box::new(transport),
        })
    }
    fn validate_context(
        runtime: &Runtime,
        candidate: &Candidate,
        device: &JoinRequest,
        origin: &str,
        binding: &str,
        context: &[u8],
    ) -> Result<()> {
        let decoded: Context = serde_json::from_slice(context)
            .map_err(|_| StageError::Invalid("Invalid account context"))?;
        if decoded.version != 1
            || decoded.source != candidate.profile.stage_id
            || decoded.target != runtime.host.local
            || decoded.account_id != candidate.account_id
            || decoded.transport_binding != binding
            || decoded.origin != origin
            || decoded.device_binding
                != crate::stage::digest(
                    &serde_json::to_vec(&device)
                        .map_err(|_| StageError::Invalid("Invalid device request"))?,
                )
            || decoded.nonce.is_nil()
        {
            return Err(StageError::Forbidden("Account channel binding failed"));
        }
        Ok(())
    }
    pub async fn finish(mut self, message: String) -> Result<String> {
        let reply = self.transport.finish(message).await?;
        let Packet::Accepted { payload } = reply else {
            return Err(StageError::Forbidden("Account login failed"));
        };
        Ok(payload)
    }
}
pub struct Network {
    runtime: Weak<Runtime>,
    publication: RwLock<(Option<String>, super::public_connection::PublicationStatus)>,
    pub(crate) publication_wake: Notify,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}
impl Network {
    pub async fn start(runtime: &Arc<Runtime>) -> Result<Arc<Self>> {
        Self::start_inner(runtime, true).await
    }
    #[cfg(test)]
    pub(crate) async fn start_without_publication(runtime: &Arc<Runtime>) -> Result<Arc<Self>> {
        Self::start_inner(runtime, false).await
    }
    async fn start_inner(runtime: &Arc<Runtime>, publication: bool) -> Result<Arc<Self>> {
        runtime.host.initialize(false)?;
        let network = Arc::new(Self {
            runtime: Arc::downgrade(runtime),
            publication: RwLock::new((
                None,
                super::public_connection::PublicationStatus::initial(false, false),
            )),
            publication_wake: Notify::new(),
            tasks: Mutex::new(vec![]),
        });
        if publication && runtime.host.settings().mode == crate::stage::host::Connectivity::Relay {
            let relay = network.clone();
            network.tasks.lock().push(tokio::spawn(async move {
                super::relay::serve(relay).await;
            }));
        }
        Ok(network)
    }
    pub fn publication_status(&self) -> super::public_connection::PublicationStatus {
        let Some(runtime) = self.runtime() else {
            return super::public_connection::PublicationStatus::initial(false, false);
        };
        let origin = runtime.host.settings().relay_origin;
        let current = self.publication.read();
        if current.0 == origin {
            current.1.clone()
        } else {
            super::public_connection::PublicationStatus::initial(
                origin.is_some(),
                runtime.host.account_id().is_some(),
            )
        }
    }
    pub(crate) fn publish_status(
        &self,
        origin: Option<&str>,
        state: super::public_connection::PublicationState,
        error: Option<super::public_connection::PublicationError>,
        retry: Option<Duration>,
        confirmed: bool,
    ) {
        let now = chrono::Utc::now().timestamp_millis();
        let mut current = self.publication.write();
        if current.0.as_deref() != origin
            || current.1.state != state
            || current.1.error_code != error.map(|e| e.code())
        {
            current.1.changed_at_ms = now;
        }
        if current.0.as_deref() != origin {
            current.1.last_confirmed_at_ms = None;
        }
        current.0 = origin.map(str::to_owned);
        current.1.state = state;
        current.1.error_code = error.map(|e| e.code());
        current.1.retry_at_ms = retry.map(|delay| now.saturating_add(delay.as_millis() as i64));
        if confirmed {
            current.1.last_confirmed_at_ms = Some(now);
        }
    }
    async fn relay_candidates(
        &self,
        name: &str,
        excluded: &[String],
    ) -> std::result::Result<Vec<Candidate>, LoginError> {
        let Some(runtime) = self.runtime.upgrade() else {
            return Err(LoginError::Unavailable);
        };
        let (Some(endpoint), Ok(identity)) = (
            runtime.host.settings().relay_origin,
            runtime.host.identity(),
        ) else {
            return Err(LoginError::ConnectionRequired);
        };
        match tokio::time::timeout(
            Duration::from_secs(40),
            super::relay::discover(&endpoint, &identity, name),
        )
        .await
        {
            Ok(Ok(candidates)) => Ok(candidates
                .into_iter()
                .filter(|candidate| !excluded.contains(&candidate.profile.stage_id.to_string()))
                .collect()),
            Ok(Err(error)) => {
                tracing::debug!(reason = %error, "Account relay discovery failed");
                Err(match error {
                    StageError::Invalid(_) | StageError::Forbidden(_) => LoginError::Protocol,
                    StageError::Capacity(_) => LoginError::Busy,
                    _ => LoginError::RelayUnavailable,
                })
            }
            Err(_) => {
                tracing::debug!("Account relay discovery timed out");
                Err(LoginError::TimedOut)
            }
        }
    }

    pub async fn login(
        &self,
        name: &str,
        request: &str,
        origin: &str,
        excluded: &[String],
    ) -> std::result::Result<RemoteLogin, LoginError> {
        let runtime = self.runtime.upgrade().ok_or(LoginError::Unavailable)?;
        if runtime.host.settings().mode != crate::stage::host::Connectivity::Relay {
            return Err(LoginError::ConnectionRequired);
        }
        let candidates = self.relay_candidates(name, excluded).await?;
        use futures_util::StreamExt;
        let mut attempts = futures_util::stream::iter(
            candidates
                .into_iter()
                .filter(|candidate| {
                    runtime
                        .host
                        .account_id()
                        .is_none_or(|account| account == candidate.account_id)
                })
                .take(8),
        )
        .map(|candidate| async {
            tokio::time::timeout(
                Duration::from_secs(60),
                RemoteLogin::connect(
                    &runtime,
                    candidate,
                    name.to_owned(),
                    request.to_owned(),
                    origin.to_owned(),
                ),
            )
            .await
        })
        .buffer_unordered(4);
        let mut failure = None;
        while let Some(result) = attempts.next().await {
            match result {
                Ok(Ok(login)) => return Ok(login),
                Ok(Err(error)) => {
                    tracing::debug!(reason=%error,"Account relay login failed");
                    failure = Some(LoginError::connection(&error));
                }
                Err(_) => failure = Some(LoginError::TimedOut),
            }
        }
        Err(failure.unwrap_or(LoginError::NoOnlineDevice))
    }
    pub async fn stop(&self) {
        let tasks = std::mem::take(&mut *self.tasks.lock());
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
    }
    pub(crate) fn runtime(&self) -> Option<Arc<Runtime>> {
        self.runtime.upgrade()
    }
}

#[cfg(test)]
mod login_diagnostic_tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test]
    async fn empty_relay_discovery_is_distinct_from_a_broken_relay() {
        let temp = tempfile::tempdir().unwrap();
        let system = crate::actors::ActorSystem::open_account_gated(
            temp.path().join("actors"),
            temp.path().join("workspace"),
            Default::default(),
        )
        .await
        .unwrap();
        let runtime = Runtime::new(crate::stage::host::Host::load(system.clone()).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut settings = runtime.host.settings();

        settings.mode = crate::stage::host::Connectivity::Relay;
        settings.relay_origin = Some(format!("ws://{}", listener.local_addr().unwrap()));
        runtime.host.save_settings(settings).unwrap();
        let relay = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let challenge = serde_json::json!({"type":"challenge","nonce":uuid::Uuid::new_v4(),"expires_at_ms":chrono::Utc::now().timestamp_millis()+30_000});
            socket
                .send(Message::Text(challenge.to_string().into()))
                .await
                .unwrap();
            let registration = socket.next().await.unwrap().unwrap();
            assert!(registration.to_text().unwrap().contains("register"));
            socket
                .send(Message::Text(r#"{"type":"registered","peers":[]}"#.into()))
                .await
                .unwrap();
            let _ = socket.next().await;
        });
        let network = Network::start(&runtime).await.unwrap();
        assert!(matches!(
            network
                .login("private-account", "", "http://127.0.0.1", &[])
                .await,
            Err(LoginError::NoOnlineDevice)
        ));
        relay.await.unwrap();
        assert!(matches!(
            network.relay_candidates("private-account", &[]).await,
            Err(LoginError::RelayUnavailable)
        ));
        network.stop().await;
        system.shutdown().await;
    }
}
