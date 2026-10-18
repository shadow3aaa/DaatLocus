use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "Inspect IDD displays or run a disposable WM_POINTER workspace experiment",
    version
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read-only display discovery; does not install a driver or change topology.
    Probe,
    /// Install our signed driver package (OS administrator privileges required).
    InstallDriver {
        #[arg(long)]
        package: PathBuf,
    },
    /// Create an owned screen and remove it after a bounded hold.
    Screen {
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=60), default_value_t = 10)]
        seconds: u64,
    },
    /// Test native touch on two disposable physical-screen windows without an IDD driver.
    PointerLab {
        #[arg(long)]
        default_activation: bool,
        #[arg(long)]
        self_test: bool,
    },
    /// Create two windows on one GUI thread. Only these disposable windows receive input.
    Lab {
        /// GDI display name printed by probe, e.g. \\.\DISPLAY2.
        #[arg(long)]
        display: Option<String>,
        /// Let the fixture use default pointer activation, to measure interference.
        #[arg(long)]
        default_activation: bool,
        /// Run a bounded verification and fail on input/focus/capture discrepancies.
        #[arg(long)]
        self_test: bool,
        /// Wait for a user click before assertions when Windows denies foreground activation.
        #[arg(long, requires = "self_test")]
        click_to_start: bool,
        /// Measure WS_EX_NOACTIVATE on the disposable receiver with default activation handling.
        #[arg(long, requires = "default_activation")]
        noactivate_window: bool,
    },
}

#[cfg(windows)]
mod support;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    #[cfg(windows)]
    match args.command {
        Command::Probe => {
            for display in daat_locus_virtual_workspace::displays()? {
                println!("{display:#?}");
            }
        }
        Command::InstallDriver { package } => {
            let reboot = daat_locus_virtual_workspace::DriverPackage::open(package)?.install()?;
            println!("Driver installed; reboot_required={reboot}");
        }
        Command::Screen {
            width,
            height,
            seconds,
        } => {
            let screen = daat_locus_virtual_workspace::VirtualDisplay::create(
                daat_locus_virtual_workspace::DisplayOptions {
                    width,
                    height,
                    ..Default::default()
                },
            )?;
            println!("Created {}: {:#?}", screen.instance_id(), screen.display()?);
            std::thread::sleep(std::time::Duration::from_secs(seconds));
            screen.remove()?;
            println!("Owned virtual screen removed");
        }
        Command::PointerLab {
            default_activation,
            self_test,
        } => {
            let display = daat_locus_virtual_workspace::displays()?
                .into_iter()
                .find(|d| !d.indirect && !d.cloned)
                .ok_or("no active physical display")?;
            support::run(&display.device_name, default_activation, self_test, true)?;
        }
        Command::Lab {
            display,
            default_activation,
            self_test,
            click_to_start,
            noactivate_window,
        } => {
            let owned = if display.is_none() {
                Some(daat_locus_virtual_workspace::VirtualDisplay::create(
                    Default::default(),
                )?)
            } else {
                None
            };
            let name = match display {
                Some(name) => name,
                None => owned.as_ref().unwrap().display()?.device_name,
            };
            support::run_with_options(
                &name,
                support::LabOptions {
                    default_activation,
                    automated: self_test,
                    click_to_start,
                    noactivate_window,
                    ..Default::default()
                },
            )?;
            if let Some(owned) = owned {
                owned.remove()?;
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        Err(daat_locus_virtual_workspace::Error::UnsupportedPlatform.into())
    }
    #[cfg(windows)]
    Ok(())
}
