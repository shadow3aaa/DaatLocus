//! Bounded UDP traversal below ICE. The winning physical socket continues to
//! carry ICE/DTLS/SCTP; no second handshake or business protocol is introduced.
use parking_lot::{Mutex, RwLock};
use rtc_stun::{
    attributes::ATTR_USERNAME,
    integrity::MessageIntegrity,
    message::{Getter, Message},
    xoraddr::XorMappedAddress,
};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    future::Future,
    io::{self, IoSliceMut},
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    pin::Pin,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};
use tokio::sync::{Semaphore, watch};
use webrtc::{
    peer_connection::{RTCSessionDescription, crypto},
    runtime::{
        AsyncInterval, AsyncTcpListener, AsyncTcpStream, AsyncUdpSocket, JoinHandle, RecvMeta,
        Runtime, TokioRuntime, Transmit,
    },
};

const ATTRIBUTE: &str = "a=x-daat-punch:1 ";
const SOCKETS: usize = 128;
const PROBES: usize = 2048;
const DURATION: Duration = Duration::from_secs(12);
static ATTEMPTS: Semaphore = Semaphore::const_new(2);

#[derive(Clone)]
struct Hint {
    hard: bool,
    endpoints: Vec<SocketAddr>,
}
struct Credentials {
    ufrag: String,
    password: String,
}
#[derive(Clone)]
struct Probe {
    destination: SocketAddr,
    bytes: Arc<Vec<u8>>,
}
struct Description {
    hint: Hint,
    credentials: Credentials,
}
struct State {
    sockets: Vec<Weak<Socket>>,
    local: Option<(Arc<Socket>, Description)>,
    remote: Option<Description>,
    started: bool,
}

pub(super) struct Traversal {
    state: Mutex<State>,
    stop: watch::Sender<bool>,
    crypto: Arc<dyn crypto::RTCCryptoProvider>,
}
impl fmt::Debug for Traversal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UdpTraversal")
    }
}
impl Traversal {
    pub fn new() -> io::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            state: Mutex::new(State {
                sockets: vec![],
                local: None,
                remote: None,
                started: false,
            }),
            stop: watch::channel(false).0,
            crypto: crypto::default_provider().map_err(io::Error::other)?,
        }))
    }
    pub fn runtime(self: &Arc<Self>) -> Arc<dyn Runtime> {
        Arc::new(Network(self.clone()))
    }

    /// Called only with our gathered SDP and the peer's verified signed SDP.
    pub fn local(self: &Arc<Self>, sdp: &mut RTCSessionDescription) {
        let Some(credentials) = credentials(&sdp.sdp) else {
            return;
        };
        let mut state = self.state.lock();
        for socket in state.sockets.iter().filter_map(Weak::upgrade) {
            let mapped = socket.observations.lock();
            if mapped.len() < 2 {
                continue;
            }
            let mut endpoints = mapped.values().copied().collect::<Vec<_>>();
            endpoints.sort_unstable();
            endpoints.dedup();
            if endpoints.is_empty()
                || endpoints.len() > 4
                || endpoints.iter().any(|x| x.ip() != endpoints[0].ip())
            {
                continue;
            }
            if !endpoints
                .iter()
                .all(|x| sdp_endpoints(&sdp.sdp).contains(x))
            {
                continue;
            }
            let samples = mapped.len();
            drop(mapped);
            let hint = Hint {
                hard: endpoints.len() > 1,
                endpoints,
            };
            tracing::debug!(
                hard = hint.hard,
                samples,
                "UDP mapping classified for direct traversal"
            );
            let mode = if hint.hard { "h" } else { "e" };
            sdp.sdp.push_str(&format!(
                "{ATTRIBUTE}{mode} {}\r\n",
                hint.endpoints
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
            state.local = Some((socket, Description { hint, credentials }));
            break;
        }
        drop(state);
        self.start();
    }
    pub fn remote(self: &Arc<Self>, sdp: &RTCSessionDescription) {
        self.state.lock().remote = parse_hint(&sdp.sdp);
        self.start();
    }
    pub fn stop(&self) {
        self.stop.send_replace(true);
    }

    fn start(self: &Arc<Self>) {
        let mut state = self.state.lock();
        if state.started || *self.stop.borrow() {
            return;
        }
        let (Some((socket, local)), Some(remote)) = (&state.local, &state.remote) else {
            return;
        };
        // A single hard NAT is tractable. Do not multiply the search space when
        // both mappings vary, or spend probes on an already ordinary ICE path.
        if local.hint.hard == remote.hint.hard {
            return;
        }
        let socket = socket.clone();
        let hard = local.hint.hard;
        let destinations = remote.hint.endpoints.clone();
        socket.peer.write().replace(Peer {
            destination: destinations[0],
            request_username: format!("{}:{}", local.credentials.ufrag, remote.credentials.ufrag),
            local_password: local.credentials.password.clone(),
            remote_password: remote.credentials.password.clone(),
        });
        state.started = true;
        tracing::debug!(hard, "Starting bounded direct UDP traversal");
        let mut stop = self.stop.subscribe();
        drop(state);
        tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            let _release = Release(socket.clone());
            // A busy device must not permanently skip enhancement for its
            // third peer. Wait fairly within the same total attempt budget.
            let permit = tokio::select! {
                permit = ATTEMPTS.acquire() => match permit { Ok(permit) => permit, Err(_) => return },
                _ = tokio::time::sleep(DURATION) => return,
                _ = stop.wait_for(|stopped| *stopped) => return,
            };
            let _permit = permit;
            let work = socket.punch(hard, destinations);
            tokio::select! {
                _ = work => {},
                _ = tokio::time::sleep_until(started + DURATION) => {},
                _ = stop.wait_for(|stopped| *stopped) => {},
            }
            tracing::debug!(
                hard,
                elapsed_ms = started.elapsed().as_millis() as u64,
                physical_socket_selected = socket.route.read().is_some(),
                "Direct UDP traversal attempt settled"
            );
        });
    }
}

