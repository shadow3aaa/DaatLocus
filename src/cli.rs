use std::{net::SocketAddr, path::PathBuf};

use clap::{
    Args, Parser, Subcommand,
    builder::{
        PathBufValueParser, TypedValueParser,
        styling::{AnsiColor, Styles},
    },
};

// The original terminal help palette; clap controls terminal color detection.
const HELP_STYLES: Styles = Styles::styled()
    .header(AnsiColor::BrightGreen.on_default().bold())
    .usage(AnsiColor::BrightGreen.on_default().bold())
    .literal(AnsiColor::BrightCyan.on_default().bold())
    .placeholder(AnsiColor::BrightMagenta.on_default())
    .valid(AnsiColor::BrightGreen.on_default())
    .invalid(AnsiColor::BrightYellow.on_default());

const AFTER_HELP: &str = "\x1b[92;1mExamples\x1b[0m\n  \x1b[96mdaat-locus serve\x1b[0m\n  \x1b[96mdaat-locus serve --profile work --listen 127.0.0.1:45825\x1b[0m\n\n\x1b[38;5;120mWebUI\x1b[0m\n  Open the URL printed at startup and sign in with your account.";

#[derive(Debug, Parser)]
#[command(name = "daat-locus", version, propagate_version = true)]
#[command(about = "Local Actor/App service with a WebUI.")]
#[command(styles = HELP_STYLES, before_help = crate::terminal_logo::render_daat_locus_logo())]
#[command(after_help = AFTER_HELP)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    service: ServiceArgs,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Internal unelevated installer lifecycle; never exposed through HTTP or Apps.
    #[cfg(windows)]
    #[command(hide = true)]
    Installer {
        #[command(subcommand)]
        action: InstallerAction,
    },
    /// Internal credential-free application launcher.
    #[cfg(windows)]
    #[command(hide = true)]
    ComputerUseLaunch {
        app: String,
        #[arg(long)]
        display: String,
        #[arg(long, allow_hyphen_values = true)]
        left: i32,
        #[arg(long, allow_hyphen_values = true)]
        top: i32,
        #[arg(long, allow_hyphen_values = true)]
        right: i32,
        #[arg(long, allow_hyphen_values = true)]
        bottom: i32,
    },
    /// Internal desktop bootstrap protocol; reads a bounded OPAQUE message from stdin.
    #[command(hide = true)]
    ClientBootstrap,
    /// Start the Actor service and WebUI in the foreground (the default).
    Serve,
    /// Start the service in the background, gracefully upgrading an older running build.
    Start {
        #[arg(long)]
        open: bool,
    },
    /// Show this profile's daemon status without starting a service.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Stop this profile's daemon gracefully.
    Stop,
    /// Restart this profile's daemon, preserving its launch options.
    Restart {
        #[arg(long)]
        open: bool,
    },
    /// Create an account using the local profile's OS permissions.
    Account {
        #[command(subcommand)]
        action: AccountCommand,
    },
}
#[cfg(windows)]
#[derive(Debug, Subcommand)]
pub enum InstallerAction {
    Prepare {
        #[arg(long)]
        installation: PathBuf,
        #[arg(long)]
        transaction: uuid::Uuid,
    },
    Resume {
        #[arg(long)]
        installation: PathBuf,
        #[arg(long)]
        transaction: uuid::Uuid,
    },
}
#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// Create the profile's account; read the password from stdin, never argv.
    Create {
        name: String,
        #[arg(long, required = true)]
        password_stdin: bool,
    },
}

#[derive(Debug, Args, Default)]
pub struct ServiceArgs {
    /// Use an isolated local account profile.
    #[arg(long, global = true, value_name = "NAME", value_parser = parse_profile)]
    pub profile: Option<String>,
    /// Bind to this address; defaults to loopback and the configured port.
    #[arg(long, global = true, value_name = "ADDRESS:PORT")]
    pub listen: Option<SocketAddr>,
    /// Root Actor workspace; defaults to ~/daat-locus-workspace.
    #[arg(long, global = true, value_name = "ABSOLUTE_PATH", value_parser = PathBufValueParser::new().try_map(absolute_workspace))]
    pub workspace: Option<PathBuf>,
}

pub fn parse_args() -> (Command, ServiceArgs) {
    let cli = Cli::parse();
    (cli.command.unwrap_or(Command::Serve), cli.service)
}

fn parse_profile(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err("profile must contain 1–64 ASCII letters, digits, '-' or '_'".into());
    }
    Ok(value.to_owned())
}

