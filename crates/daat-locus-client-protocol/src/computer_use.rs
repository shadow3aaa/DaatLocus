//! Bounded GUI envelopes and authenticated, profile-local native IPC. No GUI framework dependency.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug)]
pub struct Error(pub String);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayConfig {
    pub width: u32,
    pub height: u32,
    pub scale: u32,
}
impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            scale: 1,
        }
    }
}
impl DisplayConfig {
    pub fn validate(self) -> Result<()> {
        if !(256..=4096).contains(&self.width)
            || !(256..=4096).contains(&self.height)
            || u64::from(self.width) * u64::from(self.height) > 8_000_000
            || !matches!(self.scale, 1 | 2)
            || !self.width.is_multiple_of(self.scale)
            || !self.height.is_multiple_of(self.scale)
        {
            return Err(Error("Invalid virtual display resolution or scale".into()));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Permissions {
    pub enabled: bool,
    pub accessibility: bool,
    pub screen_recording: bool,
    pub display_supported: bool,
    pub display_ready: bool,
    pub display: DisplayConfig,
}
impl Permissions {
    pub fn ready(&self) -> bool {
        self.enabled && self.accessibility && self.screen_recording && self.display_ready
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub scope: String,
    pub session: String,
    pub epoch: u64,
    pub operation: String,
    pub arguments: Value,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    pub summary: String,
    pub payload: Value,
    pub failure: Option<String>,
    pub image: Option<String>,
    pub has_resources: bool,
}

#[cfg(target_os = "macos")]
mod transport {
    use super::*;
    use serde_json::json;
    use std::{
        io::{Read, Write},
        os::{fd::AsRawFd, unix::net::UnixStream},
        path::{Path, PathBuf},
        time::Duration,
    };
    unsafe extern "C" {
        fn proc_pidpath(pid: libc::c_int, buffer: *mut libc::c_void, size: u32) -> libc::c_int;
    }
    fn executable(pid: i32) -> Result<PathBuf> {
        let mut bytes = vec![0u8; 4096];
        let count = unsafe { proc_pidpath(pid, bytes.as_mut_ptr().cast(), bytes.len() as u32) };
        if count <= 0 {
            return Err(Error("Cannot verify GUI peer".into()));
        }
        bytes.truncate(count as usize);
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        Ok(PathBuf::from(std::ffi::OsString::from(
            String::from_utf8(bytes).map_err(|_| Error("Invalid peer path".into()))?,
        )))
    }
    pub fn verify(stream: &UnixStream, expected: &Path) -> Result<()> {
        let (mut uid, mut gid) = (0, 0);
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0
            || uid != unsafe { libc::geteuid() }
        {
            return Err(Error("GUI peer owner mismatch".into()));
        }
        let mut pid = 0i32;
        let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                0,
                2,
                (&mut pid as *mut i32).cast(),
                &mut len,
            )
        } != 0
            || executable(pid)? != expected
        {
            return Err(Error("GUI peer executable mismatch".into()));
        }
        Ok(())
    }
    pub fn write_frame(stream: &mut UnixStream, bytes: &[u8]) -> Result<()> {
        if bytes.len() > 48 * 1024 * 1024 {
            return Err(Error("GUI reply exceeds budget".into()));
        }
        stream
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .and_then(|()| stream.write_all(bytes))
            .map_err(|_| Error("GUI transport closed; do not replay".into()))
    }
    pub fn read_frame(stream: &mut UnixStream, limit: usize) -> Result<Vec<u8>> {
        let mut prefix = [0u8; 4];
        stream
            .read_exact(&mut prefix)
            .map_err(|_| Error("GUI outcome unknown; transport closed, do not replay".into()))?;
        let length = u32::from_le_bytes(prefix) as usize;
        if length > limit {
            return Err(Error("GUI frame exceeds budget".into()));
        }
        let mut bytes = vec![0; length];
        stream
            .read_exact(&mut bytes)
            .map_err(|_| Error("GUI reply incomplete; do not replay".into()))?;
        Ok(bytes)
    }
    #[derive(Clone)]
    pub struct Client {
        socket: PathBuf,
        peer: PathBuf,
    }
    impl Client {
        pub fn new(root: &Path, peer: &Path) -> Self {
            Self {
                socket: root.join("runtime/computer-use.sock"),
                peer: peer.into(),
            }
        }
        pub fn is_disconnected(&self) -> bool {
            matches!(UnixStream::connect(&self.socket),Err(error) if matches!(error.kind(),std::io::ErrorKind::NotFound|std::io::ErrorKind::ConnectionRefused))
        }
        pub fn call(&self, request: &Request) -> Result<Reply> {
            let mut stream = UnixStream::connect(&self.socket).map_err(|_| {
                Error("Open the desktop client and enable GUI operation in Settings".into())
            })?;
            stream
                .set_read_timeout(Some(Duration::from_secs(20)))
                .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(5))))
                .map_err(|_| Error("Cannot configure GUI transport".into()))?;
            verify(&stream, &self.peer)?;
            let bytes =
                serde_json::to_vec(request).map_err(|_| Error("Invalid GUI request".into()))?;
            if bytes.len() > 65536 {
                return Err(Error("GUI request exceeds budget".into()));
            }
            write_frame(&mut stream, &bytes)?;
            serde_json::from_slice(&read_frame(&mut stream, 48 * 1024 * 1024)?)
                .map_err(|_| Error("Invalid GUI reply; do not replay".into()))
        }
        pub fn cancel(&self, id: &str) {
            let _ = self.call(&Request {
                id: format!("cancel-{id}"),
                scope: String::new(),
                session: String::new(),
                epoch: 0,
                operation: "cancel".into(),
                arguments: json!({"id":id}),
            });
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn native_peer_and_frame_budget_are_checked_before_decoding() {
            let (mut left, mut right) = UnixStream::pair().unwrap();
            let executable = std::env::current_exe().unwrap();
            verify(&left, &executable).unwrap();
            assert!(verify(&left, &executable.with_file_name("another-executable")).is_err());
            right.write_all(&65537u32.to_le_bytes()).unwrap();
            assert!(read_frame(&mut left, 65536).is_err());
        }
        #[test]
        fn disconnected_endpoint_does_not_report_a_live_host() {
            let root = tempfile::tempdir().unwrap();
            let peer = std::env::current_exe().unwrap();
            assert!(Client::new(root.path(), &peer).is_disconnected());
            std::fs::create_dir(root.path().join("runtime")).unwrap();
            let _listener = std::os::unix::net::UnixListener::bind(
                root.path().join("runtime/computer-use.sock"),
            )
            .unwrap();
            assert!(!Client::new(root.path(), &peer).is_disconnected());
        }
    }
}
#[cfg(target_os = "macos")]
pub use transport::{Client, read_frame, verify, write_frame};

#[cfg(test)]
mod display_tests {
    use super::*;
    #[test]
    fn display_modes_reject_unbounded_pixels_and_fractional_hidpi_points() {
        for (width, height, scale, accepted) in [
            (1920, 1080, 1, true),
            (2560, 1600, 2, true),
            (4096, 1080, 1, true),
            (255, 720, 1, false),
            (1920, 0, 1, false),
            (4096, 4096, 1, false),
            (1920, 1080, 0, false),
            (1920, 1080, 3, false),
            (1919, 1080, 2, false),
            (1920, 1079, 2, false),
            (u32::MAX, u32::MAX, 2, false),
        ] {
            assert_eq!(
                DisplayConfig {
                    width,
                    height,
                    scale
                }
                .validate()
                .is_ok(),
                accepted,
                "mode {width}x{height}@{scale}"
            );
        }
    }
}