fn credentials(sdp: &str) -> Option<Credentials> {
    let value = |prefix| {
        let mut values = sdp
            .lines()
            .filter_map(|line| line.trim().strip_prefix(prefix));
        let first = values.next()?;
        (first.len() <= 256 && !first.is_empty() && values.all(|x| x == first))
            .then(|| first.to_owned())
    };
    Some(Credentials {
        ufrag: value("a=ice-ufrag:")?,
        password: value("a=ice-pwd:")?,
    })
}
fn public(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !matches!(a, 0 | 10 | 127)
        && a < 224
        && !(a == 100 && (64..=127).contains(&b))
        && !(a == 169 && b == 254)
        && !(a == 172 && (16..=31).contains(&b))
        && !(a == 192 && (b == 168 || (b == 0 && matches!(c, 0 | 2)) || (b == 88 && c == 99)))
        && !(a == 198 && (matches!(b, 18 | 19) || (b == 51 && c == 100)))
        && !(a == 203 && b == 0 && c == 113)
}
fn endpoint(value: &str) -> Option<SocketAddr> {
    let addr: SocketAddr = value.parse().ok()?;
    matches!(addr.ip(),IpAddr::V4(ip) if public(ip))
        .then_some(addr)
        .filter(|x| x.port() >= 1024)
}
fn sdp_endpoints(sdp: &str) -> HashSet<SocketAddr> {
    sdp.lines()
        .filter_map(|line| {
            let fields = line
                .strip_prefix("a=candidate:")?
                .split_whitespace()
                .collect::<Vec<_>>();
            if fields.len() < 8 || fields[2] != "udp" || fields[6] != "typ" || fields[7] != "srflx"
            {
                return None;
            }
            endpoint(&format!("{}:{}", fields[4], fields[5]))
        })
        .collect()
}
fn parse_hint(sdp: &str) -> Option<Description> {
    let mut hints = sdp
        .lines()
        .filter_map(|line| line.trim().strip_prefix(ATTRIBUTE));
    let hint = hints.next()?;
    if hints.next().is_some() {
        return None;
    }
    let fields = hint.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 2 || fields.len() > 5 {
        return None;
    }
    let hard = match fields[0] {
        "h" => true,
        "e" => false,
        _ => return None,
    };
    let endpoints = fields[1..]
        .iter()
        .map(|x| endpoint(x))
        .collect::<Option<Vec<_>>>()?;
    let candidates = sdp_endpoints(sdp);
    if endpoints
        .iter()
        .any(|x| !candidates.contains(x) || x.ip() != endpoints[0].ip())
        || endpoints.iter().collect::<HashSet<_>>().len() != endpoints.len()
        || hard != (endpoints.len() > 1)
    {
        return None;
    }
    Some(Description {
        hint: Hint { hard, endpoints },
        credentials: credentials(sdp)?,
    })
}

