use daat_locus::{
    actors::ActorSystem,
    config_setup,
    daat_locus_paths::daat_locus_paths,
    daemon::{self, DaemonLock},
    service::{ServiceState, router},
};
use miette::{IntoDiagnostic, Result, miette};
use std::{net::SocketAddr, path::PathBuf};

mod cli;
mod terminal_logo;

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let (command, arguments) = cli::parse_args();
    let native_host_bridge = matches!(command, cli::Command::WebHost);
    if matches!(
        command,
        cli::Command::Serve | cli::Command::Start { .. } | cli::Command::Account { .. }
    ) && let Err(error) = daemon::control::current_executable_sha256()
    {
        eprintln!("{error:?}");
        std::process::exit(1);
    }
    #[cfg(windows)]
    if let cli::Command::ComputerUseLaunch {
        app,
        display,
        left,
        top,
        right,
        bottom,
    } = &command
    {
        if let Err(error) = daat_locus::computer_use_app::launch_from_clean_process(
            app,
            display,
            [*left, *top, *right, *bottom],
        ) {
            eprintln!("{error:?}");
            std::process::exit(1);
        }
        return;
    }
    let profile_base = std::env::var_os("DAAT_LOCUS_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::home_dir()
                .expect("home directory")
                .join(".daat-locus")
        });
    let profile_base = if profile_base.is_absolute() {
        profile_base
    } else {
        std::env::current_dir()
            .expect("current directory")
            .join(profile_base)
    };
    let namespace = profile_base.clone();
    let selected_root = arguments.profile.as_ref().map_or_else(
        || namespace.clone(),
        |name| namespace.join("profiles").join(name),
    );
    if matches!(command, cli::Command::Serve | cli::Command::Account { .. })
        && let Some(name) = arguments.profile.as_deref()
    {
        let base = profile_base.clone();
        // A storage name confers no account authority. Every profile has separate
        // account credentials, membership, keys, configuration and durable Actor state.
        unsafe {
            std::env::set_var("DAAT_LOCUS_HOME", base.join("profiles").join(name));
            std::env::set_var("DAAT_LOCUS_PROFILE_NAME", name);
        }
    }
    if matches!(command, cli::Command::Serve) {
        daemon::daemonize_current_process_if_requested();
    }
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .into_diagnostic()
        .and_then(|runtime| {
            let result = runtime.block_on(dispatch(command, arguments, namespace, selected_root));
            if native_host_bridge {
                // Tokio stdin uses an uncancellable blocking read. The native
                // client keeps that pipe open until this helper closes stdout;
                // waiting for the read during runtime shutdown deadlocks their
                // reconnect handshake after a service disconnect or timeout.
                // This helper owns only transport copies, never App execution.
                runtime.shutdown_background();
            }
            result
        });
    if let Err(error) = result {
        eprintln!("{error:?}");
        std::process::exit(1);
    }
}

