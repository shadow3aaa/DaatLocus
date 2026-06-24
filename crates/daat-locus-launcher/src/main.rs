#![cfg_attr(windows, windows_subsystem = "windows")]
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
const CONFIG_FILE_NAME: &str = "config.toml";
const DEFAULT_DAEMON_PORT: u16 = 53825;
const ENABLE_TRAY_ENV: &str = "DAAT_LOCUS_ENABLE_TRAY";
const NO_TRAY_ENV: &str = "DAAT_LOCUS_NO_TRAY";
const LAUNCHER_LOG_FILE_NAME: &str = "launcher.log";
#[cfg(windows)]
const MAIN_BINARY_NAME: &str = "daat-locus.exe";
#[cfg(not(windows))]
const MAIN_BINARY_NAME: &str = "daat-locus";
fn main() {
    if let Err(err) = run() {
        let home = daat_locus_home();
        log_launcher(&home, &format!("launcher failed: {err}"));
    }
}
fn run() -> io::Result<()> {
    let home = daat_locus_home();
    let config_path = home.join("config").join(CONFIG_FILE_NAME);
    if !config_path.is_file() {
        log_launcher(
            &home,
            &format!(
                "config file missing at {}; skipped daemon startup",
                config_path.display()
            ),
        );
        return Ok(());
    }
    let port = configured_daemon_port(&config_path).unwrap_or(DEFAULT_DAEMON_PORT);
    if daemon_port_is_active(port) {
        return Ok(());
    }
    let main_binary = installed_main_binary()?;
    if !main_binary.is_file() {
        let message = format!("main executable missing at {}", main_binary.display());
        log_launcher(&home, &message);
        return Err(io::Error::new(io::ErrorKind::NotFound, message));
    }
    spawn_daemon(&main_binary, &home)
}
fn installed_main_binary() -> io::Result<PathBuf> {
    let launcher = env::current_exe()?;
    let install_dir = launcher.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "launcher executable has no parent directory",
        )
    })?;
    Ok(install_dir.join(MAIN_BINARY_NAME))
}
fn spawn_daemon(main_binary: &Path, home: &Path) -> io::Result<()> {
    let stderr = launcher_log_file(home)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    let with_tray = env::var_os(NO_TRAY_ENV).is_none();
    let mut command = Command::new(main_binary);
    configure_daemon_command(&mut command, with_tray);
    command.stderr(stderr);
    apply_detached_creation_flags(&mut command, with_tray);
    command.spawn().map(|_| ())
}
fn configure_daemon_command(command: &mut Command, with_tray: bool) {
    command
        .arg("daemon")
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    if with_tray {
        command.env(ENABLE_TRAY_ENV, "1");
    } else {
        command.env_remove(ENABLE_TRAY_ENV);
    }
}
#[cfg(windows)]
fn apply_detached_creation_flags(command: &mut Command, with_tray: bool) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x00000008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let mut flags = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
    if !with_tray {
        flags |= DETACHED_PROCESS;
    }
    command.creation_flags(flags);
}
#[cfg(not(windows))]
fn apply_detached_creation_flags(_command: &mut Command, _with_tray: bool) {}
fn configured_daemon_port(config_path: &Path) -> Option<u16> {
    let content = fs::read_to_string(config_path).ok()?;
    daemon_port_from_config(&content)
}
fn daemon_port_from_config(content: &str) -> Option<u16> {
    let mut in_daemon_section = false;
    for raw_line in content.lines() {
        let line = raw_line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            in_daemon_section = section.trim() == "daemon";
            continue;
        }
        if !in_daemon_section {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "port" {
            continue;
        }
        return value.trim().trim_matches('"').parse::<u16>().ok();
    }
    None
}
fn daemon_port_is_active(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok()
}
fn daat_locus_home() -> PathBuf {
    if let Ok(value) = env::var("DAAT_LOCUS_HOME")
        && !value.trim().is_empty()
    {
        return PathBuf::from(value);
    }
    if let Some(home_dir) = env::home_dir() {
        return home_dir.join(".daat-locus");
    }
    env::temp_dir().join(".daat-locus")
}
fn launcher_log_file(home: &Path) -> io::Result<File> {
    let logs_dir = home.join("logs");
    fs::create_dir_all(&logs_dir)?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs_dir.join(LAUNCHER_LOG_FILE_NAME))
}
fn log_launcher(home: &Path, message: &str) {
    if let Ok(mut file) = launcher_log_file(home) {
        let _ = writeln!(file, "[{}] {message}", unix_timestamp_millis());
    }
}
fn unix_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::{ENABLE_TRAY_ENV, configure_daemon_command, daemon_port_from_config};
    use std::ffi::OsStr;
    use std::process::Command;

    #[test]
    fn daemon_port_parser_reads_daemon_section() {
        let config = "[provider]\nport = 1234\n\n[daemon]\nport = 53826\n";
        assert_eq!(daemon_port_from_config(config), Some(53826));
    }
    #[test]
    fn daemon_port_parser_allows_quoted_port() {
        let config = "[daemon]\nport = \"53827\"\n";
        assert_eq!(daemon_port_from_config(config), Some(53827));
    }
    #[test]
    fn daemon_port_parser_ignores_missing_daemon_section() {
        let config = "[provider]\nport = 1234\n";
        assert_eq!(daemon_port_from_config(config), None);
    }

    #[test]
    fn daemon_command_with_tray_marks_child_startup_intent() {
        let mut command = Command::new("daat-locus");

        configure_daemon_command(&mut command, true);

        assert_eq!(
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec!["daemon".to_string(), "serve".to_string()]
        );
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == OsStr::new(ENABLE_TRAY_ENV))
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().into_owned()),
            Some("1".to_string())
        );
    }
}
