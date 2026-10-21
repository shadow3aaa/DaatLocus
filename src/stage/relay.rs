//! Multiplexed relay transport. Device TLS and the existing durable protocol
//! remain above the relay; a forwarding acknowledgement grants no authority.
use super::{
    Result, StageError, StageId,
    runtime::Runtime,
    transport::relay::{CHUNK, RelayEvent, RelaySocket, WINDOW},
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    sync::{Semaphore, mpsc},
    task::JoinSet,
};
use uuid::Uuid;

pub enum Command {
    Connect(StageId),
    Revoke(super::revocation::Change),
    Data(Uuid, Vec<u8>),
    Credit(Uuid, usize),
    Close(Uuid),
}
struct Tunnel {
    peer: StageId,
    input: mpsc::Sender<Vec<u8>>,
    credit: Arc<Semaphore>,
    pending: Option<DuplexStream>,
    created: std::time::Instant,
}
impl Drop for Tunnel {
    fn drop(&mut self) {
        self.credit.close();
    }
}

pub async fn run(runtime: Arc<Runtime>) {
    let mut failures = 0u32;
    while !runtime.is_stopped() {
        let generation = runtime.network_generation();
        let Some(endpoint) = runtime.host.settings().relay_origin else {
            break;
        };
        let result = async {
            let identity = runtime.host.identity()?;
            let enrollment = runtime.host.enrollment()?;
            let socket = RelaySocket::connect(&endpoint, identity.as_ref(), &enrollment).await?;
            connected(runtime.clone(), socket).await
        }
        .await;
        runtime.clear_relay();
        if let Err(error) = result {
            tracing::warn!(reason=%error, "Account relay transport stopped");
            runtime.relay_failed(&error);
        }
        if runtime.network_generation() != generation {
            failures = 0;
            continue;
        }
        failures = failures.saturating_add(1);
        let delay = (500u64 << failures.min(5)).min(10_000);
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(delay)) => {},
            _ = runtime.relay_notification() => {},
        }
    }
}

fn tunnel(
    id: Uuid,
    peer: StageId,
    tx: &mpsc::Sender<Command>,
    jobs: &mut JoinSet<()>,
) -> (Tunnel, DuplexStream) {
    let (application, transport) = tokio::io::duplex(CHUNK);
    let (input, mut received) = mpsc::channel::<Vec<u8>>(128);
    let credit = Arc::new(Semaphore::new(WINDOW));
    let permits = credit.clone();
    let tx = tx.clone();
    jobs.spawn(async move {
        let (mut reader, mut writer) = tokio::io::split(transport);
        let outgoing = async {
            let mut buffer = vec![0; CHUNK];
            loop {
                let first = permits.acquire().await.map_err(|_| ())?;
                let capacity = (permits.available_permits() + 1).min(CHUNK);
                let rest = permits
                    .acquire_many((capacity - 1) as u32)
                    .await
                    .map_err(|_| ())?;
                let count = reader.read(&mut buffer[..capacity]).await.map_err(|_| ())?;
                if count == 0 {
                    return Ok::<_, ()>(());
                }
                first.forget();
                rest.forget();
                permits.add_permits(capacity - count);
                tx.send(Command::Data(id, buffer[..count].to_vec()))
                    .await
                    .map_err(|_| ())?;
            }
        };
        let incoming = async {
            while let Some(bytes) = received.recv().await {
                writer.write_all(&bytes).await.map_err(|_| ())?;
                tx.send(Command::Credit(id, bytes.len()))
                    .await
                    .map_err(|_| ())?;
            }
            Ok::<_, ()>(())
        };
        tokio::pin!(outgoing, incoming);
        tokio::select! { _ = &mut outgoing => {}, _ = &mut incoming => {} }
        let _ = tx.send(Command::Close(id)).await;
    });
    (
        Tunnel {
            peer,
            input,
            credit,
            pending: None,
            created: std::time::Instant::now(),
        },
        application,
    )
}

