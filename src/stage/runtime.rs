//! Native Stage supervision and account-scoped remote observations.
use super::{Result, StageError, StageId, host::Host, ledger::StageLedger, protocol::RemoteActor};
use super::{
    channel::{PeerChannel, SendScope},
    connection::{LinkState, Route},
    peer::{PeerHandle, PeerLink},
    transport::{lan::LanDiscovery, ssh::SshTarget, tls},
};
use parking_lot::{Mutex, RwLock};
use rusqlite::params;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{sync::Notify, task::JoinHandle};
#[derive(Default)]
struct Candidates(Vec<(SocketAddr, std::time::Instant)>);
impl Candidates {
    fn add(&mut self, address: SocketAddr) {
        self.0
            .retain(|(old, seen)| *old != address && seen.elapsed().as_secs() < 120);
        self.0.push((address, std::time::Instant::now()));
        if self.0.len() > 16 {
            self.0.remove(0);
        }
    }
    fn live(&mut self) -> Vec<SocketAddr> {
        self.0.retain(|(_, seen)| seen.elapsed().as_secs() < 120);
        self.0.iter().map(|(address, _)| *address).collect()
    }
}
pub struct Runtime {
    pub host: Arc<Host>,
    account_network: tokio::sync::Mutex<Option<Arc<crate::accounts::network::Network>>>,
    pub(crate) coordination: RwLock<Option<Arc<super::coordination::Coordinator>>>,
    pub(crate) core: RwLock<Option<Arc<super::shared_runtime::Controller>>>,
    core_task: Mutex<Option<JoinHandle<()>>>,
    errors: RwLock<HashMap<StageId, String>>,
    handles: RwLock<HashMap<StageId, PeerHandle>>,
    states: RwLock<HashMap<StageId, LinkState>>,
    candidates: RwLock<HashMap<StageId, Candidates>>,
    attempts: Mutex<HashSet<StageId>>,
    wanted: RwLock<HashSet<StageId>>,
    dialing: Mutex<HashMap<StageId, Arc<tokio::sync::Mutex<()>>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    running: AtomicBool,
    stopped: AtomicBool,
    wake: Notify,
    signal_wake: Notify,
    pub(crate) signals: RwLock<Option<tokio::sync::mpsc::Sender<super::signaling::SignalCommand>>>,
    pub address: RwLock<Option<SocketAddr>>,
    listener: RwLock<Option<Arc<tokio::net::TcpListener>>>,
    public_connection: Mutex<Option<JoinHandle<()>>>,
}
impl Runtime {
    pub fn voting_member(&self, peer: StageId) -> Option<bool> {
        self.coordination
            .read()
            .as_ref()
            .map(|coordinator| coordinator.is_voter(peer))
    }
    pub async fn coordinate(
        &self,
        operation: super::shared::Operation,
        receipts: Vec<super::shared::Acknowledgement>,
    ) -> Result<(super::shared::Reply, std::time::Instant, std::time::Instant)> {
        let coordinator = self
            .coordination
            .read()
            .clone()
            .ok_or(StageError::Unavailable("Device consensus is not running"))?;
        let sent = std::time::Instant::now();
        let reply = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            coordinator.command(operation, receipts),
        )
        .await
        .map_err(|_| StageError::Unavailable("Device quorum request timed out"))??;
        Ok((reply, sent, std::time::Instant::now()))
    }
    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
    pub(crate) fn connected(&self, peer: StageId) -> bool {
        self.handles
            .read()
            .get(&peer)
            .is_some_and(PeerHandle::is_connected)
    }
    pub(crate) fn clear_signal(&self) {
        self.signals.write().take();
    }
    pub(crate) fn install_signal(
        &self,
        tx: tokio::sync::mpsc::Sender<super::signaling::SignalCommand>,
    ) {
        *self.signals.write() = Some(tx);
        self.wake.notify_waiters();
    }
    pub(crate) async fn signal_notification(&self) {
        self.signal_wake.notified().await;
    }
    pub(crate) fn signal_changed(&self) {
        self.signal_wake.notify_one();
        self.wake.notify_waiters();
        if let Ok(network) = self.account_network.try_lock()
            && let Some(network) = network.as_ref()
        {
            network.publication_wake.notify_waiters();
        }
    }
    pub(crate) fn need_member(&self, _peer: StageId) {
        for handle in self.handles.read().values() {
            handle.refresh_members();
        }
    }
    pub(crate) fn spawn(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        self.tasks.lock().push(tokio::spawn(future));
    }
    pub fn new(host: Arc<Host>) -> Arc<Self> {
        Arc::new(Self {
            coordination: RwLock::new(None),
            host,
            account_network: tokio::sync::Mutex::new(None),
            core: RwLock::new(None),
            core_task: Mutex::new(None),
            errors: RwLock::new(HashMap::new()),
            handles: RwLock::new(HashMap::new()),
            states: RwLock::new(HashMap::new()),
            candidates: RwLock::new(HashMap::new()),
            attempts: Mutex::new(HashSet::new()),
            wanted: RwLock::new(HashSet::new()),
            dialing: Mutex::new(HashMap::new()),
            tasks: Mutex::new(vec![]),
            running: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            wake: Notify::new(),
            signal_wake: Notify::new(),
            signals: RwLock::new(None),
            address: RwLock::new(None),
            listener: RwLock::new(None),
            public_connection: Mutex::new(None),
        })
    }
    pub fn state(&self, peer: StageId) -> LinkState {
        self.states.read().get(&peer).cloned().unwrap_or_default()
    }
    pub fn last_error(&self, peer: StageId) -> Option<String> {
        self.errors.read().get(&peer).cloned()
    }
    pub fn connection_error_code(&self, peer: StageId) -> Option<&'static str> {
        self.last_error(peer).map(|error| {
            if error.contains("Signaling") {
                "device_signaling_unavailable"
            } else if error.contains("timed out") || error.contains("expired") {
                "device_connection_timeout"
            } else if error.contains("Public peer is not available") {
                "device_peer_offline"
            } else {
                "device_connection_failed"
            }
        })
    }
    pub(crate) fn signaling_failed(&self, error: &StageError) {
        for peer in self.wanted.read().iter().copied().collect::<Vec<_>>() {
            let state = self.state(peer);
            if !self.connected(peer) && (state.route == Some(Route::P2p) || state.route.is_none()) {
                self.p2p_failed(peer, error);
            }
        }
    }
    pub(crate) fn begin_p2p(&self, peer: StageId) -> Result<()> {
        let mut states = self.states.write();
        let state = states.entry(peer).or_default();
        if state.status == super::connection::ConnectionStatus::Connecting {
            return Err(StageError::Unavailable(
                "Device connection is already in progress",
            ));
        }
        state.attempt(Route::P2p, chrono::Utc::now().timestamp_millis(), false)
    }
    pub(crate) fn p2p_failed(&self, peer: StageId, error: &StageError) {
        if self.connected(peer) {
            return;
        }
        tracing::debug!(%peer, reason = %error, "Public device connection failed");
        self.errors.write().insert(peer, error.to_string());
        self.states
            .write()
            .entry(peer)
            .or_default()
            .failed(chrono::Utc::now().timestamp_millis());
        self.wake.notify_waiters();
    }
    pub fn handle(&self, peer: StageId) -> Result<PeerHandle> {
        StageLedger::attach(self.host.system.store(), self.host.local)
            .peer(peer, chrono::Utc::now().timestamp_millis())?;
        if let Some(handle) = self
            .handles
            .read()
            .get(&peer)
            .filter(|handle| handle.is_connected())
        {
            return Ok(handle.clone());
        }
        self.wanted.write().insert(peer);
        self.wake.notify_waiters();
        Err(StageError::Unavailable("Device is offline or connecting"))
    }
    pub fn want(&self, peer: StageId) {
        self.wanted.write().insert(peer);
        self.wake.notify_waiters();
    }
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        self.account_network().await?;
        if self.host.account_id().is_none() {
            return Ok(());
        }
        if self.running.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let result = self.start_prepared().await;
        if result.is_err() {
            // A failed identity/listener/discovery setup must not leave a
            // half-started worker or prevent a subsequent owner retry.
            self.stop().await;
        }
        result
    }
    async fn start_prepared(self: &Arc<Self>) -> Result<()> {
        self.stopped.store(false, Ordering::Release);
        let settings = self.host.settings();
        self.host.identity()?;
        self.host.enrollment()?;
        super::reset::cleanup(&self.host.system)?;
        if self
            .host
            .configurations
            .current()
            .map_err(|_| StageError::Invalid("Cannot read account configuration"))?
            .is_none()
            && self.host.identity()?.has_group_authority()
            && self.host.system.config().validate().is_ok()
        {
            self.host
                .configurations
                .publish(&self.host, &self.host.system.config())
                .map_err(|_| StageError::Invalid("Cannot initialize account model definitions"))?;
        }
        if super::identity::verify_enrollment(
            &self.host.enrollment()?.root,
            &self.host.enrollment()?.chain,
            chrono::Utc::now().timestamp_millis(),
            &super::revocation::removed(self.host.system.store())?,
        )?
        .can_enroll
        {
            let core = super::shared_runtime::Controller::new(self)?;
            let coordinator = super::coordination::Coordinator::new(self).await?;
            *self.host.coordination.write() = Some(Arc::downgrade(&coordinator));
            *self.coordination.write() = Some(coordinator.clone());
            coordinator.bootstrap().await?;
            let membership = coordinator.clone();
            let owner = self.clone();
            self.spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                while !owner.is_stopped() {
                    interval.tick().await;
                    if let Err(error) = tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        membership.admit_connected(),
                    )
                    .await
                    {
                        tracing::debug!(reason=?error, "Device membership update deferred");
                    }
                }
            });
            *self.host.core.write() = Some(Arc::downgrade(&core));
            self.host.system.install_shared_core(&core);
            *self.core.write() = Some(core.clone());
            *self.core_task.lock() = Some(tokio::spawn(core.run()));
        }
        let prepared = self.listener.read().clone();
        let listener = if let Some(listener) = prepared {
            listener
        } else {
            match tokio::net::TcpListener::bind(&settings.listen).await {
                Ok(listener) => Arc::new(listener),
                Err(error) => {
                    self.running.store(false, Ordering::Release);
                    return Err(error.into());
                }
            }
        };
        *self.listener.write() = Some(listener.clone());
        let actual = listener.local_addr()?;
        *self.address.write() = Some(actual);
        if settings
            .listen
            .parse::<SocketAddr>()
            .map_err(|_| StageError::Invalid("Invalid Stage listener"))?
            .port()
            == 0
        {
            let mut persisted = settings.clone();
            persisted.listen = actual.to_string();
            self.host.save_settings(persisted)?;
        }
        let own = self.clone();
        self.tasks.lock().push(tokio::spawn(async move {
            let permits=Arc::new(tokio::sync::Semaphore::new(8));
            while !own.stopped.load(Ordering::Acquire){let connection=tokio::select!{result=listener.accept()=>result,_=own.wake.notified()=>{if own.stopped.load(Ordering::Acquire){break;}continue;}};
                let Ok((socket,_))=connection else{break;};let Ok(permit)=permits.clone().try_acquire_owned()else{continue;};let owner=own.clone();
                own.tasks.lock().push(tokio::spawn(async move{let _permit=permit;let _=owner.accept(socket).await;}));
            }
        }));
        {
            let known = StageLedger::attach(self.host.system.store(), self.host.local)
                .peers(chrono::Utc::now().timestamp_millis())?
                .iter()
                .map(|peer| peer.device.stage_id)
                .collect();
            let mut discovery =
                LanDiscovery::start(self.host.local, actual, known, settings.interfaces.clone())?;
            let own = self.clone();
            self.tasks.lock().push(tokio::spawn(async move{while !own.stopped.load(Ordering::Acquire){tokio::select! {
                candidate=discovery.next()=>match candidate{Ok(candidate)=>{own.discovered(candidate);},Err(_)=>break},
                _=own.wake.notified()=>{let known=StageLedger::attach(own.host.system.store(),own.host.local).peers(chrono::Utc::now().timestamp_millis()).unwrap_or_default().iter().map(|peer|peer.device.stage_id).collect();let _=discovery.update_known(known);}
            }}}));
        }
        for peer in self.host.settings().ssh.keys() {
            self.wanted.write().insert(*peer);
        }
        let own = self.clone();
        self.tasks.lock().push(tokio::spawn(async move {
            own.supervise().await;
        }));
        if settings.signaling_origin.is_some() {
            self.ensure_public_connection();
        }
        Ok(())
    }
    pub async fn public_login_status(
        &self,
    ) -> crate::accounts::public_connection::PublicationStatus {
        let network = self.account_network.lock().await;
        network
            .as_ref()
            .map(|network| network.publication_status())
            .unwrap_or_else(|| {
                crate::accounts::public_connection::PublicationStatus::initial(
                    self.host.settings().signaling_origin.is_some(),
                    self.host.account_id().is_some(),
                )
            })
    }
    pub async fn account_network(
        self: &Arc<Self>,
    ) -> Result<Arc<crate::accounts::network::Network>> {
        let mut network = self.account_network.lock().await;
        if let Some(network) = network.as_ref() {
            return Ok(network.clone());
        }
        let created = crate::accounts::network::Network::start(self).await?;
        *network = Some(created.clone());
        Ok(created)
    }
    pub fn account_candidate(&self, peer: StageId, address: SocketAddr) {
        self.candidates
            .write()
            .entry(peer)
            .or_default()
            .add(address);
        self.fresh_candidate(peer);
        self.want(peer);
    }
    fn fresh_candidate(&self, peer: StageId) {
        let mut states = self.states.write();
        if let Some(state) = states.get_mut(&peer)
            && state.status == super::connection::ConnectionStatus::Unreachable
        {
            state.retry_at_ms = 0;
        }
    }
    fn discovered(self: &Arc<Self>, candidate: super::transport::lan::Candidate) {
        if candidate.addresses.is_empty() {
            self.candidates.write().remove(&candidate.stage);
            self.probe(candidate.stage);
        } else {
            let mut addresses = Candidates::default();
            for address in candidate.addresses {
                addresses.add(address);
            }
            self.candidates.write().insert(candidate.stage, addresses);
            self.fresh_candidate(candidate.stage);
        }
        self.want(candidate.stage);
    }
    pub(crate) fn probe(self: &Arc<Self>, peer: StageId) {
        let Some(handle) = self.handles.read().get(&peer).cloned() else {
            return;
        };
        self.spawn(async move {
            if matches!(
                handle
                    .rpc_with_timeout(
                        super::protocol::Body::Heartbeat,
                        std::time::Duration::from_secs(2)
                    )
                    .await,
                Err(StageError::Unavailable(_))
            ) {
                handle.disconnect();
            }
        });
    }
    fn dial_failed(&self, peer: StageId, error: &StageError) {
        if !self.connected(peer) {
            self.errors.write().insert(peer, error.to_string());
            self.states
                .write()
                .entry(peer)
                .or_default()
                .failed(chrono::Utc::now().timestamp_millis());
        }
        self.wake.notify_waiters();
    }
    async fn accept(self: Arc<Self>, socket: tokio::net::TcpStream) -> Result<()> {
        let peers = StageLedger::attach(self.host.system.store(), self.host.local)
            .peers(chrono::Utc::now().timestamp_millis())?;
        let profiles = peers
            .iter()
            .map(|peer| peer.device.clone())
            .collect::<Vec<_>>();
        if profiles.is_empty() {
            return Err(StageError::Forbidden(
                "No authenticated devices are enrolled",
            ));
        }
        let identity = self.host.identity()?;
        let acceptor = tls::server(&identity, &profiles)?;
        let mut stream = tls::accept(&acceptor, socket).await?;
        let peer = tls::identify_client(&stream, &profiles)?;
        let lock = self.peer_lock(peer.stage_id());
        // If both ends dial simultaneously, the lower Stage's outgoing channel
        // wins. Rejecting its competing incoming channel releases the other
        // end's outgoing lock instead of deadlocking both handshakes.
        let _guard = if self.host.local < peer.stage_id() {
            lock.try_lock()
                .map_err(|_| StageError::Conflict("Concurrent outgoing channel has priority"))?
        } else {
            lock.lock().await
        };
        if self.handles.read().contains_key(&peer.stage_id()) {
            return Err(StageError::Conflict(
                "Healthy peer channel is already connected",
            ));
        }
        let binding = hex::encode(
            stream
                .get_ref()
                .1
                .export_keying_material([0; 32], b"EXPORTER-DaatLocus-Stage-v1", None)
                .map_err(|_| StageError::Forbidden("TLS exporter unavailable"))?,
        );
        let ledger = StageLedger::attach(self.host.system.store(), self.host.local);
        let (generation, _) = super::connection::handshake(
            &mut stream,
            &identity,
            &ledger,
            &peer,
            &binding,
            chrono::Utc::now().timestamp_millis(),
        )
        .await?;
        let owner = self.clone();
        let authenticated = peer.clone();
        let channel = PeerChannel::stream(peer, stream, move |frame, scope| {
            owner.authorize_send(&authenticated, frame, scope)
        });
        self.attach(channel, generation, Route::Lan);
        Ok(())
    }
    fn peer_lock(&self, peer: StageId) -> Arc<tokio::sync::Mutex<()>> {
        self.dialing
            .lock()
            .entry(peer)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
    pub async fn dial_tcp(
        self: &Arc<Self>,
        peer: StageId,
        address: SocketAddr,
        ssh: Option<SshTarget>,
    ) -> Result<()> {
        self.dial(peer, vec![address], ssh).await
    }
    async fn dial(
        self: &Arc<Self>,
        peer: StageId,
        addresses: Vec<SocketAddr>,
        ssh: Option<SshTarget>,
    ) -> Result<()> {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(12),
            self.dial_prepared(peer, addresses, ssh),
        )
        .await
        .map_err(|_| StageError::Unavailable("Device connection timed out"))
        .and_then(|result| result);
        if let Err(error) = &result {
            self.dial_failed(peer, error);
        }
        result
    }
    async fn dial_prepared(
        self: &Arc<Self>,
        peer: StageId,
        addresses: Vec<SocketAddr>,
        ssh: Option<SshTarget>,
    ) -> Result<()> {
        let lock = self.peer_lock(peer);
        let _guard = lock.lock().await;
        if self.connected(peer) {
            return Ok(());
        }
        let ledger = StageLedger::attach(self.host.system.store(), self.host.local);
        let record = ledger.peer(peer, chrono::Utc::now().timestamp_millis())?;
        let identity = self.host.identity()?;
        let connector = tls::client(&identity, &record.device)?;
        let route = if ssh.is_some() {
            Route::Ssh
        } else {
            Route::Lan
        };
        self.states.write().entry(peer).or_default().attempt(
            route,
            chrono::Utc::now().timestamp_millis(),
            self.host.settings().ssh.contains_key(&peer),
        )?;
        if let Some(target) = ssh {
            let socket = super::transport::ssh::open(&target)?;
            let mut stream = tls::connect(&connector, peer, socket).await?;
            self.authenticate_stream(peer, &mut stream, &record.device, &identity, &ledger)
                .await?;
            let verified = tls::identify_server(&stream, &record.device)?;
            let generation = ledger
                .peer(peer, chrono::Utc::now().timestamp_millis())?
                .generation;
            self.attach_stream(verified, stream, generation, route);
        } else {
            use futures_util::StreamExt;
            let mut unique = HashSet::new();
            let mut attempts = futures_util::stream::iter(
                addresses
                    .into_iter()
                    .filter(|address| unique.insert(*address))
                    .take(8),
            )
            .map(|address| {
                let connector = &connector;
                async move {
                    tokio::time::timeout(std::time::Duration::from_secs(4), async {
                        let socket = super::transport::lan::connect(address).await?;
                        tls::connect(connector, peer, socket).await
                    })
                    .await
                    .map_err(|_| StageError::Unavailable("LAN connection timed out"))?
                }
            })
            .buffer_unordered(4);
            let mut last = StageError::Unavailable("No reachable LAN address");
            let mut connected = None;
            while let Some(result) = attempts.next().await {
                match result {
                    Ok(stream) => {
                        connected = Some(stream);
                        break;
                    }
                    Err(error) => last = error,
                }
            }
            drop(attempts);
            let mut stream = connected.ok_or(last)?;
            self.authenticate_stream(peer, &mut stream, &record.device, &identity, &ledger)
                .await?;
            let verified = tls::identify_server(&stream, &record.device)?;
            let generation = ledger
                .peer(peer, chrono::Utc::now().timestamp_millis())?
                .generation;
            self.attach_stream(verified, stream, generation, route);
        }
        Ok(())
    }
    async fn authenticate_stream<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
        &self,
        _peer: StageId,
        stream: &mut tokio_rustls::client::TlsStream<S>,
        profile: &super::identity::DeviceProfile,
        identity: &super::identity::DeviceIdentity,
        ledger: &StageLedger<'_>,
    ) -> Result<()> {
        let verified = tls::identify_server(stream, profile)?;
        let binding = hex::encode(
            stream
                .get_ref()
                .1
                .export_keying_material([0; 32], b"EXPORTER-DaatLocus-Stage-v1", None)
                .map_err(|_| StageError::Forbidden("TLS exporter unavailable"))?,
        );
        super::connection::handshake(
            stream,
            identity,
            ledger,
            &verified,
            &binding,
            chrono::Utc::now().timestamp_millis(),
        )
        .await?;
        Ok(())
    }
    fn attach_stream<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static>(
        self: &Arc<Self>,
        verified: super::transport::tls::VerifiedPeer,
        stream: S,
        generation: u64,
        route: Route,
    ) {
        let owner = self.clone();
        let check = verified.clone();
        self.attach(
            PeerChannel::stream(verified, stream, move |frame, scope| {
                owner.authorize_send(&check, frame, scope)
            }),
            generation,
            route,
        );
    }
    pub(crate) fn attach(self: &Arc<Self>, channel: PeerChannel, generation: u64, route: Route) {
        let peer = channel.peer.stage_id();
        let (link, handle) = PeerLink::new(self.host.clone(), channel, generation);
        self.handles.write().insert(peer, handle);
        self.errors.write().remove(&peer);
        self.states
            .write()
            .entry(peer)
            .or_default()
            .online(generation, chrono::Utc::now().timestamp_millis());
        let own = self.clone();
        self.tasks.lock().push(tokio::spawn(async move {
            let result = link.run().await;
            if let Err(error) = &result {
                tracing::debug!(peer=%peer,generation,error=?error,"Native peer channel stopped");
            }
            let mut states = own.states.write();
            // A replaced link can finish after its successor is already online.
            // Its cleanup must not remove that new authenticated generation.
            if states
                .get(&peer)
                .is_some_and(|state| state.generation == generation)
            {
                if let Err(error) = result {
                    own.errors.write().insert(peer, error.to_string());
                }
                own.handles.write().remove(&peer);
                states
                    .entry(peer)
                    .or_default()
                    .failed(chrono::Utc::now().timestamp_millis());
            }
            drop(states);
            own.wake.notify_waiters();
        }));
        self.states.write().entry(peer).or_default().route = Some(route);
    }
    pub(crate) fn authorize_send(
        &self,
        verified: &super::transport::tls::VerifiedPeer,
        frame: &super::protocol::Envelope,
        scope: &SendScope,
    ) -> Result<()> {
        self.host.enrollment()?;
        let time = chrono::Utc::now().timestamp_millis();
        let record = StageLedger::attach(self.host.system.store(), self.host.local)
            .peer(verified.stage_id(), time)?;
        if record.device.tls_sha256 != verified.certificate_hash()
            || record.generation != frame.generation
            || frame.target != verified.stage_id()
            || frame.source != self.host.local
        {
            return Err(StageError::Forbidden(
                "Emission lost its authenticated identity or generation",
            ));
        }
        match scope {
            SendScope::Core => {
                if self.host.account_id().is_none() {
                    return Err(StageError::Forbidden(
                        "Account Actor connectivity was disabled",
                    ));
                }
                Ok(())
            }
            SendScope::Connection => Ok(()),
            SendScope::Actor(actor) => {
                if actor == super::shared::ACTOR_ID && self.host.account_id().is_some() {
                    return Ok(());
                }
                if !StageLedger::attach(self.host.system.store(), self.host.local)
                    .actor_grant(verified.stage_id(), actor, time)?
                    .read
                {
                    return Err(StageError::Forbidden("Actor access was revoked"));
                }
                self.host
                    .system
                    .actor(actor)
                    .map_err(|_| StageError::Forbidden("Actor is unavailable"))?;
                Ok(())
            }
            SendScope::Invocation {
                invocation,
                content,
            } => {
                StageLedger::attach(self.host.system.store(), self.host.local).exported_manifest(
                    verified.stage_id(),
                    *invocation,
                    content,
                    time,
                )?;
                Ok(())
            }
        }
    }
    async fn supervise(self: Arc<Self>) {
        let changes = self.host.system.store().changed();
        while !self.stopped.load(Ordering::Acquire) {
            let wake = self.wake.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            let changed = changes.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let reset = self.host.work_reset.record.lock().clone();
            if let Some(record) = reset {
                let _lifecycle = self.host.lifecycle.lock().await;
                match self.apply_account_reset(&record).await {
                    Ok(()) => {
                        super::reset::completed(&self.host, &record);
                    }
                    Err(error) => tracing::warn!(reason=%error, "Account work reset deferred"),
                }
            }
            let peers = self.wanted.read().iter().copied().collect::<Vec<_>>();
            for peer in peers {
                if let Some(handle) = self
                    .handles
                    .read()
                    .get(&peer)
                    .filter(|handle| handle.is_connected())
                {
                    handle.flush();
                    continue;
                }
                if self.state(peer).retry_at_ms > chrono::Utc::now().timestamp_millis() {
                    continue;
                }
                if self.state(peer).status == super::connection::ConnectionStatus::Connecting {
                    continue;
                }
                let ssh = self.host.settings().ssh.get(&peer).cloned();
                let addresses = self
                    .candidates
                    .write()
                    .get_mut(&peer)
                    .map(Candidates::live)
                    .unwrap_or_default();
                if ssh.is_none() && addresses.is_empty() {
                    if self.host.settings().signaling_origin.is_some()
                        && let Some(signal) = self.signals.read().clone()
                    {
                        let _ = signal.try_send(super::signaling::SignalCommand::Connect(peer));
                    }
                    continue;
                }
                if !self.attempts.lock().insert(peer) {
                    continue;
                }
                let own = self.clone();
                self.tasks.lock().push(tokio::spawn(async move {
                    let allow_public = ssh.is_none();
                    if own.dial(peer, addresses, ssh).await.is_err() && allow_public {
                        own.request_public_fallback(peer);
                    }
                    own.attempts.lock().remove(&peer);
                    own.wake.notify_waiters();
                }));
            }
            self.tasks.lock().retain(|task| !task.is_finished());
            let delay = self
                .wanted
                .read()
                .iter()
                .filter(|peer| !self.connected(**peer))
                .map(|peer| self.state(*peer).retry_at_ms)
                .filter(|deadline| *deadline > 0)
                .min();
            if let Some(deadline) = delay {
                tokio::select! {_=wake=>{},_=changed=>{},_=tokio::time::sleep(std::time::Duration::from_millis(deadline.saturating_sub(chrono::Utc::now().timestamp_millis()).clamp(100, 5000) as u64))=>{}}
            } else {
                tokio::select! {_=wake=>{},_=changed=>{},_=tokio::time::sleep(std::time::Duration::from_secs(5))=>{}}
            }
        }
    }
    fn request_public_fallback(&self, peer: StageId) -> bool {
        if self.host.settings().signaling_origin.is_none() {
            return false;
        }
        let Some(signal) = self.signals.read().clone() else {
            return false;
        };
        {
            let mut states = self.states.write();
            let state = states.entry(peer).or_default();
            if state.route == Some(Route::Lan) {
                state.status = super::connection::ConnectionStatus::Unreachable;
                state.retry_at_ms = 0;
            }
        }
        signal
            .try_send(super::signaling::SignalCommand::Connect(peer))
            .is_ok()
    }
    pub async fn stop(&self) {
        self.stop_inner(true).await;
    }
    pub async fn reset_account_work(
        self: &Arc<Self>,
        expected: Option<super::reset::Generation>,
    ) -> Result<super::reset::Generation> {
        let _lifecycle = self.host.lifecycle.lock().await;
        if super::reset::generation(&self.host)? != expected
            || self.host.work_reset.record.lock().is_some()
        {
            return Err(StageError::Conflict(
                "Account work changed; refresh before resetting",
            ));
        }
        let record = super::reset::issue(&self.host)?;
        super::reset::offer(&self.host, self.host.local, record.clone())?;
        self.apply_account_reset(&record).await?;
        super::reset::completed(&self.host, &record);
        super::reset::verify(&self.host, &record)
    }
    /// Caller holds the account lifecycle lock. Transport is retained, but all
    /// existing business channels and execution tasks are retired before deletion.
    async fn apply_account_reset(self: &Arc<Self>, record: &super::reset::Record) -> Result<()> {
        let next = super::reset::verify(&self.host, record)?;
        let current = super::reset::generation(&self.host)?;
        if current.as_ref().is_some_and(|old| old > &next)
            || current.as_ref() == Some(&next) && !super::reset::blocked(&self.host)
        {
            return Ok(());
        }
        self.host.work_reset.active.store(true, Ordering::Release);
        let peers = self.handles.read().values().cloned().collect::<Vec<_>>();
        for peer in &peers {
            peer.disconnect();
        }
        tokio::time::timeout(std::time::Duration::from_secs(6), async {
            for peer in peers {
                peer.closed().await;
            }
        })
        .await
        .map_err(|_| StageError::Unavailable("Old device operations have not stopped"))?;
        let core = self.core.write().take();
        if let Some(core) = core {
            core.stop().await;
        }
        let task = self.core_task.lock().take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        self.host.core.write().take();
        if current.as_ref() != Some(&next) {
            let replaced = self.host.system.reset_account_work(record).await;
            super::reset::initialize(&self.host)?;
            replaced.map_err(|_| StageError::Unavailable("Cannot reset account work"))?;
        } else {
            super::reset::cleanup(&self.host.system)?;
        }
        let own = self.host.enrollment()?;
        let member = super::identity::verify_enrollment(
            &own.root,
            &own.chain,
            chrono::Utc::now().timestamp_millis(),
            &super::revocation::removed(self.host.system.store())?,
        )?;
        let core = member
            .can_enroll
            .then(|| super::shared_runtime::Controller::new(self))
            .transpose()?;
        if let Some(core) = &core {
            *self.host.core.write() = Some(Arc::downgrade(core));
            self.host.system.install_shared_core(core);
            *self.core.write() = Some(core.clone());
        }
        self.host.work_reset.active.store(false, Ordering::Release);
        if let Some(core) = core
            && self.running.load(Ordering::Acquire)
        {
            *self.core_task.lock() = Some(tokio::spawn(core.run()));
        }
        self.host.system.store().changed().notify_waiters();
        Ok(())
    }
    #[cfg(test)]
    pub(crate) async fn stop_abruptly(&self) {
        self.stop_inner(false).await;
    }
    #[cfg(test)]
    pub(crate) fn disconnect_peers(&self) {
        for handle in self.handles.read().values() {
            handle.disconnect();
        }
    }
    async fn stop_inner(&self, graceful: bool) {
        if let Some(network) = self.account_network.lock().await.take() {
            network.stop().await;
        }
        let core = self.core.read().clone();
        if graceful
            && let Some(core) = &core
            && let Err(error) =
                tokio::time::timeout(std::time::Duration::from_secs(20), core.prepare_exit()).await
        {
            tracing::debug!(reason=?error,"Device conversation handoff timed out; recovery records retained");
        }
        let coordinator = self.coordination.read().clone();
        if graceful
            && let Some(coordinator) = &coordinator
            && let Err(error) = coordinator.leave().await
        {
            tracing::debug!(reason=%error,"Device authority handoff deferred; local recovery remains available");
        }
        self.core.write().take();
        if let Some(core) = core {
            core.stop().await;
        }
        let core_task = self.core_task.lock().take();
        if let Some(task) = core_task {
            task.abort();
            let _ = task.await;
        }
        self.coordination.write().take();
        if let Some(coordinator) = coordinator {
            coordinator.stop().await;
        }
        self.host.coordination.write().take();
        self.stopped.store(true, Ordering::Release);
        self.running.store(false, Ordering::Release);
        self.wake.notify_waiters();
        self.signal_wake.notify_waiters();
        self.signals.write().take();
        let public = self.public_connection.lock().take();
        if let Some(public) = public {
            public.abort();
            let _ = public.await;
        }
        let tasks = std::mem::take(&mut *self.tasks.lock());
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
        self.handles.write().clear();
        self.attempts.lock().clear();
        // Online is a live-channel fact. Durable generations stay in the ledger.
        self.states.write().clear();
        *self.address.write() = None;
        self.listener.write().take();
    }
    pub async fn reconfigure(self: &Arc<Self>, settings: super::host::Settings) -> Result<()> {
        let _guard = self.host.lifecycle.lock().await;
        self.reconfigure_prepared(settings).await
    }
    pub async fn patch_settings(
        self: &Arc<Self>,
        label: Option<String>,
        signaling_origin: Option<Option<String>>,
        ssh: Option<std::collections::BTreeMap<StageId, SshTarget>>,
    ) -> Result<super::host::Settings> {
        let _guard = self.host.lifecycle.lock().await;
        let mut settings = self.host.settings();
        if let Some(label) = label {
            settings.label = label;
        }
        if let Some(origin) = signaling_origin {
            settings.signaling_origin = origin;
        }
        if let Some(ssh) = ssh {
            settings.ssh = ssh;
        }
        self.reconfigure_prepared(settings).await?;
        Ok(self.host.settings())
    }
    async fn reconfigure_prepared(self: &Arc<Self>, settings: super::host::Settings) -> Result<()> {
        settings.validate()?;
        let old = self.host.settings();
        let base_unchanged = old.listen == settings.listen && old.interfaces == settings.interfaces;
        if base_unchanged {
            self.host.save_settings(settings)?;
            for peer in self.host.settings().ssh.keys() {
                self.wanted.write().insert(*peer);
            }
            self.ensure_public_connection();
            self.signal_changed();
            return Ok(());
        }
        let prepared = if self.host.account_id().is_some() {
            if settings.listen == old.listen {
                self.listener.read().clone()
            } else {
                Some(Arc::new(
                    tokio::net::TcpListener::bind(&settings.listen).await?,
                ))
            }
        } else {
            None
        };
        // Persist the complete candidate before interrupting a working transport.
        // An OS/database permission failure leaves the old service untouched.
        self.host.save_settings(settings)?;
        self.stop().await;
        *self.listener.write() = prepared;
        if let Err(error) = self.start().await {
            self.stop().await;
            self.host.save_settings(old)?;
            let _ = self.start().await;
            return Err(error);
        }
        Ok(())
    }
    fn ensure_public_connection(self: &Arc<Self>) {
        if self.host.settings().signaling_origin.is_none() {
            return;
        }
        let mut task = self.public_connection.lock();
        if task.as_ref().is_none_or(JoinHandle::is_finished) {
            let runtime = self.clone();
            *task = Some(tokio::spawn(async move {
                super::signaling::run(runtime).await;
            }));
        }
    }
}
pub(crate) fn save_directory(host: &Host, peer: StageId, actors: &[RemoteActor]) -> Result<()> {
    host.system.store().stage_transaction(|tx|{
        super::ledger::peer(tx,peer,chrono::Utc::now().timestamp_millis())?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_directory(stage TEXT NOT NULL,actor TEXT NOT NULL,record TEXT NOT NULL,PRIMARY KEY(stage,actor));")?;
        for actor in actors {
            if actor.address.stage_id!=peer{return Err(StageError::Forbidden("Directory host mismatch"));}
            tx.execute("INSERT INTO stage_directory VALUES(?1,?2,?3) ON CONFLICT(stage,actor) DO UPDATE SET record=excluded.record",params![peer.to_string(),actor.address.actor_id,serde_json::to_string(actor).map_err(|_|StageError::Invalid("Cannot encode Actor directory"))?])?;
        }Ok(())
    })
}
pub fn directory(host: &Host, peer: StageId) -> Result<Vec<RemoteActor>> {
    StageLedger::attach(host.system.store(), host.local)
        .peer(peer, chrono::Utc::now().timestamp_millis())?;
    host.system.store().stage_transaction(|tx|{
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='stage_directory')",[],|r|r.get(0))?;
        if !exists{return Ok(vec![]);}
        let rows=tx.prepare("SELECT record FROM stage_directory WHERE stage=?1 ORDER BY actor")?.query_map([peer.to_string()],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        rows.into_iter().map(|value|serde_json::from_str(&value).map_err(|_|StageError::Invalid("Invalid Actor directory cache"))).collect()
    })
}

