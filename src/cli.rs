//! CLI 定义和命令分发。

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "daat-locus")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<DaatLocusCommand>,
}

#[derive(Debug, Subcommand)]
pub enum DaatLocusCommand {
    Reset {
        #[command(subcommand)]
        target: ResetTarget,
    },
    Setup {
        #[command(subcommand)]
        target: SetupTarget,
    },
    Sleep,
    Hindsight {
        #[command(subcommand)]
        target: HindsightTarget,
    },
    Inspect {
        #[command(subcommand)]
        target: InspectTarget,
    },
}

#[derive(Debug, Subcommand)]
pub enum ResetTarget {
    #[command(name = "complite", alias = "compile")]
    Complite,
    State,
    Memory,
    All,
}

#[derive(Debug, Subcommand)]
pub enum SetupTarget {
    #[command(name = "browser-runtime")]
    BrowserRuntime,
}

#[derive(Debug, Subcommand)]
pub enum InspectTarget {
    #[command(name = "system-prompt")]
    SystemPrompt,
    Snapshot,
}

#[derive(Debug, Subcommand)]
pub enum HindsightTarget {
    Config,
    Directives,
    #[command(name = "mental-models")]
    MentalModels,
    #[command(name = "clear-observations")]
    ClearObservations,
    #[command(name = "refresh-mental-models")]
    RefreshMentalModels,
}
