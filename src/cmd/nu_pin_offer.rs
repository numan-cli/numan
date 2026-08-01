//! Interactive offer to install a pinned managed Nu when packages need a different Nu minor.

use anyhow::{Context, Result};
use std::io::{IsTerminal, Write};
use std::path::Path;

use crate::cmd::init::{self, InitArgs};
use crate::cmd::setup::{self, NuSetupArgs};
use crate::core::resolve::{Incompatibility, PackageIncompatibility};
use crate::util::hints::{self, CMD_INIT_REFRESH};

/// Print blast-radius warning and optionally install managed Nu + refresh paths.
///
/// Returns `Ok(true)` when a pin was installed and `init --refresh` succeeded.
/// Returns `Ok(false)` when the user declined or the session is non-interactive
/// (hints printed only; never auto-downloads Nu without explicit confirmation).
pub fn offer_managed_nu_pin(
    root: &Path,
    current_nu: &str,
    diagnosis: &PackageIncompatibility,
) -> Result<bool> {
    let Some(pin) = diagnosis.suggested_pin.as_deref() else {
        return Ok(false);
    };

    println!();
    println!("This package needs a different Nu than you are using ({current_nu}).");
    println!("Suggested managed Nu: {pin}");
    println!();
    println!("Switching Nu keeps installed packages on disk, but:");
    println!("  - Numan will refresh paths (`{CMD_INIT_REFRESH}`)");
    println!("  - activations are per-Nu; re-run `numan activate` for packages you still want");
    println!("  - packages built for your old Nu may not load on the new one");
    println!();

    let setup_cmd = hints::setup_nu_version(pin);

    if !std::io::stdin().is_terminal() {
        println!("To switch Nu, run:");
        println!("  {setup_cmd} --yes --force");
        println!("  {CMD_INIT_REFRESH}");
        println!("  then retry your install.");
        return Ok(false);
    }

    print!("Install managed Nu {pin} via `{setup_cmd}`? [y/N] ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    if !input.trim().eq_ignore_ascii_case("y") {
        println!("Skipped Nu switch.");
        return Ok(false);
    }

    install_pinned_nu_and_refresh(root, pin)?;
    Ok(true)
}

pub fn install_pinned_nu_and_refresh(root: &Path, pin: &str) -> Result<()> {
    setup::execute_nu(
        &NuSetupArgs::install(Some(pin.to_string()), true, false, true),
        root,
    )
    .with_context(|| format!("Failed to install managed Nu {pin}"))?;

    init::execute(&InitArgs { refresh: true }, root)
        .context("Failed to refresh Numan paths after Nu install")?;

    Ok(())
}

/// Returns true when the diagnosis is a Nu constraint mismatch (pin may help).
pub fn is_nu_mismatch(diagnosis: &PackageIncompatibility) -> bool {
    matches!(
        diagnosis.issue,
        Incompatibility::NuTooNew { .. }
            | Incompatibility::NuTooOld { .. }
            | Incompatibility::NuUnsatisfied { .. }
    )
}
