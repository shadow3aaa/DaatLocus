//! Same-architecture, versioned shared memory. One controller and one GUI thread.
//! Payload ownership transfers with the atomic state; no process pointers cross IPC.
use std::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU32, Ordering},
};

pub const ABI: u32 = 4;
pub const MESSAGE: &str = "DaatLocus.VirtualWorkspace.Command.v4";
pub const IDLE: u32 = 0;
pub const PENDING: u32 = 1;
pub const RUNNING: u32 = 2;
pub const DONE: u32 = 3;
pub const CANCELLED: u32 = 4;
pub const PING: u32 = 0;
pub const TEXT: u32 = 1;
pub const KEY: u32 = 2;
pub const MOUSE: u32 = 3;
pub const DRAG: u32 = 4;
pub const CLOSE: u32 = 5;
pub const MAX_TEXT: usize = 1024;
pub const MAX_DRAG_POINTS: usize = 128;

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct DragPoint {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Command {
    pub kind: u32,
    pub key: u32,
    pub modifiers: u32,
    pub length: u32,
    pub target: u64,
    pub x: i32,
    pub y: i32,
    pub text: [u16; MAX_TEXT],
    pub path: [DragPoint; MAX_DRAG_POINTS],
}
impl Default for Command {
    fn default() -> Self {
        Self {
            kind: PING,
            key: 0,
            modifiers: 0,
            length: 0,
            target: 0,
            x: 0,
            y: 0,
            text: [0; MAX_TEXT],
            path: [DragPoint::default(); MAX_DRAG_POINTS],
        }
    }
}

#[repr(C)]
pub struct Shared {
    pub abi: u32,
    pub size: u32,
    pub owner_pid: u32,
    pub target_pid: u32,
    pub owner_started: u64,
    pub target_started: u64,
    pub root: u64,
    pub transport: u64,
    pub frame_pid: u32,
    pub frame_thread: u32,
    pub frame_started: u64,
    pub token: u32,
    pub enabled: AtomicU32,
    pub state: AtomicU32,
    pub error: AtomicU32,
    pub completed: AtomicU32,
    pub activation_vetoes: AtomicU32,
    pub focus_vetoes: AtomicU32,
    pub command: UnsafeCell<Command>,
}

impl Shared {
    pub fn valid(&self) -> bool {
        self.abi == ABI && self.size as usize == std::mem::size_of::<Self>()
    }

    pub fn claim(&self) -> bool {
        self.state
            .compare_exchange(PENDING, RUNNING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn cancel_pending(&self) -> bool {
        self.state
            .compare_exchange(PENDING, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

pub fn mapping_name(pid: u32, tid: u32, started: u64) -> Vec<u16> {
    format!("Local\\DaatLocus.VirtualWorkspace.v{ABI}.{pid}.{tid}.{started:016x}")
        .encode_utf16()
        .chain(Some(0))
        .collect()
}

#[cfg(windows)]
pub mod native {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE},
        System::{
            Memory::{
                FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW,
                UnmapViewOfFile,
            },
            Threading::{
                GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
                PROCESS_SYNCHRONIZE, WaitForSingleObject,
            },
        },
    };

    pub fn process_started(process: HANDLE) -> Option<u64> {
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        if unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }
            == 0
        {
            return None;
        }
        Some((created.dwHighDateTime as u64) << 32 | created.dwLowDateTime as u64)
    }

    pub struct Mapping {
        handle: HANDLE,
        view: MEMORY_MAPPED_VIEW_ADDRESS,
    }
    impl Mapping {
        /// Takes ownership of a file mapping handle, including on failure.
        pub unsafe fn from_handle(handle: HANDLE) -> Option<Self> {
            if handle.is_null() {
                return None;
            }
            let view = unsafe {
                MapViewOfFile(
                    handle,
                    FILE_MAP_ALL_ACCESS,
                    0,
                    0,
                    std::mem::size_of::<Shared>(),
                )
            };
            if view.Value.is_null() {
                unsafe {
                    CloseHandle(handle);
                }
                return None;
            }
            Some(Self { handle, view })
        }
        pub fn open(name: &[u16]) -> Option<Self> {
            unsafe { Self::from_handle(OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, name.as_ptr())) }
        }
        pub fn shared(&self) -> &Shared {
            unsafe { &*self.view.Value.cast::<Shared>() }
        }
        /// Only the creator, before publishing any hook, may initialize the region.
        pub unsafe fn initialize(&mut self, shared: Shared) {
            unsafe {
                self.view.Value.cast::<Shared>().write(shared);
            }
        }
    }
    impl Drop for Mapping {
        fn drop(&mut self) {
            unsafe {
                UnmapViewOfFile(self.view);
                CloseHandle(self.handle);
            }
        }
    }

    pub fn owner_alive(shared: &Shared) -> bool {
        let process = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                shared.owner_pid,
            )
        };
        if process.is_null() {
            return false;
        }
        let alive = process_started(process) == Some(shared.owner_started)
            && unsafe { WaitForSingleObject(process, 0) } == 258;
        unsafe {
            CloseHandle(process);
        }
        alive
    }
}
