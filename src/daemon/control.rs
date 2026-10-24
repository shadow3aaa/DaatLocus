//! Profile-local process control. OS credentials authorize IPC; no browser token or HTTP route.
#[cfg(unix)]
use crate::actors::permissions::protect_private_path;
use miette::{IntoDiagnostic, Result, miette};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{Semaphore, watch},
};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LaunchOptions {
    pub profile: Option<String>,
    pub listen: SocketAddr,
    pub workspace: PathBuf,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Running,
    Stopping,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub instance: Uuid,
    pub pid: u32,
    pub process_started: u64,
    pub started_at_ms: i64,
    pub executable: PathBuf,
    /// Captured from the process image before service startup, never re-read from
    /// an installation path which an updater may already have replaced.
    #[serde(default)]
    pub executable_sha256: Option<String>,
    pub namespace: PathBuf,
    pub root: PathBuf,
    pub url: String,
    pub options: LaunchOptions,
    pub state: State,
}
impl Status {
    pub fn matches_current_executable(&self) -> Result<bool> {
        let current = std::env::current_exe().into_diagnostic()?;
        if self.executable.canonicalize().into_diagnostic()?
            != current.canonicalize().into_diagnostic()?
        {
            return Err(miette!("Service belongs to another installation"));
        }
        Ok(self.executable_sha256.as_deref() == Some(current_executable_sha256()?))
    }
}

/// Call before initializing the service so an update cannot change the recorded
/// image identity during startup. CLI start uses the same identity for upgrades,
/// including rebuilds which retain the same package version.
pub fn current_executable_sha256() -> Result<&'static str> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    static HASH: std::sync::OnceLock<std::io::Result<String>> = std::sync::OnceLock::new();
    HASH.get_or_init(|| {
        let mut file = std::fs::File::open(std::env::current_exe()?)?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        Ok(hex::encode(hash.finalize()))
    })
    .as_ref()
    .map(String::as_str)
    .map_err(|error| miette!("Cannot identify service executable: {error}"))
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Status,
    Stop,
    Bootstrap(daat_locus_client_protocol::BootstrapRequest),
    WebHost,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    instance: Uuid,
    action: Action,
}

