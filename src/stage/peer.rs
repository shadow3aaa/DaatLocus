//! One live peer channel multiplexes control RPCs, durable delivery and shared
//! Actor subscriptions. Fetching a blob never blocks cancellation processing.
use super::{
    ActorAddress, Result, StageError,
    blob::ContentStore,
    channel::{PeerChannel, SendScope},
    delivery::Deliveries,
    host::Host,
    ledger::StageLedger,
    protocol::{BlobAccess, BlobRef, Body, Cursor, Envelope, SubscriptionLevel},
    receiver::StageReceiver,
    scheduler::Class,
    sync::{Replica, Subscriptions},
};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod backpressure_tests {
    use super::*;
    use crate::{
        actors::ActorSystem,
        config::{Config, ModelConfig, ProviderConfig},
        stage::{
            identity::DeviceIdentity, runtime::Runtime, shared_runtime::Controller, transport,
        },
    };

    #[tokio::test]
    async fn existing_history_survives_a_slow_writer_and_still_receives_device_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = Config {
            main_model: "fixture".into(),
            ..Default::default()
        };
        config.providers.insert(
            "fixture".into(),
            ProviderConfig::OpenaiCompatible {
                base_url: "http://model.invalid/v1".into(),
                api_key: "unused-fixture-key".into(),
            },
        );
        config.models.insert(
            "fixture".into(),
            ModelConfig {
                provider: "fixture".into(),
                model_id: "fixture".into(),
                ..Default::default()
            },
        );
        let system = ActorSystem::open_account_gated(
            directory.path().join("actors"),
            directory.path().join("workspace"),
            config,
        )
        .await
        .unwrap();
        system.seed_test_actor("root");
        let host = Host::load(system.clone()).unwrap();
        host.initialize(true).unwrap();
        let remote = DeviceIdentity::generate().unwrap();
        let enrollment = host
            .enroll_authenticated_device(&remote.join_request(now()).unwrap())
            .unwrap();
        let runtime = Runtime::new(host.clone());
        let _core = Controller::new(&runtime).unwrap();
        let mut expected = std::collections::HashSet::new();
        for i in 0..128 {
            expected.insert(
                system
                    .send_user("root", format!("Existing message {i}"), vec![], None)
                    .unwrap()
                    .id,
            );
        }
        let nonce = Uuid::new_v4();
        let ledger = StageLedger::attach(system.store(), host.local);
        ledger
            .import_peer(
                enrollment,
                vec![crate::stage::ledger::ActorGrant {
                    actor_id: "root".into(),
                    read: true,
                    converse: false,
                    delegate: false,
                    read_only: true,
                }],
                now(),
            )
            .unwrap();
        ledger
            .prepare_connection(remote.profile().stage_id, 1, nonce, now())
            .unwrap();
        ledger
            .commit_generation(remote.profile().stage_id, 0, 1, nonce, now())
            .unwrap();
        let (local_stream, remote_stream) = tokio::io::duplex(64);
        let channel = PeerChannel::stream(
            transport::tls::VerifiedPeer::signed_dtls(&remote.profile()),
            local_stream,
            |_, _| Ok(()),
        );
        let (link, _) = PeerLink::new(host.clone(), channel, 1);
        let task = tokio::spawn(link.run());
        // No receiver reads: a reconnect must not discard existing history
        // merely because its initial output exceeds the 32-frame channel.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!task.is_finished(), "history burst closed the channel");
        let (mut reader, mut writer) = tokio::io::split(remote_stream);
        let name = Envelope {
            version: 1,
            source: remote.profile().stage_id,
            target: host.local,
            generation: 1,
            message_id: Uuid::new_v4(),
            invocation_id: None,
            in_reply_to: None,
            created_at_ms: now(),
            expires_at_ms: now() + 60000,
            body: Body::DeviceName {
                label: "Slow test phone".into(),
            },
        };
        let reset = Envelope {
            body: Body::AccountWorkReset { record: None },
            ..name.clone()
        };
        transport::write_frame(&mut writer, &reset).await.unwrap();
        transport::write_frame(&mut writer, &name).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while host
                .device_label(remote.profile().stage_id)
                .unwrap()
                .as_deref()
                != Some("Slow test phone")
            {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let mut received = std::collections::HashSet::new();
        let mut last_sequence = 0;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while received.len() < expected.len() {
                let (frame, _) = transport::read_frame(&mut reader).await.unwrap();
                if let Body::Notice { record } = frame.body {
                    assert!(
                        record.seq > last_sequence,
                        "notice cursor duplicated or reordered history"
                    );
                    last_sequence = record.seq;
                    if let crate::actors::ActorEvent::MessageAdded { message_id } = record.event
                        && expected.contains(&message_id)
                    {
                        received.insert(message_id);
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(received, expected);
        assert!(!task.is_finished());
        task.abort();
        let _ = task.await;
        drop(writer);
        system.shutdown().await;
    }
}
fn error_code(error: &StageError) -> &'static str {
    match error {
        StageError::Forbidden(_) => "forbidden",
        StageError::Conflict(_) => "conflict",
        StageError::Invalid(_) => "invalid",
        StageError::Capacity(_) => "capacity",
        _ => "unavailable",
    }
}
pub enum Command {
    FetchExport {
        export: Uuid,
        reference: BlobRef,
        response: oneshot::Sender<Result<Vec<u8>>>,
    },
    Fetch {
        actor: ActorAddress,
        reference: BlobRef,
        response: oneshot::Sender<Result<Vec<u8>>>,
    },
    RefreshMembers,
    Rpc {
        body: Body,
        response: oneshot::Sender<Result<Body>>,
        timeout: std::time::Duration,
    },
    Watch {
        actor: String,
        view: Uuid,
        level: SubscriptionLevel,
    },
    Unwatch {
        actor: String,
        view: Uuid,
    },
    Flush,
}
#[derive(Clone)]
pub struct PeerHandle {
    commands: mpsc::Sender<Command>,
    metrics: Arc<parking_lot::Mutex<super::scheduler::Traffic>>,
    stop: tokio::sync::watch::Sender<bool>,
}
impl PeerHandle {
    pub(crate) fn disconnect(&self) {
        self.stop.send_replace(true);
    }
    pub(crate) async fn closed(&self) {
        self.commands.closed().await;
    }
    pub(crate) fn is_connected(&self) -> bool {
        !*self.stop.borrow() && !self.commands.is_closed()
    }
    pub fn traffic(&self) -> super::scheduler::Traffic {
        self.metrics.lock().clone()
    }
    pub async fn exported_content(&self, export: Uuid, reference: BlobRef) -> Result<Vec<u8>> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .try_send(Command::FetchExport {
                export,
                reference,
                response: tx,
            })
            .map_err(|_| StageError::Capacity("Content queue is full"))?;
        tokio::time::timeout(std::time::Duration::from_secs(30), rx)
            .await
            .map_err(|_| StageError::Unavailable("Content download timed out"))?
            .map_err(|_| StageError::Unavailable("Content connection closed"))?
    }
    pub async fn content(&self, actor: ActorAddress, reference: BlobRef) -> Result<Vec<u8>> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .try_send(Command::Fetch {
                actor,
                reference,
                response: tx,
            })
            .map_err(|_| StageError::Capacity("Content queue is full"))?;
        tokio::time::timeout(std::time::Duration::from_secs(30), rx)
            .await
            .map_err(|_| StageError::Unavailable("Content download timed out"))?
            .map_err(|_| StageError::Unavailable("Content connection closed"))?
    }
    pub fn refresh_members(&self) {
        let _ = self.commands.try_send(Command::RefreshMembers);
    }
    pub async fn rpc(&self, body: Body) -> Result<Body> {
        self.rpc_with_timeout(body, std::time::Duration::from_secs(20))
            .await
    }
    pub(crate) async fn rpc_with_timeout(
        &self,
        body: Body,
        timeout: std::time::Duration,
    ) -> Result<Body> {
        let timeout = timeout.min(std::time::Duration::from_secs(1200));
        let (tx, rx) = oneshot::channel();
        self.commands
            .try_send(Command::Rpc {
                body,
                response: tx,
                timeout,
            })
            .map_err(|_| StageError::Capacity("Peer control queue is unavailable"))?;
        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| StageError::Unavailable("Peer request timed out"))?
            .map_err(|_| StageError::Unavailable("Peer connection closed"))?
    }
    pub fn watch(&self, actor: &str, view: Uuid, level: SubscriptionLevel) -> Result<()> {
        self.commands
            .try_send(Command::Watch {
                actor: actor.into(),
                view,
                level,
            })
            .map_err(|_| StageError::Capacity("Peer subscription queue is full"))
    }
    pub fn unwatch(&self, actor: &str, view: Uuid) {
        let _ = self.commands.try_send(Command::Unwatch {
            actor: actor.into(),
            view,
        });
    }
    pub fn flush(&self) {
        let _ = self.commands.try_send(Command::Flush);
    }
}
enum Pending {
    Rpc(oneshot::Sender<Result<Body>>),
    Manifest {
        hash: String,
    },
    Chunk {
        hash: String,
        chunk: u32,
        remaining: VecDeque<u32>,
    },
    Directory,
    AccountCredentials,
    AccountConfiguration,
    Members,
    Revocations,
    Snapshot {
        actor: ActorAddress,
    },
}
enum Waiter {
    Input(Box<Envelope>),
    Replica(ActorAddress),
    Fetch(oneshot::Sender<Result<Vec<u8>>>),
}
struct Download {
    deadline: i64,
    reference: BlobRef,
    access: BlobAccess,
    waiters: Vec<Waiter>,
}
struct Subscription {
    request: Envelope,
    cursor: Option<Cursor>,
}
pub struct PeerLink {
    work_generation: Option<super::reset::Generation>,
    deferred: VecDeque<(Envelope, Class, SendScope, usize)>,
    deferred_bytes: usize,
    sent_device_name: Option<String>,
    sent_account_binding: Option<String>,
    sent_configuration_binding: Option<String>,
    sent_recovery_status: Option<super::recovery::Status>,
    notice_cursor: i64,
    sent_revocations: std::collections::HashSet<super::StageId>,
    host: Arc<Host>,
    handle: PeerHandle,
    core_tasks: Vec<tokio::task::JoinHandle<()>>,
    channel: PeerChannel,
    generation: u64,
    commands: mpsc::Receiver<Command>,
    stop: tokio::sync::watch::Receiver<bool>,
    pending: HashMap<Uuid, (i64, Pending)>,
    publishing: HashMap<Uuid, Subscription>,
    views: Subscriptions,
    inflight: HashMap<Uuid, i64>,
    downloads: HashMap<String, Download>,
}
impl Drop for PeerLink {
    fn drop(&mut self) {
        for task in self.core_tasks.drain(..) {
            task.abort();
        }
    }
}
impl PeerLink {
    pub fn new(host: Arc<Host>, channel: PeerChannel, generation: u64) -> (Self, PeerHandle) {
        let (tx, commands) = mpsc::channel(32);
        let (stop_tx, stop) = tokio::sync::watch::channel(false);
        let metrics = channel.metrics.clone();
        (
            Self {
                work_generation: None,
                deferred: VecDeque::new(),
                deferred_bytes: 0,
                sent_device_name: None,
                sent_account_binding: None,
                sent_configuration_binding: None,
                sent_recovery_status: None,
                notice_cursor: 0,
                sent_revocations: std::collections::HashSet::new(),
                host,
                handle: PeerHandle {
                    commands: tx.clone(),
                    metrics: metrics.clone(),
                    stop: stop_tx.clone(),
                },
                core_tasks: vec![],
                channel,
                generation,
                commands,
                stop,
                pending: HashMap::new(),
                publishing: HashMap::new(),
                views: Subscriptions::default(),
                inflight: HashMap::new(),
                downloads: HashMap::new(),
            },
            PeerHandle {
                commands: tx,
                metrics,
                stop: stop_tx,
            },
        )
    }
    fn frame(&self, body: Body) -> Envelope {
        let time = now();
        Envelope {
            version: 1,
            source: self.host.local,
            target: self.channel.peer.stage_id(),
            generation: self.generation,
            message_id: Uuid::new_v4(),
            invocation_id: None,
            in_reply_to: None,
            created_at_ms: time,
            expires_at_ms: time + 60000,
            body,
        }
    }
    fn require_work_generation(&self) -> Result<()> {
        if super::reset::blocked(&self.host)
            || super::reset::generation(&self.host)? != self.work_generation
        {
            return Err(StageError::Conflict(
                "Account work was reset; reconnect required",
            ));
        }
        Ok(())
    }
    async fn negotiate_reset(&mut self) -> Result<()> {
        if super::reset::blocked(&self.host) {
            return Err(StageError::Unavailable("Account work is resetting"));
        }
        self.work_generation = super::reset::generation(&self.host)?;
        let record = super::reset::current(self.host.system.store())?;
        self.send(
            self.frame(Body::AccountWorkReset { record }),
            Class::Control,
            SendScope::Connection,
        )?;
        let incoming = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.channel.incoming.recv(),
        )
        .await
        .map_err(|_| StageError::Unavailable("Account reset negotiation timed out"))?
        .ok_or(StageError::Unavailable("Reset negotiation disconnected"))??;
        let frame = incoming.frame;
        frame.validate(
            self.channel.peer.stage_id(),
            self.host.local,
            self.generation,
            now(),
        )?;
        let Body::AccountWorkReset { record } = frame.body else {
            return Err(StageError::Forbidden(
                "Peer must negotiate account work before business traffic",
            ));
        };
        let remote = record
            .as_ref()
            .map(|record| super::reset::verify(&self.host, record))
            .transpose()?;
        if remote > self.work_generation {
            super::reset::offer(
                &self.host,
                self.channel.peer.stage_id(),
                record.expect("a newer generation has a signed record"),
            )?;
            return Err(StageError::Conflict(
                "Account reset must be applied before reconnecting",
            ));
        }
        if remote != self.work_generation {
            // The older peer must receive our queued signed reset before we
            // tear down this link. Returning immediately can abort the writer
            // and discard the only copy telling it to reset, on every retry.
            // No business traffic is processed while waiting for it to close.
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.channel.closed.changed(),
            )
            .await;
            return Err(StageError::Conflict(
                "Peer must apply account reset before reconnecting",
            ));
        }
        self.require_work_generation()
    }
    fn send(&mut self, mut frame: Envelope, class: Class, scope: SendScope) -> Result<()> {
        frame.generation = self.generation;
        if self.deferred.is_empty() {
            match self
                .channel
                .sender
                .send(frame.clone(), class, scope.clone())
            {
                Ok(()) => return Ok(()),
                Err(StageError::Capacity(_)) => {}
                Err(error) => return Err(error),
            }
        }
        let bytes = frame.encode()?.len() + 4;
        if self.deferred.len() >= 32 || self.deferred_bytes.saturating_add(bytes) > 1024 * 1024 {
            return Err(StageError::Capacity("Peer pending frame budget reached"));
        }
        self.deferred_bytes += bytes;
        self.deferred.push_back((frame, class, scope, bytes));
        Ok(())
    }
    fn drain_deferred(&mut self) -> Result<()> {
        while let Some((frame, class, scope, bytes)) = self.deferred.front() {
            match self
                .channel
                .sender
                .send(frame.clone(), *class, scope.clone())
            {
                Ok(()) => {
                    self.deferred_bytes -= *bytes;
                    self.deferred.pop_front();
                }
                Err(StageError::Capacity(_)) => break,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    fn request(&mut self, body: Body, pending: Pending, class: Class) -> Result<()> {
        if self.pending.len() >= 32 {
            return Err(StageError::Capacity("Peer RPC limit reached"));
        }
        let frame = self.frame(body);
        let id = frame.message_id;
        self.send(frame, class, SendScope::Connection)?;
        self.pending.insert(id, (now() + 20000, pending));
        Ok(())
    }
    pub async fn run(mut self) -> Result<()> {
        // A new link exchanges only signed reset facts until both peers agree
        // on the work generation. No old snapshot, delivery or effect can cross it.
        self.negotiate_reset().await?;
        if self.account_credential_authority().is_ok() {
            self.request(
                Body::AccountCredentialsQuery {
                    known_binding: self
                        .host
                        .accounts
                        .current()
                        .ok()
                        .flatten()
                        .map(|record| record.binding()),
                },
                Pending::AccountCredentials,
                Class::Control,
            )?;
            self.request(
                Body::AccountConfigurationQuery {
                    known_binding: self
                        .host
                        .configurations
                        .current()
                        .ok()
                        .flatten()
                        .as_ref()
                        .map(crate::accounts::configuration::ConfigurationStore::binding),
                },
                Pending::AccountConfiguration,
                Class::Control,
            )?;
        }
        self.request(
            Body::DirectoryQuery {
                after: None,
                limit: 32,
            },
            Pending::Directory,
            Class::Control,
        )?;
        self.request(
            Body::MembersQuery {
                after: None,
                limit: 8,
            },
            Pending::Members,
            Class::Control,
        )?;
        self.request(
            Body::RevocationsQuery {
                after: None,
                limit: 8,
            },
            Pending::Revocations,
            Class::Control,
        )?;
        self.publish()?;
        self.flush()?;
        let account_store = self.host.accounts.clone();
        let configuration_store = self.host.configurations.clone();
        let changes = self.host.system.store().changed();
        let write_ready = self.channel.sender.write_notification();
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(2));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let result = loop {
            if *self.stop.borrow() {
                break Err(StageError::Unavailable("Peer disconnected"));
            }
            let writable = write_ready.notified();
            tokio::pin!(writable);
            writable.as_mut().enable();
            if let Err(error) = self
                .drain_deferred()
                .and_then(|_| self.publish())
                .and_then(|_| self.flush())
            {
                break Err(error);
            }
            let changed = changes.notified();
            let account_changed = account_store.changed().notified();
            tokio::pin!(account_changed);
            account_changed.as_mut().enable();
            let configuration_changed = configuration_store.changed().notified();
            tokio::pin!(configuration_changed);
            configuration_changed.as_mut().enable();
            tokio::pin!(changed);
            changed.as_mut().enable();
            tokio::select! {
                _=self.stop.changed()=>break Err(StageError::Unavailable("Peer disconnected")),
                command=self.commands.recv(), if self.deferred.len() < 8 =>match command{Some(command)=>{if let Err(error)=self.command(command){break Err(error);}},None=>break Ok(())},
                incoming=self.channel.incoming.recv(), if self.deferred.len() < 24 =>match incoming{Some(Ok(incoming))=>{if let Err(error)=self.receive(incoming.frame){break Err(error);}},Some(Err(error))=>break Err(error),None=>break Err(StageError::Unavailable("Peer channel closed"))},
                _=writable=>{},
                _=changed=>{if let Err(error)=self.publish().and_then(|_|self.flush()){break Err(error);}},
                _=account_changed=>{if let Err(error)=self.publish_accounts(){break Err(error);}},
                _=configuration_changed=>{if let Err(error)=self.publish_configuration(){break Err(error);}},
                _=heartbeat.tick()=>{
                    if StageLedger::attach(self.host.system.store(),self.host.local).peer(self.channel.peer.stage_id(),now()).is_err(){break Err(StageError::Forbidden("Peer authorization expired"));}
                    let expired_downloads=self.downloads.iter().filter(|(_,d)|d.deadline<=now()).map(|(hash,_)|hash.clone()).collect::<Vec<_>>();
                    for hash in expired_downloads{if let Some(download)=self.downloads.remove(&hash){for waiter in download.waiters{if let Waiter::Fetch(sender)=waiter{let _=sender.send(Err(StageError::Unavailable("Content transfer timed out")));}}}}
                    let expired=self.pending.iter().filter(|(_, (deadline,_))|*deadline<=now()).map(|(id,_)|*id).collect::<Vec<_>>();
                    for id in expired{if let Some((_,Pending::Rpc(response)))=self.pending.remove(&id){let _=response.send(Err(StageError::Unavailable("Peer request timed out")));}}
                    if let Err(error)=self.publish().and_then(|_|self.send(self.frame(Body::Heartbeat),Class::Control,SendScope::Connection)){break Err(error);}
                },
                _=self.channel.closed.changed()=>break Err(StageError::Unavailable("Peer channel closed")),
            }
        };
        for task in self.core_tasks.drain(..) {
            task.abort();
            let _ = task.await;
        }
        for (_, (_, pending)) in self.pending.drain() {
            if let Pending::Rpc(response) = pending {
                let _ = response.send(Err(StageError::Unavailable("Peer connection closed")));
            }
        }
        self.channel.close().await;
        result
    }
    fn command(&mut self, command: Command) -> Result<()> {
        self.require_work_generation()?;
        match command {
            Command::FetchExport {
                export,
                reference,
                response,
            } => self.download(
                BlobAccess::Invocation { invocation: export },
                reference,
                Waiter::Fetch(response),
            ),
            Command::Fetch {
                actor,
                reference,
                response,
            } => self.download(
                BlobAccess::Actor { actor },
                reference,
                Waiter::Fetch(response),
            ),
            Command::RefreshMembers => self.request(
                Body::MembersQuery {
                    after: None,
                    limit: 8,
                },
                Pending::Members,
                Class::Control,
            ),
            Command::Rpc {
                body,
                response,
                timeout,
            } => {
                if self.pending.len() >= 32 {
                    let _ = response.send(Err(StageError::Capacity("Peer RPC limit reached")));
                    return Ok(());
                }
                let frame = self.frame(body);
                let id = frame.message_id;
                self.send(frame, Class::Control, SendScope::Connection)?;
                self.pending.insert(
                    id,
                    (now() + timeout.as_millis() as i64, Pending::Rpc(response)),
                );
                Ok(())
            }
            Command::Watch { actor, view, level } => {
                let address = ActorAddress {
                    stage_id: self.channel.peer.stage_id(),
                    actor_id: actor,
                };
                let before = self.views.level(&address);
                let added = self.views.attach(address.clone(), view, level)?;
                if !added && before == self.views.level(&address) {
                    return Ok(());
                }
                // Reuse the one upstream request for every local view. A level change
                // resumes from the persisted application cursor or requests a snapshot.
                let cursor = Replica::new(self.host.system.store())
                    .progress(&address, now())
                    .ok()
                    .map(|state| state.contiguous_received);
                let frame = self.frame(Body::Subscribe {
                    actor: address.clone(),
                    cursor,
                    level: self.views.level(&address).unwrap_or(level),
                });
                self.send(frame, Class::Control, SendScope::Connection)
            }
            Command::Unwatch { actor, view } => {
                let address = ActorAddress {
                    stage_id: self.channel.peer.stage_id(),
                    actor_id: actor,
                };
                if self.views.detach(&address, view) {
                    let frame = self.frame(Body::Unsubscribe {
                        actor: Some(address),
                        subscription: Uuid::nil(),
                    });
                    self.send(frame, Class::Control, SendScope::Connection)?;
                }
                Ok(())
            }
            Command::Flush => self.flush(),
        }
    }
    fn flush(&mut self) -> Result<()> {
        if self.deferred.len() >= 8 {
            return Ok(());
        }
        let peer = self.channel.peer.stage_id();
        let time = now();
        self.inflight.retain(|_, deadline| *deadline > time);
        let ledger = StageLedger::attach(self.host.system.store(), self.host.local);
        let mut frames = ledger.pending(peer, time, 32)?;
        frames.extend(
            Deliveries::new(self.host.system.store(), self.host.local).ready(peer, time, 32)?,
        );
        for frame in frames {
            if self.deferred.len() >= 8 {
                break;
            }
            if self.inflight.len() >= 32 {
                break;
            }
            if self.inflight.contains_key(&frame.message_id) {
                continue;
            }
            if frame.expires_at_ms <= time {
                continue;
            }
            let id = frame.message_id;
            let scope = match &frame.body {
                Body::Delivery { actor, .. } => SendScope::Actor(actor.actor_id.clone()),
                _ => SendScope::Connection,
            };
            self.send(frame, Class::Control, scope)?;
            self.inflight.insert(id, time + 60000);
        }
        Ok(())
    }
    fn account_credential_authority(&self) -> Result<String> {
        let own = self.host.enrollment()?;
        let removed = super::revocation::removed(self.host.system.store())?;
        let local = super::identity::verify_enrollment(&own.root, &own.chain, now(), &removed)?;
        let peer = StageLedger::attach(self.host.system.store(), self.host.local)
            .peer(self.channel.peer.stage_id(), now())?;
        let remote =
            super::identity::verify_enrollment(&own.root, &peer.enrollment.chain, now(), &removed)?;
        if !local.can_enroll || !remote.can_enroll || peer.enrollment.root != own.root {
            return Err(StageError::Forbidden(
                "Device cannot access private account credentials",
            ));
        }
        Ok(own.root.group_id)
    }
    fn publish_accounts(&mut self) -> Result<()> {
        if self.deferred.len() >= 8 {
            return Ok(());
        }
        let Ok(group) = self.account_credential_authority() else {
            return Ok(());
        };
        if let Some(record) = self
            .host
            .accounts
            .current()
            .map_err(|_| StageError::Invalid("Cannot read private account credentials"))?
        {
            let binding = record.binding();
            if record.account_id != group {
                return Err(StageError::Forbidden(
                    "Account credential namespace mismatch",
                ));
            }
            if self.sent_account_binding.as_ref() != Some(&binding) {
                self.send(
                    self.frame(Body::AccountCredentials {
                        record: Some(record),
                    }),
                    Class::Control,
                    SendScope::Connection,
                )?;
                self.sent_account_binding = Some(binding);
            }
        }
        Ok(())
    }
    fn publish_configuration(&mut self) -> Result<()> {
        if self.deferred.len() >= 8 {
            return Ok(());
        }
        if self.account_credential_authority().is_err() {
            return Ok(());
        }
        if let Some(record) = self
            .host
            .configurations
            .current()
            .map_err(|_| StageError::Unavailable("Cannot read account configuration"))?
        {
            let binding = crate::accounts::configuration::ConfigurationStore::binding(&record);
            if self.sent_configuration_binding.as_ref() != Some(&binding) {
                self.send(
                    self.frame(Body::AccountConfiguration {
                        record: Some(record),
                    }),
                    Class::Control,
                    SendScope::Connection,
                )?;
                self.sent_configuration_binding = Some(binding);
            }
        }
        Ok(())
    }
    fn install_configuration(
        &mut self,
        record: Option<crate::accounts::configuration::Record>,
    ) -> Result<()> {
        let Some(record) = record else {
            return Ok(());
        };
        self.account_credential_authority()?;
        self.core_tasks.retain(|task| !task.is_finished());
        if self.core_tasks.len() >= 16 {
            return Err(StageError::Capacity("Account import task budget exceeded"));
        }
        let host = self.host.clone();
        self.core_tasks.push(tokio::spawn(async move {
            if host.configurations.import(&host, record).await.is_err() {
                tracing::debug!("Account configuration import rejected");
            }
        }));
        Ok(())
    }
    fn install_account_credentials(
        &mut self,
        record: Option<crate::accounts::CredentialBundle>,
    ) -> Result<()> {
        let Some(record) = record else {
            return Ok(());
        };
        let group = self.account_credential_authority()?;
        if record.account_id != group {
            return Err(StageError::Forbidden(
                "Account credential namespace mismatch",
            ));
        }
        if let Some(current) = self
            .host
            .accounts
            .current()
            .map_err(|_| StageError::Invalid("Cannot read account state"))?
        {
            if current.account_id != record.account_id || current.name != record.name {
                return Err(StageError::Forbidden(
                    "Account credential identity mismatch",
                ));
            }
            if record.revision < current.revision {
                return Ok(());
            }
        }
        if self
            .host
            .accounts
            .install(record, &group)
            .map_err(|_| StageError::Conflict("Account credential import rejected"))?
        {
            let system = self.host.system.clone();
            self.core_tasks.push(tokio::spawn(async move {
                let _ = system.activate_account_workers().await;
            }));
        }
        Ok(())
    }
    fn publish(&mut self) -> Result<()> {
        self.require_work_generation()?;
        if self.deferred.len() >= 8 {
            return Ok(());
        }
        let label = self.host.settings().label;
        if self.sent_device_name.as_ref() != Some(&label) {
            self.send(
                self.frame(Body::DeviceName {
                    label: label.clone(),
                }),
                Class::Control,
                SendScope::Connection,
            )?;
            self.sent_device_name = Some(label);
        }
        self.publish_accounts()?;
        self.publish_configuration()?;
        let core = self
            .host
            .core
            .read()
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        if self.account_credential_authority().is_ok()
            && let Some(core) = core
        {
            let status = core.recovery_status()?;
            if self.sent_recovery_status.as_ref() != Some(&status) {
                self.send(
                    self.frame(Body::RecoveryStatus {
                        status: status.clone(),
                    }),
                    Class::Control,
                    SendScope::Core,
                )?;
                self.sent_recovery_status = Some(status);
            }
        }
        let mut after = None;
        let mut outgoing = Vec::new();
        loop {
            let records = super::revocation::page(self.host.system.store(), after, 16)?;
            if records.is_empty() {
                break;
            }
            for record in records.iter() {
                let id = super::revocation::target(record)?;
                after = Some(id);
                if !self.sent_revocations.contains(&id) {
                    outgoing.push(record.clone());
                    if outgoing.len() == 8 {
                        break;
                    }
                }
            }
            if records.len() < 16 || outgoing.len() == 8 {
                break;
            }
        }
        if !outgoing.is_empty() {
            self.send(
                self.frame(Body::RevocationsPush {
                    records: outgoing.clone(),
                }),
                Class::Control,
                SendScope::Connection,
            )?;
            for record in outgoing {
                self.sent_revocations
                    .insert(super::revocation::target(&record)?);
            }
        }
        let notices = self
            .host
            .system
            .store()
            .events(self.notice_cursor, None, true, 32)
            .map_err(|_| StageError::Unavailable("Cannot read account notices"))?;
        for record in notices {
            if self.deferred.len() >= 8 {
                break;
            }
            let sequence = record.seq;
            // Shared Core messages arrive through the canonical replica stream.
            // Qualifying them as a remote Actor creates a duplicate identity.
            if record.actor_id == super::shared::ACTOR_ID
                && matches!(
                    record.event,
                    crate::actors::ActorEvent::MessageAdded { .. }
                        | crate::actors::ActorEvent::MessageUpdated { .. }
                )
            {
                self.notice_cursor = sequence;
                continue;
            }
            if record.actor_id.contains('~') {
                self.notice_cursor = sequence;
                continue;
            }
            if let crate::actors::ActorEvent::ApprovalRequested { approval_id, .. }
            | crate::actors::ActorEvent::ApprovalResolved { approval_id, .. } = &record.event
                && !self
                    .host
                    .system
                    .store()
                    .native_approval(approval_id)
                    .unwrap_or(false)
            {
                self.notice_cursor = sequence;
                continue;
            }
            let access = StageLedger::attach(self.host.system.store(), self.host.local)
                .actor_grant(self.channel.peer.stage_id(), &record.actor_id, now());
            if access.is_ok_and(|grant| grant.read) {
                let actor = record.actor_id.clone();
                self.send(
                    self.frame(Body::Notice { record }),
                    Class::Event,
                    SendScope::Actor(actor),
                )?;
            }
            self.notice_cursor = sequence;
        }
        let peer = self.channel.peer.clone();
        let receiver = StageReceiver::new(self.host.system.clone());
        let ids = self.publishing.keys().copied().collect::<Vec<_>>();
        for id in ids {
            if self.deferred.len() >= 8 {
                break;
            }
            let Some(subscription) = self.publishing.get_mut(&id) else {
                continue;
            };
            let Body::Subscribe { actor, cursor, .. } = &mut subscription.request.body else {
                continue;
            };
            *cursor = subscription.cursor.clone();
            let actor = actor.actor_id.clone();
            subscription.request.created_at_ms = now();
            subscription.request.expires_at_ms = now() + 60000;
            let response = receiver.handle_request(&peer, &subscription.request)?;
            let Body::ActorEvents { page, .. } = &response.body else {
                continue;
            };
            if page.base.sequence == page.from.sequence {
                continue;
            }
            let next = page.base.clone();
            self.send(response, Class::Event, SendScope::Actor(actor))?;
            if let Some(subscription) = self.publishing.get_mut(&id) {
                subscription.cursor = Some(next);
            }
        }
        Ok(())
    }
    fn receive(&mut self, frame: Envelope) -> Result<()> {
        self.require_work_generation()?;
        let current = StageLedger::attach(self.host.system.store(), self.host.local)
            .peer(self.channel.peer.stage_id(), now())?;
        frame.validate(
            self.channel.peer.stage_id(),
            self.host.local,
            current.generation,
            now(),
        )?;
        if let Some(reply) = frame.in_reply_to
            && let Some((_, pending)) = self.pending.remove(&reply)
        {
            return self.response(frame, pending);
        }
        if frame.in_reply_to.is_some()
            && matches!(
                frame.body,
                Body::BlobManifest { .. }
                    | Body::BlobChunk { .. }
                    | Body::Observation { .. }
                    | Body::ActorAccess { .. }
                    | Body::ActorAdminResult { .. }
                    | Body::ConversationResult { .. }
                    | Body::ResourceStored { .. }
                    | Body::CoordinationResponse { .. }
            )
        {
            return Ok(());
        }
        let receiver = StageReceiver::new(self.host.system.clone());
        match &frame.body {
            Body::AccountWorkReset { .. } => {
                Err(StageError::Conflict("Reset negotiation already completed"))
            }
            Body::DeviceName { label } => self
                .host
                .record_device_label(self.channel.peer.stage_id(), label),
            Body::AccountConfigurationQuery { known_binding } => {
                self.account_credential_authority()?;
                let record = self
                    .host
                    .configurations
                    .current()
                    .map_err(|_| StageError::Unavailable("Cannot load account configuration"))?
                    .filter(|record| {
                        known_binding.as_ref()
                            != Some(
                                &crate::accounts::configuration::ConfigurationStore::binding(
                                    record,
                                ),
                            )
                    });
                let mut reply = self.frame(Body::AccountConfiguration { record });
                reply.in_reply_to = Some(frame.message_id);
                self.send(reply, Class::Control, SendScope::Connection)
            }
            Body::AccountConfiguration { record } => self.install_configuration(record.clone()),
            Body::AccountCredentialsQuery { known_binding } => {
                let group = self.account_credential_authority()?;
                let record = self
                    .host
                    .accounts
                    .current()
                    .map_err(|_| StageError::Invalid("Cannot read private account state"))?
                    .filter(|record| {
                        record.account_id == group
                            && known_binding.as_ref() != Some(&record.binding())
                    });
                let mut response = self.frame(Body::AccountCredentials { record });
                response.in_reply_to = Some(frame.message_id);
                self.send(response, Class::Control, SendScope::Connection)
            }
            Body::AccountCredentials { record } => self.install_account_credentials(record.clone()),
            Body::ActorAdmin { actor, action } => {
                if actor.stage_id != self.host.local
                    || !StageLedger::attach(self.host.system.store(), self.host.local)
                        .manages_actor(self.channel.peer.stage_id(), &actor.actor_id, now())?
                {
                    return self.reject(frame.message_id, "forbidden");
                }
                self.core_tasks.retain(|task| !task.is_finished());
                if self.core_tasks.len() >= 8 {
                    return self.reject(frame.message_id, "capacity");
                }
                let host = self.host.clone();
                let sender = self.channel.sender.clone();
                let peer = self.channel.peer.stage_id();
                let actor = actor.clone();
                let action = action.clone();
                let request = frame.message_id;
                let generation = self.generation;
                self.core_tasks.push(tokio::spawn(async move {
                    let result = async {
                        if !StageLedger::attach(host.system.store(), host.local).manages_actor(
                            peer,
                            &actor.actor_id,
                            now(),
                        )? {
                            return Err(StageError::Forbidden(
                                "Actor administration authority changed",
                            ));
                        }
                        let record = match action {
                            super::protocol::ActorAdminAction::Preferences {
                                model_present,
                                model,
                                approval_mode,
                            } => {
                                host.system
                                    .patch_owned_preferences(
                                        &actor.actor_id,
                                        if model_present { Some(model) } else { None },
                                        approval_mode,
                                    )
                                    .await
                            }
                        };
                        let record = record
                            .map_err(|_| StageError::Conflict("Actor preferences rejected"))?;
                        let mut value = crate::service::public_actor(&record);
                        crate::service::redact(&mut value, &host.system.config());
                        let content = Some(
                            ContentStore::new(host.system.store())
                                .store_source(
                                    &actor.actor_id,
                                    &serde_json::to_vec(&value).map_err(|_| {
                                        StageError::Invalid("Cannot encode Actor preferences")
                                    })?,
                                )?
                                .content,
                        );
                        Ok::<_, StageError>(Body::ActorAdminResult { actor, content })
                    }
                    .await;
                    let body = result.unwrap_or_else(|error| Body::Error {
                        request,
                        code: error_code(&error).into(),
                    });
                    let time = now();
                    let response = Envelope {
                        version: 1,
                        source: host.local,
                        target: peer,
                        generation,
                        message_id: Uuid::new_v4(),
                        invocation_id: None,
                        in_reply_to: Some(request),
                        created_at_ms: time,
                        expires_at_ms: time + 60000,
                        body,
                    };
                    let _ = sender
                        .send_wait(response, Class::Control, SendScope::Connection)
                        .await;
                }));
                Ok(())
            }
            Body::RevocationsPush { records } => {
                for record in records {
                    super::revocation::apply(
                        self.host.system.store(),
                        self.host.local,
                        record,
                        now(),
                    )?;
                }
                Ok(())
            }
            Body::ResourceUpload {
                actor,
                resource,
                export,
            } => {
                if actor.stage_id != self.host.local
                    || !StageLedger::attach(self.host.system.store(), self.host.local)
                        .actor_grant(self.channel.peer.stage_id(), &actor.actor_id, now())?
                        .converse
                {
                    return self.reject(frame.message_id, "forbidden");
                }
                self.core_tasks.retain(|task| !task.is_finished());
                if self.core_tasks.len() >= 8 {
                    return self.reject(frame.message_id, "capacity");
                }
                let host = self.host.clone();
                let handle = self.handle.clone();
                let sender = self.channel.sender.clone();
                let peer = self.channel.peer.stage_id();
                let actor = actor.clone();
                let resource = resource.clone();
                let export = *export;
                let generation = self.generation;
                let request = frame.message_id;
                self.core_tasks.push(tokio::spawn(async move {
                    let result = async {
                        let bytes = handle
                            .exported_content(export, resource.content.clone())
                            .await?;
                        ContentStore::new(host.system.store())
                            .store_source(&actor.actor_id, &bytes)?;
                        if !StageLedger::attach(host.system.store(), host.local)
                            .actor_grant(peer, &actor.actor_id, now())?
                            .converse
                        {
                            return Err(StageError::Forbidden("Upload authority changed"));
                        }
                        crate::resources::install(&host.system, &actor.actor_id, &resource)?;
                        Ok::<_, StageError>(Body::ResourceStored {
                            actor: actor.clone(),
                            resource,
                        })
                    }
                    .await;
                    let body = result.unwrap_or(Body::Error {
                        request,
                        code: "resource_upload_rejected".into(),
                    });
                    let time = now();
                    let response = Envelope {
                        version: 1,
                        source: host.local,
                        target: peer,
                        generation,
                        message_id: Uuid::new_v4(),
                        invocation_id: None,
                        in_reply_to: Some(request),
                        created_at_ms: time,
                        expires_at_ms: time + 60000,
                        body,
                    };
                    let _ = sender
                        .send_wait(response, Class::Control, SendScope::Actor(actor.actor_id))
                        .await;
                }));
                Ok(())
            }
            Body::Notice { record } => {
                self.host
                    .system
                    .store()
                    .receive_notice(self.channel.peer.stage_id(), record)
                    .map_err(|_| StageError::Unavailable("Cannot persist account notice"))?;
                Ok(())
            }
            Body::CoordinationRequest { request } => {
                let coordinator = self
                    .host
                    .coordination
                    .read()
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade);
                let Some(coordinator) = coordinator else {
                    return self.reject(frame.message_id, "unavailable");
                };
                let peer = self.channel.peer.stage_id();
                if coordinator.authorize(peer).is_err() {
                    return self.reject(frame.message_id, "forbidden");
                }
                self.core_tasks.retain(|task| !task.is_finished());
                if self.core_tasks.len() >= 16 {
                    return self.reject(frame.message_id, "capacity");
                }
                let sender = self.channel.sender.clone();
                let mut reply = self.frame(Body::Heartbeat);
                reply.in_reply_to = Some(frame.message_id);
                let request = *request.clone();
                self.core_tasks.push(tokio::spawn(async move {
                    let response = coordinator
                        .handle(peer, request)
                        .await
                        .unwrap_or(super::coordination::Response::Rejected);
                    reply.body = Body::CoordinationResponse {
                        response: Box::new(response),
                    };
                    let _ = sender
                        .send_wait(reply, Class::Control, SendScope::Connection)
                        .await;
                }));
                Ok(())
            }
            Body::CoreRequest { content } => {
                self.account_credential_authority()?;
                self.core_tasks.retain(|task| !task.is_finished());
                if self.core_tasks.len() >= 8 {
                    return Err(StageError::Capacity("Core control task budget reached"));
                }
                let core = self
                    .host
                    .core
                    .read()
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade)
                    .ok_or(StageError::Unavailable("Account Actor is not enabled"))?;
                let host = self.host.clone();
                let handle = self.handle.clone();
                let sender = self.channel.sender.clone();
                let content = content.clone();
                let peer = self.channel.peer.stage_id();
                let generation = self.generation;
                let request_id = frame.message_id;
                self.core_tasks.push(tokio::spawn(async move {
                    let result = async {
                        let bytes = handle
                            .content(
                                ActorAddress {
                                    stage_id: peer,
                                    actor_id: super::shared::ACTOR_ID.into(),
                                },
                                content,
                            )
                            .await?;
                        let request: super::core_wire::Request = serde_json::from_slice(&bytes)
                            .map_err(|_| StageError::Invalid("Invalid core control payload"))?;
                        core.handle(peer, request).await
                    }
                    .await;
                    let response = result.unwrap_or_else(|error| {
                        tracing::debug!(peer=%peer,reason=%error,"Shared Actor request rejected");
                        if let StageError::ApprovalDenied(reason) = error {
                            return super::core_wire::Response::ApprovalDenied {
                                reason: host
                                    .system
                                    .safe_error(&reason)
                                    .chars()
                                    .take(1000)
                                    .collect(),
                            };
                        }
                        super::core_wire::Response::Error {
                            code: "core_operation_rejected".into(),
                        }
                    });
                    if let Ok(bytes) = serde_json::to_vec(&response)
                        && let Ok(manifest) = ContentStore::new(host.system.store())
                            .store_source(super::shared::ACTOR_ID, &bytes)
                    {
                        let time = now();
                        let response = Envelope {
                            version: 1,
                            source: host.local,
                            target: peer,
                            generation,
                            message_id: Uuid::new_v4(),
                            invocation_id: None,
                            in_reply_to: Some(request_id),
                            created_at_ms: time,
                            expires_at_ms: time + 60000,
                            body: Body::CoreResponse {
                                content: manifest.content,
                            },
                        };
                        let _ = sender
                            .send_wait(response, Class::Control, SendScope::Core)
                            .await;
                    }
                }));
                Ok(())
            }
            Body::Invoke { content, .. } => {
                let access = BlobAccess::Invocation {
                    invocation: frame
                        .invocation_id
                        .ok_or(StageError::Invalid("Invocation required"))?,
                };
                let reference = content.clone();
                self.download(access, reference, Waiter::Input(Box::new(frame)))
            }
            Body::Delivery { actor, content, .. } => {
                Deliveries::new(self.host.system.store(), self.host.local).expected_reply(
                    self.channel.peer.stage_id(),
                    &frame,
                    now(),
                )?;
                let access = BlobAccess::Actor {
                    actor: actor.clone(),
                };
                let reference = content.clone();
                self.download(access, reference, Waiter::Input(Box::new(frame)))
            }
            Body::Received { invocation } => {
                StageLedger::attach(self.host.system.store(), self.host.local)
                    .acknowledge_received(self.channel.peer.stage_id(), *invocation, now())?;
                if let Some(request) = frame.in_reply_to {
                    self.inflight.remove(&request);
                }
                Ok(())
            }
            Body::TransferAck { message } => {
                Deliveries::new(self.host.system.store(), self.host.local).acknowledge(
                    self.channel.peer.stage_id(),
                    *message,
                    now(),
                )?;
                self.inflight.remove(message);
                Ok(())
            }
            Body::Subscribe { actor, cursor, .. } => {
                if self.publishing.len()>=8&&!self.publishing.values().any(|sub|matches!(&sub.request.body,Body::Subscribe{actor:existing,..}if existing==actor)){return Err(StageError::Capacity("Peer subscription limit reached"));}
                self.publishing.retain(|_,sub|!matches!(&sub.request.body,Body::Subscribe{actor:existing,..}if existing==actor));
                let response = receiver.handle_request(&self.channel.peer, &frame);
                match response {
                    Ok(response) => {
                        let next = match &response.body {
                            Body::ActorEvents { page, .. } => Some(page.base.clone()),
                            _ => cursor.clone(),
                        };
                        self.send(
                            response,
                            Class::Event,
                            SendScope::Actor(actor.actor_id.clone()),
                        )?;
                        self.publishing.insert(
                            frame.message_id,
                            Subscription {
                                request: frame,
                                cursor: next,
                            },
                        );
                        Ok(())
                    }
                    Err(StageError::Conflict(_)) => {
                        let mut response = self.frame(Body::Error {
                            request: frame.message_id,
                            code: "snapshot_required".into(),
                        });
                        response.in_reply_to = Some(frame.message_id);
                        self.send(response, Class::Control, SendScope::Connection)
                    }
                    Err(error) => Err(error),
                }
            }
            Body::Unsubscribe {
                subscription,
                actor,
            } => {
                self.publishing.remove(subscription);
                if let Some(actor) = actor {
                    self.publishing.retain(|_,sub|!matches!(&sub.request.body,Body::Subscribe{actor:existing,..}if existing==actor));
                }
                Ok(())
            }
            Body::ActorEvents { actor, page } => {
                if self.views.level(actor).is_none() {
                    return Err(StageError::Forbidden("Unrequested Actor events"));
                }
                Replica::new(self.host.system.store()).receive(actor, page, now())?;
                self.fetch_missing(actor.clone())
            }
            Body::RecoveryStatus { status } => {
                self.account_credential_authority()?;
                if let Some(core) = self
                    .host
                    .core
                    .read()
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade)
                {
                    core.note_recovery(self.channel.peer.stage_id(), status.clone())?;
                }
                Ok(())
            }
            Body::Heartbeat => {
                if frame.in_reply_to.is_none() {
                    let mut reply = self.frame(Body::Heartbeat);
                    reply.in_reply_to = Some(frame.message_id);
                    self.send(reply, Class::Control, SendScope::Connection)
                } else {
                    Ok(())
                }
            }
            Body::Error { request, code } if code == "snapshot_required" => {
                let _ = request;
                let actors = self.views.actors().into_iter().cloned().collect::<Vec<_>>();
                for actor in actors {
                    self.request(
                        Body::SnapshotQuery {
                            actor: actor.clone(),
                            token: None,
                            page: 0,
                        },
                        Pending::Snapshot { actor },
                        Class::Control,
                    )?;
                }
                Ok(())
            }
            Body::Error { .. } => Ok(()),
            _ => {
                let scope = match &frame.body {
                    Body::ResultQuery { invocation } | Body::Cancel { invocation } => {
                        let receipt =
                            StageLedger::attach(self.host.system.store(), self.host.local).result(
                                self.channel.peer.stage_id(),
                                *invocation,
                                now(),
                            )?;
                        SendScope::Actor(receipt.actor_id)
                    }
                    Body::ReadQuery { actor, .. } | Body::SnapshotQuery { actor, .. } => {
                        SendScope::Actor(actor.actor_id.clone())
                    }
                    Body::BlobRead { access, .. } | Body::BlobManifestQuery { access, .. } => {
                        match access {
                            BlobAccess::Actor { actor } => SendScope::Actor(actor.actor_id.clone()),
                            BlobAccess::Invocation { invocation } => {
                                let content = match &frame.body {
                                    Body::BlobRead { content, .. }
                                    | Body::BlobManifestQuery { content, .. } => content.clone(),
                                    _ => return Err(StageError::Invalid("Invalid blob request")),
                                };
                                SendScope::Invocation {
                                    invocation: *invocation,
                                    content,
                                }
                            }
                        }
                    }
                    _ => SendScope::Connection,
                };
                let response = match receiver.handle_request(&self.channel.peer, &frame) {
                    Ok(response) => response,
                    Err(error) => {
                        let mut response = self.frame(Body::Error {
                            request: frame.message_id,
                            code: error_code(&error).into(),
                        });
                        response.in_reply_to = Some(frame.message_id);
                        return self.send(response, Class::Control, SendScope::Connection);
                    }
                };
                let class = if matches!(response.body, Body::BlobChunk { .. }) {
                    Class::Blob
                } else {
                    Class::Control
                };
                self.send(response, class, scope)
            }
        }
    }
    fn reject(&mut self, request: Uuid, code: &str) -> Result<()> {
        let mut frame = self.frame(Body::Error {
            request,
            code: code.into(),
        });
        frame.in_reply_to = Some(request);
        self.send(frame, Class::Control, SendScope::Connection)
    }
    fn response(&mut self, frame: Envelope, pending: Pending) -> Result<()> {
        if let Body::Error { code, .. } = &frame.body {
            if let Pending::Manifest { hash } | Pending::Chunk { hash, .. } = &pending
                && let Some(download) = self.downloads.remove(hash)
            {
                for waiter in download.waiters {
                    if let Waiter::Fetch(sender) = waiter {
                        let _ =
                            sender.send(Err(StageError::Forbidden("Peer denied content access")));
                    }
                }
            }
            if let Pending::Rpc(response) = pending {
                let _ = response.send(Err(match code.as_str() {
                    "forbidden" => StageError::Forbidden("Peer request rejected"),
                    "conflict" => StageError::Conflict("Peer state changed"),
                    "invalid" => StageError::Invalid("Peer request rejected"),
                    "capacity" => StageError::Capacity("Peer budget reached"),
                    _ => StageError::Unavailable("Peer operation unavailable"),
                }));
            }
            return Ok(());
        }
        let peer = self.channel.peer.stage_id();
        match pending {
            Pending::AccountCredentials => {
                let Body::AccountCredentials { record } = frame.body else {
                    return Err(StageError::Invalid("Expected account credential response"));
                };
                self.install_account_credentials(record)
            }
            Pending::AccountConfiguration => {
                let Body::AccountConfiguration { record } = frame.body else {
                    return Err(StageError::Invalid(
                        "Expected account configuration response",
                    ));
                };
                self.install_configuration(record)
            }
            Pending::Rpc(response) => {
                let _ = response.send(Ok(frame.body));
                Ok(())
            }
            Pending::Directory => {
                let Body::Directory { actors, next } = frame.body else {
                    return Err(StageError::Invalid("Expected Actor directory"));
                };
                super::runtime::save_directory(&self.host, peer, &actors)?;
                if let Some(after) = next {
                    self.request(
                        Body::DirectoryQuery {
                            after: Some(after),
                            limit: 32,
                        },
                        Pending::Directory,
                        Class::Control,
                    )?;
                }
                Ok(())
            }
            Pending::Revocations => {
                let Body::Revocations { records, next } = frame.body else {
                    return Err(StageError::Invalid("Expected account revocations"));
                };
                for record in records {
                    super::revocation::apply(
                        self.host.system.store(),
                        self.host.local,
                        &record,
                        now(),
                    )?;
                }
                if let Some(after) = next {
                    self.request(
                        Body::RevocationsQuery {
                            after: Some(after),
                            limit: 8,
                        },
                        Pending::Revocations,
                        Class::Control,
                    )?;
                }
                Ok(())
            }
            Pending::Members => {
                let Body::Members {
                    enrollments,
                    revocations,
                    next,
                } = frame.body
                else {
                    return Err(StageError::Invalid("Expected account members"));
                };
                for change in revocations {
                    super::revocation::apply(
                        self.host.system.store(),
                        self.host.local,
                        &change,
                        now(),
                    )?;
                }
                for enrollment in enrollments {
                    if let Err(error) = self.host.discover_member(enrollment)
                        && !matches!(error, StageError::Forbidden(_))
                    {
                        return Err(error);
                    }
                }
                if let Some(after) = next {
                    self.request(
                        Body::MembersQuery {
                            after: Some(after),
                            limit: 8,
                        },
                        Pending::Members,
                        Class::Control,
                    )?;
                }
                Ok(())
            }
            Pending::Manifest { hash } => {
                let Body::BlobManifest { manifest, .. } = frame.body else {
                    return Err(StageError::Invalid("Expected content manifest"));
                };
                let download = self
                    .downloads
                    .get_mut(&hash)
                    .ok_or(StageError::Conflict("Content waiter expired"))?;
                download.deadline = now() + 30000;
                if manifest.content != download.reference {
                    return Err(StageError::Conflict("Content manifest changed"));
                }
                let missing =
                    VecDeque::from(ContentStore::new(self.host.system.store()).begin_download(
                        peer,
                        &manifest,
                        now(),
                    )?);
                self.next_chunk(hash, missing)
            }
            Pending::Chunk {
                hash,
                chunk,
                remaining,
            } => {
                let Body::BlobChunk {
                    content,
                    chunk: actual,
                    bytes,
                    ..
                } = frame.body
                else {
                    return Err(StageError::Invalid("Expected content chunk"));
                };
                let download = self
                    .downloads
                    .get_mut(&hash)
                    .ok_or(StageError::Conflict("Content waiter expired"))?;
                download.deadline = now() + 30000;
                if content != download.reference || actual != chunk {
                    return Err(StageError::Conflict("Content chunk changed"));
                }
                ContentStore::new(self.host.system.store()).receive_chunk(
                    peer,
                    &hash,
                    chunk,
                    &bytes,
                    now(),
                )?;
                self.next_chunk(hash, remaining)
            }
            Pending::Snapshot { actor } => {
                let Body::ActorSnapshot { page, .. } = frame.body else {
                    return Err(StageError::Invalid("Expected Actor snapshot"));
                };
                let last = page.last;
                let token = page.token;
                let next = page.page + 1;
                Replica::new(self.host.system.store()).receive_snapshot(&actor, &page, now())?;
                self.fetch_missing(actor.clone())?;
                if !last {
                    self.request(
                        Body::SnapshotQuery {
                            actor: actor.clone(),
                            token: Some(token),
                            page: next,
                        },
                        Pending::Snapshot { actor },
                        Class::Control,
                    )?;
                } else {
                    let cursor = page.base;
                    let level = self
                        .views
                        .level(&actor)
                        .unwrap_or(SubscriptionLevel::Summary);
                    self.send(
                        self.frame(Body::Subscribe {
                            actor,
                            cursor: Some(cursor),
                            level,
                        }),
                        Class::Control,
                        SendScope::Connection,
                    )?;
                }
                Ok(())
            }
        }
    }
    fn accept(&mut self, frame: Envelope) -> Result<()> {
        let receiver = StageReceiver::new(self.host.system.clone());
        let body = if matches!(frame.body, Body::Invoke { .. }) {
            let receipt = receiver.accept_cached_input(&self.channel.peer, &frame)?;
            Body::Received {
                invocation: receipt.invocation,
            }
        } else {
            receiver.accept_cached_reply(&self.channel.peer, &frame)?;
            Body::TransferAck {
                message: frame.message_id,
            }
        };
        let mut response = self.frame(body);
        response.in_reply_to = Some(frame.message_id);
        self.send(response, Class::Control, SendScope::Connection)
    }
    fn download(&mut self, access: BlobAccess, reference: BlobRef, waiter: Waiter) -> Result<()> {
        if let Ok(bytes) = ContentStore::new(self.host.system.store()).verified_bytes(&reference) {
            return self.complete(waiter, bytes);
        }
        if let Some(download) = self.downloads.get_mut(&reference.hash) {
            if download.reference != reference {
                return Err(StageError::Conflict("Content reference changed"));
            }
            if download.waiters.len() >= 32 {
                return Err(StageError::Capacity("Content waiter limit reached"));
            }
            if let Waiter::Replica(actor) = &waiter
                && download
                    .waiters
                    .iter()
                    .any(|w| matches!(w,Waiter::Replica(existing) if existing==actor))
            {
                return Ok(());
            }
            if let Waiter::Input(frame) = &waiter && download.waiters.iter().any(|w|matches!(w,Waiter::Input(existing) if existing.message_id==frame.message_id)){return Ok(());}
            download.waiters.push(waiter);
            return Ok(());
        }
        if self.downloads.len() >= 24 {
            return Err(StageError::Capacity("Content download limit reached"));
        }
        let hash = reference.hash.clone();
        self.request(
            Body::BlobManifestQuery {
                access: access.clone(),
                content: reference.clone(),
            },
            Pending::Manifest { hash: hash.clone() },
            Class::Control,
        )?;
        self.downloads.insert(
            hash,
            Download {
                access,
                reference,
                waiters: vec![waiter],
                deadline: now() + 30000,
            },
        );
        Ok(())
    }
    fn next_chunk(&mut self, hash: String, mut remaining: VecDeque<u32>) -> Result<()> {
        if let Some(chunk) = remaining.pop_front() {
            let download = self
                .downloads
                .get(&hash)
                .ok_or(StageError::Conflict("Content waiter expired"))?;
            self.request(
                Body::BlobRead {
                    access: download.access.clone(),
                    content: download.reference.clone(),
                    chunk,
                },
                Pending::Chunk {
                    hash,
                    chunk,
                    remaining,
                },
                Class::Blob,
            )
        } else {
            let download = self
                .downloads
                .remove(&hash)
                .ok_or(StageError::Conflict("Content waiter expired"))?;
            let bytes =
                ContentStore::new(self.host.system.store()).verified_bytes(&download.reference)?;
            for waiter in download.waiters {
                self.complete(waiter, bytes.clone())?;
            }
            Ok(())
        }
    }
    fn complete(&mut self, waiter: Waiter, bytes: Vec<u8>) -> Result<()> {
        match waiter {
            Waiter::Input(frame) => self.accept(*frame),
            Waiter::Fetch(response) => {
                let _ = response.send(Ok(bytes));
                Ok(())
            }
            Waiter::Replica(actor) => {
                Replica::new(self.host.system.store()).apply_available(&actor, now())?;
                Ok(())
            }
        }
    }
    fn fetch_missing(&mut self, actor: ActorAddress) -> Result<()> {
        let missing = Replica::new(self.host.system.store()).missing_content(&actor, now())?;
        for reference in missing {
            if self.pending.len() >= 28 {
                break;
            }
            self.download(
                BlobAccess::Actor {
                    actor: actor.clone(),
                },
                reference,
                Waiter::Replica(actor.clone()),
            )?;
        }
        Ok(())
    }
}