struct Peer {
    destination: SocketAddr,
    request_username: String,
    local_password: String,
    remote_password: String,
}
struct Socket {
    primary: Arc<dyn AsyncUdpSocket>,
    traversal: Weak<Traversal>,
    // Bounded discovery transactions keyed by the actual STUN destination.
    transactions: Mutex<HashMap<SocketAddr, [u8; 12]>>,
    observations: Mutex<HashMap<SocketAddr, SocketAddr>>,
    peer: RwLock<Option<Peer>>,
    seed: watch::Sender<Option<Probe>>,
    pool: RwLock<Vec<Arc<dyn AsyncUdpSocket>>>,
    route: RwLock<Option<Arc<dyn AsyncUdpSocket>>>,
    frozen: AtomicBool,
    wake: Mutex<Option<Waker>>,
}
impl fmt::Debug for Socket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TraversalSocket")
    }
}
impl Socket {
    fn new(primary: Arc<dyn AsyncUdpSocket>, traversal: &Arc<Traversal>) -> Arc<Self> {
        Arc::new(Self {
            primary,
            traversal: Arc::downgrade(traversal),
            transactions: Mutex::new(HashMap::new()),
            observations: Mutex::new(HashMap::new()),
            peer: RwLock::new(None),
            seed: watch::channel(None).0,
            pool: RwLock::new(vec![]),
            route: RwLock::new(None),
            frozen: AtomicBool::new(false),
            wake: Mutex::new(None),
        })
    }
    fn sent(&self, bytes: &[u8], target: SocketAddr) {
        let Some(message) = stun(bytes, 1) else {
            return;
        };
        if !message.contains(ATTR_USERNAME) {
            let mut requests = self.transactions.lock();
            if requests.len() < 8 || requests.contains_key(&target) {
                requests.insert(target, bytes[8..20].try_into().unwrap());
            }
        } else if self
            .peer
            .read()
            .as_ref()
            .is_some_and(|p| p.destination == target)
        {
            self.seed.send_if_modified(|seed| {
                if seed.is_some() {
                    return false;
                }
                *seed = Some(Probe {
                    destination: target,
                    bytes: Arc::new(bytes.to_vec()),
                });
                true
            });
        }
    }
    fn received(&self, bytes: &[u8], from: SocketAddr, socket: &Arc<dyn AsyncUdpSocket>) {
        if let Some(message) = stun(bytes, 0x101)
            && self
                .transactions
                .lock()
                .get(&from)
                .is_some_and(|id| bytes[8..20] == *id)
        {
            let mut mapped = XorMappedAddress::default();
            if mapped.get_from(&message).is_ok()
                && let IpAddr::V4(ip) = mapped.ip
                && public(ip)
                && mapped.port >= 1024
            {
                self.observations
                    .lock()
                    .insert(from, SocketAddr::new(mapped.ip, mapped.port));
            }
        }
        if self.frozen.load(Ordering::Relaxed) {
            return;
        }
        let peer = self.peer.read();
        let Some(peer) = peer.as_ref().filter(|p| p.destination == from) else {
            return;
        };
        let Some(traversal) = self.traversal.upgrade() else {
            return;
        };
        let authenticated = if let Some(mut request) = stun(bytes, 1) {
            if request.get(ATTR_USERNAME).ok().as_deref() != Some(peer.request_username.as_bytes())
            {
                return;
            }
            if MessageIntegrity::check(
                &mut request,
                peer.local_password.as_bytes(),
                traversal.crypto.crypto(),
            )
            .is_err()
            {
                return;
            }
            true
        } else if let Some(mut response) = stun(bytes, 0x101) {
            let seed = self.seed.borrow();
            if seed.as_ref().is_none_or(|p| bytes[8..20] != p.bytes[8..20]) {
                return;
            }
            if MessageIntegrity::check(
                &mut response,
                peer.remote_password.as_bytes(),
                traversal.crypto.crypto(),
            )
            .is_err()
            {
                return;
            }
            true
        } else {
            false
        };
        if authenticated {
            // Every pool member represents the same logical ICE candidate. Once
            // one authenticated path responds, stop the others immediately;
            // later checks on another member must not move DTLS/consent traffic
            // away from the physical endpoint the peer is about to nominate.
            self.route.write().get_or_insert_with(|| socket.clone());
            self.frozen.store(true, Ordering::Relaxed);
            traversal.stop();
        }
    }
    fn notify(&self) {
        if let Some(wake) = self.wake.lock().take() {
            wake.wake();
        }
    }
    fn release(&self) {
        self.frozen.store(true, Ordering::Relaxed);
        let route = self.route.read().clone();
        self.pool.write().retain(|socket| {
            route
                .as_ref()
                .is_some_and(|selected| Arc::ptr_eq(socket, selected))
        });
        tracing::debug!(
            selected = route.is_some(),
            retained = self.pool.read().len(),
            "Direct UDP traversal finished"
        );
        self.notify();
    }
    async fn punch(&self, hard: bool, destinations: Vec<SocketAddr>) {
        let mut seed = self.seed.subscribe();
        let Ok(probe) = seed
            .wait_for(|value| value.is_some())
            .await
            .map(|value| value.as_ref().unwrap().clone())
        else {
            return;
        };
        if hard {
            let Ok(local) = self.primary.local_addr() else {
                return;
            };
            let mut pool = Vec::new();
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            for round in 0..DURATION.as_secs() {
                interval.tick().await;
                let target = match round {
                    0 => 16,
                    1 => 64,
                    _ => SOCKETS,
                };
                while pool.len() < target {
                    let Ok(socket) = UdpSocket::bind(SocketAddr::new(local.ip(), 0)) else {
                        break;
                    };
                    let Ok(socket) = TokioRuntime.wrap_udp_socket(socket) else {
                        break;
                    };
                    pool.push(socket);
                }
                *self.pool.write() = pool.clone();
                self.notify();
                for socket in &pool {
                    // One congested socket must not stall all the others.
                    let _ = tokio::time::timeout(
                        Duration::from_millis(10),
                        socket.send_to(&probe.bytes, probe.destination),
                    )
                    .await;
                }
            }
        } else {
            let Ok(ports) = ports(&destinations) else {
                return;
            };
            let started = tokio::time::Instant::now();
            let mut last_probe = started - Duration::from_millis(2);
            for (index, port) in ports.into_iter().enumerate() {
                // Spend a small first stage on nearby mappings, then pace the
                // randomized search across the rest of the existing budget.
                let offset = if index < 128 {
                    Duration::from_millis(index as u64 * 2)
                } else {
                    Duration::from_millis(256)
                        + (DURATION - Duration::from_millis(256)) * (index as u32 - 128)
                            / (PROBES as u32 - 128)
                };
                tokio::time::sleep_until(
                    (started + offset).max(last_probe + Duration::from_millis(2)),
                )
                .await;
                last_probe = tokio::time::Instant::now();
                let target = SocketAddr::new(destinations[0].ip(), port);
                let _ = tokio::time::timeout(
                    Duration::from_millis(10),
                    self.primary.send_to(&probe.bytes, target),
                )
                .await;
            }
        }
    }
}
struct Release(Arc<Socket>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn stun(bytes: &[u8], kind: u16) -> Option<Message> {
    if bytes.len() < 20
        || bytes.len() > 1500
        || bytes[..2] != kind.to_be_bytes()
        || bytes[4..8] != [0x21, 0x12, 0xa4, 0x42]
        || u16::from_be_bytes([bytes[2], bytes[3]]) as usize + 20 != bytes.len()
    {
        return None;
    }
    let mut message = Message {
        raw: bytes.to_vec(),
        ..Default::default()
    };
    message.decode().ok()?;
    Some(message)
}
fn ports(endpoints: &[SocketAddr]) -> io::Result<Vec<u16>> {
    let mut ports = Vec::with_capacity(PROBES);
    let mut seen = HashSet::new();
    // Try bounded neighbours first; samples do not justify assuming a fixed
    // allocation stride. The remaining budget covers randomized mappings.
    for distance in 0..=32u16 {
        for endpoint in endpoints {
            for port in [
                endpoint.port().checked_add(distance),
                endpoint.port().checked_sub(distance),
            ]
            .into_iter()
            .flatten()
            {
                if port >= 1024 && seen.insert(port) {
                    ports.push(port);
                }
            }
        }
    }
    while ports.len() < PROBES {
        let mut random = [0u8; 512];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        for bytes in random.as_chunks::<2>().0 {
            let port = u16::from_le_bytes([bytes[0], bytes[1]]);
            if port >= 1024 && seen.insert(port) {
                ports.push(port);
            }
            if ports.len() == PROBES {
                break;
            }
        }
    }
    Ok(ports)
}
impl AsyncUdpSocket for Socket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.primary.local_addr()
    }
    fn max_gso_segments(&self) -> usize {
        self.route
            .read()
            .as_ref()
            .unwrap_or(&self.primary)
            .max_gso_segments()
    }
    fn max_gro_segments(&self) -> usize {
        self.pool
            .read()
            .iter()
            .fold(self.primary.max_gro_segments(), |max, socket| {
                max.max(socket.max_gro_segments())
            })
    }
    fn poll_send(&self, cx: &mut Context<'_>, transmit: &Transmit<'_>) -> Poll<io::Result<usize>> {
        self.sent(transmit.contents, transmit.destination);
        let selected = self
            .peer
            .read()
            .as_ref()
            .is_some_and(|p| p.destination == transmit.destination);
        let route = self.route.read();
        route
            .as_ref()
            .filter(|_| selected)
            .unwrap_or(&self.primary)
            .poll_send(cx, transmit)
    }
    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        {
            let mut wake = self.wake.lock();
            if wake.as_ref().is_none_or(|w| !w.will_wake(cx.waker())) {
                *wake = Some(cx.waker().clone());
            }
        }
        let sockets = self.pool.read();
        for socket in std::iter::once(&self.primary).chain(sockets.iter()) {
            match socket.poll_recv(cx, bufs, meta) {
                Poll::Ready(Ok(n)) => {
                    for (buf, meta) in bufs.iter().zip(meta.iter()).take(n) {
                        for bytes in buf[..meta.len].chunks(meta.stride.max(1)) {
                            self.received(bytes, meta.addr, socket);
                        }
                    }
                    return Poll::Ready(Ok(n));
                }
                Poll::Ready(Err(error)) if Arc::ptr_eq(socket, &self.primary) => {
                    return Poll::Ready(Err(error));
                }
                _ => {}
            }
        }
        Poll::Pending
    }
}

