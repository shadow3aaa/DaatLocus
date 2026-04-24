use crate::{
    commands::reset::{run_complite_reset, run_memory_reset, run_reset_all, run_state_reset},
    config::load_config,
    daemon::{
        DaemonClient, connect_existing_daemon, connect_or_start_daemon,
        spawn_detached_daemon_process, status_summary, wait_for_daemon_ready,
        wait_for_daemon_shutdown,
    },
    dashboard::run_tui_dashboard,
    logging::init_logging,
};
use crate::{config, config_wizard};
use clap::{Parser, Subcommand};
use miette::{Result, miette};

pub(crate) fn parse_args() -> Cli {
    Cli::parse()
}

#[derive(Debug, Parser)]
#[command(name = "daat-locus", about = "Daat Locus Agent")]
pub(crate) struct Cli {
    #[command(subcommand)]
    command: Option<DaatLocusCommand>,
}

#[derive(Debug, Subcommand)]
enum DaatLocusCommand {
    /// 启动前台运行时（等同于 daemon serve + attach）
    Run,
    /// 连接到已运行的 daemon，进入交互会话
    Attach,
    /// 管理后台 daemon 进程
    Daemon {
        #[command(subcommand)]
        target: DaemonTarget,
    },
    /// 清除本地状态或缓存数据
    Reset {
        #[command(subcommand)]
        target: ResetTarget,
    },
    /// 交互式配置管理（无子命令时进入菜单）
    Config {
        #[command(subcommand)]
        target: Option<ConfigTarget>,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigTarget {
    /// 显示当前配置摘要（secrets 已遮蔽）
    Show,
    /// 交互式添加一个 provider
    #[command(name = "add-provider")]
    AddProvider,
    /// 交互式添加一个 model
    #[command(name = "add-model")]
    AddModel,
    /// 更改主模型
    #[command(name = "set-main-model")]
    SetMainModel,
    /// 更改 hindsight 使用的模型
    #[command(name = "set-hindsight-model")]
    SetHindsightModel,
}

#[derive(Debug, Subcommand)]
enum ResetTarget {
    /// 清除编译缓存（compiled prompts）
    #[command(name = "complite", alias = "compile")]
    Complite,
    /// 清除运行时状态（daemon lock、socket 等）
    State,
    /// 清除对话历史、hindsight 记录及推理 traces
    Memory,
    /// 清除全部（state + memory + complite）
    All,
}

#[derive(Debug, Subcommand)]
enum DaemonTarget {
    /// 查看 daemon 运行状态
    Status,
    /// 停止后台 daemon
    Stop,
    /// 重启后台 daemon
    Restart,
    /// 在前台启动 daemon（内部使用）
    Serve,
}

pub(crate) async fn async_main(cli: Cli) -> Result<()> {
    let _log_guard = init_logging().await;

    match cli.command.as_ref() {
        Some(DaatLocusCommand::Reset {
            target: ResetTarget::Complite,
        }) => {
            run_complite_reset().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Reset {
            target: ResetTarget::State,
        }) => {
            run_state_reset().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Reset {
            target: ResetTarget::Memory,
        }) => {
            run_memory_reset().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Reset {
            target: ResetTarget::All,
        }) => {
            run_reset_all().await?;
            return Ok(());
        }
        // Config 子命令：可能在无 config 时运行（add-provider/add-model 除外）
        Some(DaatLocusCommand::Config { target }) => {
            return run_config_command(target.as_ref()).await;
        }
        Some(DaatLocusCommand::Daemon {
            target: DaemonTarget::Status,
        }) => {
            run_daemon_status_command().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Daemon {
            target: DaemonTarget::Stop,
        }) => {
            run_daemon_stop_command().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Daemon {
            target: DaemonTarget::Restart,
        }) => {
            run_daemon_restart_command().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Attach) => {
            let client = connect_existing_daemon().await?;
            attach_to_daemon(client).await?;
            return Ok(());
        }
        _ => {}
    }

    if matches!(cli.command, None | Some(DaatLocusCommand::Run))
        && let Ok(client) = connect_existing_daemon().await
    {
        attach_to_daemon(client).await?;
        return Ok(());
    }

    // 首次运行：config.toml 不存在时触发交互式 setup
    let config = if !config::config_file_exists().await {
        match config_wizard::run_first_time_setup().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("初始化失败: {e:?}");
                std::process::exit(1);
            }
        }
    } else {
        match load_config().await {
            Ok(o) => o,
            Err(e) => {
                tracing::error!("failed to load config: {e}");
                eprintln!("配置加载失败: {e:?}");
                std::process::exit(1);
            }
        }
    };

    match cli.command.as_ref() {
        Some(DaatLocusCommand::Daemon {
            target: DaemonTarget::Serve,
        }) => {
            crate::runtime::daemon_server::run_daemon_serve(config).await?;
            return Ok(());
        }
        _ => {}
    }

    if matches!(cli.command, None | Some(DaatLocusCommand::Run)) {
        let client = connect_or_start_daemon().await?;
        attach_to_daemon(client).await?;
        return Ok(());
    }
    Ok(())
}

/// Tail `~/.hindsight/profiles/<profile>.log` and forward new lines to stdout/tracing.
/// Designed to run concurrently with `HindsightManagedServer::start()` so the user

async fn run_config_command(target: Option<&ConfigTarget>) -> Result<()> {
    match target {
        None => config_wizard::run_config_menu().await,
        Some(ConfigTarget::Show) => config_wizard::show_config().await,
        Some(ConfigTarget::AddProvider) => config_wizard::run_add_provider().await,
        Some(ConfigTarget::AddModel) => config_wizard::run_add_model().await,
        Some(ConfigTarget::SetMainModel) => config_wizard::run_set_main_model().await,
        Some(ConfigTarget::SetHindsightModel) => config_wizard::run_set_hindsight_model().await,
    }
}

async fn run_daemon_status_command() -> Result<()> {
    let client = connect_existing_daemon().await?;
    let status = client.status().await?;
    println!("{}", status_summary(&status));
    Ok(())
}

async fn run_daemon_stop_command() -> Result<()> {
    let client = connect_existing_daemon().await?;
    let port = client.port();
    client.shutdown().await?;
    wait_for_daemon_shutdown(port).await?;
    println!("daemon stopped");
    Ok(())
}

async fn run_daemon_restart_command() -> Result<()> {
    if let Ok(client) = connect_existing_daemon().await {
        let port = client.port();
        client.shutdown().await?;
        wait_for_daemon_shutdown(port).await?;
    }
    spawn_detached_daemon_process().await?;
    let status = wait_for_daemon_ready().await?;
    println!("daemon restarted: {}", status_summary(&status));
    Ok(())
}

async fn attach_to_daemon(client: DaemonClient) -> Result<()> {
    let initial = client.snapshot().await?;
    let (tx, mut rx) = tokio::sync::watch::channel(initial);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let stream_client = DaemonClient::new(client.port());
    let stream_task = tokio::spawn(async move {
        let _ = stream_client.stream_to(tx, stop_rx).await;
    });
    run_tui_dashboard(&mut rx, &client)
        .await
        .map_err(|err| miette!("dashboard attach failed: {err}"))?;
    let _ = stop_tx.send(());
    let _ = stream_task.await;
    Ok(())
}