async fn connected(runtime: Arc<Runtime>, mut socket: RelaySocket) -> Result<()> {
    let generation = runtime.network_generation();
    let origin = runtime.host.settings().relay_origin;
    let mut after = None;
    loop {
        let changes = super::revocation::page(runtime.host.system.store(), after, 8)?;
        for change in &changes {
            after = Some(super::revocation::target(change)?);
            socket.send_revocation(change).await?;
        }
        if changes.len() < 8 {
            break;
        }
    }
    let (tx, mut commands) = mpsc::channel(128);
    runtime.install_relay(tx.clone());
    let mut tunnels: HashMap<Uuid, Tunnel> = HashMap::new();
    let mut online: HashSet<StageId> = socket.peers.iter().copied().collect();
    let mut jobs = JoinSet::new();
    let mut sweep = tokio::time::interval(std::time::Duration::from_secs(1));
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result = async {
        while !runtime.is_stopped() && runtime.network_generation() == generation && runtime.host.settings().relay_origin == origin {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Revoke(change)) => socket.send_revocation(&change).await?,
                    Some(Command::Connect(peer)) => {
                        if runtime.connected(peer) || tunnels.values().any(|t|t.peer==peer) || !super::connection::automatic_dialer(runtime.host.local,peer) { continue; }
                        if !online.contains(&peer) { runtime.peer_failed(peer,&StageError::Unavailable("Relay peer is offline"));continue; }
                        if tunnels.len() >= 32 || runtime.begin_relay(peer).is_err() { continue; }
                        let id = Uuid::new_v4();
                        let (mut entry, stream) = tunnel(id,peer,&tx,&mut jobs);
                        entry.pending = Some(stream); tunnels.insert(id,entry);
                        socket.control("open",id,Some(peer)).await?;
                    },
                    Some(Command::Data(id,bytes)) if tunnels.contains_key(&id) => socket.data(id,&bytes).await?,
                    Some(Command::Credit(id,amount)) if tunnels.contains_key(&id) => socket.credit(id,amount).await?,
                    Some(Command::Close(id)) => {
                        if let Some(entry) = tunnels.remove(&id) {
                            runtime.peer_failed(entry.peer,&StageError::Unavailable("Relay channel closed"));
                            socket.control("close",id,None).await?;
                        }
                    },
                    Some(_) => {},
                    None => break,
                },
                event = socket.receive() => match event? {
                    RelayEvent::Presence {stage_id,online:present} => {
                        if present { online.insert(stage_id); } else {
                            online.remove(&stage_id);
                            tunnels.retain(|_,entry|entry.peer!=stage_id);
                        }
                        runtime.relay_changed();
                    },
                    RelayEvent::Revocations {records} => {
                        for change in records { super::revocation::apply(runtime.host.system.store(),runtime.host.local,&change,chrono::Utc::now().timestamp_millis())?; }
                    },
                    RelayEvent::Incoming {channel,source} => {
                        let member = super::ledger::StageLedger::attach(runtime.host.system.store(),runtime.host.local).peer(source,chrono::Utc::now().timestamp_millis());
                        if source >= runtime.host.local || member.is_err() || tunnels.len() >= 32 || tunnels.contains_key(&channel) || tunnels.values().any(|entry|entry.peer==source) || runtime.connected(source) {
                            socket.control("close",channel,None).await?;continue;
                        }
                        let (entry,stream)=tunnel(channel,source,&tx,&mut jobs);
                        tunnels.insert(channel,entry);
                        socket.control("accept",channel,None).await?;
                        establish(&runtime,source,stream,false,generation);
                    },
                    RelayEvent::Opened {channel} => {
                        if let Some(entry)=tunnels.get_mut(&channel) && let Some(stream)=entry.pending.take() {
                            establish(&runtime,entry.peer,stream,true,generation);
                        }
                    },
                    RelayEvent::Closed {channel} => {
                        if let Some(entry)=tunnels.remove(&channel) {
                            runtime.peer_failed(entry.peer,&StageError::Unavailable("Relay peer is offline"));
                        }
                    },
                    RelayEvent::Data {channel,bytes} => {
                        let failed=tunnels.get(&channel).is_some_and(|entry| entry.input.try_send(bytes).is_err());
                        if failed { tunnels.remove(&channel);socket.control("close",channel,None).await?; }
                    },
                    RelayEvent::Credit {channel,amount} => {
                        if let Some(entry)=tunnels.get(&channel) {
                            if entry.credit.available_permits()+amount>WINDOW {return Err(StageError::Forbidden("Relay credit exceeds window"));}
                            entry.credit.add_permits(amount);
                        }
                    },
                    _ => return Err(StageError::Invalid("Unexpected relay response")),
                },
                _ = sweep.tick() => {
                    socket.heartbeat().await?;
                    let expired=tunnels.iter().filter(|(_,entry)|entry.pending.is_some()&&entry.created.elapsed().as_secs()>=20).map(|(id,_)|*id).collect::<Vec<_>>();
                    for id in expired { if let Some(entry)=tunnels.remove(&id){runtime.peer_failed(entry.peer,&StageError::Unavailable("Relay channel timed out"));}socket.control("close",id,None).await?; }
                    let ledger=super::ledger::StageLedger::attach(runtime.host.system.store(),runtime.host.local);
                    for peer in &online { if *peer != runtime.host.local {if ledger.peer(*peer,chrono::Utc::now().timestamp_millis()).is_ok(){runtime.want(*peer);}else{runtime.need_member(*peer);}} }
                },
                _ = jobs.join_next(), if !jobs.is_empty() => {},
                _ = runtime.relay_notification() => {},
            }
        }
        Ok(())
    }.await;
    drop(tunnels);
    jobs.shutdown().await;
    let _ = socket.close().await;
    result
}
fn establish(
    runtime: &Arc<Runtime>,
    peer: StageId,
    stream: DuplexStream,
    outgoing: bool,
    generation: u64,
) {
    let owner = runtime.clone();
    runtime.spawn(async move {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(45),
            owner.establish_relay(peer, stream, outgoing, generation),
        )
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => owner.peer_failed(peer, &error),
            Err(_) => owner.peer_failed(
                peer,
                &StageError::Unavailable("Relay device handshake timed out"),
            ),
        }
    });
}