#[derive(Debug)]
struct Network(Arc<Traversal>);
impl Runtime for Network {
    fn spawn(&self, task: Pin<Box<dyn Future<Output = ()> + Send>>) -> Box<dyn JoinHandle> {
        TokioRuntime.spawn(task)
    }
    fn spawn_reactor(
        &self,
        count: usize,
        task: Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> Box<dyn JoinHandle> {
        TokioRuntime.spawn_reactor(count, task)
    }
    fn wrap_udp_socket(&self, socket: UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        let local = socket.local_addr()?;
        let primary = TokioRuntime.wrap_udp_socket(socket)?;
        if !local.is_ipv4()
            || local.ip().is_loopback()
            || local.ip().is_unspecified()
            || local.port() == 5353
        {
            return Ok(primary);
        }
        let socket = Socket::new(primary, &self.0);
        self.0.state.lock().sockets.push(Arc::downgrade(&socket));
        Ok(socket)
    }
    fn wrap_tcp_listener(
        &self,
        listener: std::net::TcpListener,
    ) -> io::Result<Arc<dyn AsyncTcpListener>> {
        TokioRuntime.wrap_tcp_listener(listener)
    }
    fn connect_tcp<'a>(
        &'a self,
        addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = io::Result<Arc<dyn AsyncTcpStream>>> + Send + 'a>> {
        TokioRuntime.connect_tcp(addr)
    }
    fn resolve_host<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send + 'a>> {
        TokioRuntime.resolve_host(host)
    }
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        TokioRuntime.sleep(duration)
    }
    fn interval(&self, duration: Duration) -> Box<dyn AsyncInterval> {
        TokioRuntime.interval(duration)
    }
    fn block_on(&self, task: Pin<Box<dyn Future<Output = ()> + '_>>) {
        TokioRuntime.block_on(task);
    }
    fn yield_now(&self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        TokioRuntime.yield_now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtc_stun::{
        message::{BINDING_REQUEST, BINDING_SUCCESS, TransactionId},
        textattrs::Username,
    };

    fn request(traversal: &Traversal, username: &str, password: &str) -> Vec<u8> {
        let mut message = Message::default();
        message
            .build(&[
                Box::new(BINDING_REQUEST),
                Box::new(TransactionId::new()),
                Box::new(Username::new(ATTR_USERNAME, username.to_owned())),
                Box::new(MessageIntegrity::new_short_term_integrity_with_provider(
                    password.to_owned(),
                    traversal.crypto.crypto(),
                )),
            ])
            .unwrap();
        message.raw
    }
    fn description(mode: &str, endpoints: &[&str]) -> String {
        format!(
            "a=ice-ufrag:peer\r\na=ice-pwd:ephemeral-password\r\na=candidate:1 1 udp 1 8.8.8.8 20000 typ srflx raddr 10.0.0.2 rport 30000\r\na=candidate:2 1 udp 1 8.8.8.8 20001 typ srflx raddr 10.0.0.2 rport 30000\r\n{ATTRIBUTE}{mode} {}\r\n",
            endpoints.join(" ")
        )
    }
    #[test]
    fn peer_cannot_expand_probes_beyond_its_signed_public_candidates() {
        let valid = description("h", &["8.8.8.8:20000", "8.8.8.8:20001"]);
        assert!(parse_hint(&valid).is_some());
        for invalid in [
            description("h", &["8.8.8.8:20000", "8.8.8.8:21000"]),
            description("h", &["8.8.8.8:20000", "1.1.1.1:20001"]),
            description("h", &["8.8.8.8:20000", "8.8.8.8:20000"]),
            description("h", &["8.8.8.8:20000"]),
            description("e", &["127.0.0.1:20000"]),
            format!("{valid}{ATTRIBUTE}e 8.8.8.8:20000\r\n"),
            format!("{valid}a=ice-pwd:another-transport\r\n"),
        ] {
            assert!(
                parse_hint(&invalid).is_none(),
                "accepted an invalid traversal scope"
            );
        }
    }
    #[test]
    fn port_predictions_are_unique_bounded_and_do_not_wrap_into_services() {
        let sequence = ports(&[
            "8.8.8.8:65530".parse().unwrap(),
            "8.8.8.8:1024".parse().unwrap(),
        ])
        .unwrap();
        assert!(sequence.contains(&65531) && sequence.contains(&1025));
        assert!(sequence.iter().all(|port| *port >= 1024));
        assert_eq!(
            sequence.len(),
            sequence.iter().collect::<HashSet<_>>().len()
        );
        assert!(sequence.len() <= PROBES);
    }
    #[tokio::test]
    async fn mapping_classification_requires_two_correlated_stun_destinations() {
        for (second_port, hard) in [(20000, false), (20001, true)] {
            let traversal = Traversal::new().unwrap();
            let primary = TokioRuntime
                .wrap_udp_socket(UdpSocket::bind("127.0.0.1:0").unwrap())
                .unwrap();
            let socket = Socket::new(primary, &traversal);
            traversal.state.lock().sockets.push(Arc::downgrade(&socket));
            let base = description("h", &["8.8.8.8:20000", "8.8.8.8:20001"])
                .lines()
                .filter(|line| !line.starts_with(ATTRIBUTE))
                .map(|line| format!("{line}\r\n"))
                .collect::<String>();
            let mut bytes = [0u8; 1500];
            for (index, port) in [20000, second_port].into_iter().enumerate() {
                let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
                let mut request = Message::default();
                request
                    .build(&[Box::new(BINDING_REQUEST), Box::new(TransactionId::new())])
                    .unwrap();
                socket
                    .send_to(&request.raw, server.local_addr().unwrap())
                    .await
                    .unwrap();
                let (_, source) =
                    tokio::time::timeout(Duration::from_secs(2), server.recv_from(&mut bytes))
                        .await
                        .unwrap()
                        .unwrap();
                let mut response = Message::default();
                let mapped = || XorMappedAddress {
                    ip: "8.8.8.8".parse().unwrap(),
                    port,
                };
                response
                    .build(&[
                        Box::new(BINDING_SUCCESS),
                        Box::new(TransactionId::new()),
                        Box::new(mapped()),
                    ])
                    .unwrap();
                server.send_to(&response.raw, source).await.unwrap();
                tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut bytes))
                    .await
                    .unwrap()
                    .unwrap();
                let observations = socket.observations.lock().len();
                assert_eq!(
                    observations, index,
                    "unrelated transactions cannot classify a mapping"
                );
                response
                    .build(&[
                        Box::new(BINDING_SUCCESS),
                        Box::new(request.transaction_id),
                        Box::new(mapped()),
                    ])
                    .unwrap();
                server.send_to(&response.raw, source).await.unwrap();
                tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut bytes))
                    .await
                    .unwrap()
                    .unwrap();
                let mut sdp = RTCSessionDescription::default();
                sdp.sdp = base.clone();
                traversal.local(&mut sdp);
                if index == 0 {
                    assert!(parse_hint(&sdp.sdp).is_none());
                } else {
                    assert_eq!(parse_hint(&sdp.sdp).unwrap().hint.hard, hard);
                }
            }
        }
    }
    #[tokio::test]
    async fn authenticated_winning_socket_survives_pool_cleanup_and_forged_packets() {
        let traversal = Traversal::new().unwrap();
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let destination = server.local_addr().unwrap();
        let primary = TokioRuntime
            .wrap_udp_socket(UdpSocket::bind("127.0.0.1:0").unwrap())
            .unwrap();
        let primary_addr = primary.local_addr().unwrap();
        let socket = Socket::new(primary, &traversal);
        socket.peer.write().replace(Peer {
            destination,
            request_username: "local:peer".into(),
            local_password: "local-password".into(),
            remote_password: "peer-password".into(),
        });
        socket.sent(
            &request(&traversal, "peer:local", "peer-password"),
            destination,
        );
        let owner = socket.clone();
        let worker = tokio::spawn(async move {
            let _release = Release(owner.clone());
            owner.punch(true, vec![destination]).await;
        });
        let mut bytes = [0u8; 1500];
        let (_, winning_addr) =
            tokio::time::timeout(Duration::from_secs(2), server.recv_from(&mut bytes))
                .await
                .unwrap()
                .unwrap();
        assert_ne!(winning_addr, primary_addr);
        for (password, accepted) in [("wrong-password", false), ("local-password", true)] {
            server
                .send_to(&request(&traversal, "local:peer", password), winning_addr)
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut bytes))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(socket.route.read().is_some(), accepted);
        }
        server
            .send_to(
                &request(&traversal, "local:peer", "local-password"),
                primary_addr,
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        worker.abort();
        let _ = worker.await;
        assert_eq!(
            socket.pool.read().len(),
            1,
            "unused NAT mappings should be released"
        );
        socket
            .send_to(b"after traversal", destination)
            .await
            .unwrap();
        let source = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let (n, source) = server.recv_from(&mut bytes).await.unwrap();
                if &bytes[..n] == b"after traversal" {
                    break source;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            source, winning_addr,
            "reopening or switching sockets destroys the punched mapping"
        );
    }
}
