use super::*;
use daat_locus_virtual_workspace::{
    DesktopState, DisplayRegion, InputBackend, InputSurface, MouseAction, Point, Rect,
    SharedDisplay, VirtualWorkspace, Window, WindowInput, displays,
};
use miette::IntoDiagnostic;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Read,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HWND, RECT},
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromRect},
    System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    },
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
mod settings;
pub(crate) use settings::Workspace;

fn desktop_operations() -> &'static parking_lot::Mutex<()> {
    static OPERATIONS: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    &OPERATIONS
}

pub(super) struct Reply {
    pub payload: Value,
    pub image: Option<Vec<u8>>,
    pub summary: String,
    pub failure: Option<String>,
}
impl Reply {
    fn data(summary: &str, payload: Value) -> Self {
        Self {
            payload,
            image: None,
            summary: summary.into(),
            failure: None,
        }
    }
}
struct Request {
    reset: bool,
    generation: u64,
    call: Option<AgentToolCall>,
    context: Option<AppToolExecutionContext>,
    cancelled: Arc<AtomicBool>,
    reply: flume::Sender<Result<Reply>>,
}
struct Registration {
    requests: flume::Sender<Request>,
}
pub(super) struct Client {
    registration: Arc<Registration>,
    workspace: Arc<Workspace>,
    live: Arc<AtomicBool>,
}
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Client {
    pub fn start(actor: String, workspace: Arc<Workspace>) -> Result<Self> {
        let _permit = workspace.permit(workspace.generation())?;
        let (tx, rx) = flume::bounded::<Request>(1);
        let registration = Arc::new(Registration { requests: tx });
        let worker_workspace = workspace.clone();
        let live = Arc::new(AtomicBool::new(false));
        let worker_live = live.clone();
        std::thread::Builder::new()
            .name("dl-computer-use".into())
            .spawn(move || {
                // This thread is private to the backend; all host HWND geometry
                // and image coordinates use physical pixels, including child HWNDs.
                unsafe {
                    windows_sys::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(
                        windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                    );
                }
                let mut host = Host::new(actor, worker_workspace.display.clone());
                while let Ok(request) = rx.recv() {
                    if request.cancelled.load(Ordering::Acquire) {
                        continue;
                    }
                    let result = match request.call {
                        Some(call) => {
                            let context = request.context.expect("tool context");
                            worker_workspace
                                .permit(request.generation)
                                .and_then(|_permit| {
                                    ComputerUseApp::check_context(&context).and_then(|()| {
                                        host.execute(&call, &context, &request.cancelled)
                                    })
                                })
                        }
                        None if request.reset => host.release_all().map(|()| {
                            host.apps.clear();
                            host.candidates.clear();
                            host.generation = id();
                            Reply::data("ComputerUse references revoked", json!({}))
                        }),
                        None => {
                            let result = host.release_all();
                            let _ = request.reply.send(
                                result.map(|()| Reply::data("ComputerUse stopped", json!({}))),
                            );
                            break;
                        }
                    };
                    worker_live.store(
                        !host.taken.is_empty()
                            || !host.returning.is_empty()
                            || !host.hooks.is_empty(),
                        Ordering::Release,
                    );
                    let _ = request.reply.send(result);
                }
                // Keep disabled guards and the display alive while an application is
                // still executing a prior command. Never expose it by dropping early.
                while host.release_all().is_err()
                    && (!host.returning.is_empty() || !host.taken.is_empty())
                {
                    std::thread::sleep(Duration::from_millis(200));
                }
                worker_live.store(false, Ordering::Release);
            })
            .into_diagnostic()?;
        workspace.register(&registration);
        drop(_permit);
        Ok(Self {
            registration,
            workspace,
            live,
        })
    }
    pub fn has_resources(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }
    pub async fn call(
        &self,
        call: AgentToolCall,
        context: AppToolExecutionContext,
    ) -> Result<Reply> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let _guard = CancelOnDrop(cancelled.clone());
        let (tx, rx) = flume::bounded(1);
        self.registration
            .requests
            .send_async(Request {
                reset: false,
                generation: self.workspace.generation(),
                call: Some(call),
                context: Some(context),
                cancelled,
                reply: tx,
            })
            .await
            .map_err(|_| miette!("ComputerUse host stopped; observe before retrying"))?;
        rx.recv_async()
            .await
            .map_err(|_| miette!("ComputerUse outcome unknown; host stopped, do not replay"))?
    }
    pub async fn shutdown(self) -> Result<()> {
        let (tx, rx) = flume::bounded(1);
        self.registration
            .requests
            .send_async(Request {
                reset: false,
                generation: 0,
                call: None,
                context: None,
                cancelled: Arc::new(AtomicBool::new(false)),
                reply: tx,
            })
            .await
            .map_err(|_| miette!("ComputerUse host stopped"))?;
        rx.recv_async()
            .await
            .map_err(|_| miette!("ComputerUse shutdown outcome unknown"))??;
        Ok(())
    }
}

#[derive(Clone)]
struct AppRecord {
    id: String,
    name: String,
    launch: String,
}
#[derive(Clone)]
struct Candidate {
    id: String,
    window: Window,
    app_id: String,
    title: String,
}
struct InputTarget {
    hwnd: usize,
    surface: InputSurface,
    bounds: RECT,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchReceipt {
    process_id: Option<u32>,
    process_started: Option<u64>,
}
fn matches_launch(
    app: &str,
    process_id: u32,
    process_started: u64,
    receipt: &LaunchReceipt,
    application_matches: impl FnOnce(&str) -> bool,
) -> bool {
    if let Some(application) = app
        .strip_prefix("shell:AppsFolder\\")
        .filter(|key| key.contains('!'))
    {
        // A broker's PID can host several apps. Packaged activation must match
        // the requested package/app, even when the receipt contains that PID.
        application_matches(application)
    } else {
        Some(process_id) == receipt.process_id && Some(process_started) == receipt.process_started
    }
}
struct Taken {
    region: DisplayRegion,
    pending_region: Option<DisplayRegion>,
    window: Window,
    original: Rect,
    original_minimized: bool,
    handoff_placement: Option<Placement>,
    title: String,
    receiver: Option<InputTarget>,
    state: Option<String>,
    view: Option<ObservedView>,
}
type ThreadKey = (u32, u32);
static RESERVATIONS: OnceLock<parking_lot::Mutex<HashMap<ThreadKey, String>>> = OnceLock::new();
fn reservations() -> &'static parking_lot::Mutex<HashMap<ThreadKey, String>> {
    RESERVATIONS.get_or_init(Default::default)
}
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn launch_window_snapshot() -> Result<HashSet<usize>> {
    unsafe extern "system" fn collect(hwnd: HWND, data: isize) -> i32 {
        let windows = unsafe { &mut *(data as *mut HashSet<usize>) };
        windows.insert(hwnd as usize);
        i32::from(windows.len() < 65_536)
    }
    let mut windows = HashSet::new();
    // Include hidden windows: activating a cached user window is not creation of
    // a new window. Recycled handles are conservatively excluded for this launch.
    if unsafe { EnumWindows(Some(collect), &mut windows as *mut _ as isize) } == 0 {
        return Err(miette!("could not snapshot existing windows before launch"));
    }
    Ok(windows)
}
#[cfg(test)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn thread_key(window: &Window) -> ThreadKey {
    (window.process_id, window.thread_id)
}

