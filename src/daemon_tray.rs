use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
use std::time::{Duration, Instant};

use miette::Result;
#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
use miette::miette;
use tokio::sync::{mpsc, oneshot};

use crate::{daemon::DaemonControlCommand, open_url::open_url};

pub(crate) const NO_TRAY_ENV: &str = "DAAT_LOCUS_NO_TRAY";
pub(crate) const ENABLE_TRAY_ENV: &str = "DAAT_LOCUS_ENABLE_TRAY";

#[derive(Clone)]
pub(crate) struct DaemonTrayHandle {
    shutdown: Arc<AtomicBool>,
}

impl DaemonTrayHandle {
    pub(crate) fn new() -> Self {
        Self {
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

pub(crate) struct DaemonTrayStartup {
    pub(crate) port: u16,
    pub(crate) control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
}

pub(crate) fn should_attempt_daemon_tray() -> bool {
    if std::env::var_os(NO_TRAY_ENV).is_some() {
        return false;
    }
    std::env::var_os(ENABLE_TRAY_ENV).is_some() || platform_tray::gui_session_available()
}

pub(crate) fn run_daemon_tray(startup: DaemonTrayStartup, handle: DaemonTrayHandle) -> Result<()> {
    platform_tray::run_tray_loop(startup.port, startup.control_tx, handle.shutdown)
}

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
mod platform_tray {
    use super::*;
    use tao::{
        event::{Event, StartCause},
        event_loop::{ControlFlow, EventLoopBuilder},
        platform::run_return::EventLoopExtRunReturn,
    };
    use tray_icon::{
        Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent,
        menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    };

    const OPEN_WEBUI_ID: &str = "open-webui";
    const EXIT_DAEMON_ID: &str = "exit-daemon";
    const TRAY_POLL_INTERVAL: Duration = Duration::from_millis(500);

    enum TrayEvent {
        Menu(MenuEvent),
        TrayIcon(TrayIconEvent),
    }

    pub(super) fn gui_session_available() -> bool {
        #[cfg(target_os = "linux")]
        {
            std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            true
        }
    }

    pub(super) fn run_tray_loop(
        port: u16,
        control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
        shutdown: Arc<AtomicBool>,
    ) -> Result<()> {
        let mut event_loop = EventLoopBuilder::<TrayEvent>::with_user_event().build();
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

        let mut tray = None;
        let mut startup_error = None;
        event_loop.run_return(|event, _, control_flow| {
            *control_flow = ControlFlow::WaitUntil(Instant::now() + TRAY_POLL_INTERVAL);
            if shutdown.load(Ordering::Relaxed) {
                *control_flow = ControlFlow::Exit;
                return;
            }

            match event {
                Event::NewEvents(StartCause::Init) if tray.is_none() => {
                    match build_tray_icon(port) {
                        Ok(tray_icon) => tray = Some(tray_icon),
                        Err(err) => {
                            startup_error = Some(err);
                            *control_flow = ControlFlow::Exit;
                        }
                    }
                }
                Event::UserEvent(TrayEvent::Menu(event)) => {
                    if event.id == OPEN_WEBUI_ID {
                        open_webui(port);
                    } else if event.id == EXIT_DAEMON_ID {
                        request_daemon_shutdown(&control_tx);
                        *control_flow = ControlFlow::Exit;
                    }
                }
                Event::UserEvent(TrayEvent::TrayIcon(TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                })) => {
                    open_webui(port);
                }
                _ => {}
            }
        });

        if let Some(err) = startup_error {
            return Err(err);
        }
        Ok(())
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
            .with_menu_on_left_click(true)
            .with_icon(daemon_icon()?)
            .with_icon_as_template(true)
            .build()
            .map_err(|err| miette!("failed to create tray icon: {err}"))
    }

    fn daemon_icon() -> Result<Icon> {
        let (rgba, width, height) = daemon_icon_rgba()?;
        Icon::from_rgba(rgba, width, height)
            .map_err(|err| miette!("failed to build daemon tray icon: {err}"))
    }

    const TRAY_ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

    fn daemon_icon_rgba() -> Result<(Vec<u8>, u32, u32)> {
        let decoder = png::Decoder::new(std::io::Cursor::new(TRAY_ICON_PNG));
        let mut reader = decoder
            .read_info()
            .map_err(|err| miette!("failed to read daemon tray icon png: {err}"))?;
        let buffer_size = reader
            .output_buffer_size()
            .ok_or_else(|| miette!("daemon tray icon png is too large to decode"))?;
        let mut rgba = vec![0; buffer_size];
        let output = reader
            .next_frame(&mut rgba)
            .map_err(|err| miette!("failed to decode daemon tray icon png: {err}"))?;
        rgba.truncate(output.buffer_size());

        if output.color_type != png::ColorType::Rgba || output.bit_depth != png::BitDepth::Eight {
            return Err(miette!(
                "daemon tray icon png must be 8-bit RGBA, got {:?} {:?}",
                output.color_type,
                output.bit_depth
            ));
        }

        Ok((rgba, output.width, output.height))
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
        let url = format!("http://{}:{}", crate::daemon::DAEMON_CLIENT_HOST, port);
        if let Err(err) = open_url(&url) {
            tracing::warn!("daemon tray failed to open WebUI: {err}");
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
mod platform_tray {
    use super::*;

    pub(super) fn gui_session_available() -> bool {
        false
    }

    pub(super) fn run_tray_loop(
        _port: u16,
        _control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
        _shutdown: Arc<AtomicBool>,
    ) -> Result<()> {
        Ok(())
    }
}
