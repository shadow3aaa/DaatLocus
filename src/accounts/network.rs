//! Password-authenticated device login. Discovery supplies candidates, never trust.
use super::{CredentialBundle, login_error::LoginError, normalize_name};
use crate::stage::{
    Result, StageError, StageId,
    identity::{DeviceProfile, Enrollment, JoinRequest, verify_join_request},
    runtime::Runtime,
    transport::tls,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use mdns_sd::{ResolvedService, ScopedIp, ServiceDaemon, ServiceEvent, ServiceInfo};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Weak},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::Notify,
    task::JoinHandle,
};
use zeroize::Zeroizing;
pub const SERVICE: &str = "_dl-account._tcp.local.";
const MAX_FRAME: usize = 48 * 1024;
pub fn lookup(name: &str) -> String {
    hex::encode(Sha256::digest(format!("DaatLocus.AccountLookup.v1/{name}")))
}
#[derive(Clone)]
pub struct Candidate {
    pub profile: DeviceProfile,
    pub account_id: String,
    pub name_hash: String,
    pub addresses: Vec<SocketAddr>,
    pub label: String,
    pub relay: Option<String>,
    pub(crate) seen: Instant,
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
    pub stage_port: Option<u16>,
}
pub async fn write<S: AsyncWrite + Unpin>(stream: &mut S, value: &Packet) -> Result<()> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| StageError::Invalid("Invalid account packet"))?;
    if bytes.len() > MAX_FRAME {
        return Err(StageError::Capacity("Account packet too large"));
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}
pub async fn read<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Packet> {
    let size = stream.read_u32().await? as usize;
    if size > MAX_FRAME {
        return Err(StageError::Capacity("Account packet too large"));
    }
    let mut bytes = Zeroizing::new(vec![0; size]);
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|_| StageError::Invalid("Invalid account packet"))
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
        let stage_port = runtime.address.read().map(|address| address.port());
        let payload = AccountImport {
            source,
            enrollment,
            credentials: self.credentials,
            label: runtime.host.settings().label,
            stage_port,
        };
        let bytes = Zeroizing::new(
            serde_json::to_vec(&payload)
                .map_err(|_| StageError::Invalid("Invalid private account record"))?,
        );
        let encrypted = channel
            .seal(&bytes)
            .map_err(|_| StageError::Invalid("Cannot seal account record"))?;
        runtime.want(self.device.device.stage_id);
        runtime.signal_changed();
        Ok(Packet::Accepted {
            payload: URL_SAFE_NO_PAD.encode(encrypted),
        })
    }
}
enum Transport {
    Tcp(Box<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>),
    Relay(Box<super::relay::Client>),
}
pub struct RemoteLogin {
    pub response: String,
    pub context: Vec<u8>,
    pub candidate: Candidate,
    pub device: JoinRequest,
    transport: Transport,
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
        if let Some(endpoint) = candidate.relay.as_deref() {
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
            return Ok(Self {
                response,
                context,
                candidate,
                device,
                transport: Transport::Relay(Box::new(transport)),
            });
        }
        use futures_util::StreamExt;
        let connector = tls::account_client(&candidate.profile)?;
        let stage = candidate.profile.stage_id;
        let mut unique = std::collections::HashSet::new();
        let mut connections = futures_util::stream::iter(
            candidate
                .addresses
                .iter()
                .copied()
                .filter(|address| unique.insert(*address))
                .take(8),
        )
        .map(|address| {
            let connector = &connector;
            async move {
                tokio::time::timeout(Duration::from_secs(4), async {
                    let socket = tokio::net::TcpStream::connect(address).await?;
                    socket.set_nodelay(true)?;
                    connector
                        .connect(tls::server_name(stage)?, socket)
                        .await
                        .map_err(StageError::from)
                })
                .await
                .map_err(|_| StageError::Unavailable("Account connection timed out"))?
            }
        })
        .buffer_unordered(4);
        let mut connected = None;
        while let Some(result) = connections.next().await {
            if let Ok(stream) = result {
                connected = Some(stream);
                break;
            }
        }
        drop(connections);
        let mut stream = connected.ok_or(StageError::Unavailable("Account device unreachable"))?;
        if stream.get_ref().1.alpn_protocol() != Some(b"daat-locus-account/1") {
            return Err(StageError::Forbidden("Account protocol mismatch"));
        }
        let binding = hex::encode(
            stream
                .get_ref()
                .1
                .export_keying_material([0; 32], b"EXPORTER-DaatLocus-Account-v1", None)
                .map_err(|_| StageError::Forbidden("Account channel unavailable"))?,
        );
        let login = DeviceLogin {
            name,
            account_id: candidate.account_id.clone(),
            request,
            device: device.clone(),
            origin: origin.clone(),
        };
        write(&mut stream, &Packet::Start { login }).await?;
        let Packet::Challenge { response, context } = read(&mut stream).await? else {
            return Err(StageError::Forbidden("Account login failed"));
        };
        let context = decode(&context)?;
        Self::validate_context(runtime, &candidate, &device, &origin, &binding, &context)?;
        Ok(Self {
            response,
            context,
            candidate,
            device,
            transport: Transport::Tcp(Box::new(stream)),
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
        let reply = match &mut self.transport {
            Transport::Tcp(stream) => {
                write(stream, &Packet::Finish { message }).await?;
                read(stream).await?
            }
            Transport::Relay(relay) => relay.finish(message).await?,
        };
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
    candidates: RwLock<HashMap<StageId, Candidate>>,
    services: RwLock<HashMap<String, StageId>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    changed: Notify,
    daemon: Option<ServiceDaemon>,
    pub address: SocketAddr,
    budget: Mutex<HashMap<IpAddr, (Instant, u32)>>,
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
        let ip = runtime
            .host
            .settings()
            .listen
            .parse::<SocketAddr>()
            .map_err(|_| StageError::Invalid("Invalid device listener"))?
            .ip();
        let listener = tokio::net::TcpListener::bind(SocketAddr::new(ip, 0)).await?;
        let address = listener.local_addr()?;
        let daemon = ServiceDaemon::new().ok();
        let network = Arc::new(Self {
            runtime: Arc::downgrade(runtime),
            publication: RwLock::new((
                None,
                super::public_connection::PublicationStatus::initial(false, false),
            )),
            publication_wake: Notify::new(),
            candidates: RwLock::new(HashMap::new()),
            services: RwLock::new(HashMap::new()),
            tasks: Mutex::new(vec![]),
            changed: Notify::new(),
            daemon,
            address,
            budget: Mutex::new(HashMap::new()),
        });
        let own = network.clone();
        let identity = runtime.host.identity()?;
        let acceptor = tls::account_server(&identity)?;
        network.tasks.lock().push(tokio::spawn(async move {
            let permits = Arc::new(tokio::sync::Semaphore::new(8));
            while let Ok((stream, peer)) = listener.accept().await {
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    continue;
                };
                if !own.permit(peer.ip()) {
                    continue;
                }
                let acceptor = acceptor.clone();
                let server = own.clone();
                own.tasks.lock().retain(|task| !task.is_finished());
                own.tasks.lock().push(tokio::spawn(async move {
                    let _permit = permit;
                    let result = async {
                        let mut stream = acceptor.accept(stream).await?;
                        if stream.get_ref().1.alpn_protocol() != Some(b"daat-locus-account/1") {
                            return Err(StageError::Forbidden("Account protocol mismatch"));
                        }
                        let binding = hex::encode(
                            stream
                                .get_ref()
                                .1
                                .export_keying_material(
                                    [0; 32],
                                    b"EXPORTER-DaatLocus-Account-v1",
                                    None,
                                )
                                .map_err(|_| {
                                    StageError::Forbidden("Account channel unavailable")
                                })?,
                        );
                        let Some(runtime) = server.runtime.upgrade() else {
                            return Err(StageError::Unavailable("Account device stopped"));
                        };
                        let result = async {
                            let Packet::Start { login } = read(&mut stream).await? else {
                                return Err(StageError::Invalid("Account login required"));
                            };
                            let (attempt, reply) = Attempt::start(&runtime, login, binding)?;
                            write(&mut stream, &reply).await?;
                            let Packet::Finish { message } = read(&mut stream).await? else {
                                return Err(StageError::Invalid("Account proof required"));
                            };
                            let reply = attempt.finish(&runtime, &message)?;
                            write(&mut stream, &reply).await
                        }
                        .await;
                        if result.is_err() {
                            let _ = write(&mut stream, &Packet::Rejected).await;
                        }
                        result
                    };
                    let _ = tokio::time::timeout(Duration::from_secs(120), result).await;
                }));
            }
        }));
        network.start_discovery();
        if publication {
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
        let origin = runtime.host.settings().signaling_origin;
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
    fn permit(&self, ip: IpAddr) -> bool {
        let mut budget = self.budget.lock();
        budget.retain(|_, (time, _)| time.elapsed() < Duration::from_secs(60));
        if budget.len() >= 1024 && !budget.contains_key(&ip) {
            return false;
        }
        let (_, count) = budget.entry(ip).or_insert((Instant::now(), 0));
        *count += 1;
        *count <= 20
    }
    fn start_discovery(self: &Arc<Self>) {
        let Some(daemon) = self.daemon.clone() else {
            return;
        };
        let Ok(events) = daemon.browse(SERVICE) else {
            return;
        };
        let own = self.clone();
        self.tasks.lock().push(tokio::spawn(async move {
            let mut published=None;let mut timer=tokio::time::interval(Duration::from_secs(5));
            loop {tokio::select! {
                event=events.recv_async()=>match event {
                    Ok(ServiceEvent::ServiceResolved(info))=>own.discovered(&info),
                    Ok(ServiceEvent::ServiceRemoved(_,name))=>{let stage=own.services.write().remove(&name);if let Some(stage)=stage {own.candidates.write().remove(&stage);own.changed.notify_waiters();}},
                    Err(_)=>break,
                    _=>{},
                },
                _=timer.tick()=>{
                    let Some(runtime)=own.runtime.upgrade() else {break;};
                    let Ok(Some(record))=runtime.host.accounts.current() else {continue;};
                    let Ok(profile)=runtime.host.identity().map(|identity|identity.profile()) else {continue;};
                    let label=runtime.host.settings().label;
                    let properties=[("protocol","1".into()),("id",profile.stage_id.to_string()),("account",record.account_id.clone()),("login",lookup(&record.name)),("key",profile.public_key.clone()),("tls",profile.tls_sha256.clone()),("label",label)];
                    let current=serde_json::to_string(&properties).unwrap_or_default();
                    if published.as_ref()==Some(&current){continue;}
                    let registration=ServiceInfo::new(SERVICE,&format!("dl-account-{}",profile.stage_id),&format!("dl-{}.local.",profile.stage_id),if own.address.ip().is_unspecified(){String::new()}else{own.address.ip().to_string()},own.address.port(),&properties[..]);
                    if let Ok(info)=registration && daemon.register(info.enable_addr_auto()).is_ok(){published=Some(current);}
                }
            }}
        }));
    }
    fn discovered(&self, info: &ResolvedService) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        if info.get_property_val_str("protocol") != Some("1") {
            return;
        }
        let property = |key| info.get_property_val_str(key).map(str::to_owned);
        let Some(stage) = property("id")
            .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            .map(StageId)
        else {
            return;
        };
        if stage == runtime.host.local {
            return;
        }
        let (Some(account_id), Some(name_hash), Some(public_key), Some(tls_sha256)) = (
            property("account"),
            property("login"),
            property("key"),
            property("tls"),
        ) else {
            return;
        };
        if [&account_id, &name_hash, &tls_sha256]
            .iter()
            .any(|value| value.len() != 64 || hex::decode(value).is_err())
            || public_key.len() > 64
        {
            return;
        }
        let addresses = info
            .get_addresses()
            .iter()
            .take(16)
            .map(|ip| match ip {
                ScopedIp::V6(ip) => SocketAddr::V6(std::net::SocketAddrV6::new(
                    *ip.addr(),
                    info.get_port(),
                    0,
                    ip.scope_id().index,
                )),
                _ => SocketAddr::new(ip.to_ip_addr(), info.get_port()),
            })
            .filter(|address| !address.ip().is_unspecified() && !address.ip().is_multicast())
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            return;
        }
        let mut candidates = self.candidates.write();
        candidates.retain(|_, candidate| candidate.seen.elapsed() < Duration::from_secs(120));
        if candidates.len() >= 64 && !candidates.contains_key(&stage) {
            return;
        }
        candidates.insert(
            stage,
            Candidate {
                profile: DeviceProfile {
                    stage_id: stage,
                    public_key,
                    tls_sha256,
                },
                account_id,
                name_hash,
                addresses,
                label: property("label")
                    .unwrap_or_default()
                    .chars()
                    .take(100)
                    .collect(),
                relay: None,
                seen: Instant::now(),
            },
        );
        drop(candidates);
        self.services
            .write()
            .insert(info.get_fullname().to_owned(), stage);
        self.changed.notify_waiters();
    }
    pub async fn candidates(&self, name: &str, excluded: &[String]) -> Vec<Candidate> {
        let wake = self.changed.notified();
        tokio::pin!(wake);
        wake.as_mut().enable();
        let hash = lookup(name);
        let select = || {
            let mut result = self
                .candidates
                .read()
                .values()
                .filter(|candidate| {
                    candidate.name_hash == hash
                        && !excluded.contains(&candidate.profile.stage_id.to_string())
                        && candidate.seen.elapsed() < Duration::from_secs(120)
                })
                .cloned()
                .collect::<Vec<_>>();
            result.sort_by_key(|candidate| candidate.profile.stage_id);
            result
        };
        let mut result = select();
        if result.is_empty() {
            let _ = tokio::time::timeout(Duration::from_secs(3), wake).await;
            result = select();
        }
        result
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
            runtime.host.settings().signaling_origin,
            runtime.host.identity(),
        ) else {
            return Err(LoginError::ConnectionRequired);
        };
        match tokio::time::timeout(
            Duration::from_secs(10),
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

    /// A discovery announcement does not establish reachability. Keep working
    /// LAN login independent of the relay, but try public discovery after all
    /// matching LAN connections fail, including routes to the same device.
    pub async fn login(
        &self,
        name: &str,
        request: &str,
        origin: &str,
        excluded: &[String],
    ) -> std::result::Result<RemoteLogin, LoginError> {
        let runtime = self.runtime.upgrade().ok_or(LoginError::Unavailable)?;
        let mut connection_failure = None;
        for public in [false, true] {
            let candidates = if public {
                match self.relay_candidates(name, excluded).await {
                    Ok(candidates) => candidates,
                    Err(error) => {
                        return Err(if error == LoginError::ConnectionRequired {
                            connection_failure.unwrap_or(error)
                        } else {
                            error
                        });
                    }
                }
            } else {
                self.candidates(name, excluded).await
            };
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
                    Duration::from_secs(10),
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
            while let Some(result) = attempts.next().await {
                match result {
                    Ok(Ok(login)) => return Ok(login),
                    Ok(Err(error)) => {
                        tracing::debug!(public, reason = %error, "Account device login connection failed");
                        connection_failure = Some(LoginError::connection(&error));
                    }
                    Err(_) => {
                        tracing::debug!(public, "Account device login connection timed out");
                        connection_failure = Some(LoginError::TimedOut);
                    }
                }
            }
        }
        Err(connection_failure.unwrap_or(LoginError::NoOnlineDevice))
    }
    #[cfg(test)]
    pub fn add_test_candidate(&self, candidate: Candidate) {
        self.candidates
            .write()
            .insert(candidate.profile.stage_id, candidate);
        self.changed.notify_waiters();
    }
    pub async fn stop(&self) {
        let tasks = std::mem::take(&mut *self.tasks.lock());
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
        if let Some(daemon) = &self.daemon {
            let _ = daemon.shutdown();
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
        settings.listen = "127.0.0.1:0".into();
        settings.signaling_origin = Some(format!("ws://{}", listener.local_addr().unwrap()));
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
