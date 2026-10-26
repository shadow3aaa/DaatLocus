use miette::{IntoDiagnostic, Result, miette};
#[cfg(not(target_os = "android"))]
use std::time::Duration;

#[cfg(windows)]
pub type Stream = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(unix)]
pub type Stream = tokio::net::UnixStream;

#[cfg(windows)]
pub fn is_administrator() -> Result<bool> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(miette!("Cannot inspect OS elevation"));
        }
        let mut elevation = std::mem::zeroed::<TOKEN_ELEVATION>();
        let mut size = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of_val(&elevation) as u32,
            &mut size,
        );
        CloseHandle(token);
        if ok == 0 {
            return Err(miette!("Cannot inspect OS elevation"));
        }
        Ok(elevation.TokenIsElevated != 0)
    }
}
#[cfg(all(unix, not(target_os = "android")))]
pub fn is_administrator() -> Result<bool> {
    Ok(unsafe { libc::geteuid() } == 0)
}
#[cfg(target_os = "android")]
pub fn is_administrator() -> Result<bool> {
    Ok(false)
}

#[cfg(not(target_os = "android"))]
pub async fn launch() -> Result<Stream> {
    launch_executable(std::env::current_exe().into_diagnostic()?).await
}

#[cfg(windows)]
pub(super) async fn launch_executable(executable: std::path::PathBuf) -> Result<Stream> {
    use std::os::windows::io::AsRawHandle;
    let endpoint = format!(r"\\.\pipe\DaatLocus-Sudo-{}", uuid::Uuid::new_v4().simple());
    let server = crate::daemon::control::sudo_listener(std::path::Path::new(&endpoint))?;
    let parameters = format!(
        "sudo-host --endpoint {endpoint} --parent-pid {}",
        std::process::id()
    );
    // UAC may wait on the secure desktop. A dedicated thread must not hold
    // Tokio shutdown open after its Actor/connection has been cancelled.
    let (launched, response) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("dl-sudo-authorization".into())
        .spawn(move || {
            let result = (|| -> Result<std::os::windows::io::OwnedHandle> {
                use std::os::windows::{ffi::OsStrExt, io::FromRawHandle};
                use windows_sys::Win32::UI::Shell::{
                    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
                    SHELLEXECUTEINFOW, ShellExecuteExW,
                };
                let file: Vec<u16> = executable.as_os_str().encode_wide().chain([0]).collect();
                let parameters: Vec<u16> = parameters.encode_utf16().chain([0]).collect();
                let verb: Vec<u16> = "runas".encode_utf16().chain([0]).collect();
                unsafe {
                    use windows::Win32::System::Com::{
                        COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize,
                    };
                    let com = CoInitializeEx(None, COINIT_MULTITHREADED);
                    let mut info = std::mem::zeroed::<SHELLEXECUTEINFOW>();
                    info.cbSize = std::mem::size_of_val(&info) as u32;
                    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC;
                    info.lpVerb = verb.as_ptr();
                    info.lpFile = file.as_ptr();
                    info.lpParameters = parameters.as_ptr();
                    info.nShow = 0;
                    let ok = ShellExecuteExW(&mut info);
                    if com.is_ok() {
                        CoUninitialize();
                    }
                    if ok == 0 || info.hProcess.is_null() {
                        return Err(miette!(
                            "Windows administrator authorization was declined or unavailable"
                        ));
                    }
                    Ok(std::os::windows::io::OwnedHandle::from_raw_handle(
                        info.hProcess,
                    ))
                }
            })();
            let _ = launched.send(result);
        })
        .into_diagnostic()?;
    let process = tokio::time::timeout(Duration::from_secs(120), response)
        .await
        .map_err(|_| miette!("OS administrator authorization timed out"))?
        .into_diagnostic()??;
    tokio::time::timeout(Duration::from_secs(30), server.connect())
        .await
        .map_err(|_| miette!("Elevated native host did not connect"))?
        .into_diagnostic()?;
    let mut pid = 0;
    if unsafe {
        windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId(
            server.as_raw_handle(),
            &mut pid,
        )
    } == 0
        || pid
            != unsafe {
                windows_sys::Win32::System::Threading::GetProcessId(process.as_raw_handle())
            }
    {
        return Err(miette!(
            "Elevated host process identity does not match the authorized launch"
        ));
    }
    Ok(server)
}

