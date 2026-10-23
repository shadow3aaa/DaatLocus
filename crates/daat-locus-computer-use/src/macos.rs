use super::*;
use daat_locus_client_protocol::computer_use::{read_frame, verify, write_frame};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    ffi::{CStr, CString},
    fs::{self, OpenOptions},
    io::Write,
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

#[cfg(feature = "native-contract")]
mod contract;
#[cfg(feature = "native-contract")]
pub use contract::run_native_contract;

unsafe extern "C" {
    fn dl_mac_request(
        input: *const libc::c_char,
        permit: extern "C" fn(*const libc::c_void) -> bool,
        context: *const libc::c_void,
    ) -> *mut libc::c_char;
}
static NATIVE: Mutex<()> = Mutex::new(());
fn native(request: Value) -> Result<Value> {
    native_admitted(request, None)
}
extern "C" fn permit_effect(context: *const libc::c_void) -> bool {
    if context.is_null() {
        return true;
    }
    // The synchronous FFI call cannot retain this pointer. NATIVE serializes the
    // Swift host, and its deferred cleanup clears the callback before returning.
    unsafe { &*context.cast::<Admission>() }.allowed()
}
fn native_admitted(request: Value, admission: Option<&Admission>) -> Result<Value> {
    let _guard = NATIVE
        .lock()
        .map_err(|_| Error("Native worker unavailable".into()))?;
    let input =
        CString::new(request.to_string()).map_err(|_| Error("Invalid native request".into()))?;
    let ptr = unsafe {
        dl_mac_request(
            input.as_ptr(),
            permit_effect,
            admission.map_or(std::ptr::null(), |a| std::ptr::from_ref(a).cast()),
        )
    };
    if ptr.is_null() {
        return Err(Error("Native operation unavailable".into()));
    }
    let value = unsafe { CStr::from_ptr(ptr) }.to_bytes().to_vec();
    unsafe { libc::free(ptr.cast()) };
    serde_json::from_slice(&value).map_err(|_| Error("Invalid native reply".into()))
}
fn result(value: Value) -> Result<Value> {
    if let Some(error) = value["error"].as_str() {
        Err(Error(error.into()))
    } else {
        Ok(value["result"].clone())
    }
}
fn preference(root: &Path) -> PathBuf {
    root.join("computer-use-enabled")
}
pub fn permissions(root: &Path) -> Result<Permissions> {
    let value = result(native(json!({"op":"permissions"}))?)?;
    Ok(Permissions {
        enabled: fs::read(preference(root)).is_ok_and(|v| v == b"enabled\n"),
        accessibility: value["accessibility"] == true,
        screen_recording: value["screen_recording"] == true,
        display_supported: value["display"]["supported"] == true,
        display_ready: value["display"]["ready"] == true,
        display: display_config(root)?,
    })
}
fn save_enabled(root: &Path, enabled: bool) -> Result<()> {
    let path = preference(root);
    let temporary = root.join(format!(".gui-{}", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW);
    let mut file = options
        .open(&temporary)
        .map_err(|_| Error("Cannot save GUI preference".into()))?;
    let saved = (|| -> std::io::Result<()> {
        file.write_all(if enabled { b"enabled\n" } else { b"disabled\n" })?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if saved.is_err() {
        let _ = fs::remove_file(temporary);
        return Err(Error("Cannot save GUI preference".into()));
    }
    Ok(())
}
fn display_config(root: &Path) -> Result<DisplayConfig> {
    let path = root.join("computer-use-display.json");
    let value = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| Error("Invalid display preference".into()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => DisplayConfig::default(),
        Err(_) => return Err(Error("Cannot read display preference".into())),
    };
    value.validate()?;
    Ok(value)
}
fn apply_display(root: &Path, display: DisplayConfig) -> Result<()> {
    use std::hash::{Hash, Hasher};
    let mut serial = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut serial);
    result(native(json!({"op":"virtual_configure","width":display.width,"height":display.height,"scale":display.scale,"serial":(serial.finish() as u32).max(1)}))?).map(|_| ())
}
pub fn configure(root: &Path, enabled: bool) -> Result<Permissions> {
    // Disable admission before waiting on native work or returning windows.
    save_enabled(root, enabled)?;
    if !enabled {
        result(native(json!({"op":"virtual_stop"}))?)?;
    } else {
        let status = permissions(root)?;
        if status.accessibility && status.screen_recording && !status.display_ready {
            apply_display(root, status.display)?;
        }
    }
    permissions(root)
}
pub fn request_permission(kind: &str) -> Result<()> {
    let op = match kind {
        "accessibility" => "request_accessibility",
        "screen_recording" => "request_screen_recording",
        _ => return Err(Error("Unknown GUI permission".into())),
    };
    result(native(json!({"op":op}))?).map(|_| ())
}
fn configure_connection(stream: &UnixStream) -> Result<()> {
    // BSD accept inherits O_NONBLOCK. A large image must wait for its peer to
    // drain the socket rather than dropping a partially written frame.
    stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_read_timeout(Some(Duration::from_secs(5))))
        .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(5))))
        .map_err(|_| Error("Cannot configure GUI connection".into()))
}

