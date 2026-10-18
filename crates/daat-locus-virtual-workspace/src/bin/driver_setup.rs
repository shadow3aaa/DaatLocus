//! Small elevated installer prerequisite. No service, account storage or GUI input.
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "daat-locus-display-setup", version)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Trust the bundled display certificate and install/repair the driver.
    /// Requires administrator privileges; only the embedded public identity is imported.
    Install {
        #[arg(long)]
        package: Option<PathBuf>,
    },
    /// Remove all shared DL display adapters/packages and installer-owned trust.
    /// Close ComputerUse workspaces first. Requires administrator privileges.
    Uninstall,
    /// Verify the driver CAT's pinned signing identity without changing machine trust.
    Verify {
        #[arg(long)]
        package: Option<PathBuf>,
    },
}
fn main() {
    let cli = Cli::parse();
    #[cfg(windows)]
    let result = (|| -> Result<bool, Box<dyn std::error::Error>> {
        let (package, install) = match cli.command {
            Action::Install { package } => (package, true),
            Action::Verify { package } => (package, false),
            Action::Uninstall => {
                return Ok(daat_locus_virtual_workspace::uninstall_bundled_driver()?);
            }
        };
        let package = package.unwrap_or(
            std::env::current_exe()?
                .parent()
                .ok_or("installer directory missing")?
                .to_path_buf(),
        );
        if install {
            Ok(daat_locus_virtual_workspace::install_bundled_driver(
                &package,
            )?)
        } else {
            daat_locus_virtual_workspace::verify_bundled_driver_identity(&package)?;
            Ok(false)
        }
    })();
    #[cfg(not(windows))]
    let result: Result<bool, Box<dyn std::error::Error>> = {
        let _ = cli;
        Err("display driver installation requires Windows x64".into())
    };
    match result {
        Ok(reboot) => {
            println!("driver_operation_completed=true reboot_required={reboot}");
            if reboot {
                std::process::exit(3010);
            }
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