async fn dispatch(
    command: cli::Command,
    arguments: cli::ServiceArgs,
    namespace: PathBuf,
    root: PathBuf,
) -> Result<()> {
    use daemon::control::{self, Action, LaunchOptions};
    let opening = |status: &control::Status| daemon::open_client(status);
    match command {
        cli::Command::WebHost => control::web_host(&root).await,
        #[cfg(windows)]
        cli::Command::Installer { action } => match action {
            cli::InstallerAction::Prepare {
                installation,
                transaction,
            } => daemon::installation::prepare(&namespace, &installation, transaction).await,
            cli::InstallerAction::Resume {
                installation,
                transaction,
            } => daemon::installation::resume(&namespace, &installation, transaction).await,
        },
        #[cfg(windows)]
        cli::Command::ComputerUseLaunch { .. } => unreachable!("handled before service startup"),
        cli::Command::ClientBootstrap => {
            use std::io::Read;
            let mut bytes = zeroize::Zeroizing::new(Vec::new());
            std::io::stdin()
                .take(64 * 1024 + 1)
                .read_to_end(&mut bytes)
                .into_diagnostic()?;
            if bytes.len() > 64 * 1024 {
                return Err(miette!("Bootstrap message too large"));
            }
            let input = serde_json::from_slice(&bytes).into_diagnostic()?;
            let reply = control::bootstrap(&root, input).await?;
            println!("{}", serde_json::to_string(&reply).into_diagnostic()?);
            Ok(())
        }
        cli::Command::Serve => serve(arguments, namespace, None).await,
        cli::Command::Account { action } => serve(arguments, namespace, Some(action)).await,
        cli::Command::Status { json } => {
            let status = control::query(&root, Action::Status).await?;
            if json {
                println!("{}", serde_json::to_string(&status).into_diagnostic()?);
            } else if let Some(status) = status {
                println!("{:?} · PID {} · {}", status.state, status.pid, status.url);
            } else {
                println!("Stopped");
            }
            Ok(())
        }
        cli::Command::Stop => {
            control::stop(&root).await?;
            println!("Stopped");
            Ok(())
        }
        cli::Command::Start { open } | cli::Command::Restart { open } => {
            #[cfg(windows)]
            {
                let _guard = daemon::installation::startup_guard(&namespace).await?;
                daemon::installation::check_start(&namespace)?;
            }
            let previous = control::query(&root, Action::Status).await?;
            if matches!(command, cli::Command::Start { .. })
                && let Some(status) = &previous
                && status.matches_current_executable()?
                && status.state == control::State::Running
            {
                println!("Already running · {}", status.url);
                if open {
                    opening(status)?;
                }
                return Ok(());
            }
            if previous.is_some() {
                control::stop(&root).await?;
            }
            let paths = daat_locus::daat_locus_paths::DaatLocusPaths::from_root(root.clone());
            let fallback_port = daat_locus::config::DaemonConfig::default().port;
            let port = std::fs::read_to_string(paths.config_file("config.toml"))
                .ok()
                .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
                .and_then(|data| data.get("daemon")?.get("port")?.as_integer())
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port > 0)
                .unwrap_or(fallback_port);
            let listen = arguments
                .listen
                .or_else(|| previous.as_ref().map(|status| status.options.listen))
                .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], port)));
            let workspace = arguments
                .workspace
                .or_else(|| {
                    previous
                        .as_ref()
                        .map(|status| status.options.workspace.clone())
                })
                .unwrap_or_else(|| {
                    std::env::home_dir()
                        .expect("home directory")
                        .join("daat-locus-workspace")
                });
            let options = LaunchOptions {
                profile: arguments.profile,
                listen,
                workspace,
            };
            daemon::spawn_detached_daemon_process(&namespace, &root, &options)?;
            let status = control::wait_ready(&root).await?;
            println!("Running · PID {} · {}", status.pid, status.url);
            if open {
                opening(&status)?;
            }
            Ok(())
        }
    }
}

async fn serve(
    arguments: cli::ServiceArgs,
    namespace: PathBuf,
    account_action: Option<cli::AccountCommand>,
) -> Result<()> {
    #[cfg(windows)]
    let startup = {
        let guard = daemon::installation::startup_guard(&namespace).await?;
        daemon::installation::check_start(&namespace)?;
        guard
    };
    let paths = daat_locus_paths().await;
    for path in [
        paths.root().to_path_buf(),
        paths.config_dir(),
        paths.runtime_dir(),
        paths.codex_auth_dir(),
        paths.opencode_auth_dir(),
    ] {
        std::fs::create_dir_all(&path).into_diagnostic()?;
        daat_locus::actors::permissions::protect_private_path(&path)?;
    }
    let boot = config_setup::read_service_boot_config().await;
    let address = arguments
        .listen
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], boot.port)));
    let workspace = match arguments.workspace {
        Some(workspace) => workspace,
        None => std::env::home_dir()
            .ok_or_else(|| miette!("home directory unavailable"))?
            .join("daat-locus-workspace"),
    };
    let _logging = daat_locus::logging::init_logging(&paths).await;
    let mut lock = DaemonLock::acquire().await?;
    let readiness = config_setup::ensure_config_readiness().await;
    let config = if readiness.is_complete() {
        config_setup::load_complete_config().await?
    } else {
        daat_locus::config::Config::default()
    };
    let system =
        ActorSystem::open_account_gated(paths.root().join("actors"), workspace, config).await?;
    if let Some(action) = account_action {
        let result = maintain_account(&system, action).await;
        system.shutdown().await;
        lock.release();
        return result;
    }
    #[cfg(windows)]
    if let Err(error) = system.prepare_computer_workspace().await {
        tracing::warn!(reason=%error, "ComputerUse virtual display could not be prepared during service startup");
    }
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .into_diagnostic()?;
    let actual = listener.local_addr().into_diagnostic()?;
    let state = ServiceState::new(system.clone(), format!("http://{actual}"))?
        .with_listen_address(actual)
        .with_origins(vec![
            format!("http://{actual}"),
            format!("http://localhost:{}", actual.port()),
        ]);
    let stage = state.stage.clone();
    if let Some(stage) = &stage {
        if let Some(record) = stage.host.accounts.current()? {
            if stage.host.account_id().as_deref() != Some(&record.account_id) {
                return Err(miette!("Account credentials do not belong to this profile"));
            }
            if let Some(definitions) = stage.host.configurations.current()? {
                // Verified account definitions override the local exported model
                // registry; device listener/locale and OAuth file paths stay local.
                let candidate = daat_locus::accounts::configuration::ConfigurationStore::candidate(
                    &stage.host,
                    &definitions,
                )?;
                system.update_config(candidate).await?;
            }
            system.activate_account_workers().await?;
        }
        if let Err(error) = stage.start().await {
            // Execution stays stopped, but account login and renewal must remain
            // available when a device certificate has expired.
            tracing::warn!(reason=%error,"Device execution is unavailable; account service remains available");
        }
    }
    let url_address = if actual.ip().is_unspecified() {
        SocketAddr::new(
            if actual.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            },
            actual.port(),
        )
    } else {
        actual
    };
    let control = daemon::control::Server::bind(
        namespace,
        paths.root().to_path_buf(),
        daemon::control::LaunchOptions {
            profile: arguments.profile,
            listen: actual,
            workspace: system.device_workspace().to_owned(),
        },
        format!("http://{url_address}"),
        Some(state.clone()),
    )
    .await?;
    #[cfg(windows)]
    drop(startup);
    let mut stop_requested = control.stopping();
    println!(
        "Daat Locus API listening on http://{actual}; sign in or create an account in the WebUI"
    );
    let stopped = system.clone();
    let stopped_stage = stage.clone();
    let result = axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        tokio::select! {
            _ = termination_signal() => {},
            _ = async { while !*stop_requested.borrow() { if stop_requested.changed().await.is_err() { break; } } } => {},
        }
        if let Some(stage) = stopped_stage {
            stage.stop().await;
        }
        stopped.shutdown().await;
    })
    .await
    .into_diagnostic();
    system.shutdown().await;
    if let Some(stage) = stage {
        stage.stop().await;
    }
    drop(control);
    lock.release();
    result
}