struct Host {
    display: SharedDisplay,
    actor: String,
    generation: String,
    epoch: u64,
    apps: BTreeMap<String, AppRecord>,
    candidates: BTreeMap<String, Candidate>,
    taken: BTreeMap<String, Taken>,
    returning: BTreeMap<String, Taken>,
    hooks: HashMap<ThreadKey, WindowInput>,
    workspace: Option<VirtualWorkspace>,
    private_roots: Vec<PathBuf>,
    protected_env: Vec<String>,
    effect_started: bool,
    hook_dll: PathBuf,
    launcher_exe: Option<PathBuf>,
}

impl Host {
    fn new(actor: String, display: SharedDisplay) -> Self {
        Self {
            display,
            actor,
            generation: id(),
            epoch: 0,
            apps: BTreeMap::new(),
            candidates: BTreeMap::new(),
            taken: BTreeMap::new(),
            returning: BTreeMap::new(),
            hooks: HashMap::new(),
            workspace: None,
            private_roots: Vec::new(),
            protected_env: Vec::new(),
            effect_started: false,
            hook_dll: std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.join("daat_locus_workspace_hook.dll")))
                .unwrap_or_default(),
            launcher_exe: std::env::current_exe().ok(),
        }
    }
    fn execute(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
        cancelled: &AtomicBool,
    ) -> Result<Reply> {
        // A native operation includes preparation, validation, dispatch and its
        // observation. Other Actors cannot change stacking midway through it.
        let _transaction = (!matches!(
            call.name.as_str(),
            "list_apps" | "list_windows" | "launch_app"
        ))
        .then(|| desktop_operations().lock());
        self.effect_started = false;
        let result = self.execute_inner(call, context, cancelled);
        match result {
            Err(error) if self.effect_started => {
                let window_id = call.arguments.get("window_id").and_then(Value::as_str);
                let status = if window_id.is_some_and(|id| self.returning.contains_key(id)) {
                    "handoff_pending"
                } else {
                    "outcome_unknown"
                };
                Ok(Reply {
                    payload: json!({"status":status,"window_id":window_id,"error":error.to_string(),"input_revoked":window_id.is_some_and(|id| self.taken.get(id).and_then(|t| self.hooks.get(&thread_key(&t.window))).is_none_or(|hook| !hook.input_enabled()))}),
                    image: None,
                    summary: "ComputerUse effect requires observation".into(),
                    failure: Some(error.to_string()),
                })
            }
            other => other,
        }
    }
    fn execute_inner(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
        cancelled: &AtomicBool,
    ) -> Result<Reply> {
        if context.turn_epoch < self.epoch {
            return Err(miette!("stale ComputerUse execution epoch"));
        }
        if context
            .actor_call
            .as_ref()
            .is_some_and(|call| call.actor_id != self.actor)
        {
            return Err(miette!("ComputerUse host belongs to another Actor"));
        }
        self.epoch = context.turn_epoch;
        self.protected_env = context.sandbox_policy.protected_env_vars.clone();
        self.private_roots = context
            .actor_call
            .as_ref()
            .map(|call| call.private_roots.clone())
            .unwrap_or_default();
        self.private_roots
            .extend(context.sandbox_policy.filesystem.deny_read_paths.clone());
        if cancelled.load(Ordering::Acquire) {
            return Err(miette!("ComputerUse cancelled before dispatch"));
        }
        match call.name.as_str() {
            "list_apps" | "list_windows" => {
                let args: ListArgs = parse(&call.arguments)?;
                if args.limit == 0 || args.limit > 100 {
                    return Err(miette!("page limit must be 1..=100"));
                }
                self.discover()?;
                if call.name == "list_apps" {
                    let rows: Vec<Value> = self
                        .apps
                        .values()
                        .filter(|a| {
                            args.query
                                .as_ref()
                                .is_none_or(|q| a.name.to_lowercase().contains(&q.to_lowercase()))
                        })
                        .map(|app| json!({"app_id":app.id,"name":app.name}))
                        .collect();
                    Ok(Reply::data(
                        "Discovered apps",
                        page("apps", rows, args.cursor.as_deref(), args.limit)?,
                    ))
                } else {
                    let rows = self.candidates.values().filter(|c| args.app_id.as_ref().is_none_or(|app| &c.app_id == app)
                        && args.query.as_ref().is_none_or(|q| c.title.to_lowercase().contains(&q.to_lowercase())))
                        .filter(|c| reservations().lock().get(&thread_key(&c.window)).is_none_or(|owner| owner == &self.generation))
                        .map(|c| json!({"window_id":c.id,"app_id":c.app_id,"title":c.title,"minimized":c.window.minimized,"taken":self.taken.contains_key(&c.id),"handoff_pending":self.returning.contains_key(&c.id),"blocked":unsafe{IsWindowEnabled(c.window.handle as HWND)}==0})).collect();
                    Ok(Reply::data(
                        "Discovered windows",
                        page("windows", rows, args.cursor.as_deref(), args.limit)?,
                    ))
                }
            }
            "launch_app" => self.launch(&parse::<AppArgs>(&call.arguments)?.app_id, cancelled),
            "open_window" => self.take(&parse::<WindowArgs>(&call.arguments)?.window_id, cancelled),
            "get_window_state" => {
                let args: ObserveArgs = parse(&call.arguments)?;
                self.observe_view(&args.window_id, args.region, args.scale)
            }
            "adjust_window" => self.adjust(parse(&call.arguments)?, cancelled),
            "release_window" => self.give(
                &parse::<WindowArgs>(&call.arguments)?.window_id,
                Placement::Original,
            ),
            "give_window" => {
                let args: GiveArgs = parse(&call.arguments)?;
                self.check_observation(&args.window_id, &args.state_id)?;
                self.give(&args.window_id, args.placement)
            }
            "click" => {
                let args: ClickArgs = parse(&call.arguments)?;
                if !matches!(args.button, Button::Left) || !(1..=2).contains(&args.click_count) {
                    return Err(miette!(
                        "unsupported click; only a single left click is currently supported"
                    ));
                }
                if args.click_count != 1 {
                    return Err(miette!("double click is not yet supported"));
                }
                self.check_state(&args.window_id, &args.state_id)?;
                let local = self.image_point(
                    &args.window_id,
                    ImagePoint {
                        x: args.x,
                        y: args.y,
                    },
                )?;
                let (hwnd, point) = self.taken[&args.window_id]
                    .window
                    .input_at(local)
                    .into_diagnostic()?;
                let receiver = input_target(&self.taken[&args.window_id].window, hwnd)?;
                self.input(&args.window_id, hwnd, cancelled, |hook| {
                    hook.mouse(hwnd, MouseAction::ClickLeft, point, Duration::from_secs(3))
                })?;
                if let Some(window) = self.taken.get_mut(&args.window_id) {
                    window.receiver = Some(receiver);
                }
                self.after_input(&args.window_id)
            }
            "type_text" => {
                let args: TextArgs = parse(&call.arguments)?;
                self.check_state(&args.window_id, &args.state_id)?;
                let hwnd = self.receiver(&args.window_id)?;
                if args.text.encode_utf16().count() > 1024
                    || args.text.contains('\0')
                    || args.text.chars().any(|c| c.is_control())
                {
                    return Err(miette!(
                        "literal text must contain at most 1024 UTF-16 units; use press_key for control keys"
                    ));
                }
                self.input(&args.window_id, hwnd, cancelled, |hook| {
                    hook.text(hwnd, &args.text, Duration::from_secs(3))
                })?;
                self.after_input(&args.window_id)
            }
            "press_key" => {
                let args: KeyArgs = parse(&call.arguments)?;
                let (key, modifiers) = key(&args.key)?;
                self.check_state(&args.window_id, &args.state_id)?;
                let hwnd = self.receiver(&args.window_id)?;
                self.input(&args.window_id, hwnd, cancelled, |hook| {
                    hook.key(hwnd, key, modifiers, Duration::from_secs(3))
                })?;
                self.after_input(&args.window_id)
            }
            "scroll" => {
                let args: ScrollArgs = parse(&call.arguments)?;
                if args.delta_x != 0 {
                    return Err(miette!("horizontal scrolling is unsupported"));
                }
                let delta = args
                    .delta_y
                    .checked_mul(-120)
                    .and_then(|v| i16::try_from(v).ok())
                    .ok_or_else(|| miette!("scroll amount exceeds one native operation"))?;
                self.check_state(&args.window_id, &args.state_id)?;
                let local = self.image_point(
                    &args.window_id,
                    ImagePoint {
                        x: args.x,
                        y: args.y,
                    },
                )?;
                let (hwnd, point) = self.taken[&args.window_id]
                    .window
                    .input_at(local)
                    .into_diagnostic()?;
                self.input(&args.window_id, hwnd, cancelled, |hook| {
                    hook.mouse(
                        hwnd,
                        MouseAction::Wheel(delta),
                        point,
                        Duration::from_secs(3),
                    )
                })?;
                self.after_input(&args.window_id)
            }
            "drag" => {
                let args: DragArgs = parse(&call.arguments)?;
                let image_points = trajectory(&args.path)?;
                self.check_state(&args.window_id, &args.state_id)?;
                let points: Vec<Point> = image_points
                    .into_iter()
                    .map(|p| self.image_point(&args.window_id, ImagePoint { x: p.x, y: p.y }))
                    .collect::<Result<_>>()?;
                let window = &self.taken[&args.window_id].window;
                let (hwnd, _) = window.input_at(points[0]).into_diagnostic()?;
                let points: Vec<Point> = points
                    .into_iter()
                    .map(|p| window.control_point(hwnd, p).into_diagnostic())
                    .collect::<Result<_>>()?;
                if points
                    .iter()
                    .any(|p| i16::try_from(p.x).is_err() || i16::try_from(p.y).is_err())
                {
                    return Err(miette!("drag coordinates exceed the native range"));
                }
                let receiver = input_target(&self.taken[&args.window_id].window, hwnd)?;
                self.input(&args.window_id, hwnd, cancelled, |hook| {
                    hook.drag(hwnd, &points, Duration::from_secs(3))
                })?;
                if let Some(window) = self.taken.get_mut(&args.window_id) {
                    window.receiver = Some(receiver);
                }
                self.after_input(&args.window_id)
            }
            "close_window" => {
                let args: StateArgs = parse(&call.arguments)?;
                self.check_state(&args.window_id, &args.state_id)?;
                let hwnd = self.taken[&args.window_id].window.handle;
                self.input(&args.window_id, hwnd, cancelled, |hook| {
                    hook.request_close(hwnd, Duration::from_secs(3))
                })?;
                self.after_input(&args.window_id)
            }
            _ => Err(miette!("unknown ComputerUse tool")),
        }
    }
    fn register_app(&mut self, name: String, launch: String) -> String {
        if let Some(app) = self
            .apps
            .values()
            .find(|app| app.launch.eq_ignore_ascii_case(&launch))
        {
            return app.id.clone();
        }
        let app = AppRecord {
            id: id(),
            name,
            launch,
        };
        let id = app.id.clone();
        self.apps.insert(id.clone(), app);
        id
    }
    fn discover(&mut self) -> Result<()> {
        self.reconcile_returns()?;
        if self.apps.is_empty() {
            for (name, launch) in installed_apps()? {
                self.register_app(name, launch);
            }
        }
        let windows = daat_locus_virtual_workspace::windows().into_diagnostic()?;
        let mut alive = Vec::new();
        for window in windows {
            if !self.allowed(&window) {
                continue;
            }
            alive.push((window.handle, window.process_started));
            if let Some(candidate) = self.candidates.values_mut().find(|c| {
                c.window.handle == window.handle
                    && c.window.process_started == window.process_started
                    && c.window.thread_id == window.thread_id
            }) {
                candidate.title = title(window.handle);
                candidate.window = window;
                continue;
            }
            let path = process_path(window.process_id)?;
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let app_id = self.register_app(name, path.to_string_lossy().into_owned());
            let reference = self
                .returning
                .iter()
                .find(|(_, target)| {
                    target.window.handle == window.handle
                        && target.window.process_started == window.process_started
                        && target.window.thread_id == window.thread_id
                })
                .map(|(id, _)| id.clone())
                .unwrap_or_else(id);
            let candidate = Candidate {
                id: reference,
                title: title(window.handle),
                window,
                app_id,
            };
            self.candidates.insert(candidate.id.clone(), candidate);
        }
        self.candidates
            .retain(|_, c| alive.contains(&(c.window.handle, c.window.process_started)));
        Ok(())
    }
    /// Observe completed handoffs without replaying a placement request. A window
    /// can have reached a physical display despite an earlier asynchronous timeout.
    fn reconcile_returns(&mut self) -> Result<()> {
        if self.returning.is_empty() {
            return Ok(());
        }
        let physical: Vec<Rect> = displays()
            .into_diagnostic()?
            .into_iter()
            .filter(|d| !d.workspace)
            .map(|d| d.bounds)
            .collect();
        let mut completed = Vec::new();
        for (id, target) in &self.returning {
            let key = thread_key(&target.window);
            // Failed Hook shutdown still owns the thread; never drop it early.
            if self.hooks.contains_key(&key) {
                continue;
            }
            if unsafe { IsWindow(target.window.handle as HWND) } == 0 {
                completed.push((id.clone(), key));
                continue;
            }
            let Ok(current) = Window::inspect(target.window.handle) else {
                continue;
            };
            if ensure_identity(&target.window, &current).is_err() {
                completed.push((id.clone(), key));
                continue;
            }
            let minimized = target.original_minimized
                && matches!(target.handoff_placement, Some(Placement::Original));
            if current.minimized != minimized {
                continue;
            }
            let Ok(bounds) = current.restored_bounds() else {
                continue;
            };
            if handoff_visible(bounds, &physical) {
                completed.push((id.clone(), key));
            }
        }
        for (id, key) in completed {
            self.returning.remove(&id);
            self.candidates.remove(&id);
            if !self
                .taken
                .values()
                .chain(self.returning.values())
                .any(|t| thread_key(&t.window) == key)
            {
                let mut registry = reservations().lock();
                if registry.get(&key) == Some(&self.generation) {
                    registry.remove(&key);
                }
            }
        }
        if self.taken.is_empty()
            && self.returning.is_empty()
            && let Some(workspace) = self.workspace.take()
        {
            workspace.close().into_diagnostic()?;
        }
        Ok(())
    }
    fn allowed(&self, window: &Window) -> bool {
        window.input_surfaces().is_ok_and(|surfaces| {
            surfaces
                .iter()
                .all(|surface| self.allowed_process(surface.process_id))
        })
    }
    fn allowed_process(&self, pid: u32) -> bool {
        if pid == std::process::id() {
            return false;
        }
        let Ok(path) = process_path(pid) else {
            return false;
        };
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if stem.starts_with("daat-locus")
            || self.private_roots.iter().any(|root| path.starts_with(root))
        {
            return false;
        }
        true
    }
    fn launch(&mut self, app_id: &str, cancelled: &AtomicBool) -> Result<Reply> {
        let app = self
            .apps
            .get(app_id)
            .ok_or_else(|| miette!("unknown app; list_apps first"))?
            .clone();
        if !app.launch.starts_with("shell:AppsFolder\\") {
            let path = Path::new(&app.launch);
            if !path.is_file() || self.private_roots.iter().any(|root| path.starts_with(root)) {
                return Err(miette!("application launch path is unavailable or private"));
            }
        }
        // Prepare the destination before the application's first window exists.
        // The service retains this output after the temporary launch lease ends.
        let region = self.display.reserve(960, 720).into_diagnostic()?;
        let bounds = region.bounds();
        let before = DesktopState::read().into_diagnostic()?;
        let existing = launch_window_snapshot()?;
        let mut launcher = Command::new(
            self.launcher_exe
                .as_ref()
                .ok_or_else(|| miette!("application launcher unavailable"))?,
        );
        launcher.current_dir(
            std::env::home_dir().ok_or_else(|| miette!("Windows user profile unavailable"))?,
        );
        launcher
            .arg("computer-use-launch")
            .arg(&app.launch)
            .arg("--display")
            .arg(&region.display().id.monitor_path)
            .arg("--left")
            .arg(bounds.left.to_string())
            .arg("--top")
            .arg(bounds.top.to_string())
            .arg("--right")
            .arg(bounds.right.to_string())
            .arg("--bottom")
            .arg(bounds.bottom.to_string())
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000);
        for name in [
            "SystemRoot",
            "WINDIR",
            "PATH",
            "PATHEXT",
            "USERPROFILE",
            "USERNAME",
            "USERDOMAIN",
            "APPDATA",
            "LOCALAPPDATA",
            "TEMP",
            "TMP",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramData",
        ] {
            if !self
                .protected_env
                .iter()
                .any(|protected| protected.eq_ignore_ascii_case(name))
                && let Some(value) = std::env::var_os(name)
            {
                launcher.env(name, value);
            }
        }
        if cancelled.load(Ordering::Acquire) {
            return Err(miette!("launch cancelled before dispatch"));
        }
        self.effect_started = true;
        let mut child = launcher.spawn().into_diagnostic()?;
        let stdout = child.stdout.take().expect("launcher receipt pipe");
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.take(4097).read_to_end(&mut bytes).map(|_| bytes);
            let _ = tx.send(result);
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().into_diagnostic()? {
                if !status.success() {
                    return Err(miette!("application launcher failed"));
                }
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(miette!(
                    "application launch timed out; observe before retrying"
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let bytes = rx
            .recv_timeout(Duration::from_secs(1))
            .map_err(|_| miette!("launcher receipt unavailable; do not replay launch"))?
            .into_diagnostic()?;
        if bytes.len() > 4096 {
            return Err(miette!("invalid launcher receipt; do not replay launch"));
        }
        let receipt: LaunchReceipt = serde_json::from_slice(&bytes).into_diagnostic()?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let candidates = loop {
            self.discover()?;
            let candidates: Vec<Candidate> = self
                .candidates
                .values()
                .filter(|c| {
                    !existing.contains(&c.window.handle)
                        && matches_launch(
                            &app.launch,
                            c.window.process_id,
                            c.window.process_started,
                            &receipt,
                            |application| c.window.has_application_id(application).unwrap_or(false),
                        )
                })
                .cloned()
                .collect();
            if !candidates.is_empty()
                || std::time::Instant::now() >= deadline
                || cancelled.load(Ordering::Acquire)
            {
                break candidates;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let after = DesktopState::read().into_diagnostic()?;
        let changed = before.foreground != after.foreground
            || before.focus != after.focus
            || before.capture != after.capture
            || before.cursor != after.cursor;
        let rows: Vec<Value> = candidates.iter().map(|c| json!({"window_id":c.id,"title":c.title,"app_id":c.app_id,"on_virtual_screen":region.display().bounds.contains_rect(c.window.bounds)})).collect();
        if !cancelled.load(Ordering::Acquire)
            && candidates.len() == 1
            && !candidates[0].window.owns_control(after.foreground)
            && region
                .display()
                .bounds
                .contains_rect(candidates[0].window.bounds)
        {
            // Startup waiting does not monopolize input. Only its confirmed
            // native takeover joins the same placement/input/capture transaction.
            let _transaction = desktop_operations().lock();
            let mut reply = self.take_with_region(&candidates[0].id, cancelled, Some(region))?;
            reply.payload["discovered_windows"] = json!(rows);
            reply.payload["startup_placement"] = json!("observed_on_virtual_screen");
            reply.summary = "Launched window in virtual workspace".into();
            return Ok(reply);
        }
        let mut reply = Reply::data(
            "Application launch requested",
            json!({"status":"outcome_unknown","discovered_windows":rows,"desktop_changed":changed,"startup_placement":if candidates.iter().any(|c| !region.display().bounds.contains_rect(c.window.bounds)){"application_ignored_placement"}else{"unconfirmed"}}),
        );
        reply.failure = Some(if candidates.iter().any(|c| c.window.owns_control(after.foreground)) {
            "A discovered application window is on the user's foreground desktop; it was not automatically taken. Observe before continuing; do not replay launch."
        } else {
            "No unique new window was confirmed on the virtual screen. The app may have ignored startup placement, used a broker or reused an existing window. List windows; do not replay launch or assume ownership of an existing window."
        }.into());
        Ok(reply)
    }
    fn take(&mut self, window_id: &str, cancelled: &AtomicBool) -> Result<Reply> {
        self.take_with_region(window_id, cancelled, None)
    }
    fn take_with_region(
        &mut self,
        window_id: &str,
        cancelled: &AtomicBool,
        reserved: Option<DisplayRegion>,
    ) -> Result<Reply> {
        if self.returning.contains_key(window_id) {
            return Err(miette!(
                "window handoff is pending; release_window with this window_id before taking it again"
            ));
        }
        if self.taken.contains_key(window_id) {
            return self.observe(window_id);
        }
        let candidate = self
            .candidates
            .get(window_id)
            .ok_or_else(|| miette!("unknown or revoked window; list_windows again"))?
            .clone();
        candidate.window.validate_discovery().into_diagnostic()?;
        let original = candidate.window.restored_bounds().into_diagnostic()?;
        if !self.allowed(&candidate.window) {
            return Err(miette!("target window is not accessible"));
        }
        let key = thread_key(&candidate.window);
        {
            let mut registry = reservations().lock();
            if registry
                .get(&key)
                .is_some_and(|owner| owner != &self.generation)
            {
                return Err(miette!(
                    "GUI thread is owned by another ComputerUse instance"
                ));
            }
            if self.hooks.contains_key(&key) {
                let parent = self
                    .taken
                    .values()
                    .find(|t| thread_key(&t.window) == key)
                    .ok_or_else(|| miette!("missing thread owner"))?;
                if !parent.window.owns_control(candidate.window.handle) {
                    return Err(miette!(
                        "this GUI thread already controls another independent window; give it first"
                    ));
                }
            }
            registry.insert(key, self.generation.clone());
        }
        let existing_hook = self.hooks.contains_key(&key);
        if !existing_hook {
            let installed =
                WindowInput::attach(&candidate.window, &self.hook_dll).into_diagnostic();
            match installed {
                Ok(hook) => {
                    self.hooks.insert(key, hook);
                }
                Err(error) => {
                    reservations().lock().remove(&key);
                    return Err(error);
                }
            }
        }
        let region = (|| {
            let region = match reserved {
                Some(region) => region,
                None => self
                    .display
                    .reserve(
                        original.width().into_diagnostic()?,
                        original.height().into_diagnostic()?,
                    )
                    .into_diagnostic()?,
            };
            if self
                .workspace
                .as_ref()
                .is_none_or(|workspace| workspace.display() != region.display())
            {
                self.workspace =
                    Some(VirtualWorkspace::attach(&region.display().id).into_diagnostic()?);
            }
            Ok::<_, miette::Report>(region)
        })();
        let region = match region {
            Ok(region) => region,
            Err(error) => {
                if !existing_hook {
                    self.hooks.remove(&key);
                    reservations().lock().remove(&key);
                }
                return Err(error);
            }
        };
        let bounds = region.bounds();
        // Record ownership before the first placement effect, including failure paths.
        self.taken.insert(
            window_id.into(),
            Taken {
                region,
                pending_region: None,
                window: candidate.window.clone(),
                original,
                original_minimized: candidate.window.minimized,
                handoff_placement: None,
                title: candidate.title,
                receiver: None,
                state: None,
                view: None,
            },
        );
        if cancelled.load(Ordering::Acquire) {
            return Err(miette!(
                "take cancelled before placement; release the recorded lease"
            ));
        }
        self.effect_started = true;
        let current = Window::inspect(candidate.window.handle).into_diagnostic()?;
        ensure_identity(&candidate.window, &current)?;
        let moved = self
            .hooks
            .get_mut(&key)
            .expect("thread hook")
            .place_window(&current, bounds)
            .into_diagnostic()?;
        self.taken
            .get_mut(window_id)
            .expect("recorded lease")
            .window = moved;
        self.hooks
            .get_mut(&key)
            .expect("thread hook")
            .reobserve()
            .into_diagnostic()?;
        self.observe(window_id)
    }
    fn check_state(&self, window_id: &str, state_id: &str) -> Result<()> {
        self.check_observation(window_id, state_id)?;
        let window = &self.taken[window_id];
        if unsafe { IsWindowEnabled(window.window.handle as HWND) } == 0 {
            return Err(miette!(
                "window is blocked; select its returned modal window"
            ));
        }
        Ok(())
    }
    fn check_observation(&self, window_id: &str, state_id: &str) -> Result<()> {
        let window = self
            .taken
            .get(window_id)
            .ok_or_else(|| miette!("window is not controlled or has been given to the user"))?;
        if window.state.as_deref() != Some(state_id) {
            return Err(miette!("stale window observation; observe again"));
        }
        window.window.validate().into_diagnostic()?;
        window.region.validate().into_diagnostic()?;
        if !window.region.bounds().contains_rect(window.window.bounds) {
            return Err(miette!(
                "window left its shared screen region; release it before taking it again"
            ));
        }
        if !self.allowed(&window.window) {
            return Err(miette!("target input surface is not accessible"));
        }
        if !self
            .workspace
            .as_ref()
            .is_some_and(|w| w.display().bounds.contains_rect(window.window.bounds))
        {
            return Err(miette!("target left the virtual workspace; input refused"));
        }
        Ok(())
    }
    fn receiver(&self, window_id: &str) -> Result<usize> {
        let window = self
            .taken
            .get(window_id)
            .ok_or_else(|| miette!("window is not controlled"))?;
        let receiver = window
            .receiver
            .as_ref()
            .ok_or_else(|| miette!("click an input surface before typing or pressing a key"))?;
        let current = input_target(&window.window, receiver.hwnd)?;
        if current.surface != receiver.surface {
            return Err(miette!(
                "input receiver identity changed; click the observed surface again"
            ));
        }
        if !same_rect(current.bounds, receiver.bounds) {
            return Err(miette!(
                "input receiver geometry changed; click the observed surface again"
            ));
        }
        Ok(receiver.hwnd)
    }
    fn image_point(&self, window_id: &str, point: ImagePoint) -> Result<Point> {
        let taken = self
            .taken
            .get(window_id)
            .ok_or_else(|| miette!("window is not controlled"))?;
        let view = taken
            .view
            .ok_or_else(|| miette!("observe the window before input"))?;
        let point = view.window_point(point)?;
        Ok(Point {
            x: taken.window.bounds.left + point.x - taken.window.client_bounds.left,
            y: taken.window.bounds.top + point.y - taken.window.client_bounds.top,
        })
    }
    fn input(
        &mut self,
        window_id: &str,
        control: usize,
        cancelled: &AtomicBool,
        action: impl FnOnce(
            &mut WindowInput,
        ) -> daat_locus_virtual_workspace::Result<
            daat_locus_virtual_workspace::WindowInputReceipt,
        >,
    ) -> Result<()> {
        if cancelled.load(Ordering::Acquire) {
            return Err(miette!("input cancelled before dispatch"));
        }
        let window = self
            .taken
            .get_mut(window_id)
            .ok_or_else(|| miette!("window was given away"))?;
        let key = thread_key(&window.window);
        let hook = self
            .hooks
            .get_mut(&key)
            .ok_or_else(|| miette!("window hook is unavailable"))?;
        if !hook.input_enabled() {
            return Err(miette!(
                "input is stopped; give/release the window before taking it again"
            ));
        }
        hook.prepare(control).into_diagnostic()?;
        let previous_state = window.state.take();
        self.effect_started = true;
        match action(hook) {
            Err(error @ daat_locus_virtual_workspace::Error::UnsupportedInput(_)) => {
                self.effect_started = false;
                window.state = previous_state;
                return Err(error).into_diagnostic();
            }
            result => result.into_diagnostic()?,
        };
        Ok(())
    }
    fn observe(&mut self, window_id: &str) -> Result<Reply> {
        self.observe_view(window_id, None, None)
    }
    fn adjust(&mut self, args: AdjustArgs, cancelled: &AtomicBool) -> Result<Reply> {
        let taken = self
            .taken
            .get(&args.window_id)
            .ok_or_else(|| miette!("window is not controlled or has been given to the user"))?;
        if args
            .state_id
            .as_ref()
            .is_some_and(|state| taken.state.as_ref() != Some(state))
        {
            return Err(miette!(
                "stale window observation; observe again or use null state_id for explicit recovery"
            ));
        }
        if taken.pending_region.is_some() {
            return Err(miette!(
                "previous window adjustment is unresolved; observe or release it before another adjustment"
            ));
        }
        taken.region.validate().into_diagnostic()?;
        let current = Window::inspect(taken.window.handle).into_diagnostic()?;
        ensure_identity(&taken.window, &current)?;
        if !self.allowed(&current) {
            return Err(miette!("target window is not accessible"));
        }
        let desktop = DesktopState::read().into_diagnostic()?;
        if current.owns_control(desktop.foreground) || desktop.capture != 0 {
            return Err(miette!(
                "adjustment requires a background window and no active mouse capture"
            ));
        }
        let (size, origin) = match args.action {
            WindowAdjustment::Resize if args.x.is_none() && args.y.is_none() => {
                let size = args
                    .width
                    .zip(args.height)
                    .ok_or_else(|| miette!("resize requires width and height"))?;
                (Some(size), None)
            }
            WindowAdjustment::Move if args.width.is_none() && args.height.is_none() => {
                let (x, y) = args
                    .x
                    .zip(args.y)
                    .ok_or_else(|| miette!("move requires x and y"))?;
                (
                    Some((
                        current.bounds.width().into_diagnostic()?,
                        current.bounds.height().into_diagnostic()?,
                    )),
                    Some(Point { x, y }),
                )
            }
            WindowAdjustment::Fit | WindowAdjustment::Restore
                if args.width.is_none()
                    && args.height.is_none()
                    && args.x.is_none()
                    && args.y.is_none() =>
            {
                if matches!(args.action, WindowAdjustment::Fit) {
                    (None, None)
                } else {
                    let bounds = taken.region.bounds();
                    (
                        Some((
                            bounds.width().into_diagnostic()?,
                            bounds.height().into_diagnostic()?,
                        )),
                        Some(Point {
                            x: bounds.left - taken.region.display().bounds.left,
                            y: bounds.top - taken.region.display().bounds.top,
                        }),
                    )
                }
            }
            _ => return Err(miette!("unused adjustment parameters must be null")),
        };
        let plan = taken.region.replacement(size, origin).into_diagnostic()?;
        let bounds = plan.bounds();
        let key = thread_key(&current);
        let hook = self
            .hooks
            .get_mut(&key)
            .ok_or_else(|| miette!("window hook is unavailable"))?;
        hook.reobserve().into_diagnostic()?;
        hook.prepare(current.handle).into_diagnostic()?;
        if cancelled.load(Ordering::Acquire) {
            return Err(miette!("adjustment cancelled before dispatch"));
        }
        let taken = self
            .taken
            .get_mut(&args.window_id)
            .expect("controlled window");
        taken.pending_region = Some(plan);
        taken.state = None;
        taken.view = None;
        taken.receiver = None;
        self.effect_started = true;
        let moved = hook.place_window(&current, bounds).into_diagnostic()?;
        self.taken
            .get_mut(&args.window_id)
            .expect("controlled window")
            .window = moved;
        self.observe(&args.window_id)
    }
    fn observe_view(
        &mut self,
        window_id: &str,
        region: Option<ImageRegion>,
        scale: Option<u32>,
    ) -> Result<Reply> {
        if self.returning.contains_key(window_id) {
            self.reconcile_returns()?;
            return Ok(Reply::data(
                "Window handoff observation",
                json!({
                    "status": if self.returning.contains_key(window_id) { "handoff_pending" } else { "given_to_user" },
                    "window_id":window_id,"input_revoked":true,"references_revoked":true,
                "supported_actions": if self.returning.contains_key(window_id) { vec!["release_window"] } else { vec![] }
                }),
            ));
        }
        let current = {
            let window = self
                .taken
                .get_mut(window_id)
                .ok_or_else(|| miette!("window is not controlled or has been given to the user"))?;
            window.state = None;
            window.view = None;
            let current = Window::inspect(window.window.handle).into_diagnostic()?;
            ensure_identity(&window.window, &current)?;
            current
        };
        if !self.allowed(&current) {
            return Err(miette!("target input surface is not accessible"));
        }
        let window = self.taken.get_mut(window_id).expect("controlled window");
        if window
            .pending_region
            .as_ref()
            .is_some_and(|region| region.bounds().contains_rect(current.bounds))
        {
            window
                .pending_region
                .as_ref()
                .expect("planned region")
                .validate()
                .into_diagnostic()?;
            let pending = window.pending_region.take().expect("planned region");
            window.region = pending;
        }
        if window.pending_region.is_some() {
            return Err(miette!(
                "window adjustment is unresolved; observe again or release it without replaying the adjustment"
            ));
        }
        window.region.validate().into_diagnostic()?;
        if !window.region.bounds().contains_rect(current.bounds) {
            return Err(miette!(
                "window left its shared screen region; release it before taking it again"
            ));
        }
        let workspace = self
            .workspace
            .as_ref()
            .ok_or_else(|| miette!("workspace is unavailable"))?;
        if !workspace.display().bounds.contains_rect(current.bounds) {
            return Err(miette!("window left the owned workspace"));
        }
        let view = ObservedView::new(
            current.bounds.width().into_diagnostic()?,
            current.bounds.height().into_diagnostic()?,
            region,
            scale,
        )?;
        let frame = current.capture().into_diagnostic()?;
        current.validate().into_diagnostic()?;
        window.region.validate().into_diagnostic()?;
        let image_bounds = Rect {
            left: current.bounds.left + view.region.x,
            top: current.bounds.top + view.region.y,
            right: current.bounds.left + view.region.x + view.region.width,
            bottom: current.bounds.top + view.region.y + view.region.height,
        };
        let png = crop_png_scaled(&frame, image_bounds, view.scale)?;
        window.window = current;
        window.title = title(window.window.handle);
        let state = id();
        window.state = Some(state.clone());
        window.view = Some(view);
        let hook = self
            .hooks
            .get_mut(&thread_key(&window.window))
            .ok_or_else(|| miette!("hook lease disappeared"))?;
        let enabled = hook.input_enabled();
        if enabled {
            hook.reobserve().into_diagnostic()?;
            // Clicking or a per-monitor DPI transition can relayout the control.
            // Bind the route to THIS fresh image; subsequent input still rejects
            // geometry changes after this observation.
            window.receiver = window.receiver.as_ref().and_then(|previous| {
                input_target(&window.window, previous.hwnd)
                    .ok()
                    .filter(|current| current.surface == previous.surface)
            });
        } else {
            window.receiver = None;
        }
        let backend = match &window.receiver {
            Some(receiver) => hook.backend_for(receiver.hwnd).into_diagnostic()?,
            None => hook.backend(),
        };
        let supported_actions = match &window.receiver {
            Some(receiver) => hook
                .supported_actions_for(receiver.hwnd)
                .into_diagnostic()?,
            None => hook.supported_actions(),
        };
        let receiver = window.receiver.as_ref().map(|r| json!({"class":r.surface.class,
            "kind":match backend{InputBackend::Hook=>"message_receiver",InputBackend::Automation=>"accessible_control"}}));
        let observation = json!({"window_id":window_id,"state_id":state,"title":window.title,"width":view.width(),"height":view.height(),
            "window_width":window.window.bounds.width().into_diagnostic()?,"window_height":window.window.bounds.height().into_diagnostic()?,
            "workspace":{"width":workspace.display().bounds.width().into_diagnostic()?,"height":workspace.display().bounds.height().into_diagnostic()?,"window_x":window.window.bounds.left-workspace.display().bounds.left,"window_y":window.window.bounds.top-workspace.display().bounds.top},
            "client_area":{"x":window.window.client_bounds.left-window.window.bounds.left,"y":window.window.client_bounds.top-window.window.bounds.top,"width":window.window.client_bounds.width().into_diagnostic()?,"height":window.window.client_bounds.height().into_diagnostic()?},
            "view":{"region":view.region,"scale":view.scale},
            "input_target":receiver,"supported_actions":supported_actions,"screenshot":null,"screenshot_url":null});
        self.discover()?;
        let base = self.taken.get(window_id).expect("controlled window");
        let related: Vec<Value> = self.candidates.values().filter(|c| c.id != window_id && base.window.owns_control(c.window.handle)).map(|c| json!({"window_id":c.id,"title":c.title,"taken":self.taken.contains_key(&c.id)})).collect();
        Ok(Reply {
            payload: json!({"status":"observed","observation":observation,"related_windows":related}),
            image: Some(png),
            summary: "Observed window".into(),
            failure: None,
        })
    }
    fn after_input(&mut self, window_id: &str) -> Result<Reply> {
        match self.observe(window_id) {
            Ok(mut reply) => {
                reply.payload["status"] = json!("dispatched");
                reply.summary = "Window input dispatched".into();
                Ok(reply)
            }
            Err(error) => Ok(Reply {
                payload: json!({"status":"outcome_unknown","window_id":window_id,"observation_error":error.to_string()}),
                image: None,
                summary: "Input dispatched; observation unavailable".into(),
                failure: Some(
                    "Observe before deciding whether more input is needed; do not replay".into(),
                ),
            }),
        }
    }
    fn give(&mut self, window_id: &str, placement: Placement) -> Result<Reply> {
        let key = self
            .taken
            .get(window_id)
            .or_else(|| self.returning.get(window_id))
            .map(|t| thread_key(&t.window))
            .ok_or_else(|| miette!("window was already given away or its reference is unknown"))?;
        let family: Vec<String> = self
            .taken
            .iter()
            .filter(|(_, t)| thread_key(&t.window) == key)
            .map(|(id, _)| id.clone())
            .collect();
        self.effect_started = true;
        // Revoke every input/observation reference BEFORE unhooking or moving anything.
        for id in family {
            let mut target = self.taken.remove(&id).expect("family member");
            target.receiver = None;
            target.state = None;
            target.view = None;
            target.handoff_placement = Some(if id == window_id {
                placement
            } else {
                Placement::Original
            });
            self.candidates.remove(&id);
            self.returning.insert(id, target);
        }
        if let Some(target) = self.returning.get_mut(window_id) {
            target.handoff_placement = Some(placement);
        }
        if let Some(hook) = self.hooks.get_mut(&key) {
            hook.stop().into_diagnostic()?;
        }
        self.hooks.remove(&key);
        let family: Vec<String> = self
            .returning
            .iter()
            .filter(|(_, t)| thread_key(&t.window) == key)
            .map(|(id, _)| id.clone())
            .collect();
        let mut returned = Vec::new();
        for id in family {
            let target = self.returning.get(&id).expect("returning member");
            if unsafe { IsWindow(target.window.handle as HWND) } == 0 {
                self.returning.remove(&id);
                continue;
            }
            let current = Window::inspect(target.window.handle).into_diagnostic()?;
            ensure_identity(&target.window, &current)?;
            let selected = target.handoff_placement.unwrap_or(Placement::Original);
            let destination = placement_bounds(target.original, selected)?;
            let return_original = matches!(selected, Placement::Original);
            let moved = if current.minimized && target.original_minimized && return_original {
                if current.restored_bounds().into_diagnostic()? != destination {
                    return Err(miette!(
                        "minimized window placement is unresolved; observe or release again after the pending restoration settles"
                    ));
                }
                current
            } else {
                current
                    .relocate_for_handoff(destination)
                    .into_diagnostic()?
            };
            if target.original_minimized && return_original {
                moved.minimize().into_diagnostic()?;
            }
            returned.push(json!({"title":target.title,"placement":selected,"controlled":false}));
            self.returning.remove(&id);
        }
        reservations().lock().remove(&key);
        if self.taken.is_empty()
            && self.returning.is_empty()
            && let Some(workspace) = self.workspace.take()
        {
            workspace.close().into_diagnostic()?;
        }
        Ok(Reply::data(
            "Window given to user",
            json!({"status":"given_to_user","windows":returned,"references_revoked":true}),
        ))
    }
    fn release_all(&mut self) -> Result<()> {
        let _transaction = desktop_operations().lock();
        self.reconcile_returns()?;
        let ids: Vec<String> = self
            .taken
            .keys()
            .chain(self.returning.keys())
            .cloned()
            .collect();
        let mut errors = Vec::new();
        for id in ids {
            if (self.taken.contains_key(&id) || self.returning.contains_key(&id))
                && let Err(error) = self.give(&id, Placement::Original)
            {
                errors.push(error.to_string());
            }
        }
        if self.taken.is_empty()
            && self.returning.is_empty()
            && let Some(workspace) = self.workspace.take()
            && let Err(error) = workspace.close()
        {
            errors.push(error.to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(miette!("ComputerUse cleanup: {}", errors.join("; ")))
        }
    }
}

pub(super) fn launch_from_clean_process(app: &str, display: &str, bounds: [i32; 4]) -> Result<()> {
    let display = displays()
        .into_diagnostic()?
        .into_iter()
        .find(|d| d.id.monitor_path == display)
        .ok_or_else(|| miette!("launch virtual display is unavailable"))?;
    let result = daat_locus_virtual_workspace::launch_on_display(
        app,
        &display.id,
        Rect {
            left: bounds[0],
            top: bounds[1],
            right: bounds[2],
            bottom: bounds[3],
        },
    )
    .into_diagnostic()?;
    println!(
        "{}",
        json!({"process_id":result.map(|p|p.process_id),"process_started":result.map(|p|p.process_started)})
    );
    Ok(())
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}

fn ensure_identity(old: &Window, current: &Window) -> Result<()> {
    if old.handle != current.handle
        || old.process_id != current.process_id
        || old.process_started != current.process_started
        || old.thread_id != current.thread_id
    {
        return Err(miette!("window identity changed"));
    }
    Ok(())
}
fn title(hwnd: usize) -> String {
    let mut text = vec![0u16; unsafe { GetWindowTextLengthW(hwnd as HWND) }.max(0) as usize + 1];
    let count = unsafe { GetWindowTextW(hwnd as HWND, text.as_mut_ptr(), text.len() as i32) }.max(0)
        as usize;
    String::from_utf16_lossy(&text[..count])
}
fn process_path(pid: u32) -> Result<PathBuf> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(miette!("process metadata is unavailable"));
    }
    let mut path = vec![0u16; 32768];
    let mut length = path.len() as u32;
    let result = unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) };
    unsafe {
        CloseHandle(process);
    }
    if result == 0 {
        return Err(miette!("process path is unavailable"));
    }
    std::fs::canonicalize(PathBuf::from(String::from_utf16_lossy(
        &path[..length as usize],
    )))
    .into_diagnostic()
}
fn input_target(window: &Window, hwnd: usize) -> Result<InputTarget> {
    let surface = InputSurface::inspect(window, hwnd).into_diagnostic()?;
    let mut bounds = RECT::default();
    if unsafe { GetWindowRect(hwnd as HWND, &mut bounds) } == 0 {
        return Err(miette!("input receiver disappeared"));
    }
    Ok(InputTarget {
        hwnd,
        surface,
        bounds,
    })
}
fn same_rect(a: RECT, b: RECT) -> bool {
    a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom
}

