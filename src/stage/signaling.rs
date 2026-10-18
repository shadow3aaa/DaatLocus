//! One account rendezvous socket; business data only uses authenticated peers.
use super::{
    Result, StageError, StageId,
    channel::PeerChannel,
    connection::Route,
    runtime::Runtime,
    transport::{
        p2p::{Attempt, Negotiation, P2pChannel, verify_signal},
        signaling::{SignalEvent, SignalSocket},
    },
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::mpsc;
use uuid::Uuid;
pub enum SignalCommand {
    Revoke(super::revocation::Change),
    Connect(StageId),
    Offer(super::identity::SignedProof),
    Answer(super::identity::SignedProof),
}
struct Candidate {
    channel: P2pChannel,
    attempt: Attempt,
    deadline: i64,
}
pub async fn run(runtime: Arc<Runtime>) {
    let mut failures = 0u32;
    loop {
        let network_epoch = runtime.network_generation();
        if runtime.is_stopped() || runtime.host.settings().signaling_origin.is_none() {
            break;
        }
        let endpoint = match runtime.host.settings().signaling_origin {
            Some(endpoint) => endpoint,
            None => break,
        };
        let identity = match runtime.host.identity() {
            Ok(identity) => identity,
            Err(_) => break,
        };
        let enrollment = match runtime.host.enrollment() {
            Ok(enrollment) => enrollment,
            Err(_) => break,
        };
        match SignalSocket::connect(&endpoint, &identity, &enrollment).await {
            Ok(socket) => {
                failures = 0;
                if let Err(error) = connected(runtime.clone(), socket).await {
                    tracing::warn!(reason=%error,"Account signaling channel closed");
                    runtime.signaling_failed(&error);
                }
            }
            Err(error) => {
                tracing::warn!(reason=%error,"Account signaling registration failed");
                runtime.signaling_failed(&error);
                failures = failures.saturating_add(1);
            }
        }
        runtime.clear_signal();
        if runtime.network_generation() != network_epoch {
            failures = 0;
            continue;
        }
        let delay = (1000u64 << failures.min(6)).min(10000);
        tokio::select! {_=tokio::time::sleep(std::time::Duration::from_millis(delay))=>{},_=runtime.signal_notification()=>{}}
    }
}
async fn connected(runtime: Arc<Runtime>, mut socket: SignalSocket) -> Result<()> {
    let network_epoch = runtime.network_generation();
    let connected_origin = runtime.host.settings().signaling_origin;
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
    let (tx, mut commands) = mpsc::channel(32);
    runtime.install_signal(tx);
    let mut candidates: HashMap<StageId, Candidate> = HashMap::new();
    let mut updates: HashMap<StageId, (Attempt, super::transport::p2p::CandidateUpdates, i64)> =
        HashMap::new();
    let mut jobs = tokio::task::JoinSet::new();
    let mut negotiating: HashMap<StageId, (Uuid, tokio::task::AbortHandle)> = HashMap::new();
    let mut sweep = tokio::time::interval(std::time::Duration::from_millis(500));
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut online = socket
        .peers
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let mut discovered = std::collections::HashSet::new();
    discover_online(&runtime, &online, &mut discovered);
    let result = async {
        while !runtime.is_stopped() {
            if runtime.host.settings().signaling_origin != connected_origin || runtime.network_generation() != network_epoch { break; }
            tokio::select! {
                command=commands.recv()=>match command {
                    Some(SignalCommand::Revoke(change))=>socket.send_revocation(&change).await?,
                    Some(SignalCommand::Offer(proof)|SignalCommand::Answer(proof))=>socket.send_negotiation(&proof).await?,
                    Some(SignalCommand::Connect(peer))=>{
                        if negotiating.contains_key(&peer) || candidates.contains_key(&peer) || runtime.connected(peer) || runtime.host.settings().ssh.contains_key(&peer) { continue; }
                        if !online.contains(&peer) { runtime.p2p_failed(peer, &StageError::Unavailable("Public peer is not available")); continue; }
                        if !super::connection::automatic_dialer(runtime.host.local,peer) || negotiating.len()+updates.len()>=8 || runtime.begin_p2p(peer).is_err() { continue; }
                        let record = super::ledger::StageLedger::attach(runtime.host.system.store(),runtime.host.local).peer(peer,chrono::Utc::now().timestamp_millis())?;
                        let attempt = Attempt {generation:record.generation+1,nonce:Uuid::new_v4()};
                        let owner = runtime.clone();
                        let abort = jobs.spawn(async move {
                            let result = tokio::time::timeout(std::time::Duration::from_secs(15),offer(&owner,peer,attempt)).await.map_err(|_|StageError::Unavailable("RTC offer timed out")).and_then(|result|result);
                            (peer,attempt,true,result)
                        });
                        negotiating.insert(peer,(attempt.nonce,abort));
                    }
                    None=>break,
                },
                done=jobs.join_next(), if !jobs.is_empty()=>{
                    let (peer,attempt,is_offer,result) = match done {
                        Some(Ok(result))=>result,
                        Some(Err(error))=>{
                            let peer=negotiating.iter().find(|(_,(_,abort))|abort.id()==error.id()).map(|(peer,_)|*peer);
                            if let Some(peer)=peer {negotiating.remove(&peer);runtime.p2p_failed(peer,&StageError::Unavailable("RTC negotiation stopped"));}
                            continue;
                        }
                        None=>continue,
                    };
                    // An offline/replaced attempt cannot publish a late offer.
                    if negotiating.get(&peer).is_none_or(|(nonce,_)|*nonce!=attempt.nonce) || !online.contains(&peer) {
                        if let Ok((channel,_)) = result { close_later(&runtime,channel); }
                        continue;
                    }
                    negotiating.remove(&peer);
                    match result {
                        Ok((channel,proof))=>{
                            if let Err(error)=socket.send_negotiation(&proof).await { close_later(&runtime,channel); return Err(error); }
                            let updater = channel.signal_updates();
                            updates.insert(peer, (attempt, updater.clone(), chrono::Utc::now().timestamp_millis()+30_000));
                            let identity = runtime.host.identity()?;
                            let root = runtime.host.enrollment()?.root;
                            let commands = runtime.signals.read().clone();
                            if let Some(commands) = commands {
                                runtime.spawn(updater.publish(identity, root, peer, attempt, commands));
                            }
                            if is_offer { candidates.insert(peer,Candidate {channel,attempt,deadline:chrono::Utc::now().timestamp_millis()+20000}); }
                            else { complete(runtime.clone(),peer,channel,attempt,network_epoch); }
                        }
                        Err(error)=>runtime.p2p_failed(peer,&error),
                    }
                },
                event=socket.receive()=>match event? {
                    SignalEvent::Revocations{records}=>{for change in records {super::revocation::apply(runtime.host.system.store(),runtime.host.local,&change,chrono::Utc::now().timestamp_millis())?;} runtime.signal_changed();},
                    SignalEvent::Presence{stage_id,online:available}=>{
                        if available { online.insert(stage_id); }
                        else {
                            online.remove(&stage_id); discovered.remove(&stage_id);
                            if let Some((_,job))=negotiating.remove(&stage_id) { job.abort(); }
                            if let Some(candidate)=candidates.remove(&stage_id) { close_later(&runtime,candidate.channel); }
                            runtime.p2p_failed(stage_id,&StageError::Unavailable("Public peer is not available"));
                            runtime.probe(stage_id);
                        }
                        discover_online(&runtime,&online,&mut discovered); runtime.signal_changed();
                    },
                    SignalEvent::Signal{source,proof}=>{
                        let ledger=super::ledger::StageLedger::attach(runtime.host.system.store(),runtime.host.local);
                        let record=match ledger.peer(source,chrono::Utc::now().timestamp_millis()) {Ok(record)=>record,Err(_)=>{runtime.need_member(source);continue;}};
                        let value:Negotiation=proof.verify("rtc-negotiation",&record.device.public_key)?;
                        let attempt=Attempt{generation:value.generation,nonce:value.nonce};
                        verify_signal(&proof,&record.device,&runtime.host.enrollment()?.root,runtime.host.local,attempt,chrono::Utc::now().timestamp_millis())?;
                        if value.description.sdp.lines().any(|line| line == "a=x-daat-update:1") {
                            if let Some((expected, updater, deadline)) = updates.get(&source)
                                && expected.generation == attempt.generation && expected.nonce == attempt.nonce
                                && *deadline > chrono::Utc::now().timestamp_millis() {
                                let updater = updater.clone();
                                runtime.spawn(async move {
                                    if let Err(error) = updater.apply(value).await {
                                        tracing::debug!(reason = %error, "Rejected incremental ICE candidates");
                                    }
                                });
                            }
                            continue;
                        }
                        if value.description.sdp_type==webrtc::peer_connection::RTCSdpType::Offer {
                            if runtime.connected(source)||runtime.host.settings().ssh.contains_key(&source)||super::connection::automatic_dialer(runtime.host.local,source)||negotiating.contains_key(&source)||negotiating.len()+updates.len()>=8||runtime.begin_p2p(source).is_err() { continue; }
                            let owner=runtime.clone();
                            let abort=jobs.spawn(async move {
                                let result=tokio::time::timeout(std::time::Duration::from_secs(15),answer(&owner,&proof,&record.device,attempt)).await.map_err(|_|StageError::Unavailable("RTC answer timed out")).and_then(|result|result);
                                (source,attempt,false,result)
                            });
                            negotiating.insert(source,(attempt.nonce,abort));
                        } else {
                            let Some(mut candidate)=candidates.remove(&source) else {continue;};
                            if candidate.attempt.generation!=attempt.generation||candidate.attempt.nonce!=attempt.nonce||candidate.deadline<=chrono::Utc::now().timestamp_millis() {
                                close_later(&runtime,candidate.channel);runtime.p2p_failed(source,&StageError::Unavailable("RTC negotiation expired"));continue;
                            }
                            // Applying SDP and waiting for DTLS stay outside the
                            // rendezvous reader, just like ICE gathering.
                            let owner=runtime.clone();
                            runtime.spawn(async move {
                                let result=async {
                                    candidate.channel.apply_signal(&proof,&record.device,&owner.host.enrollment()?.root,owner.host.local,attempt,chrono::Utc::now().timestamp_millis()).await?;
                                    finish(owner.clone(),candidate.channel,attempt,network_epoch).await
                                }.await;
                                if let Err(error)=result && owner.network_generation() == network_epoch { owner.p2p_failed(source,&error); }
                            });
                        }
                    },
                    SignalEvent::Unreachable{target,..}=>{
                        if let Some((_,job))=negotiating.remove(&target) {job.abort();}
                        if let Some(candidate)=candidates.remove(&target) {close_later(&runtime,candidate.channel);}
                        runtime.p2p_failed(target,&StageError::Unavailable("Public peer is not available"));
                    },
                    _=>return Err(StageError::Invalid("Unexpected signaling event")),
                },
                _=runtime.signal_notification()=>{},
                _=sweep.tick()=>{
                    updates.retain(|peer, (_, _, deadline)| *deadline > chrono::Utc::now().timestamp_millis() && !runtime.connected(*peer));
                    socket.heartbeat().await?;
                    discover_online(&runtime,&online,&mut discovered);
                    let expired=candidates.iter().filter(|(_,candidate)|candidate.deadline<=chrono::Utc::now().timestamp_millis()).map(|(peer,_)|*peer).collect::<Vec<_>>();
                    for peer in expired {if let Some(candidate)=candidates.remove(&peer) {close_later(&runtime,candidate.channel);runtime.p2p_failed(peer,&StageError::Unavailable("RTC negotiation timed out"));}}
                },
            }
        }
        Ok(())
    }.await;
    jobs.abort_all();
    for peer in negotiating.keys().copied() {
        if runtime.network_generation() == network_epoch {
            runtime.p2p_failed(peer, &StageError::Unavailable("Public negotiation ended"));
        }
    }
    for (peer, candidate) in candidates {
        close_later(&runtime, candidate.channel);
        if runtime.network_generation() == network_epoch {
            runtime.p2p_failed(peer, &StageError::Unavailable("Public negotiation ended"));
        }
    }
    result.and(socket.close().await)
}
fn close_later(runtime: &Runtime, channel: P2pChannel) {
    runtime.spawn(async move {
        let _ = channel.close().await;
    });
}
fn discover_online(
    runtime: &Arc<Runtime>,
    online: &std::collections::HashSet<StageId>,
    discovered: &mut std::collections::HashSet<StageId>,
) {
    let ledger =
        super::ledger::StageLedger::attach(runtime.host.system.store(), runtime.host.local);
    for peer in online {
        // A public presence hint never establishes membership. Start transport
        // only for a device whose current enrollment verifies in our ledger.
        if *peer != runtime.host.local
            && !discovered.contains(peer)
            && ledger
                .peer(*peer, chrono::Utc::now().timestamp_millis())
                .is_ok()
        {
            discovered.insert(*peer);
            runtime.want(*peer);
        }
    }
}
async fn offer(
    runtime: &Arc<Runtime>,
    peer: StageId,
    attempt: Attempt,
) -> Result<(P2pChannel, super::identity::SignedProof)> {
    let channel = P2pChannel::new(runtime.host.settings().public_stun_urls(), false).await?;
    let identity = runtime.host.identity()?;
    let result = channel
        .offer(
            identity.as_ref(),
            &runtime.host.enrollment()?.root,
            peer,
            attempt,
            chrono::Utc::now().timestamp_millis(),
        )
        .await;
    match result {
        Ok(proof) => Ok((channel, proof)),
        Err(error) => {
            let _ = channel.close().await;
            Err(error)
        }
    }
}
fn complete(
    runtime: Arc<Runtime>,
    peer: StageId,
    channel: P2pChannel,
    attempt: Attempt,
    network_epoch: u64,
) {
    let own = runtime.clone();
    runtime.spawn(async move {
        if let Err(error) = finish(own.clone(), channel, attempt, network_epoch).await
            && own.network_generation() == network_epoch
        {
            own.p2p_failed(peer, &error);
        }
    });
}
async fn answer(
    runtime: &Arc<Runtime>,
    proof: &super::identity::SignedProof,
    peer: &super::identity::DeviceProfile,
    attempt: Attempt,
) -> Result<(P2pChannel, super::identity::SignedProof)> {
    let mut channel = P2pChannel::new(runtime.host.settings().public_stun_urls(), false).await?;
    let result = async {
        let root = runtime.host.enrollment()?.root;
        channel
            .apply_signal(
                proof,
                peer,
                &root,
                runtime.host.local,
                attempt,
                chrono::Utc::now().timestamp_millis(),
            )
            .await?;
        let identity = runtime.host.identity()?;
        channel
            .answer(
                identity.as_ref(),
                &root,
                peer.stage_id,
                attempt,
                chrono::Utc::now().timestamp_millis(),
            )
            .await
    }
    .await;
    match result {
        Ok(proof) => Ok((channel, proof)),
        Err(error) => {
            let _ = channel.close().await;
            Err(error)
        }
    }
}
pub(super) async fn finish(
    runtime: Arc<Runtime>,
    mut channel: P2pChannel,
    attempt: Attempt,
    network_epoch: u64,
) -> Result<()> {
    if let Err(error) = channel.connected().await {
        let _ = channel.close().await;
        return Err(error);
    }
    let peer = channel.verified_peer()?;
    tracing::debug!(local = %runtime.host.local, peer = %peer.stage_id(), chooses_path = super::connection::automatic_dialer(runtime.host.local, peer.stage_id()), "Selecting authenticated RTC path");
    let preface = if !super::connection::automatic_dialer(runtime.host.local, peer.stage_id()) {
        Some(super::connection::read_p2p_preface(&mut channel).await?)
    } else {
        None
    };
    let lock = runtime.peer_lock(peer.stage_id());
    let _guard = lock.lock().await;
    if runtime.network_generation() != network_epoch {
        channel.close().await?;
        return Err(StageError::Unavailable(
            "Network changed during ICE negotiation",
        ));
    }
    if runtime.connected(peer.stage_id()) {
        channel.close().await?;
        return Ok(());
    }
    let identity = runtime.host.identity()?;
    let ledger =
        super::ledger::StageLedger::attach(runtime.host.system.store(), runtime.host.local);
    let result = super::connection::handshake_p2p(
        &mut channel,
        &identity,
        &ledger,
        attempt,
        chrono::Utc::now().timestamp_millis(),
        preface,
    )
    .await;
    let (generation, _) = match result {
        Ok(result) => result,
        Err(error) => {
            let _ = channel.close().await;
            return Err(error);
        }
    };
    if runtime.network_generation() != network_epoch {
        channel.close().await?;
        return Err(StageError::Unavailable(
            "Network changed during channel confirmation",
        ));
    }
    let check = peer.clone();
    let owner = runtime.clone();
    let framed = PeerChannel::p2p(peer, channel, move |frame, scope| {
        owner.authorize_send(&check, frame, scope)
    });
    runtime.attach(framed, generation, Route::P2p);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{actors::ActorSystem, config::Config, stage::host::Host};

    #[tokio::test]
    #[ignore = "requires DAAT_LOCUS_TEST_STAGE_SIGNALING with a WSS rendezvous"]
    async fn public_runtime_connects_enrolled_devices() {
        public_runtime_connects(false).await;
    }
    #[tokio::test]
    #[ignore = "requires DAAT_LOCUS_TEST_STAGE_SIGNALING with a WSS rendezvous"]
    async fn stale_lan_address_falls_back_to_public_runtime() {
        public_runtime_connects(true).await;
    }
    async fn public_runtime_connects(stale_lan: bool) {
        if std::env::var("DAAT_LOCUS_TEST_STAGE_TRACE").as_deref() == Ok("1") {
            let _ = tracing_subscriber::fmt()
                .with_env_filter("daat_locus::stage=debug,daat_locus::stage::transport::p2p=trace")
                .try_init();
        }
        let endpoint = std::env::var("DAAT_LOCUS_TEST_STAGE_SIGNALING").unwrap();
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let a = ActorSystem::open_account_gated(
            a_dir.path().join("actors"),
            a_dir.path().join("workspace"),
            Config::default(),
        )
        .await
        .unwrap();
        let b = ActorSystem::open_account_gated(
            b_dir.path().join("actors"),
            b_dir.path().join("workspace"),
            Config::default(),
        )
        .await
        .unwrap();
        let a_host = Host::load(a.clone()).unwrap();
        let b_host = Host::load(b.clone()).unwrap();
        a_host.initialize(true).unwrap();
        let request = b_host.initialize(false).unwrap();
        let own = a_host.enrollment().unwrap();
        b_host
            .join(
                a_host.enroll_authenticated_device(&request).unwrap(),
                &own.root.group_id,
            )
            .unwrap();
        for host in [&a_host, &b_host] {
            let mut settings = host.settings();
            settings.signaling_origin = Some(endpoint.clone());
            settings.interfaces = vec!["daat-locus-test-no-interface".into()];
            settings.label = if host.local == a_host.local {
                "Test desktop"
            } else {
                "Test phone"
            }
            .into();
            host.save_settings(settings).unwrap();
        }
        let a_runtime = Runtime::new(a_host.clone());
        let b_runtime = Runtime::new(b_host.clone());
        if stale_lan {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            drop(listener);
            if a_host.local < b_host.local {
                a_runtime.account_candidate(b_host.local, address);
            } else {
                b_runtime.account_candidate(a_host.local, address);
            }
        }
        a_runtime.start().await.unwrap();
        b_runtime.start().await.unwrap();
        let connected_result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if a_runtime.connected(b_host.local)
                    && b_runtime.connected(a_host.local)
                    && a_host.device_label(b_host.local).unwrap().as_deref() == Some("Test phone")
                    && b_host.device_label(a_host.local).unwrap().as_deref() == Some("Test desktop")
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await;
        let connected = a_runtime.connected(b_host.local) && b_runtime.connected(a_host.local);
        let routes = (
            a_runtime.state(b_host.local).route,
            b_runtime.state(a_host.local).route,
        );
        let errors = (
            a_runtime.last_error(b_host.local),
            b_runtime.last_error(a_host.local),
        );
        a_runtime.stop().await;
        b_runtime.stop().await;
        a.shutdown().await;
        b.shutdown().await;
        assert!(
            connected_result.is_ok() && connected,
            "public runtime did not connect; routes={routes:?}, errors={errors:?}"
        );
        assert_eq!(routes, (Some(Route::P2p), Some(Route::P2p)));
    }
}