#[cfg(test)]
mod fallback_tests {
    use super::*;
    async fn hosts() -> (tempfile::TempDir, Arc<Runtime>, Arc<Runtime>) {
        let directory = tempfile::tempdir().unwrap();
        let mut hosts = vec![];
        for name in ["a", "b"] {
            let system = crate::actors::ActorSystem::open_account_gated(
                directory.path().join(name).join("actors"),
                directory.path().join(name).join("workspace"),
                crate::config::Config::default(),
            )
            .await
            .unwrap();
            hosts.push(Host::load(system).unwrap());
        }
        hosts[0].initialize(true).unwrap();
        let request = hosts[1].initialize(false).unwrap();
        let enrollment = hosts[0].enroll_authenticated_device(&request).unwrap();
        hosts[1]
            .join(enrollment, &hosts[0].account_id().unwrap())
            .unwrap();
        for host in &hosts {
            let mut settings = host.settings();
            settings.listen = "127.0.0.1:0".into();
            host.save_settings(settings).unwrap();
        }
        (
            directory,
            Runtime::new(hosts[0].clone()),
            Runtime::new(hosts[1].clone()),
        )
    }
    #[tokio::test]
    async fn failed_initial_dial_retries_without_actor_or_discovery_wakes() {
        let (_directory, a, b) = hosts().await;
        let endpoint = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = endpoint.local_addr().unwrap();
        drop(endpoint);
        let mut settings = b.host.settings();
        settings.listen = address.to_string();
        b.host.save_settings(settings).unwrap();
        a.account_candidate(b.host.local, address);
        let supervisor = tokio::spawn(a.clone().supervise());
        tokio::time::timeout(std::time::Duration::from_secs(4), async {
            while a.state(b.host.local).retries == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(a.last_error(b.host.local).is_some());
        b.start().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_millis(2800), async {
            while !a.connected(b.host.local) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dial failure did not schedule an independent retry");
        supervisor.abort();
        let _ = supervisor.await;
        a.stop_abruptly().await;
        b.stop_abruptly().await;
        a.host.system.shutdown().await;
        b.host.system.shutdown().await;
    }
    #[tokio::test]
    async fn a_working_address_connects_while_other_addresses_stall_during_tls() {
        let (_directory, a, b) = hosts().await;
        b.start().await.unwrap();
        let mut listeners = vec![];
        let mut addresses = vec![];
        for _ in 0..3 {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            addresses.push(listener.local_addr().unwrap());
            listeners.push(listener);
        }
        addresses.push(b.address.read().unwrap());
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            a.dial(b.host.local, addresses, None),
        )
        .await
        .expect("a stalled address blocked the working route")
        .unwrap();
        assert!(a.connected(b.host.local));
        a.stop_abruptly().await;
        b.stop_abruptly().await;
        a.host.system.shutdown().await;
        b.host.system.shutdown().await;
        drop(listeners);
    }
    #[test]
    fn discovery_candidates_expire_and_repeated_announcements_do_not_duplicate_addresses() {
        let address = "127.0.0.1:12345".parse().unwrap();
        let mut candidates = Candidates(vec![(
            address,
            std::time::Instant::now() - std::time::Duration::from_secs(121),
        )]);
        assert!(candidates.live().is_empty());
        candidates.add(address);
        candidates.add(address);
        assert_eq!(candidates.live(), vec![address]);
    }
    #[tokio::test]
    async fn failed_lan_attempt_does_not_block_or_back_off_public_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let system = crate::actors::ActorSystem::open_account_gated(
            directory.path().join("actors"),
            directory.path().join("workspace"),
            crate::config::Config::default(),
        )
        .await
        .unwrap();
        let host = Host::load(system.clone()).unwrap();
        let mut settings = host.settings();
        settings.signaling_origin = Some("wss://example.invalid".into());
        host.save_settings(settings).unwrap();
        let runtime = Runtime::new(host);
        let peer = StageId::new();
        runtime
            .states
            .write()
            .entry(peer)
            .or_default()
            .attempt(Route::Lan, chrono::Utc::now().timestamp_millis(), false)
            .unwrap();
        let (sender, mut commands) = tokio::sync::mpsc::channel(1);
        runtime.install_signal(sender);
        assert!(runtime.request_public_fallback(peer));
        assert!(
            matches!(commands.recv().await, Some(super::super::signaling::SignalCommand::Connect(id)) if id == peer)
        );
        runtime.begin_p2p(peer).unwrap();
        let state = runtime.state(peer);
        assert_eq!(
            state.status,
            super::super::connection::ConnectionStatus::Connecting
        );
        assert_eq!(state.route, Some(Route::P2p));
        assert_eq!(state.retries, 0);
        assert_eq!(state.retry_at_ms, 0);
        // Signal loss marks a pending public peer retryable; it cannot remain connecting.
        runtime.want(peer);
        runtime.signaling_failed(&StageError::Unavailable("Signaling heartbeat timed out"));
        assert_eq!(
            runtime.state(peer).status,
            super::super::connection::ConnectionStatus::Unreachable
        );
        assert_eq!(
            runtime.connection_error_code(peer),
            Some("device_signaling_unavailable")
        );
        system.shutdown().await;
    }
}
