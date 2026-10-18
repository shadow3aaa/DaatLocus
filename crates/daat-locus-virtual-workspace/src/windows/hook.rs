use super::{DesktopState, InputReceipt, InputSurface, Window, last_error};
use crate::{
    Error, Point, Result,
    hook_protocol::{native::*, *},
};
use std::{
    cell::UnsafeCell,
    marker::PhantomData,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::Path,
    ptr,
    rc::Rc,
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, FreeLibrary, GetLastError, HMODULE, HWND,
        INVALID_HANDLE_VALUE,
    },
    System::{
        LibraryLoader::{
            GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
            LoadLibraryExW,
        },
        Memory::{CreateFileMappingW, PAGE_READWRITE},
        Threading::{
            GetCurrentProcess, GetCurrentProcessId, GetProcessMitigationPolicy, IsWow64Process2,
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            ProcessSignaturePolicy, WaitForSingleObject,
        },
    },
    UI::{Input::KeyboardAndMouse::IsWindowEnabled, WindowsAndMessaging::*},
};

/// Synchronous message-backed key dispatch, not system-wide keyboard injection.
/// Only Shift and Control are currently supported. No Raw Input or async-key virtualization.
#[derive(Clone, Copy, Default)]
pub struct KeyModifiers {
    pub shift: bool,
    pub control: bool,
}

/// Message-backed mouse actions. Click down/up are dispatched in one callback;
/// longer trajectories use WindowHook::drag. Independent capture is not implemented.
#[derive(Clone, Copy)]
pub enum MouseAction {
    Move,
    ClickLeft,
    Wheel(i16),
}

#[derive(Debug)]
pub struct HookReceipt {
    pub desktop: InputReceipt,
    /// Messages dispatched, not proof that the application applied them.
    pub messages: u32,
}

/// A live thread-scoped DLL hook for one observed top-level window and its children.
/// Holds activation/focus vetoes until close/Drop. It never restores foreground/cursor.
/// Only the native host architecture is supported; protected/elevated targets may reject it.
pub(super) struct ThreadHook {
    root: Window,
    process: OwnedHandle,
    pub(super) surface: InputSurface,
    mapping: Mapping,
    module: HMODULE,
    hooks: Vec<HHOOK>,
    message: u32,
    poisoned: bool,
    _thread: PhantomData<Rc<()>>,
}

