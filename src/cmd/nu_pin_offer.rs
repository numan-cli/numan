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
    offer_managed_nu_pin_with_interaction(
        root,
        current_nu,
        diagnosis,
        std::io::stdin().is_terminal(),
        || {
            let mut input = String::new();
            std::io::stdin()
                .read_line(&mut input)
                .context("Failed to read Nu pin confirmation from stdin")?;
            Ok(input)
        },
    )
}

/// Testable offer path with explicit terminal/interaction state.
///
/// When `interactive` is false, prints setup hints and returns `Ok(false)` without
/// installing managed Nu (never auto-downloads).
pub fn offer_managed_nu_pin_with_interaction<F>(
    root: &Path,
    current_nu: &str,
    diagnosis: &PackageIncompatibility,
    interactive: bool,
    read_line: F,
) -> Result<bool>
where
    F: FnOnce() -> Result<String>,
{
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

    if !interactive {
        println!("To switch Nu, run:");
        println!("  {setup_cmd} --yes --force");
        println!("  {CMD_INIT_REFRESH}");
        println!("  then retry your install.");
        return Ok(false);
    }

    print!("Install managed Nu {pin} via `{setup_cmd}`? [y/N] ");
    std::io::stdout().flush()?;
    let input = read_line().context("Failed to read Nu pin confirmation input")?;
    if !input.trim().eq_ignore_ascii_case("y") {
        println!("Skipped Nu switch.");
        return Ok(false);
    }

    install_pinned_nu_and_refresh(root, pin)?;
    Ok(true)
}

pub fn install_pinned_nu_and_refresh(root: &Path, pin: &str) -> Result<()> {
    setup::execute_nu(
        &NuSetupArgs::install(Some(pin.to_string()), true, false, true, false),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnosis_with_pin(pin: &str) -> PackageIncompatibility {
        PackageIncompatibility {
            issue: Incompatibility::NuTooOld {
                constraint: ">=0.113.0".to_string(),
            },
            suggested_pin: Some(pin.to_string()),
            available_versions: vec![],
        }
    }

    #[test]
    fn accept_proceeds_to_install_and_fails_hermetically_on_bad_pin() {
        // A malformed pin fails local version normalization before any
        // network call, so this exercises the accept branch deterministically.
        let dir = tempfile::tempdir().unwrap();
        let diagnosis = diagnosis_with_pin("not-a-version");
        let err =
            offer_managed_nu_pin_with_interaction(dir.path(), "0.112.0", &diagnosis, true, || {
                Ok("y\n".to_string())
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("Failed to install managed Nu"),
            "expected install failure context, got: {err}"
        );
        let chain: String = err
            .chain()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(" / ");
        assert!(
            chain.contains("Failed to normalize requested version 'not-a-version'"),
            "expected version-normalization failure in the error chain, got: {chain}"
        );
    }

    #[test]
    fn decline_returns_false_without_installing() {
        let dir = tempfile::tempdir().unwrap();
        let diagnosis = diagnosis_with_pin("0.113.1");
        let result =
            offer_managed_nu_pin_with_interaction(dir.path(), "0.112.0", &diagnosis, true, || {
                Ok("n\n".to_string())
            })
            .unwrap();
        assert!(!result);
        assert!(
            std::fs::read_dir(dir.path()).unwrap().next().is_none(),
            "declining must not install anything under root"
        );
    }

    #[test]
    fn invalid_input_is_treated_as_decline() {
        let dir = tempfile::tempdir().unwrap();
        let diagnosis = diagnosis_with_pin("0.113.1");
        let result =
            offer_managed_nu_pin_with_interaction(dir.path(), "0.112.0", &diagnosis, true, || {
                Ok("maybe\n".to_string())
            })
            .unwrap();
        assert!(!result);
        assert!(
            std::fs::read_dir(dir.path()).unwrap().next().is_none(),
            "invalid input must not install anything under root"
        );
    }

    #[test]
    fn non_interactive_short_circuits_without_reading_input() {
        let dir = tempfile::tempdir().unwrap();
        let diagnosis = diagnosis_with_pin("0.113.1");
        let result =
            offer_managed_nu_pin_with_interaction(dir.path(), "0.112.0", &diagnosis, false, || {
                panic!("read_line must not be called when non-interactive")
            })
            .unwrap();
        assert!(!result);
    }
}