fn metadata(root: &Path) -> PathBuf {
    root.join("runtime/daemon.json")
}
fn process_identity(pid: u32) -> Option<(u64, PathBuf)> {
    let system = sysinfo::System::new_all();
    let process = system.process(sysinfo::Pid::from_u32(pid))?;
    Some((process.start_time(), process.exe()?.to_path_buf()))
}
fn alive(record: &Status) -> bool {
    // Installers can move a live image into rollback storage. Its path is not a
    // stable process identity; IPC separately verifies the peer's OS owner and PID.
    let system = sysinfo::System::new_all();
    system
        .process(sysinfo::Pid::from_u32(record.pid))
        .is_some_and(|process| process.start_time() == record.process_started)
}
pub fn load(root: &Path) -> Result<Option<Status>> {
    let path = metadata(root);
    if !path.exists() {
        return Ok(None);
    }
    let mut policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
    policy.filesystem.full_disk_read = false;
    let canonical = std::fs::canonicalize(root).into_diagnostic()?;
    policy.filesystem.readable_roots = vec![canonical.clone()];
    policy.filesystem.bound_roots = vec![canonical];
    let bytes = crate::file_io::read_authorized_bytes(&path, 16384, &policy)?;
    let record: Status = serde_json::from_slice(&bytes).into_diagnostic()?;
    if record.root != root || record.instance.is_nil() {
        return Err(miette!("Daemon metadata belongs to a different profile"));
    }
    Ok(alive(&record).then_some(record))
}
fn endpoint(record: &Status) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(format!(r"\\.\pipe\DaatLocus-{}", record.instance.simple()))
    }
    #[cfg(unix)]
    {
        // UNIX sun_path has a small platform limit; private endpoint names stay short.
        crate::sandbox::unix_control_directory().join(format!("{}.sock", record.instance.simple()))
    }
}
pub struct Server {
    status: Status,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    pub async fn bind(
        namespace: PathBuf,
        root: PathBuf,
        options: LaunchOptions,
        url: String,
        bootstrap: Option<crate::service::ServiceState>,
    ) -> Result<Self> {
        let (process_started, executable) = process_identity(std::process::id())
            .ok_or_else(|| miette!("Cannot identify daemon process"))?;
        let status = Status {
            instance: Uuid::new_v4(),
            pid: std::process::id(),
            process_started,
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            executable,
            executable_sha256: Some(current_executable_sha256()?.to_owned()),
            namespace,
            root,
            url,
            options,
            state: State::Running,
        };
        let listener = platform::bind(&status)?;
        let (stop, _) = watch::channel(false);
        let record = Arc::new(status.clone());
        let stopping = stop.clone();
        let budget = Arc::new(Semaphore::new(16));
        let task = tokio::spawn(async move {
            let mut listener = listener;
            loop {
                let permit = match budget.clone().acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => break,
                };
                let stream = match platform::accept(&mut listener).await {
                    Ok(stream) => stream,
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let record = record.clone();
                let stop = stopping.clone();
                let bootstrap = bootstrap.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = respond(stream, &record, stop, bootstrap).await;
                });
            }
        });
        let server = Self { status, stop, task };
        crate::persistence::write_json_pretty_atomic_sync(
            &metadata(&server.status.root),
            &server.status,
            crate::persistence::PersistenceFileMode::Private,
        )?;
        Ok(server)
    }
    pub fn stopping(&self) -> watch::Receiver<bool> {
        self.stop.subscribe()
    }
    pub fn status(&self) -> &Status {
        &self.status
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        self.task.abort();
        if load(&self.status.root)
            .ok()
            .flatten()
            .is_some_and(|record| record.instance == self.status.instance)
        {
            let _ = std::fs::remove_file(metadata(&self.status.root));
        }
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(endpoint(&self.status));
        }
    }
}
async fn respond<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    status: &Status,
    stop: watch::Sender<bool>,
    bootstrap: Option<crate::service::ServiceState>,
) -> Result<()> {
    let bytes = zeroize::Zeroizing::new(
        tokio::time::timeout(Duration::from_secs(30), read_frame(&mut stream, 64 * 1024))
            .await
            .map_err(|_| miette!("Control handshake timed out"))??,
    );
    let request: Request = serde_json::from_slice(&bytes).into_diagnostic()?;
    if request.instance != status.instance {
        return Err(miette!("Daemon instance changed"));
    }
    if matches!(request.action, Action::WebHost) {
        if *stop.borrow() {
            return Err(miette!("Service is stopping"));
        }
        let state = bootstrap.ok_or_else(|| miette!("Native Web host unavailable"))?;
        return serve_web_host(stream, state.system.web_host(), stop.subscribe()).await;
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        if let Action::Bootstrap(input) = request.action {
            if *stop.borrow() {
                return Err(miette!("Service is stopping"));
            }
            let state = bootstrap.ok_or_else(|| miette!("Account bootstrap unavailable"))?;
            let reply = crate::service::native_bootstrap(state, &status.url, input).await?;
            write_frame(&mut stream, &serde_json::to_vec(&reply).into_diagnostic()?).await?;
            return stream.shutdown().await.into_diagnostic();
        }
        let mut reply = status.clone();
        if matches!(request.action, Action::Stop) {
            stop.send_replace(true);
        }
        if *stop.borrow() {
            reply.state = State::Stopping;
        }
        write_frame(&mut stream, &serde_json::to_vec(&reply).into_diagnostic()?).await?;
        stream.shutdown().await.into_diagnostic()
    })
    .await
    .map_err(|_| miette!("Control operation timed out"))?
}
async fn read_frame<S: AsyncRead + Unpin>(stream: &mut S, limit: usize) -> Result<Vec<u8>> {
    let length = stream.read_u32().await.into_diagnostic()? as usize;
    if length == 0 || length > limit {
        return Err(miette!("Control message exceeds its limit"));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await.into_diagnostic()?;
    Ok(bytes)
}
async fn write_frame<S: AsyncWrite + Unpin>(stream: &mut S, bytes: &[u8]) -> Result<()> {
    stream
        .write_u32(bytes.len() as u32)
        .await
        .into_diagnostic()?;
    stream.write_all(bytes).await.into_diagnostic()?;
    stream.flush().await.into_diagnostic()
}
pub async fn query(root: &Path, action: Action) -> Result<Option<Status>> {
    if matches!(action, Action::Bootstrap(_) | Action::WebHost) {
        return Err(miette!("Use the account bootstrap dispatcher"));
    }
    let Some(record) = load(root)? else {
        return Ok(None);
    };
    let reply = tokio::time::timeout(Duration::from_secs(3), async {
        let mut stream = platform::connect(&record).await?;
        write_frame(
            &mut stream,
            &serde_json::to_vec(&Request {
                instance: record.instance,
                action,
            })
            .into_diagnostic()?,
        )
        .await?;
        let bytes = read_frame(&mut stream, 16384).await?;
        let reply: Status = serde_json::from_slice(&bytes).into_diagnostic()?;
        if reply.instance != record.instance || reply.root != record.root || reply.pid != record.pid
        {
            return Err(miette!("Daemon endpoint identity changed"));
        }
        Ok::<_, miette::Report>(reply)
    })
    .await
    .map_err(|_| miette!("Daemon control timed out"))??;
    Ok(Some(reply))
}
async fn serve_web_host<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    host: Arc<crate::web_host::Host>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let mut attachment = host.attach()?;
    let (mut reader, mut writer) = tokio::io::split(stream);
    loop {
        let pending = tokio::select! {
            _ = stop.changed() => return Ok(()),
            unexpected = read_frame(&mut reader, 1024) => { unexpected?; return Err(miette!("Unexpected native Web frame")); },
            pending = attachment.requests.recv() => match pending {Some(pending)=>pending,None=>return Ok(())},
        };
        if pending.reply.is_closed() {
            continue;
        }
        let result = async {
            write_frame(&mut writer, &serde_json::to_vec(&pending.request).into_diagnostic()?).await?;
            let bytes = tokio::select! {
                _ = stop.changed() => return Err(miette!("Service is stopping")),
                response = tokio::time::timeout(Duration::from_secs(32),read_frame(&mut reader,12*1024*1024)) => response.map_err(|_|miette!("Native Web host timed out; do not replay navigation"))??,
            };
            let response=serde_json::from_slice(&bytes).into_diagnostic()?;
            crate::web_host::validate_reply(&pending.request,&response)?;
            Ok(response)
        }.await;
        let failed = result.is_err();
        let _ = pending
            .reply
            .send(result.and_then(|response| crate::web_host::resolve(&pending.request, response)));
        if failed {
            return Err(miette!(
                "Native Web host operation failed; reconnect without replay"
            ));
        }
    }
}
/// An authenticated native stdio bridge, never an HTTP or WebView command.
pub async fn web_host(root: &Path) -> Result<()> {
    let record = load(root)?.ok_or_else(|| miette!("Service is not running"))?;
    let mut stream = platform::connect(&record).await?;
    write_frame(
        &mut stream,
        &serde_json::to_vec(&Request {
            instance: record.instance,
            action: Action::WebHost,
        })
        .into_diagnostic()?,
    )
    .await?;
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    tokio::select! {
        result=tokio::io::copy(&mut stdin,&mut writer)=>{result.into_diagnostic()?;},
        result=tokio::io::copy(&mut reader,&mut stdout)=>{result.into_diagnostic()?;},
    }
    Ok(())
}
pub async fn bootstrap(
    root: &Path,
    input: daat_locus_client_protocol::BootstrapRequest,
) -> Result<daat_locus_client_protocol::BootstrapReply> {
    let record = load(root)?.ok_or_else(|| miette!("Service is not running"))?;
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut stream = platform::connect(&record).await?;
        let bytes = zeroize::Zeroizing::new(
            serde_json::to_vec(&Request {
                instance: record.instance,
                action: Action::Bootstrap(input),
            })
            .into_diagnostic()?,
        );
        if bytes.len() > 64 * 1024 {
            return Err(miette!("Bootstrap request too large"));
        }
        write_frame(&mut stream, &bytes).await?;
        serde_json::from_slice(&read_frame(&mut stream, 64 * 1024).await?).into_diagnostic()
    })
    .await
    .map_err(|_| miette!("Account bootstrap timed out"))?
}
pub async fn wait_ready(root: &Path) -> Result<Status> {
    for _ in 0..300 {
        if let Ok(Some(record)) = query(root, Action::Status).await {
            return Ok(record);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(miette!(
        "Daemon did not become ready; inspect its profile log"
    ))
}
pub async fn stop(root: &Path) -> Result<()> {
    let Some(record) = query(root, Action::Stop).await? else {
        return Ok(());
    };
    for _ in 0..600 {
        if !alive(&record) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(miette!("Daemon did not finish orderly shutdown"))
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    pub type Listener = tokio::net::UnixListener;
    pub fn bind(record: &Status) -> Result<Listener> {
        let path = endpoint(record);
        let parent = path.parent().expect("socket parent");
        if let Ok(info) = std::fs::symlink_metadata(parent)
            && (info.file_type().is_symlink() || info.uid() != unsafe { libc::geteuid() })
        {
            return Err(miette!("Control directory has a different OS owner"));
        }
        std::fs::create_dir_all(parent).into_diagnostic()?;
        protect_private_path(parent)?;
        let listener = Listener::bind(&path).into_diagnostic()?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).into_diagnostic()?;
        Ok(listener)
    }
    fn authorized(stream: &tokio::net::UnixStream) -> bool {
        let Ok(peer) = stream.peer_cred() else {
            return false;
        };
        if peer.uid() != unsafe { libc::geteuid() } {
            return false;
        }
        #[cfg(target_os = "linux")]
        {
            let Some(pid) = peer.pid() else {
                return false;
            };
            // Sandboxed App processes inhabit a different user namespace.
            matches!((std::fs::read_link(format!("/proc/{pid}/ns/user")), std::fs::read_link("/proc/self/ns/user")), (Ok(peer), Ok(own)) if peer == own)
        }
        #[cfg(not(target_os = "linux"))]
        {
            true
        }
    }
    pub async fn accept(listener: &mut Listener) -> Result<tokio::net::UnixStream> {
        let (stream, _) = listener.accept().await.into_diagnostic()?;
        if !authorized(&stream) {
            return Err(miette!("Control requires the local OS owner"));
        }
        Ok(stream)
    }
    pub async fn connect(record: &Status) -> Result<tokio::net::UnixStream> {
        let stream = tokio::net::UnixStream::connect(endpoint(record))
            .await
            .into_diagnostic()?;
        if !authorized(&stream) {
            return Err(miette!("Daemon has a different OS owner"));
        }
        if stream
            .peer_cred()
            .into_diagnostic()?
            .pid()
            .is_some_and(|pid| pid as u32 != record.pid)
        {
            return Err(miette!("Daemon endpoint identity changed"));
        }
        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn native_web_control_outlives_handshake_deadline_and_stops_with_server() {
        use daat_locus_client_protocol::web::{Operation, Reply};
        let temp = tempfile::tempdir().unwrap();
        let system = crate::actors::ActorSystem::open_account_gated(
            temp.path().join("actors"),
            temp.path().join("workspace"),
            crate::config::Config::default(),
        )
        .await
        .unwrap();
        let url = "http://127.0.0.1:41000".to_owned();
        let state = crate::service::ServiceState::new(system.clone(), url.clone()).unwrap();
        let server = Server::bind(
            temp.path().into(),
            temp.path().into(),
            LaunchOptions {
                profile: None,
                listen: "127.0.0.1:41000".parse().unwrap(),
                workspace: temp.path().join("workspace"),
            },
            url,
            Some(state),
        )
        .await
        .unwrap();
        let mut native = platform::connect(server.status()).await.unwrap();
        write_frame(
            &mut native,
            &serde_json::to_vec(&Request {
                instance: server.status().instance,
                action: Action::WebHost,
            })
            .unwrap(),
        )
        .await
        .unwrap();
        let host = system.web_host();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !host.available() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(31)).await;
        tokio::task::yield_now().await;
        assert!(
            host.available(),
            "Authenticated native Web hosting is a persistent stream, not a single control reply"
        );
        tokio::time::resume();
        let caller = host.clone();
        let call =
            tokio::spawn(async move { caller.request("a".repeat(64), Operation::Close).await });
        let command: daat_locus_client_protocol::web::Request =
            serde_json::from_slice(&read_frame(&mut native, 65536).await.unwrap()).unwrap();
        write_frame(
            &mut native,
            &serde_json::to_vec(&Reply {
                id: command.id,
                capture: None,
                browser: None,
                error: None,
            })
            .unwrap(),
        )
        .await
        .unwrap();
        assert!(call.await.unwrap().unwrap().is_none());
        drop(server);
        tokio::time::timeout(Duration::from_secs(2), async {
            while host.available() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        system.shutdown().await;
    }
    #[tokio::test]
    async fn web_host_page_failures_preserve_connection_but_mismatched_replies_disconnect() {
        use daat_locus_client_protocol::web::{Operation, Reply, Request};
        let host = Arc::new(crate::web_host::Host::default());
        let (server, mut native) = tokio::io::duplex(65536);
        let (_stop, receiver) = watch::channel(false);
        let bridge = tokio::spawn(serve_web_host(server, host.clone(), receiver));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !host.available() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        for wrong_id in [false, true] {
            let caller = host.clone();
            let call = tokio::spawn(async move {
                caller
                    .request(
                        "a".repeat(64),
                        Operation::Load {
                            url: "https://example.com".into(),
                        },
                    )
                    .await
            });
            let request: Request =
                serde_json::from_slice(&read_frame(&mut native, 65536).await.unwrap()).unwrap();
            let reply = Reply {
                id: if wrong_id { "0".repeat(32) } else { request.id },
                capture: None,
                browser: None,
                error: Some("Page unavailable".into()),
            };
            write_frame(&mut native, &serde_json::to_vec(&reply).unwrap())
                .await
                .unwrap();
            assert!(call.await.unwrap().is_err());
            if !wrong_id {
                assert!(
                    host.available(),
                    "A website error must not close other pages"
                );
            }
        }
        assert!(bridge.await.unwrap().is_err());
        assert!(!host.available());
    }
    use super::*;
    #[tokio::test]
    async fn native_bootstrap_proves_os_owner_but_never_creates_a_session_or_edits_an_account() {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
        use daat_locus_client_protocol::BootstrapRequest as Operation;
        let temp = tempfile::tempdir().unwrap();
        let system = crate::actors::ActorSystem::open_account_gated(
            temp.path().join("actors"),
            temp.path().join("workspace"),
            crate::config::Config::default(),
        )
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}");
        // No HTTP socket owner proof is available; only the native IPC path may bootstrap.
        let state = crate::service::ServiceState::new(system.clone(), url.clone()).unwrap();
        // Discovery may already have created a device identity without joining an account.
        state
            .stage
            .as_ref()
            .unwrap()
            .host
            .initialize(false)
            .unwrap();
        let server = Server::bind(
            temp.path().into(),
            temp.path().into(),
            LaunchOptions {
                profile: None,
                listen: address,
                workspace: temp.path().join("workspace"),
            },
            url.clone(),
            Some(state.clone()),
        )
        .await
        .unwrap();
        let http = tokio::spawn(async move {
            axum::serve(
                listener,
                crate::service::router(state)
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = crate::http_client::builder().build().expect("HTTP client");
        let public: serde_json::Value = client
            .get(format!("{url}/v1/account"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(public["can_create"], false);
        let native = bootstrap(temp.path(), Operation::Info {}).await.unwrap();
        assert_eq!(native.status, 200);
        assert_eq!(native.body["can_create"], true);
        let registration =
            daat_locus_auth::RegistrationClient::start(b"native fixture password".to_vec())
                .unwrap();
        let request = B64.encode(registration.request());
        let forbidden = client
            .post(format!("{url}/v1/account/registration/start"))
            .header("origin", &url)
            .header("x-daat-request", "1")
            .header("x-daat-native-owner", "1")
            .json(&serde_json::json!({"name":"native-fixture","request":request}))
            .send()
            .await
            .unwrap();
        assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);
        let start = bootstrap(
            temp.path(),
            Operation::RegisterStart {
                name: "native-fixture".into(),
                request,
            },
        )
        .await
        .unwrap();
        assert_eq!(start.status, 200);
        let proof = registration
            .finish(
                &B64.decode(start.body["response"].as_str().unwrap())
                    .unwrap(),
            )
            .unwrap();
        let finish = bootstrap(
            temp.path(),
            Operation::RegisterFinish {
                id: start.body["id"].as_str().unwrap().into(),
                message: B64.encode(proof),
            },
        )
        .await
        .unwrap();
        assert_eq!(finish.status, 204);
        assert!(finish.body.is_null());
        assert_eq!(
            bootstrap(temp.path(), Operation::Connection { relay_origin: None })
                .await
                .unwrap()
                .status,
            403
        );
        assert_eq!(
            bootstrap(
                temp.path(),
                Operation::RegisterStart {
                    name: "other".into(),
                    request: "invalid".into()
                }
            )
            .await
            .unwrap()
            .status,
            403
        );
        assert_eq!(
            client
                .get(format!("{url}/v1/access"))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
        http.abort();
        drop(server);
        system.shutdown().await;
    }
    #[tokio::test]
    async fn local_control_checks_instance_profile_and_owner_and_only_stops_explicitly() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("profiles/test");
        std::fs::create_dir_all(root.join("runtime")).unwrap();
        let options = LaunchOptions {
            profile: Some("test".into()),
            listen: "127.0.0.1:41000".parse().unwrap(),
            workspace: dir.path().join("workspace"),
        };
        let server = Server::bind(
            dir.path().to_path_buf(),
            root.clone(),
            options,
            "http://127.0.0.1:41000".into(),
            None,
        )
        .await
        .unwrap();
        let mut stopped = server.stopping();
        let reply = query(&root, Action::Status).await.unwrap().unwrap();
        assert_eq!(reply.instance, server.status.instance);
        assert!(reply.matches_current_executable().unwrap());
        // Old releases have no startup fingerprint. A pathname alone must not
        // make them eligible for reuse after an installer replaces that file.
        let mut legacy = serde_json::to_value(&reply).unwrap();
        legacy.as_object_mut().unwrap().remove("executable_sha256");
        let legacy: Status = serde_json::from_value(legacy).unwrap();
        assert!(!legacy.matches_current_executable().unwrap());
        let mut another_installation = reply.clone();
        another_installation.executable = dir.path().join("another-installation");
        std::fs::write(&another_installation.executable, b"another installation").unwrap();
        assert!(another_installation.matches_current_executable().is_err());
        assert!(!*stopped.borrow());
        let mut forged = platform::connect(server.status()).await.unwrap();
        write_frame(
            &mut forged,
            &serde_json::to_vec(&Request {
                instance: Uuid::new_v4(),
                action: Action::Stop,
            })
            .unwrap(),
        )
        .await
        .unwrap();
        assert!(read_frame(&mut forged, 16384).await.is_err());
        assert!(!*stopped.borrow());
        assert!(
            query(&dir.path().join("profiles/other"), Action::Status)
                .await
                .unwrap()
                .is_none()
        );
        let reply = query(&root, Action::Stop).await.unwrap().unwrap();
        assert_eq!(reply.state, State::Stopping);
        stopped.changed().await.unwrap();
        drop(server);
        assert!(load(&root).unwrap().is_none());
    }
    #[test]
    fn process_lock_is_exclusive_and_release_keeps_the_lock_inode() {
        let dir = tempfile::tempdir().unwrap();
        // macOS temp paths can contain /var -> /private/var. The profile
        // directory passed to guarded I/O must already be canonical.
        let path = dir.path().canonicalize().unwrap().join("daemon.lock");
        let mut lock = super::super::DaemonLock::acquire_at_path(&path).unwrap();
        assert!(super::super::DaemonLock::acquire_at_path(&path).is_err());
        lock.release();
        assert!(path.exists());
        assert!(super::super::DaemonLock::acquire_at_path(&path).is_ok());
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, LocalFree},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1,
            },
            GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenIsAppContainer,
            TokenUser,
        },
        System::{
            Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId},
            Threading::{
                GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    };
    fn owner(pid: u32) -> Result<String> {
        unsafe {
            let process = if pid == std::process::id() {
                GetCurrentProcess()
            } else {
                OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)
            };
            if process.is_null() {
                return Err(miette!("Cannot identify control peer"));
            }
            let mut token: HANDLE = std::ptr::null_mut();
            let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
            if pid != std::process::id() {
                CloseHandle(process);
            }
            if opened == 0 {
                return Err(miette!("Cannot identify OS owner"));
            }
            let result = (|| {
                let mut container = 0u32;
                let mut length = 0;
                if GetTokenInformation(
                    token,
                    TokenIsAppContainer,
                    (&mut container as *mut u32).cast(),
                    4,
                    &mut length,
                ) == 0
                    || container != 0
                {
                    return Err(miette!("Sandboxed Apps cannot control the daemon"));
                }
                GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut length);
                if length == 0 || length > 65536 {
                    return Err(miette!("Invalid owner identity"));
                }
                let mut data =
                    vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
                if GetTokenInformation(
                    token,
                    TokenUser,
                    data.as_mut_ptr().cast(),
                    length,
                    &mut length,
                ) == 0
                {
                    return Err(miette!("Cannot read owner identity"));
                }
                let user = &*data.as_ptr().cast::<TOKEN_USER>();
                let mut sid = std::ptr::null_mut();
                if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
                    return Err(miette!("Cannot read owner SID"));
                }
                let mut count = 0;
                while count < 1024 && *sid.add(count) != 0 {
                    count += 1;
                }
                let value =
                    String::from_utf16(std::slice::from_raw_parts(sid, count)).into_diagnostic();
                LocalFree(sid.cast());
                value
            })();
            CloseHandle(token);
            result
        }
    }
    fn pipe(path: &Path, first: bool) -> Result<NamedPipeServer> {
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{})", owner(std::process::id())?)
            .encode_utf16()
            .chain([0])
            .collect();
        unsafe {
            let mut descriptor = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(miette!("Cannot secure daemon control"));
            }
            let mut attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            let result = ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(
                    path,
                    (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
                )
                .into_diagnostic();
            LocalFree(descriptor);
            result
        }
    }
    pub struct Listener {
        path: PathBuf,
        next: NamedPipeServer,
    }
    pub fn bind(record: &Status) -> Result<Listener> {
        let path = endpoint(record);
        let next = pipe(&path, true)?;
        Ok(Listener { path, next })
    }
    pub async fn accept(listener: &mut Listener) -> Result<NamedPipeServer> {
        listener.next.connect().await.into_diagnostic()?;
        let next = pipe(&listener.path, false)?;
        let current = std::mem::replace(&mut listener.next, next);
        let mut pid = 0;
        if unsafe { GetNamedPipeClientProcessId(current.as_raw_handle().cast(), &mut pid) } == 0
            || owner(pid)? != owner(std::process::id())?
        {
            return Err(miette!("Control requires the local OS owner"));
        }
        Ok(current)
    }
    pub async fn connect(record: &Status) -> Result<NamedPipeClient> {
        let path = endpoint(record);
        let stream = ClientOptions::new().open(path).into_diagnostic()?;
        let mut pid = 0;
        if unsafe { GetNamedPipeServerProcessId(stream.as_raw_handle().cast(), &mut pid) } == 0
            || pid != record.pid
            || owner(pid)? != owner(std::process::id())?
        {
            return Err(miette!("Daemon endpoint identity changed"));
        }
        Ok(stream)
    }
}
