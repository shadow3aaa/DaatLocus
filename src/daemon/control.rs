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
    pub namespace: PathBuf,
    pub root: PathBuf,
    pub url: String,
    pub options: LaunchOptions,
    pub state: State,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Status,
    Stop,
    Bootstrap(daat_locus_client_protocol::BootstrapRequest),
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
        PathBuf::from("/tmp")
            .join(format!("dl-control-{}", unsafe { libc::geteuid() }))
            .join(format!("{}.sock", record.instance.simple()))
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
                    let _ = tokio::time::timeout(
                        Duration::from_secs(30),
                        respond(stream, &record, stop, bootstrap),
                    )
                    .await;
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
    let bytes = zeroize::Zeroizing::new(read_frame(&mut stream, 64 * 1024).await?);
    let request: Request = serde_json::from_slice(&bytes).into_diagnostic()?;
    if request.instance != status.instance {
        return Err(miette!("Daemon instance changed"));
    }
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
    if matches!(action, Action::Bootstrap(_)) {
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
        if let Ok(info) = std::fs::symlink_metadata(parent) {
            if info.file_type().is_symlink() || info.uid() != unsafe { libc::geteuid() } {
                return Err(miette!("Control directory has a different OS owner"));
            }
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
            return matches!((std::fs::read_link(format!("/proc/{pid}/ns/user")), std::fs::read_link("/proc/self/ns/user")), (Ok(peer), Ok(own)) if peer == own);
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
            bootstrap(
                temp.path(),
                Operation::Connection {
                    signaling_origin: None
                }
            )
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
        let path = dir.path().join("daemon.lock");
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