#[cfg(windows)]
pub async fn connect(
    endpoint: &str,
    parent: u32,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    use std::os::windows::io::AsRawHandle;
    let stream = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(endpoint)
        .into_diagnostic()?;
    let mut pid = 0;
    if unsafe {
        windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId(
            stream.as_raw_handle(),
            &mut pid,
        )
    } == 0
        || pid != parent
    {
        return Err(miette!("Sudo native parent identity changed"));
    }
    Ok(stream)
}

#[cfg(all(unix, not(target_os = "android")))]
pub(super) async fn launch_executable(executable: std::path::PathBuf) -> Result<Stream> {
    use std::{os::unix::fs::PermissionsExt, path::PathBuf};
    let endpoint =
        PathBuf::from("/tmp").join(format!("dl-sudo-{}.sock", uuid::Uuid::new_v4().simple()));
    struct SocketPath(PathBuf);
    impl Drop for SocketPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let cleanup = SocketPath(endpoint.clone());
    let listener = tokio::net::UnixListener::bind(&endpoint).into_diagnostic()?;
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600))
        .into_diagnostic()?;
    let parameters = vec![
        "sudo-host".to_owned(),
        "--endpoint".into(),
        endpoint.to_string_lossy().into_owned(),
        "--parent-pid".into(),
        std::process::id().to_string(),
    ];
    let mut command = if is_administrator()? {
        tokio::process::Command::new(&executable)
    } else {
        #[cfg(target_os = "linux")]
        {
            let mut command = tokio::process::Command::new("/usr/bin/pkexec");
            command.arg("--disable-internal-agent").arg(&executable);
            command
        }
        #[cfg(target_os = "macos")]
        {
            let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
            let script = std::iter::once(executable.to_string_lossy().into_owned())
                .chain(parameters.clone())
                .map(|value| quote(&value))
                .collect::<Vec<_>>()
                .join(" ");
            let apple = format!(
                "do shell script \"{} >/dev/null 2>&1 &\" with administrator privileges",
                script.replace('\\', "\\\\").replace('"', "\\\"")
            );
            let mut command = tokio::process::Command::new("/usr/bin/osascript");
            command.arg("-e").arg(apple);
            command
        }
    };
    #[cfg(target_os = "linux")]
    command.args(&parameters);
    #[cfg(target_os = "macos")]
    if is_administrator()? {
        command.args(&parameters);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut launcher = command
        .spawn()
        .map_err(|_| miette!("OS administrator authorization service is unavailable"))?;
    let (stream, _) = tokio::select! {
        result=listener.accept()=>result.into_diagnostic()?,
        result=launcher.wait()=> {
            // osascript launches the helper in the background and may finish
            // before its authenticated socket connection arrives.
            #[cfg(target_os = "macos")]
            if result.as_ref().is_ok_and(|status| status.success()) {
                tokio::time::timeout(Duration::from_secs(30), listener.accept())
                    .await.map_err(|_| miette!("Elevated native host did not connect"))?
                    .into_diagnostic()?
            } else {
                return Err(miette!("Administrator authorization was declined or the helper did not connect"));
            }
            #[cfg(not(target_os = "macos"))]
            { let _ = result; return Err(miette!("Administrator authorization was declined or the helper did not connect")); }
        },
        ()=tokio::time::sleep(Duration::from_secs(120))=>return Err(miette!("OS administrator authorization timed out")),
    };
    if stream.peer_cred().into_diagnostic()?.uid() != 0 {
        return Err(miette!("Elevated host has no root authority"));
    }
    tokio::spawn(async move {
        let _ = launcher.wait().await;
    });
    drop(cleanup);
    Ok(stream)
}

#[cfg(unix)]
pub async fn connect(endpoint: &str, parent: u32) -> Result<Stream> {
    let stream = tokio::net::UnixStream::connect(endpoint)
        .await
        .into_diagnostic()?;
    if stream.peer_cred().into_diagnostic()?.pid() != Some(parent as _) {
        return Err(miette!("Sudo native parent identity changed"));
    }
    Ok(stream)
}
#[cfg(target_os = "android")]
pub async fn launch() -> Result<Stream> {
    Err(miette!("Sudo is not available on Android"))
}
