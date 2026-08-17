use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::cmd;

#[derive(Parser)]
#[command(
    name = "numan",
    about = "A cross-platform package manager for Nushell",
    version,
    after_help = "Run 'numan <command> --help' for more information on a command."
)]
pub struct Cli {
    /// Path to numan root directory
    #[arg(long, global = true)]
    pub root: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Search registry by name/description/tags
    Search(cmd::search::SearchArgs),
    /// Show package details, versions, platforms
    Info {
        /// Package ID (owner/name)
        id: String,
    },
    /// Install a package
    Install(cmd::install::InstallArgs),
    /// Update installed packages to their latest compatible versions
    Update(cmd::update::UpdateArgs),
    /// Remove an installed package
    Remove(cmd::remove::RemoveArgs),
    /// Garbage-collect orphaned package directories
    Gc(cmd::gc::GcArgs),
    /// Activate installed plugins and modules with Nu
    Activate(cmd::activate::ActivateArgs),
    /// Deactivate active plugins and modules
    Deactivate(cmd::deactivate::DeactivateArgs),
    /// List all installed packages
    List,
    /// Initialize Numan and probe the local Nu installation
    Init(cmd::init::InitArgs),
    /// Registry management
    #[command(subcommand)]
    Registry(cmd::registry::RegistryCommands),
    /// Immutable activation snapshots and rollback
    #[command(subcommand)]
    Snapshot(cmd::snapshot::SnapshotCommands),
    /// Read-only nupm discovery and inspection
    Nupm(cmd::nupm::NupmArgs),
    /// Install shell completions (use `--print` to emit the script)
    Completions(cmd::completions::CompletionsArgs),
    /// Diagnose Numan root health and apply safe repairs (use `--scan` for report-only)
    Doctor(cmd::doctor::DoctorArgs),
    /// Manage Nushell installations and external shell CLI tool integrations
    #[command(subcommand)]
    Setup(cmd::setup::SetupCommands),
    /// Try a package against your current Nu and platform, with compatibility guidance
    Try(cmd::try_cmd::TryArgs),
    /// Switch the active managed Nu version (`latest`, `list`, or a specific version)
    Use(cmd::use_cmd::UseArgs),
}