fn absolute_workspace(path: PathBuf) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("workspace must be an absolute path".into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, error::ErrorKind};

    #[test]
    fn account_maintenance_requires_stdin_password_and_preserves_profile_selection() {
        for args in [
            vec![
                "daat-locus",
                "--profile",
                "work_1",
                "account",
                "create",
                "alice",
                "--password-stdin",
            ],
            vec![
                "daat-locus",
                "account",
                "create",
                "alice",
                "--password-stdin",
                "--profile",
                "work_1",
            ],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert_eq!(cli.service.profile.as_deref(), Some("work_1"));
            assert!(matches!(
                cli.command,
                Some(Command::Account {
                    action: AccountCommand::Create {
                        password_stdin: true,
                        ..
                    }
                })
            ));
        }
        for args in [
            vec!["daat-locus", "account", "create", "alice"],
            vec![
                "daat-locus",
                "account",
                "create",
                "alice",
                "--password",
                "secret",
            ],
            vec!["daat-locus", "account", "join"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
        assert!(Cli::try_parse_from(["daat-locus", "account", "request"]).is_err());
    }

    #[test]
    fn current_options_work_before_and_after_serve_and_without_a_subcommand() {
        for args in [
            vec![
                "daat-locus",
                "--profile",
                "work_1",
                "serve",
                "--listen",
                "[::1]:45825",
            ],
            vec![
                "daat-locus",
                "serve",
                "--profile",
                "work_1",
                "--listen",
                "[::1]:45825",
            ],
            vec![
                "daat-locus",
                "--profile",
                "work_1",
                "--listen",
                "[::1]:45825",
            ],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert_eq!(cli.service.profile.as_deref(), Some("work_1"));
            assert_eq!(cli.service.listen, Some("[::1]:45825".parse().unwrap()));
        }
        let workspace = std::env::current_dir().unwrap().join("目录 with spaces");
        let cli = Cli::try_parse_from([
            std::ffi::OsString::from("daat-locus"),
            "serve".into(),
            "--workspace".into(),
            workspace.as_os_str().to_owned(),
        ])
        .unwrap();
        assert_eq!(cli.service.workspace, Some(workspace));
        assert!(
            Cli::try_parse_from(["daat-locus"])
                .unwrap()
                .command
                .is_none()
        );
    }

    #[test]
    fn invalid_input_is_rejected_before_service_startup() {
        for args in [
            vec!["daat-locus", "serve", "--listen"],
            vec!["daat-locus", "--profile", "../escape"],
            vec!["daat-locus", "--profile", ""],
            vec!["daat-locus", "--profile", "工作"],
            vec!["daat-locus", "--workspace", "relative/path"],
            vec!["daat-locus", "--listen", "invalid"],
            vec!["daat-locus", "--profile", "one", "--profile", "two"],
            vec!["daat-locus", "serve", "--unknown"],
            vec!["daat-locus", "workflow"],
        ] {
            assert!(
                Cli::try_parse_from(args.clone()).is_err(),
                "accepted {args:?}"
            );
        }
        assert!(parse_profile(&"a".repeat(65)).is_err());
    }
    #[test]
    fn daemon_commands_keep_profile_and_launch_arguments() {
        let start = Cli::try_parse_from([
            "daat-locus",
            "--profile",
            "work",
            "start",
            "--open",
            "--listen",
            "127.0.0.1:0",
        ])
        .unwrap();
        assert!(matches!(start.command, Some(Command::Start { open: true })));
        assert_eq!(start.service.profile.as_deref(), Some("work"));
        assert!(matches!(
            Cli::try_parse_from(["daat-locus", "status", "--json", "--profile", "work"])
                .unwrap()
                .command,
            Some(Command::Status { json: true })
        ));
        assert!(matches!(
            Cli::try_parse_from(["daat-locus", "stop"]).unwrap().command,
            Some(Command::Stop)
        ));
        assert!(matches!(
            Cli::try_parse_from(["daat-locus", "restart"])
                .unwrap()
                .command,
            Some(Command::Restart { open: false })
        ));
    }

    #[test]
    fn help_and_version_are_clap_early_exits() {
        Cli::command().debug_assert();
        for args in [
            vec!["daat-locus", "-h"],
            vec!["daat-locus", "--help"],
            vec!["daat-locus", "help"],
            vec!["daat-locus", "serve", "--help"],
        ] {
            assert_eq!(
                Cli::try_parse_from(args).unwrap_err().kind(),
                ErrorKind::DisplayHelp
            );
        }
        assert_eq!(
            Cli::try_parse_from(["daat-locus", "--version"])
                .unwrap_err()
                .kind(),
            ErrorKind::DisplayVersion
        );
    }
}
