use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[cfg(target_os = "windows")]
use std::{
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use tokio::sync::mpsc;
#[cfg(target_os = "windows")]
use tokio::sync::oneshot;

#[cfg(target_os = "windows")]
use crate::daemon::DAEMON_CLIENT_HOST;
use crate::daemon::DaemonControlCommand;

#[cfg(target_os = "windows")]
mod platform_tray {
    use super::*;
    use miette::{Result, miette};
    use tao::{
        event::Event,
        event_loop::{ControlFlow, EventLoopBuilder},
        platform::run_return::EventLoopExtRunReturn,
    };
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
        menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    };

    #[cfg(all(unix, not(target_os = "macos")))]
    use tao::platform::unix::EventLoopBuilderExtUnix;
    #[cfg(target_os = "windows")]
    use tao::platform::windows::EventLoopBuilderExtWindows;

    const OPEN_WEBUI_ID: &str = "open-webui";
    const EXIT_DAEMON_ID: &str = "exit-daemon";
    const TRAY_POLL_INTERVAL: Duration = Duration::from_millis(500);

    enum TrayEvent {
        Menu(MenuEvent),
        TrayIcon(TrayIconEvent),
    }

    pub(super) fn spawn(
        port: u16,
        control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
        shutdown: Arc<AtomicBool>,
    ) {
        let _ = thread::Builder::new()
            .name("daat-locus-tray".to_string())
            .spawn(move || {
                if let Err(err) = run_tray_loop(port, control_tx, shutdown) {
                    tracing::warn!("daemon tray stopped: {err:?}");
                }
            });
    }

    fn run_tray_loop(
        port: u16,
        control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
        shutdown: Arc<AtomicBool>,
    ) -> Result<()> {
        let mut builder = EventLoopBuilder::<TrayEvent>::with_user_event();
        allow_event_loop_on_tray_thread(&mut builder);
        let mut event_loop = builder.build();
        let proxy = event_loop.create_proxy();
        TrayIconEvent::set_event_handler(Some({
            let proxy = proxy.clone();
            move |event| {
                let _ = proxy.send_event(TrayEvent::TrayIcon(event));
            }
        }));
        MenuEvent::set_event_handler(Some(move |event| {
            let _ = proxy.send_event(TrayEvent::Menu(event));
        }));

        let _tray = build_tray_icon(port)?;

        event_loop.run_return(move |event, _, control_flow| {
            *control_flow = ControlFlow::WaitUntil(Instant::now() + TRAY_POLL_INTERVAL);
            if shutdown.load(Ordering::Relaxed) {
                *control_flow = ControlFlow::Exit;
                return;
            }

            match event {
                Event::UserEvent(TrayEvent::Menu(event)) => {
                    if event.id == OPEN_WEBUI_ID {
                        open_webui(port);
                    } else if event.id == EXIT_DAEMON_ID {
                        request_daemon_shutdown(&control_tx);
                        *control_flow = ControlFlow::Exit;
                    }
                }
                Event::UserEvent(TrayEvent::TrayIcon(TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }))
                | Event::UserEvent(TrayEvent::TrayIcon(TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                })) => {
                    open_webui(port);
                }
                _ => {}
            }
        });
        Ok(())
    }

    #[cfg(target_os = "windows")]
    fn allow_event_loop_on_tray_thread(builder: &mut EventLoopBuilder<TrayEvent>) {
        builder.with_any_thread(true);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn allow_event_loop_on_tray_thread(builder: &mut EventLoopBuilder<TrayEvent>) {
        builder.with_any_thread(true);
    }

    fn build_tray_icon(port: u16) -> Result<TrayIcon> {
        let menu = Menu::new();
        let open_webui = MenuItem::with_id(OPEN_WEBUI_ID, "Open WebUI", true, None);
        let separator = PredefinedMenuItem::separator();
        let exit_daemon =
            MenuItem::with_id(EXIT_DAEMON_ID, "Exit (Stop DaatLocus Daemon)", true, None);
        menu.append_items(&[&open_webui, &separator, &exit_daemon])
            .map_err(|err| miette!("failed to build tray menu: {err}"))?;

        TrayIconBuilder::new()
            .with_tooltip(format!("DaatLocus Daemon on :{port}"))
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_icon(daemon_icon()?)
            .build()
            .map_err(|err| miette!("failed to create tray icon: {err}"))
    }

    const TRAY_ICON_BYTES: &[u8] = include_bytes!("../assets/tray-icon.ico");

    fn daemon_icon() -> Result<Icon> {
        let path = materialize_tray_icon()?;
        Icon::from_path(&path, Some((32, 32)))
            .map_err(|err| miette!("failed to load tray icon {}: {err}", path.display()))
    }

    fn materialize_tray_icon() -> Result<std::path::PathBuf> {
        let path = std::env::temp_dir().join("daat-locus-tray-icon.ico");
        let should_write = match std::fs::read(&path) {
            Ok(current) => current != TRAY_ICON_BYTES,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
            Err(err) => {
                return Err(miette!(
                    "failed to read tray icon {}: {err}",
                    path.display()
                ));
            }
        };

        if should_write {
            std::fs::write(&path, TRAY_ICON_BYTES)
                .map_err(|err| miette!("failed to write tray icon {}: {err}", path.display()))?;
        }

        Ok(path)
    }

    fn request_daemon_shutdown(control_tx: &mpsc::UnboundedSender<DaemonControlCommand>) {
        let (completion_tx, _completion_rx) = oneshot::channel();
        if control_tx
            .send(DaemonControlCommand::ShutdownRequested { completion_tx })
            .is_err()
        {
            tracing::warn!("daemon tray could not request daemon shutdown");
        }
    }

    fn open_webui(port: u16) {
        let url = format!("http://{}:{}", DAEMON_CLIENT_HOST, port);
        if let Err(err) = open_url(&url) {
            tracing::warn!("daemon tray failed to open WebUI: {err}");
        }
    }
}

pub(crate) struct DaemonTrayHandle {
    shutdown: Arc<AtomicBool>,
}

impl DaemonTrayHandle {
    pub(crate) fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

pub(crate) fn spawn_daemon_tray(
    port: u16,
    control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
) -> Option<DaemonTrayHandle> {
    let shutdown = Arc::new(AtomicBool::new(false));
    spawn_platform_tray(port, control_tx, shutdown.clone())?;
    Some(DaemonTrayHandle { shutdown })
}

#[cfg(target_os = "windows")]
fn spawn_platform_tray(
    port: u16,
    control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
    shutdown: Arc<AtomicBool>,
) -> Option<()> {
    platform_tray::spawn(port, control_tx, shutdown);
    Some(())
}

#[cfg(not(target_os = "windows"))]
fn spawn_platform_tray(
    _port: u16,
    _control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
    _shutdown: Arc<AtomicBool>,
) -> Option<()> {
    None
}

#[cfg(target_os = "windows")]
fn open_url(url: &str) -> std::io::Result<()> {
    Command::new("cmd")
        .args(["/C", "start", "", url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}