#[cfg(test)]
fn crop_png(frame: &daat_locus_virtual_workspace::Frame, bounds: Rect) -> Result<Vec<u8>> {
    crop_png_scaled(frame, bounds, 1)
}
fn crop_png_scaled(
    frame: &daat_locus_virtual_workspace::Frame,
    bounds: Rect,
    scale: u32,
) -> Result<Vec<u8>> {
    if !frame.bounds.contains_rect(bounds) {
        return Err(miette!(
            "requested image is outside the workspace; no partial image was returned"
        ));
    }
    let view = ObservedView::new(
        bounds.width().into_diagnostic()?,
        bounds.height().into_diagnostic()?,
        None,
        Some(scale),
    )?;
    let width = view.width() as usize;
    let height = view.height() as usize;
    let scale = scale as usize;
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let offset =
            ((bounds.top - frame.bounds.top) as usize + y / scale) * frame.width as usize * 4
                + (bounds.left - frame.bounds.left) as usize * 4;
        for x in 0..width {
            let start = offset + (x / scale) * 4;
            let bgra = &frame.bgra[start..start + 4];
            rgba.extend_from_slice(&[bgra[2], bgra[1], bgra[0], 255]);
        }
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .into_diagnostic()?
            .write_image_data(&rgba)
            .into_diagnostic()?;
    }
    Ok(bytes)
}

