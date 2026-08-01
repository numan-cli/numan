//! `numan use` — switch the active managed Nu version (reserved, post-1.0).
//!
//! This command is reserved for side-by-side Nu version management.
//! See `docs/plans/consolidated-multi-repo-roadmap.md` § Post-1.0.

use anyhow::{bail, Result};
use clap::Args;
use std::path::Path;

#[derive(Args, Debug)]
pub struct UseArgs {
    /// Nu version to switch to (e.g. 0.113.1), or "latest"
    #[arg(required = true)]
    pub version: String,
}

pub fn execute(args: &UseArgs, _root: &Path) -> Result<()> {
    bail!(
        "'numan use {}' is reserved for side-by-side Nu version management (planned for 0.2).\n\
         For now, use 'numan setup nu {}' to install a specific Nu version.",
        args.version,
        args.version,
    )
}
