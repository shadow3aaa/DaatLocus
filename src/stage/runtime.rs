//! Native Stage supervision and account-scoped remote observations.
use super::{Result, StageError, StageId, host::Host, ledger::StageLedger, protocol::RemoteActor};
use super::{
    channel::{PeerChannel, SendScope},
    connection::{LinkState, Route},
    peer::{PeerHandle, PeerLink},
    transport::tls,
};
use parking_lot::{Mutex, RwLock};
use rusqlite::params;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::{sync::Notify, task::JoinHandle};
#[cfg(not(target_os = "android"))]
fn network_addresses(
    networks: &sysinfo::Networks,
) -> std::collections::BTreeSet<(String, sysinfo::IpNetwork)> {
    networks
        .iter()
        .flat_map(|(name, data)| {
            data.ip_networks()
                .iter()
                .filter(|network| !network.addr.is_loopback() && !network.addr.is_unspecified())
                .map(move |network| (name.clone(), *network))
        })
        .collect()
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
    wanted: RwLock<HashSet<StageId>>,
    dialing: Mutex<HashMap<StageId, Arc<tokio::sync::Mutex<()>>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    running: AtomicBool,
    stopped: AtomicBool,
    network_epoch: AtomicU64,
    wake: Notify,
    relay_wake: Notify,
    pub(crate) relay_tx: RwLock<Option<tokio::sync::mpsc::Sender<super::relay::Command>>>,
    public_connection: Mutex<Option<JoinHandle<()>>>,
}
impl Runtime {
    pub(crate) fn network_generation(&self) -> u64 {
        self.network_epoch.load(Ordering::Acquire)
    }
    /// OS interface changes and native resume are hints, never trust grants.
    pub fn connectivity_changed(self: &Arc<Self>) {
        self.network_epoch.fetch_add(1, Ordering::AcqRel);
        for state in self.states.write().values_mut() {
            if state.status != super::connection::ConnectionStatus::Online {
                state.status = super::connection::ConnectionStatus::Unreachable;
                state.retry_at_ms = 0;
            }
        }
        let peers = self.handles.read().keys().copied().collect::<Vec<_>>();
        for peer in peers {
            self.probe(peer);
        }
        self.relay_changed();
        self.wake.notify_waiters();
        tracing::debug!("Network changed; refreshing authenticated device paths");
    }
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
    pub(crate) fn clear_relay(&self) {
        self.relay_tx.write().take();
    }
    pub(crate) fn install_relay(&self, tx: tokio::sync::mpsc::Sender<super::relay::Command>) {
        *self.relay_tx.write() = Some(tx);
        for peer in self.wanted.read().iter().copied() {
            self.fresh_candidate(peer);
        }
        self.wake.notify_waiters();
    }
    pub(crate) async fn relay_notification(&self) {
        self.relay_wake.notified().await;
    }
    pub(crate) fn relay_changed(&self) {
        self.relay_wake.notify_one();
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
            wanted: RwLock::new(HashSet::new()),
            dialing: Mutex::new(HashMap::new()),
            tasks: Mutex::new(vec![]),
            running: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            network_epoch: AtomicU64::new(0),
            wake: Notify::new(),
            relay_wake: Notify::new(),
            relay_tx: RwLock::new(None),
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
            if error.contains("Relay") {
                "device_relay_unavailable"
            } else if error.contains("timed out") || error.contains("expired") {
                "device_connection_timeout"
            } else if error.contains("Relay peer is offline") {
                "device_peer_offline"
            } else {
                "device_connection_failed"
            }
        })
    }
    pub(crate) fn relay_failed(&self, error: &StageError) {
        for peer in self.wanted.read().iter().copied().collect::<Vec<_>>() {
            self.peer_failed(peer, error);
        }
    }
    pub(crate) fn begin_relay(&self, peer: StageId) -> Result<()> {
        self.states
            .write()
            .entry(peer)
            .or_default()
            .attempt(Route::Relay, chrono::Utc::now().timestamp_millis())
    }
    pub(crate) fn peer_failed(&self, peer: StageId, error: &StageError) {
        if self.connected(peer) {
            return;
        }
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
        if self.host.settings().mode == super::host::Connectivity::Relay {
            self.account_network().await?;
        }
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
        if settings.mode == super::host::Connectivity::Local {
            self.host
                .system
                .install_local_core()
                .map_err(|_| StageError::Unavailable("Cannot initialize local Core"))?;
            let account = self
                .host
                .account_id()
                .ok_or(StageError::Unavailable("Account unavailable"))?;
            super::ledger::initialize_account_ownership(self.host.system.store(), &account)?;
            self.host
                .system
                .ensure_core_actor()
                .map_err(|_| StageError::Unavailable("Cannot initialize local Core"))?;
            let own = self.clone();
            self.spawn(async move {
                own.supervise().await;
            });
            return Ok(());
        }
        self.host.system.disable_local_core();

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
        let own = self.clone();
        self.tasks.lock().push(tokio::spawn(async move {
            own.supervise().await;
        }));
        #[cfg(not(target_os = "android"))]
        {
            let own = self.clone();
            self.spawn(async move {
                // Native resume provides an immediate wake on Android. This small
                // OS-interface watcher also covers headless and desktop services.
                let mut networks = sysinfo::Networks::new_with_refreshed_list();
                let mut previous = network_addresses(&networks);
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                while !own.is_stopped() {
                    interval.tick().await;
                    let refreshed = tokio::task::spawn_blocking(move || {
                        networks.refresh(true);
                        networks
                    })
                    .await;
                    let Ok(refreshed) = refreshed else {
                        break;
                    };
                    networks = refreshed;
                    let current = network_addresses(&networks);
                    if current != previous {
                        previous = current;
                        own.connectivity_changed();
                    }
                }
            });
        }
        if settings.relay_origin.is_some() {
            self.ensure_public_connection();
        }
        Ok(())
    }
    pub fn core_status(&self) -> Option<serde_json::Value> {
        if self.host.settings().mode == super::host::Connectivity::Local
            && self.host.account_id().is_some()
            && self.host.system.local_core()
        {
            let model_ready = self.host.system.config().validate().is_ok();
            Some(
                serde_json::json!({"worker_stage_id":self.host.local,"epoch":0,"synchronized":true,"ready":self.host.system.core_permitted(),"model_ready":model_ready,"state":if model_ready{"local"}else{"model_required"},"error_code":null,"config_revision":null}),
            )
        } else {
            self.core.read().as_ref().map(|core| core.status())
        }
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
                    self.host.settings().relay_origin.is_some(),
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
    fn fresh_candidate(&self, peer: StageId) {
        if let Some(state) = self.states.write().get_mut(&peer)
            && state.status == super::connection::ConnectionStatus::Unreachable
        {
            state.retry_at_ms = 0;
        }
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
    pub(crate) async fn establish_relay(
        self: &Arc<Self>,
        peer: StageId,
        socket: tokio::io::DuplexStream,
        outgoing: bool,
        network_epoch: u64,
    ) -> Result<()> {
        let ledger = StageLedger::attach(self.host.system.store(), self.host.local);
        let record = ledger.peer(peer, chrono::Utc::now().timestamp_millis())?;
        let identity = self.host.identity()?;
        let lock = self.peer_lock(peer);
        let _guard = lock.lock().await;
        if self.connected(peer) {
            return Err(StageError::Conflict("Device channel is already connected"));
        }
        if self.network_generation() != network_epoch || self.is_stopped() {
            return Err(StageError::Unavailable("Relay connection changed"));
        }
        if outgoing {
            let connector = tls::client(&identity, &record.device)?;
            let mut stream = tls::connect(&connector, peer, socket).await?;
            self.authenticate_stream(peer, &mut stream, &record.device, &identity, &ledger, None)
                .await?;
            if self.network_generation() != network_epoch || self.is_stopped() {
                return Err(StageError::Unavailable("Relay connection changed"));
            }
            let verified = tls::identify_server(&stream, &record.device)?;
            let generation = ledger
                .peer(peer, chrono::Utc::now().timestamp_millis())?
                .generation;
            self.attach_stream(verified, stream, generation, Route::Relay);
        } else {
            let acceptor = tls::server(&identity, std::slice::from_ref(&record.device))?;
            let mut stream = tls::accept(&acceptor, socket).await?;
            let verified = tls::identify_client(&stream, std::slice::from_ref(&record.device))?;
            let binding = hex::encode(
                stream
                    .get_ref()
                    .1
                    .export_keying_material([0; 32], b"EXPORTER-DaatLocus-Stage-v1", None)
                    .map_err(|_| StageError::Forbidden("TLS exporter unavailable"))?,
            );
            let (generation, _) = super::connection::handshake(
                &mut stream,
                &identity,
                &ledger,
                &verified,
                &binding,
                chrono::Utc::now().timestamp_millis(),
                None,
            )
            .await?;
            if self.network_generation() != network_epoch || self.is_stopped() {
                return Err(StageError::Unavailable("Relay connection changed"));
            }
            let format =
                super::transport::wire::Format::negotiated(stream.get_ref().1.alpn_protocol());
            let owner = self.clone();
            let check = verified.clone();
            self.attach(
                PeerChannel::stream_format(verified, stream, format, move |frame, scope| {
                    owner.authorize_send(&check, frame, scope)
                }),
                generation,
                Route::Relay,
            );
        }
        Ok(())
    }
    pub(crate) fn peer_lock(&self, peer: StageId) -> Arc<tokio::sync::Mutex<()>> {
        self.dialing
            .lock()
            .entry(peer)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
    async fn authenticate_stream<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
        &self,
        _peer: StageId,
        stream: &mut tokio_rustls::client::TlsStream<S>,
        profile: &super::identity::DeviceProfile,
        identity: &super::identity::DeviceIdentity,
        ledger: &StageLedger<'_>,
        preface: Option<super::identity::SignedProof>,
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
            preface,
        )
        .await?;
        Ok(())
    }
    fn attach_stream<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static>(
        self: &Arc<Self>,
        verified: super::transport::tls::VerifiedPeer,
        stream: tokio_rustls::client::TlsStream<S>,
        generation: u64,
        route: Route,
    ) {
        let owner = self.clone();
        let check = verified.clone();
        let format = super::transport::wire::Format::negotiated(stream.get_ref().1.alpn_protocol());
        self.attach(
            PeerChannel::stream_format(verified, stream, format, move |frame, scope| {
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
                tracing::debug!(stage=%own.host.local,peer=%peer,generation,error=?error,"Native peer channel stopped");
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
                if let Some(relay) = self.relay_tx.read().clone() {
                    let _ = relay.try_send(super::relay::Command::Connect(peer));
                }
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
        let local = self.host.settings().mode == super::host::Connectivity::Local;
        if local {
            self.host
                .system
                .install_local_core()
                .map_err(|_| StageError::Unavailable("Cannot restart local Core"))?;
            let account = self
                .host
                .account_id()
                .ok_or(StageError::Unavailable("Account unavailable"))?;
            super::ledger::initialize_account_ownership(self.host.system.store(), &account)?;
        }
        let core = (member.can_enroll && !local)
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
        self.relay_wake.notify_waiters();
        self.relay_tx.write().take();
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
        // Online is a live-channel fact. Durable generations stay in the ledger.
        self.states.write().clear();
        self.host.system.disable_local_core();
    }
    pub async fn reconfigure(self: &Arc<Self>, settings: super::host::Settings) -> Result<()> {
        let _guard = self.host.lifecycle.lock().await;
        self.reconfigure_prepared(settings).await
    }
    pub async fn patch_settings(
        self: &Arc<Self>,
        label: Option<String>,
        relay_origin: Option<Option<String>>,
        mode: Option<super::host::Connectivity>,
    ) -> Result<super::host::Settings> {
        let _guard = self.host.lifecycle.lock().await;
        let mut settings = self.host.settings();
        if let Some(label) = label {
            settings.label = label;
        }
        if let Some(origin) = relay_origin {
            settings.relay_origin = origin;
        }
        if let Some(mode) = mode {
            settings.mode = mode;
        }
        self.reconfigure_prepared(settings).await?;
        Ok(self.host.settings())
    }
    async fn reconfigure_prepared(self: &Arc<Self>, settings: super::host::Settings) -> Result<()> {
        settings.validate()?;
        let old = self.host.settings();
        if old.mode == settings.mode && old.relay_origin == settings.relay_origin {
            self.host.save_settings(settings)?;
            return Ok(());
        }
        if old.mode == super::host::Connectivity::Relay
            && settings.mode == super::host::Connectivity::Local
            && self.host.account_id().is_some()
        {
            let peers = StageLedger::attach(self.host.system.store(), self.host.local)
                .peers(chrono::Utc::now().timestamp_millis())?;
            if !peers.is_empty() {
                return Err(StageError::Conflict(
                    "A multi-device account cannot become purely local",
                ));
            }
        }
        self.host.prepare_settings(&settings)?;
        self.stop().await;
        self.host
            .system
            .quiesce_core()
            .await
            .map_err(|_| StageError::Unavailable("Cannot stop Core during connection change"))?;
        self.host
            .system
            .store()
            .interrupt_actor(
                super::shared::ACTOR_ID,
                "Connection settings changed; uncertain effects were not replayed",
            )
            .map_err(|_| StageError::Unavailable("Cannot preserve interrupted Core input"))?;
        self.host.publish_settings(settings);
        if let Err(error) = self.start().await {
            self.stop().await;
            self.host.save_settings(old)?;
            let _ = self.start().await;
            return Err(error);
        }
        Ok(())
    }
    fn ensure_public_connection(self: &Arc<Self>) {
        if self.host.settings().relay_origin.is_none() {
            return;
        }
        let mut task = self.public_connection.lock();
        if task.as_ref().is_none_or(JoinHandle::is_finished) {
            let runtime = self.clone();
            *task = Some(tokio::spawn(async move {
                super::relay::run(runtime).await;
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