pub struct Server {
    stop: Arc<AtomicBool>,
    socket: PathBuf,
    host: Arc<Mutex<Host>>,
}
impl Server {
    pub fn start(root: &Path, peer: &Path) -> Result<Self> {
        let root = root.to_path_buf();
        let peer =
            fs::canonicalize(peer).map_err(|_| Error("Service executable unavailable".into()))?;
        let socket = root.join("runtime/computer-use.sock");
        // Only replace a socket after proving that no live host owns it.
        if socket.exists() {
            if UnixStream::connect(&socket).is_ok() {
                return Err(Error("GUI host already running".into()));
            }
            let metadata = fs::symlink_metadata(&socket)
                .map_err(|_| Error("Cannot inspect GUI socket".into()))?;
            use std::os::unix::fs::FileTypeExt;
            if !metadata.file_type().is_socket() {
                return Err(Error("GUI endpoint is not a socket".into()));
            }
            fs::remove_file(&socket)
                .map_err(|_| Error("Cannot replace stale GUI socket".into()))?;
        }
        let listener = UnixListener::bind(&socket)
            .map_err(|_| Error("Cannot create profile GUI socket".into()))?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
            .map_err(|_| Error("Cannot protect GUI socket".into()))?;
        listener
            .set_nonblocking(true)
            .map_err(|_| Error("Cannot configure GUI listener".into()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let host = Arc::new(Mutex::new(Host::default()));
        let server_host = host.clone();
        let cancelled = Arc::new(Mutex::new(HashMap::<String, std::time::Instant>::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let startup_root = root.clone();
        let startup_host = server_host.clone();
        std::thread::Builder::new()
            .name("dl-gui-listener".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            if active.fetch_add(1, Ordering::AcqRel) >= 16 {
                                active.fetch_sub(1, Ordering::AcqRel);
                                continue;
                            }
                            let (root, peer, host, cancelled, active) = (
                                root.clone(),
                                peer.clone(),
                                host.clone(),
                                cancelled.clone(),
                                active.clone(),
                            );
                            std::thread::spawn(move || {
                                let _ = (|| -> Result<()> {
                                    configure_connection(&stream)?;
                                    verify(&stream, &peer)?;
                                    let request: Request =
                                        serde_json::from_slice(&read_frame(&mut stream, 65536)?)
                                            .map_err(|_| Error("Invalid GUI envelope".into()))?;
                                    let deadline =
                                        std::time::Instant::now() + Duration::from_secs(18);
                                    let reply = if request.operation == "cancel" {
                                        let id = request.arguments["id"].as_str().unwrap_or("");
                                        let mut set =
                                            cancelled.lock().unwrap_or_else(|e| e.into_inner());
                                        set.retain(|_, time| {
                                            time.elapsed() < Duration::from_secs(30)
                                        });
                                        if Uuid::parse_str(id).is_ok() && set.len() < 1024 {
                                            set.insert(id.into(), std::time::Instant::now());
                                        }
                                        Reply {
                                            summary: "Cancelled".into(),
                                            payload: json!({"status":"cancelled"}),
                                            failure: None,
                                            image: None,
                                            has_resources: false,
                                        }
                                    } else if matches!(
                                        request.operation.as_str(),
                                        "preview_catalog" | "preview_frame"
                                    ) {
                                        host.lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .preview(&root, &request)
                                    } else {
                                        host.lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .call_at(&root, &request, &cancelled, deadline)
                                    };
                                    write_frame(
                                        &mut stream,
                                        &serde_json::to_vec(&reply)
                                            .map_err(|_| Error("Cannot encode GUI reply".into()))?,
                                    )
                                })();
                                active.fetch_sub(1, Ordering::AcqRel);
                            });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(30))
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|_| Error("Cannot start GUI host".into()))?;
        // CoreGraphics publishes display changes on the client's main run loop.
        // Never wait for that run loop from the Tauri setup callback itself.
        std::thread::spawn(move || {
            let _host = startup_host.lock().unwrap_or_else(|e| e.into_inner());
            if let Ok(status) = permissions(&startup_root)
                && status.enabled
                && status.accessibility
                && status.screen_recording
            {
                let _ = apply_display(&startup_root, status.display);
            }
        });
        Ok(Self {
            stop,
            socket,
            host: server_host,
        })
    }
    pub fn configure(&self, root: &Path, enabled: bool) -> Result<Permissions> {
        if !enabled {
            save_enabled(root, false)?;
        }
        let mut host = self.host.lock().unwrap_or_else(|e| e.into_inner());
        let status = configure(root, enabled)?;
        if !enabled {
            host.scopes.clear();
        }
        Ok(status)
    }
    pub fn configure_display(&self, root: &Path, display: DisplayConfig) -> Result<Permissions> {
        display.validate()?;
        let mut host = self.host.lock().unwrap_or_else(|e| e.into_inner());
        let previous = display_config(root)?;
        let active = permissions(root)?.display_ready;
        if previous == display {
            return permissions(root);
        }
        let temporary = root.join(format!(".gui-display-{}", Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)
            .map_err(|_| Error("Cannot save display preference".into()))?;
        let bytes =
            serde_json::to_vec(&display).map_err(|_| Error("Invalid display preference".into()))?;
        let saved = (|| -> Result<()> {
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| Error("Cannot save display preference".into()))?;
            if active {
                apply_display(root, display)?;
                host.scopes.clear();
            }
            if fs::rename(&temporary, root.join("computer-use-display.json")).is_err() {
                if active {
                    let _ = apply_display(root, previous);
                }
                return Err(Error("Cannot save display preference".into()));
            }
            Ok(())
        })();
        if saved.is_err() {
            let _ = fs::remove_file(temporary);
        }
        saved?;
        permissions(root)
    }
    pub fn shutdown(&self) -> Result<()> {
        let mut host = self.host.lock().unwrap_or_else(|e| e.into_inner());
        result(native(json!({"op":"virtual_stop"}))?)?;
        host.scopes.clear();
        Ok(())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.shutdown();
        let _ = fs::remove_file(&self.socket);
    }
}

#[derive(Default)]
struct Scope {
    session: String,
    epoch: u64,
    apps: HashMap<String, Value>,
    windows: HashMap<String, Window>,
    retired: HashMap<String, std::time::Instant>,
    closed: bool,
}
impl Scope {
    fn admit(&mut self, session: &str, epoch: u64) -> Result<()> {
        self.retired
            .retain(|_, time| time.elapsed() < Duration::from_secs(30));
        if self.retired.contains_key(session) || self.closed && self.session == session {
            return Err(Error("Retired GUI session".into()));
        }
        if self.session != session {
            if !self.session.is_empty() {
                self.retired
                    .insert(self.session.clone(), std::time::Instant::now());
            }
            Host::release(self)?;
            self.apps.clear();
            self.session = session.into();
            self.epoch = 0;
            self.closed = false;
        }
        if epoch < self.epoch {
            return Err(Error("Stale execution epoch".into()));
        }
        self.epoch = epoch;
        Ok(())
    }
}
struct Window {
    receipt: Value,
    lease: Option<String>,
    state: Option<String>,
    view: Value,
}
#[derive(Default)]
struct Host {
    scopes: HashMap<String, Scope>,
    effect: bool,
    pending: Option<Admission>,
}
struct Admission {
    root: PathBuf,
    id: String,
    cancelled: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    deadline: std::time::Instant,
}
impl Admission {
    fn allowed(&self) -> bool {
        std::time::Instant::now() < self.deadline
            && fs::read(preference(&self.root)).is_ok_and(|v| v == b"enabled\n")
            && !self
                .cancelled
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&self.id)
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        for scope in self.scopes.values_mut() {
            let _ = Self::release(scope);
        }
    }
}
impl Host {
    fn preview(&self, root: &Path, request: &Request) -> Reply {
        let result = (|| -> Result<(Value, Option<String>)> {
            let status = permissions(root)?;
            let state = if !status.enabled {
                "disabled"
            } else if status.ready() {
                "ready"
            } else {
                "unavailable"
            };
            let display = result(native(json!({"op":"preview_catalog"}))?)?;
            let id = display["id"]
                .as_u64()
                .filter(|value| *value != 0)
                .map(|value| format!("macos-{value}"));
            if request.operation == "preview_catalog" {
                return Ok((
                    json!({"supported":status.display_supported,"state":state,"displays":if state=="ready"{id.map(|id|vec![json!({"id":id,"actor_id":null,"name":null,"width":display["width"],"height":display["height"]})]).unwrap_or_default()}else{vec![]}}),
                    None,
                ));
            }
            if state != "ready" || id.as_deref() != request.arguments["id"].as_str() {
                return Err(Error("Virtual display preview unavailable".into()));
            }
            let frame = result(native(json!({"op":"preview_frame"}))?)?;
            Ok((json!({}), frame["image"].as_str().map(str::to_owned)))
        })();
        match result {
            Ok((payload, image)) => Reply {
                summary: "Virtual display".into(),
                payload,
                image,
                failure: None,
                has_resources: false,
            },
            Err(error) => Reply {
                summary: "Virtual display unavailable".into(),
                payload: json!({}),
                image: None,
                failure: Some(error.0),
                has_resources: false,
            },
        }
    }
    fn invoke(&mut self, value: Value) -> Result<Value> {
        let admission = if value["op"] == "release" {
            None
        } else {
            self.pending.as_ref()
        };
        if admission.is_some_and(|a| !a.allowed()) {
            return Err(Error(
                "GUI dispatch cancelled, disabled or expired; observe before continuing".into(),
            ));
        }
        let reply = native_admitted(value, admission)?;
        self.effect |= reply["effect"] == true;
        result(reply)
    }
    fn release(scope: &mut Scope) -> Result<()> {
        let mut failure = None;
        scope.windows.retain(|_, window| {
            if let Some(lease) = &window.lease
                && let Err(error) = native(json!({"op":"release","lease":lease})).and_then(result)
            {
                window.state = None;
                failure = Some(error);
                return true;
            }
            false
        });
        failure.map_or(Ok(()), Err)
    }
    #[cfg(feature = "native-contract")]
    fn call(
        &mut self,
        root: &Path,
        request: &Request,
        cancelled: &Arc<Mutex<HashMap<String, std::time::Instant>>>,
    ) -> Reply {
        self.call_at(
            root,
            request,
            cancelled,
            std::time::Instant::now() + Duration::from_secs(18),
        )
    }
    fn call_at(
        &mut self,
        root: &Path,
        request: &Request,
        cancelled: &Arc<Mutex<HashMap<String, std::time::Instant>>>,
        deadline: std::time::Instant,
    ) -> Reply {
        self.effect = false;
        self.pending = Some(Admission {
            root: root.into(),
            id: request.id.clone(),
            cancelled: cancelled.clone(),
            deadline,
        });
        let result = (|| -> Result<(Value, Option<String>)> {
            if std::time::Instant::now() >= deadline {
                return Err(Error("GUI request expired before dispatch".into()));
            }
            if !permissions(root)?.ready() {
                for scope in self.scopes.values_mut() {
                    let _ = Self::release(scope);
                }
                return Err(Error(
                    "Enable GUI operation, Accessibility and Screen Recording in Settings".into(),
                ));
            }
            if request.scope.is_empty()
                || request.scope.len() > 128
                || request.session.len() > 128
                || Uuid::parse_str(&request.id).is_err()
            {
                return Err(Error("Invalid GUI scope".into()));
            }
            if cancelled
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&request.id)
                .is_some()
            {
                return Err(Error("GUI operation cancelled before dispatch".into()));
            }
            let mut scope = self.scopes.remove(&request.scope).unwrap_or_default();
            if request.operation == "detach" {
                if scope.session == request.session {
                    scope.closed = true;
                    let released = Self::release(&mut scope);
                    scope.apps.clear();
                    if let Err(error) = released {
                        self.scopes.insert(request.scope.clone(), scope);
                        return Err(error);
                    }
                }
                self.scopes.insert(request.scope.clone(), scope);
                return Ok((json!({"status":"detached"}), None));
            }
            if let Err(error) = scope.admit(&request.session, request.epoch) {
                self.scopes.insert(request.scope.clone(), scope);
                return Err(error);
            }
            let result = self.execute(&mut scope, request);
            self.scopes.insert(request.scope.clone(), scope);
            result
        })();
        cancelled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&request.id);
        self.pending = None;
        let has_resources = self
            .scopes
            .get(&request.scope)
            .is_some_and(|scope| scope.windows.values().any(|window| window.lease.is_some()));
        match result {
            Ok((payload, image)) => Reply {
                summary: "ComputerUse".into(),
                payload,
                failure: None,
                image,
                has_resources,
            },
            Err(error) => Reply {
                summary: "ComputerUse requires observation".into(),
                payload: json!({"status":if self.effect{"outcome_unknown"}else{"not_started"},"error":error.0}),
                failure: Some(error.0),
                image: None,
                has_resources,
            },
        }
    }
    fn app_id(scope: &mut Scope, value: Value) -> String {
        if let Some((id, _)) = scope.apps.iter().find(|(_, v)| v["path"] == value["path"]) {
            return id.clone();
        }
        let id = Uuid::new_v4().to_string();
        scope.apps.insert(id.clone(), value);
        id
    }
    fn page(field: &str, mut rows: Vec<Value>, args: &Value) -> Result<Value> {
        rows.sort_by_key(|a| a.to_string());
        let cursor = args["cursor"]
            .as_str()
            .map(str::parse::<usize>)
            .transpose()
            .map_err(|_| Error("Invalid page cursor".into()))?
            .unwrap_or(0);
        let limit = args["limit"]
            .as_u64()
            .filter(|v| *v > 0 && *v <= 100)
            .ok_or_else(|| Error("Page limit must be 1..100".into()))? as usize;
        if cursor > rows.len() {
            return Err(Error("Page changed; enumerate again".into()));
        }
        let end = (cursor + limit).min(rows.len());
        let mut payload = json!({"status":"observed","next_cursor":if end<rows.len(){Some(end.to_string())}else{None}});
        payload[field] = json!(rows[cursor..end]);
        Ok(payload)
    }
    fn execute(&mut self, scope: &mut Scope, request: &Request) -> Result<(Value, Option<String>)> {
        let args = &request.arguments;
        let op = request.operation.as_str();
        if op == "list_apps" {
            let found = self.invoke(json!({"op":"list_apps"}))?;
            let mut rows = Vec::new();
            for app in found["apps"]
                .as_array()
                .ok_or_else(|| Error("Invalid native app list".into()))?
            {
                let id = Self::app_id(scope, app.clone());
                let query = args["query"].as_str().unwrap_or("").to_lowercase();
                if !app["name"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&query)
                    || args["app_id"].as_str().is_some_and(|v| v != id)
                {
                    continue;
                }
                rows.push(json!({"app_id":id,"name":app["name"]}));
            }
            return Ok((Self::page("apps", rows, args)?, None));
        }
        if op == "list_windows" {
            let found = self.invoke(json!({"op":"list_windows"}))?;
            let mut live = HashSet::new();
            let mut rows = Vec::new();
            for receipt in found["windows"]
                .as_array()
                .ok_or_else(|| Error("Invalid native window list".into()))?
            {
                if self.scopes.values().any(|s| {
                    s.windows.values().any(|w| {
                        w.lease.is_some()
                            && w.receipt["id"] == receipt["id"]
                            && w.receipt["pid"] == receipt["pid"]
                            && w.receipt["started"] == receipt["started"]
                    })
                }) {
                    continue;
                }
                let id = scope
                    .windows
                    .iter()
                    .find(|(_, w)| {
                        w.receipt["id"] == receipt["id"]
                            && w.receipt["pid"] == receipt["pid"]
                            && w.receipt["started"] == receipt["started"]
                    })
                    .map(|(id, _)| id.clone())
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                let app = Self::app_id(
                    scope,
                    json!({"path":receipt["path"],"name":receipt["name"]}),
                );
                live.insert(id.clone());
                let window = scope.windows.entry(id.clone()).or_insert_with(|| Window {
                    receipt: receipt.clone(),
                    lease: None,
                    state: None,
                    view: Value::Null,
                });
                let title = receipt["title"].as_str().unwrap_or("");
                if args["app_id"].as_str().is_some_and(|v| v != app)
                    || !title
                        .to_lowercase()
                        .contains(&args["query"].as_str().unwrap_or("").to_lowercase())
                {
                    continue;
                }
                rows.push(json!({"window_id":id,"app_id":app,"title":title,"taken":window.lease.is_some()}));
            }
            let mut cleanup_failure = None;
            scope.windows.retain(|id, window| {
                if live.contains(id) {
                    return true;
                }
                if let Some(lease) = &window.lease
                    && let Err(error) =
                        native(json!({"op":"release","lease":lease})).and_then(result)
                {
                    window.state = None;
                    cleanup_failure = Some(error);
                    return true;
                }
                false
            });
            if let Some(error) = cleanup_failure {
                return Err(error);
            }
            return Ok((Self::page("windows", rows, args)?, None));
        }
        let mut acquired = None;
        if op == "launch_app" {
            let app = scope
                .apps
                .get(args["app_id"].as_str().unwrap_or(""))
                .ok_or_else(|| Error("Unknown app; discover again".into()))?;
            let launched = self.invoke(json!({"op":"launch","path":app["path"]}))?;
            let receipt = launched["receipt"].clone();
            let id = Uuid::new_v4().to_string();
            scope.windows.insert(
                id.clone(),
                Window {
                    receipt,
                    lease: None,
                    state: None,
                    view: Value::Null,
                },
            );
            acquired = Some(id);
        }
        let id = acquired
            .as_deref()
            .or_else(|| args["window_id"].as_str())
            .ok_or_else(|| Error("Window reference required".into()))?;
        let window = scope
            .windows
            .get_mut(id)
            .ok_or_else(|| Error("Unknown or revoked window; discover again".into()))?;
        if op == "give_window"
            && (window.state.is_none() || window.state.as_deref() != args["state_id"].as_str())
        {
            return Err(Error("Stale observation; observe again".into()));
        }
        if op == "release_window" || op == "give_window" {
            if let Some(lease) = window.lease.as_ref() {
                self.invoke(json!({"op":"release","lease":lease,"placement":args["placement"].as_str().unwrap_or("original")}))?;
            }
            scope.windows.remove(id);
            return Ok((json!({"status":"released","input_revoked":true}), None));
        }
        if op == "open_window" || op == "launch_app" {
            if self.scopes.values().any(|other| {
                other.windows.values().any(|owned| {
                    owned.lease.is_some()
                        && owned.receipt["id"] == window.receipt["id"]
                        && owned.receipt["pid"] == window.receipt["pid"]
                        && owned.receipt["started"] == window.receipt["started"]
                })
            }) {
                return Err(Error("Window is owned by another scope".into()));
            }
            if window.lease.is_none() {
                let bound = self.invoke(json!({"op":"bind","id":window.receipt["id"]}))?;
                window.lease = Some(
                    bound["lease"]
                        .as_str()
                        .ok_or_else(|| Error("Cannot bind window".into()))?
                        .into(),
                );
                window.receipt = bound["receipt"].clone();
                self.invoke(json!({"op":"take","lease":window.lease}))?;
            }
        }
        let lease = window
            .lease
            .as_ref()
            .ok_or_else(|| Error("Explicitly open the window first".into()))?
            .clone();
        if !matches!(op, "open_window" | "launch_app" | "get_window_state") {
            let recovery = op == "adjust_window" && args["state_id"].is_null();
            if !recovery
                && (window.state.as_deref() != args["state_id"].as_str() || window.state.is_none())
            {
                return Err(Error("Stale observation; observe again".into()));
            }
            let mut command = json!({"op":op,"lease":lease,"check_snapshot":!recovery});
            if !recovery {
                command["bounds"] = window.receipt["bounds"].clone();
            }
            for key in [
                "text",
                "key",
                "button",
                "click_count",
                "action",
                "width",
                "height",
                "x",
                "y",
                "delta_x",
                "delta_y",
            ] {
                command[key] = args[key].clone();
            }
            if matches!(op, "click" | "scroll") {
                let point = map_point(window, args)?;
                command["x"] = point["x"].clone();
                command["y"] = point["y"].clone();
            }
            if op == "drag" {
                let path = args["path"]
                    .as_array()
                    .filter(|v| (2..=256).contains(&v.len()))
                    .ok_or_else(|| Error("Drag path must contain 2..256 points".into()))?;
                command["path"] = Value::Array(
                    path.iter()
                        .map(|p| map_point(window, p))
                        .collect::<Result<_>>()?,
                );
            }
            window.state = None;
            if op == "close_window" {
                command["op"] = json!("close");
                self.invoke(command)?;
                scope.windows.remove(id);
                return Ok((json!({"status":"dispatched","input_revoked":true}), None));
            }
            self.invoke(command)?;
        }
        window.state = None;
        let capture = json!({"op":"capture","lease":lease,"region":if op=="get_window_state"{args["region"].clone()}else{Value::Null},"scale":if op=="get_window_state"{args["scale"].clone()}else{Value::Null}});
        let captured = match self.invoke(capture.clone()) {
            Err(error) if error.0 == "Window geometry changed during capture; observe again" => {
                // AX window changes settle asynchronously. Retry only the read,
                // never the input or window mutation that preceded it.
                self.invoke(capture)?
            }
            result => result?,
        };
        let state = Uuid::new_v4().to_string();
        window.receipt = captured["receipt"].clone();
        window.state = Some(state.clone());
        window.view = captured.clone();
        window.view["image"] = Value::Null;
        let mut supported = vec![
            "click_left",
            "scroll_accessible_area",
            "drag_accessible_slider",
            "adjust_window",
            "type_text",
            "Backspace",
            "ArrowLeft",
            "ArrowRight",
            "Super+a",
            "release_window",
            "close_window",
        ];
        if captured["input_target"].is_null() {
            supported.retain(|v| {
                !matches!(
                    *v,
                    "type_text" | "Backspace" | "ArrowLeft" | "ArrowRight" | "Super+a"
                )
            });
        }
        Ok((
            json!({"status":"observed","observation":{"window_id":id,"state_id":state,"title":window.receipt["title"],"width":captured["width"],"height":captured["height"],"window_width":captured["window_width"],"window_height":captured["window_height"],"view":{"region":captured["region"],"scale":captured["scale"]},"input_target":captured["input_target"],"controls":captured["controls"],"supported_actions":supported,"screenshot":null,"screenshot_url":null}}),
            captured["image"].as_str().map(str::to_owned),
        ))
    }
}