async fn maintain_account(system: &ActorSystem, action: cli::AccountCommand) -> Result<()> {
    let host =
        daat_locus::stage::host::Host::load(system.clone()).map_err(|error| miette!("{error}"))?;
    let _lifecycle = host.lifecycle.lock().await;
    if host.accounts.current()?.is_some() {
        return Err(miette!("This profile already has account credentials"));
    }
    match action {
        cli::AccountCommand::Create {
            name,
            password_stdin: _,
        } => {
            use std::io::Read;
            let name = daat_locus::accounts::normalize_name(&name)?;
            let mut password = zeroize::Zeroizing::new(Vec::new());
            if let Err(error) = std::io::stdin().take(1025).read_to_end(&mut password) {
                password.fill(0);
                return Err(error).into_diagnostic();
            }
            if password.last() == Some(&b'\n') {
                password.pop();
                if password.last() == Some(&b'\r') {
                    password.pop();
                }
            }
            let valid = std::str::from_utf8(&password)
                .is_ok_and(|value| value.chars().count() >= 12)
                && password.len() <= 1024;
            if !valid {
                password.fill(0);
                return Err(miette!(
                    "Use a password of at least 12 characters, at most 1024 UTF-8 bytes"
                ));
            }
            let client =
                daat_locus_auth::RegistrationClient::start(password.to_vec()).into_diagnostic();
            password.fill(0);
            let client = client?;
            host.initialize(true).map_err(|error| miette!("{error}"))?;
            if !host
                .identity()
                .map_err(|error| miette!("{error}"))?
                .has_group_authority()
            {
                return Err(miette!(
                    "A joined device must import the existing account credentials"
                ));
            }
            let group = host
                .account_id()
                .ok_or_else(|| miette!("Account identity unavailable"))?;
            let setup = daat_locus_auth::new_setup();
            let response = daat_locus_auth::registration_response(
                &setup,
                format!("DaatLocus.Account.v1/{group}/{name}").as_bytes(),
                &client.request(),
            )
            .into_diagnostic()?;
            let record =
                daat_locus_auth::password_file(&client.finish(&response).into_diagnostic()?)
                    .into_diagnostic()?;
            host.accounts.install(
                daat_locus::accounts::CredentialBundle::new(
                    group.clone(),
                    name.clone(),
                    &setup,
                    &record,
                    None,
                )?,
                &group,
            )?;
            println!("Account {name} created. Public account identity: {group}");
        }
    }
    Ok(())
}

async fn termination_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        } else {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
    #[cfg(windows)]
    {
        if let Ok(mut close) = tokio::signal::windows::ctrl_close() {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = close.recv() => {} }
        } else {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}