impl ThreadHook {
    pub fn input_enabled(&self) -> bool {
        !self.poisoned && self.mapping.shared().enabled.load(Ordering::Acquire) != 0
    }
    /// Update geometry after a guarded nonactivating move, preserving the Hook lease.
    pub fn reobserve(&mut self) -> Result<Window> {
        let current = Window::inspect(self.root.handle)?;
        if current.process_id != self.root.process_id
            || current.process_started != self.root.process_started
            || current.thread_id != self.root.thread_id
            || self.mapping.shared().enabled.load(Ordering::Acquire) == 0
        {
            return Err(Error::InvalidState("hook window identity changed"));
        }
        self.root = current.clone();
        Ok(current)
    }
    /// Hot-install the explicitly selected DLL on the target's GUI thread. This loads
    /// code in the application. The DLL must come from the matching Cargo build.
    pub fn attach(target: &Window, surface: InputSurface, dll: impl AsRef<Path>) -> Result<Self> {
        target.validate()?;
        surface.validate(target)?;
        if surface.process_id == unsafe { GetCurrentProcessId() } {
            return Err(Error::InvalidState(
                "hook controller must be outside the target process",
            ));
        }
        let before = DesktopState::read()?;
        if before.foreground == target.handle {
            return Err(Error::InvalidState(
                "select a background target before installing the hook",
            ));
        }
        let process = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                surface.process_id,
            )
        };
        if process.is_null() {
            return Err(last_error("OpenProcess(hook target)"));
        }
        let process = unsafe { OwnedHandle::from_raw_handle(process) };
        // PROCESS_MITIGATION_BINARY_SIGNATURE_POLICY's first two bits require
        // Microsoft / Store signatures. Our companion DLL cannot satisfy either.
        let mut signature_policy = 0u32;
        let policy_ok = unsafe {
            GetProcessMitigationPolicy(
                process.as_raw_handle(),
                ProcessSignaturePolicy,
                (&mut signature_policy as *mut u32).cast(),
                std::mem::size_of::<u32>(),
            )
        } != 0;
        if !policy_ok || signature_policy & 3 != 0 {
            let error = if !policy_ok {
                last_error("GetProcessMitigationPolicy(hook target)")
            } else {
                Error::HookSignaturePolicy
            };
            return Err(error);
        }
        let mut target_machine = 0;
        let mut native_machine = 0;
        let architecture_ok = unsafe {
            IsWow64Process2(
                process.as_raw_handle(),
                &mut target_machine,
                &mut native_machine,
            )
        } != 0;
        let mut our_machine = 0;
        let mut our_native = 0;
        let ours_ok =
            unsafe { IsWow64Process2(GetCurrentProcess(), &mut our_machine, &mut our_native) } != 0;
        if !architecture_ok || !ours_ok {
            return Err(last_error("IsWow64Process2"));
        }
        if target_machine != our_machine || native_machine != our_native {
            return Err(Error::InvalidState(
                "hook DLL and target must have the same architecture",
            ));
        }
        let name = mapping_name(
            surface.process_id,
            surface.thread_id,
            surface.process_started,
        );
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                ptr::null(),
                PAGE_READWRITE,
                0,
                std::mem::size_of::<Shared>() as u32,
                name.as_ptr(),
            )
        };
        if handle.is_null() {
            return Err(last_error("CreateFileMappingW(hook)"));
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                CloseHandle(handle);
            }
            return Err(Error::InvalidState(
                "this GUI thread already has a workspace hook lease",
            ));
        }
        let mut mapping = unsafe { Mapping::from_handle(handle) }
            .ok_or_else(|| last_error("MapViewOfFile(hook)"))?;
        let owner_started = process_started(unsafe { GetCurrentProcess() })
            .ok_or_else(|| last_error("GetProcessTimes(hook owner)"))?;
        let token =
            u32::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..4].try_into().unwrap()) | 1;
        unsafe {
            mapping.initialize(Shared {
                abi: ABI,
                size: std::mem::size_of::<Shared>() as u32,
                owner_pid: GetCurrentProcessId(),
                target_pid: surface.process_id,
                owner_started,
                target_started: surface.process_started,
                root: target.handle as u64,
                transport: surface.handle as u64,
                frame_pid: target.process_id,
                frame_thread: target.thread_id,
                frame_started: target.process_started,
                token,
                enabled: AtomicU32::new(1),
                state: AtomicU32::new(IDLE),
                error: AtomicU32::new(0),
                completed: AtomicU32::new(0),
                activation_vetoes: AtomicU32::new(0),
                focus_vetoes: AtomicU32::new(0),
                command: UnsafeCell::new(Command::default()),
            });
        }
        use std::os::windows::ffi::OsStrExt;
        let dll = std::fs::canonicalize(dll).map_err(|source| Error::Windows {
            operation: "resolve hook DLL",
            source,
        })?;
        let dll: Vec<u16> = dll.as_os_str().encode_wide().chain(Some(0)).collect();
        let module = unsafe {
            LoadLibraryExW(
                dll.as_ptr(),
                ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            return Err(last_error("LoadLibraryExW(hook)"));
        }
        let mut session = Self {
            root: target.clone(),
            process,
            surface,
            mapping,
            module,
            hooks: Vec::new(),
            message: 0,
            poisoned: false,
            _thread: PhantomData,
        };
        let abi = unsafe { GetProcAddress(module, c"DlWorkspaceHookAbi".as_ptr().cast()) }
            .ok_or_else(|| last_error("GetProcAddress(hook ABI)"))?;
        let abi: unsafe extern "system" fn() -> u32 = unsafe { std::mem::transmute(abi) };
        if unsafe { abi() } != ABI {
            return Err(Error::InvalidState("hook DLL ABI mismatch"));
        }
        for (kind, symbol) in [
            (WH_CBT, c"DlWorkspaceCbt"),
            (WH_GETMESSAGE, c"DlWorkspaceGetMessage"),
        ] {
            let callback = unsafe { GetProcAddress(module, symbol.as_ptr().cast()) }
                .ok_or_else(|| last_error("GetProcAddress(hook callback)"))?;
            let hook = unsafe {
                SetWindowsHookExW(
                    kind,
                    Some(std::mem::transmute::<
                        unsafe extern "system" fn() -> isize,
                        unsafe extern "system" fn(i32, usize, isize) -> isize,
                    >(callback)),
                    module,
                    session.surface.thread_id,
                )
            };
            if hook.is_null() {
                return Err(last_error("SetWindowsHookExW"));
            }
            session.hooks.push(hook);
        }
        let name: Vec<u16> = MESSAGE.encode_utf16().chain(Some(0)).collect();
        session.message = unsafe { RegisterWindowMessageW(name.as_ptr()) };
        if session.message == 0 {
            return Err(last_error("RegisterWindowMessageW"));
        }
        session.dispatch(Command::default(), Duration::from_secs(3))?;
        if session.poisoned {
            return Err(Error::InvalidState(
                "desktop state changed during hook handshake",
            ));
        }
        Ok(session)
    }

    fn dispatch(&mut self, command: Command, timeout: Duration) -> Result<HookReceipt> {
        if timeout.is_zero() || timeout > Duration::from_secs(10) {
            return Err(Error::InvalidState(
                "hook timeout must be greater than zero and at most 10 seconds",
            ));
        }
        if self.poisoned {
            return Err(Error::InvalidState(
                "hook outcome uncertain; close and observe before further input",
            ));
        }
        self.root.validate()?;
        let shared = self.mapping.shared();
        if shared.enabled.load(Ordering::Acquire) == 0 {
            return Err(Error::InvalidState(
                "hook target was destroyed or hook disabled",
            ));
        }
        if shared.state.load(Ordering::Acquire) != IDLE {
            return Err(Error::InvalidState("hook command is still outstanding"));
        }
        let before = DesktopState::read()?;
        if before.foreground == self.root.handle {
            return Err(Error::InvalidState(
                "target is now foreground; close the hook for user takeover",
            ));
        }
        unsafe {
            shared.command.get().write(command);
        }
        shared.error.store(0, Ordering::Relaxed);
        shared.completed.store(0, Ordering::Relaxed);
        shared.state.store(PENDING, Ordering::Release);
        if unsafe {
            PostMessageW(
                self.surface.handle as HWND,
                self.message,
                shared.token as usize,
                0,
            )
        } == 0
        {
            shared.cancel_pending();
            self.poisoned = true;
            return Err(last_error("PostMessageW(hook command)"));
        }
        let deadline = Instant::now() + timeout;
        loop {
            if shared.state.load(Ordering::Acquire) == DONE {
                break;
            }
            if Instant::now() >= deadline {
                shared.cancel_pending();
                self.poisoned = true;
                shared.enabled.store(0, Ordering::Release);
                return Err(Error::Timeout(
                    "hook command; execution may have started, do not replay",
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let error = shared.error.load(Ordering::Relaxed);
        let messages = shared.completed.load(Ordering::Relaxed);
        shared.state.store(IDLE, Ordering::Release);
        // A failed post-effect observation must also stop further input.
        self.poisoned = true;
        let after = DesktopState::read()?;
        self.poisoned = error != 0
            || before.foreground != after.foreground
            || before.focus != after.focus
            || before.capture != after.capture;
        if error != 0 {
            return Err(Error::Windows {
                operation: "target-thread hook command (may be partially applied)",
                source: std::io::Error::from_raw_os_error(error as i32),
            });
        }
        Ok(HookReceipt {
            desktop: InputReceipt { before, after },
            messages,
        })
    }

    fn validate_control(&self, handle: usize) -> Result<()> {
        let hwnd = handle as HWND;
        let mut pid = 0;
        self.surface.validate(&self.root)?;
        if unsafe { GetWindowThreadProcessId(hwnd, &mut pid) } != self.surface.thread_id
            || pid != self.surface.process_id
            || !self.root.owns_control(handle)
            || unsafe { IsWindowVisible(hwnd) == 0 || IsWindowEnabled(hwnd) == 0 }
        {
            return Err(Error::InvalidState(
                "control must be a visible enabled child on the hooked GUI thread",
            ));
        }
        Ok(())
    }

    /// Request closing a controlled top-level window inside the tracked callback.
    /// A modal close handler remains RUNNING until it returns; handoff must wait.
    pub fn request_close(&mut self, window: usize, timeout: Duration) -> Result<HookReceipt> {
        self.validate_control(window)?;
        self.dispatch(
            Command {
                kind: CLOSE,
                target: window as u64,
                ..Default::default()
            },
            timeout,
        )
    }

    /// Dispatch UTF-16 WM_CHAR on the target thread to an explicit control. This is
    /// caret insertion if the control implements it.
    pub fn text(&mut self, control: usize, text: &str, timeout: Duration) -> Result<HookReceipt> {
        self.validate_control(control)?;
        let mut command = Command {
            kind: TEXT,
            target: control as u64,
            ..Default::default()
        };
        for (index, unit) in text.encode_utf16().enumerate() {
            if index >= MAX_TEXT || unit == 0 {
                return Err(Error::InvalidState(
                    "text must contain at most 1024 UTF-16 units and no NUL",
                ));
            }
            command.text[index] = unit;
            command.length += 1;
        }
        self.dispatch(command, timeout)
    }

    /// Dispatch WM_KEYDOWN/UP with temporary target-thread keyboard state. It does
    /// not run the application's accelerator loop or synthesize WM_CHAR; use text for text.
    pub fn key(
        &mut self,
        control: usize,
        virtual_key: u16,
        modifiers: KeyModifiers,
        timeout: Duration,
    ) -> Result<HookReceipt> {
        self.validate_control(control)?;
        if virtual_key == 0 || virtual_key > 254 {
            return Err(Error::InvalidState("invalid virtual key"));
        }
        self.dispatch(
            Command {
                kind: KEY,
                target: control as u64,
                key: virtual_key as u32,
                modifiers: u32::from(modifiers.shift) | (u32::from(modifiers.control) << 1),
                ..Default::default()
            },
            timeout,
        )
    }

    /// Coordinates are local to the explicit control's client area. This dispatches
    /// mouse messages without moving the real cursor. Apps using Raw Input, polling
    /// GetCursorPos or custom focus logic may not respond. Existing capture is refused;
    /// native controls can transiently acquire capture during a synchronous click.
    pub fn mouse(
        &mut self,
        control: usize,
        action: MouseAction,
        local: Point,
        timeout: Duration,
    ) -> Result<HookReceipt> {
        self.validate_control(control)?;
        let (key, modifiers) = match action {
            MouseAction::Move => (0, 0),
            MouseAction::ClickLeft => (1, 0),
            MouseAction::Wheel(delta) => (2, delta as u16 as u32),
        };
        self.dispatch(
            Command {
                kind: MOUSE,
                target: control as u64,
                key,
                modifiers,
                x: local.x,
                y: local.y,
                ..Default::default()
            },
            timeout,
        )
    }

    /// Send one bounded left-button drag in a single target-thread callback. The first
    /// point must be in the control's client area; later points may leave it (signed
    /// 16-bit client coordinates). No sleeps, persistent button hold or OLE drag/drop.
    /// Existing capture is refused. Native controls can hold real capture during this
    /// synchronous gesture; this is not independent concurrent capture virtualization.
    pub fn drag(
        &mut self,
        control: usize,
        path: &[Point],
        timeout: Duration,
    ) -> Result<HookReceipt> {
        self.validate_control(control)?;
        if !(2..=MAX_DRAG_POINTS).contains(&path.len())
            || path
                .iter()
                .any(|p| i16::try_from(p.x).is_err() || i16::try_from(p.y).is_err())
        {
            return Err(Error::InvalidState(
                "drag needs 2..=128 points in signed 16-bit client coordinates",
            ));
        }
        let mut command = Command {
            kind: DRAG,
            target: control as u64,
            length: path.len() as u32,
            ..Default::default()
        };
        for (destination, source) in command.path.iter_mut().zip(path) {
            *destination = DragPoint {
                x: source.x,
                y: source.y,
            };
        }
        self.dispatch(command, timeout)
    }

    pub fn veto_counts(&self) -> (u32, u32) {
        let shared = self.mapping.shared();
        (
            shared.activation_vetoes.load(Ordering::Relaxed),
            shared.focus_vetoes.load(Ordering::Relaxed),
        )
    }

    /// Revoke input, wait for an already-started command, then remove hooks. On timeout
    /// the guard remains owned and disabled so a handoff can wait again without moving
    /// the window while a prior command is still running.
    pub fn stop(&mut self) -> Result<()> {
        self.revoke();
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.mapping.shared().state.load(Ordering::Acquire) == RUNNING {
            if Instant::now() >= deadline {
                return Err(Error::Timeout(
                    "hook input still executing; window cannot be handed off yet",
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        self.detach()
    }

    pub(super) fn revoke(&self) {
        self.mapping.shared().enabled.store(0, Ordering::Release);
        self.mapping.shared().cancel_pending();
    }

    fn detach(&mut self) -> Result<()> {
        self.mapping.shared().enabled.store(0, Ordering::Release);
        self.mapping.shared().cancel_pending();
        let mut first_error = None;
        while let Some(hook) = self.hooks.pop() {
            if unsafe { UnhookWindowsHookEx(hook) } == 0
                && unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } != 0
                && first_error.is_none()
            {
                first_error = Some(last_error("UnhookWindowsHookEx"));
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}
impl Drop for ThreadHook {
    fn drop(&mut self) {
        let _ = self.detach();
        unsafe {
            FreeLibrary(self.module);
        }
    }
}