fn map_point(window: &Window, point: &Value) -> Result<Value> {
    let x = point["x"]
        .as_i64()
        .ok_or_else(|| Error("Invalid X coordinate".into()))?;
    let y = point["y"]
        .as_i64()
        .ok_or_else(|| Error("Invalid Y coordinate".into()))?;
    let scale = window.view["scale"].as_f64().unwrap_or(1.0);
    let region = &window.view["region"];
    if x < 0
        || y < 0
        || x as f64 >= region["width"].as_f64().unwrap_or(0.0) * scale
        || y as f64 >= region["height"].as_f64().unwrap_or(0.0) * scale
    {
        return Err(Error("Point outside observed image".into()));
    }
    let coordinate = |axis: &str, size: &str, v: i64| -> Result<f64> {
        let number = |value: &Value| {
            value
                .as_f64()
                .filter(|v| v.is_finite())
                .ok_or_else(|| Error("Observation geometry unavailable".into()))
        };
        let whole = number(&window.view[format!("window_{size}")])?;
        if whole <= 0.0 {
            return Err(Error("Observation geometry unavailable".into()));
        }
        Ok(number(&window.receipt["bounds"][axis])?
            + ((v as f64 / scale) + number(&region[axis])?) / whole
                * number(&window.receipt["bounds"][size])?)
    };
    Ok(json!({"x":coordinate("x","width",x)?,"y":coordinate("y","height",y)?}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restarted_service_can_begin_a_new_session_without_reviving_retired_work() {
        let mut scope = Scope::default();
        scope.admit("before-restart", 100).unwrap();
        scope.windows.insert(
            "old-window".into(),
            Window {
                receipt: json!({}),
                lease: None,
                state: Some("old-image".into()),
                view: Value::Null,
            },
        );
        scope.admit("after-restart", 1).unwrap();
        assert!(
            scope.windows.is_empty(),
            "A new client session must revoke old window/image references"
        );
        assert!(
            scope.admit("before-restart", 101).is_err(),
            "Queued old-session work must not take the new session back"
        );
        assert!(scope.admit("after-restart", 0).is_err());
        scope.closed = true;
        assert!(
            scope.admit("after-restart", 2).is_err(),
            "Detached work must not reopen its old session"
        );
        scope.admit("fresh-worker", 1).unwrap();
    }
    #[test]
    fn cropped_retina_coordinates_map_to_window_points_and_reject_edges() {
        let window = Window {
            receipt: json!({"bounds":{"x":120.0,"y":80.0,"width":640.0,"height":480.0}}),
            lease: None,
            state: Some("image".into()),
            view: json!({"scale":3,"region":{"x":200,"y":100,"width":300,"height":200},"window_width":1280,"window_height":960}),
        };
        assert_eq!(
            map_point(&window, &json!({"x":60,"y":90})).unwrap(),
            json!({"x":230.0,"y":145.0})
        );
        for point in [
            json!({"x":-1,"y":0}),
            json!({"x":900,"y":0}),
            json!({"x":0,"y":600}),
            json!({"x":"0","y":0}),
        ] {
            assert!(map_point(&window, &point).is_err());
        }
    }
}

#[cfg(test)]
mod host_transport_tests {
    use super::*;
    use daat_locus_client_protocol::computer_use::Client;
    #[test]
    fn accepted_connection_sends_a_complete_large_reply_under_backpressure() {
        use std::os::fd::AsRawFd;
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        // BSD accept inherits O_NONBLOCK from the nonblocking listener.
        sender.set_nonblocking(true).unwrap();
        configure_connection(&sender).unwrap();
        let flags = unsafe { libc::fcntl(sender.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            flags & libc::O_NONBLOCK,
            0,
            "accepted framed connections must block with bounded timeouts"
        );
        let size: libc::c_int = 4096;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    sender.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    std::ptr::from_ref(&size).cast(),
                    std::mem::size_of_val(&size) as _,
                )
            },
            0
        );
        receiver
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let expected: Vec<u8> = (0..=255).cycle().take(2 * 1024 * 1024).collect();
        let bytes = expected.clone();
        let writer = std::thread::spawn(move || write_frame(&mut sender, &bytes));
        let received = read_frame(&mut receiver, 3 * 1024 * 1024).unwrap();
        writer.join().unwrap().unwrap();
        assert_eq!(received, expected);
    }

    #[test]
    fn private_host_rejects_an_unexpected_service_before_native_dispatch() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("runtime")).unwrap();
        let host = Server::start(root.path(), Path::new("/usr/bin/true")).unwrap();
        let socket = root.path().join("runtime/computer-use.sock");
        assert_eq!(
            fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let client = Client::new(root.path(), &std::env::current_exe().unwrap());
        let result = client.call(&Request {
            id: Uuid::new_v4().to_string(),
            scope: "fixture".into(),
            session: "fixture".into(),
            epoch: 1,
            operation: "list_windows".into(),
            arguments: json!({"query":null,"app_id":null,"cursor":null,"limit":10}),
        });
        assert!(
            result.is_err(),
            "An unrecognized executable must not reach native observation"
        );
        drop(host);
    }
}