fn handoff_visible(bounds: Rect, physical: &[Rect]) -> bool {
    let (Ok(width), Ok(height)) = (bounds.width(), bounds.height()) else {
        return false;
    };
    let center = Point {
        x: bounds.left + width / 2,
        y: bounds.top + height / 2,
    };
    physical.iter().any(|screen| screen.contains(center))
}

fn placement_bounds(original: Rect, placement: Placement) -> Result<Rect> {
    let physical = displays()
        .into_diagnostic()?
        .into_iter()
        .filter(|d| !d.workspace)
        .collect::<Vec<_>>();
    let screen = physical
        .iter()
        .find(|d| d.bounds.contains_rect(original))
        .or_else(|| physical.first())
        .ok_or_else(|| miette!("no user display available for handoff"))?;
    let rect = RECT {
        left: screen.bounds.left,
        top: screen.bounds.top,
        right: screen.bounds.right,
        bottom: screen.bounds.bottom,
    };
    let monitor = unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return Err(miette!("user display work area unavailable"));
    }
    let area = Rect {
        left: info.rcWork.left,
        top: info.rcWork.top,
        right: info.rcWork.right,
        bottom: info.rcWork.bottom,
    };
    Ok(place_in_area(original, area, placement))
}
fn installed_apps() -> Result<Vec<(String, String)>> {
    use ::windows::{
        Win32::{
            System::Com::{
                COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
            },
            UI::Shell::{
                BHID_EnumItems, IEnumShellItems, IShellItem, SHCreateItemFromParsingName,
                SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY,
            },
        },
        core::w,
    };
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .into_diagnostic()?;
    struct Com;
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    let _com = Com;
    let folder: IShellItem =
        unsafe { SHCreateItemFromParsingName(w!("shell:AppsFolder"), None) }.into_diagnostic()?;
    let items: IEnumShellItems =
        unsafe { folder.BindToHandler(None, &BHID_EnumItems) }.into_diagnostic()?;
    let mut apps = Vec::new();
    loop {
        let mut item = [None];
        let mut count = 0;
        unsafe { items.Next(&mut item, Some(&mut count)) }.into_diagnostic()?;
        if count == 0 {
            break;
        }
        let Some(item) = item[0].take() else {
            continue;
        };
        let name = unsafe { item.GetDisplayName(SIGDN_NORMALDISPLAY) }.into_diagnostic()?;
        let parsing =
            unsafe { item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING) }.into_diagnostic()?;
        let label = unsafe { name.to_string() }.into_diagnostic();
        let value = unsafe { parsing.to_string() }.into_diagnostic();
        unsafe {
            CoTaskMemFree(Some(name.0.cast()));
            CoTaskMemFree(Some(parsing.0.cast()));
        }
        let (label, value) = (label?, value?);
        let key = value.rsplit('\\').next().unwrap_or(&value);
        apps.push((label, format!("shell:AppsFolder\\{key}")));
    }
    Ok(apps)
}

#[cfg(test)]
mod launch_tests;
#[cfg(test)]
mod tests;
