//! Shared interactive confirmation utility.
//!
//! Centralizes the TTY-check + prompt + read_line pattern that was previously
//! copy-pasted across ~16 sites. The policy is:
//! - `--yes` → skip prompting, return true
//! - non-TTY (piped / CI) → auto-confirm (print a note to stderr)
//! - TTY → prompt `[y/N]` and return the user's answer

use anyhow::Result;
use std::io::{IsTerminal, Write};

/// Prompt for confirmation on TTY, auto-confirm on non-TTY.
///
/// Returns `true` when the action should proceed, `false` when the user
/// declined. Never bails on non-TTY — scripts and CI get idempotent success.
pub fn confirm_or_auto(prompt: &str, yes: bool) -> Result<bool> {
    confirm_or_auto_with_tty(prompt, yes, std::io::stdin().is_terminal())
}

/// Same as [`confirm_or_auto`] but lets the caller inject the TTY decision.
///
/// Tests use this variant so they can assert both the non-TTY auto-confirm
/// branch and the TTY interactive branch without depending on the host machine's
/// stdin state.
pub fn confirm_or_auto_with_tty(prompt: &str, yes: bool, is_tty: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }

    if !is_tty {
        eprintln!("(non-interactive: auto-confirming)");
        return Ok(true);
    }

    print!("{prompt} [y/N] ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim().eq_ignore_ascii_case("y"))
}

/// Prompt for confirmation, bailing with `cancel_msg` when the user declines.
///
/// Convenience wrapper for the common "confirm or abort" pattern.
pub fn confirm_or_bail(prompt: &str, yes: bool, cancel_msg: &str) -> Result<()> {
    if confirm_or_auto(prompt, yes)? {
        Ok(())
    } else {
        anyhow::bail!("{cancel_msg}");
    }
}

/// Hard-fail guard for destructive `setup`-family ops in non-interactive sessions.
///
/// `confirm_or_bail` (and therefore `confirm_or_auto`) auto-confirm on non-TTY,
/// which is the right default for idempotent setup steps but silently destructive
/// for ones that delete or rewrite user-visible state (managed-Nu tree, PATH,
/// loader script). To keep `numan setup nu use …` / `setup nu remove` safe in
/// unattended CI without `--yes`, callers wrap this around the destructive step.
///
/// `what` is a short human-readable label used in both the audit log and the
/// bail message so safe-batch automation can grep it out of stderr.
///
/// Audit log conventions:
/// - `--yes` set → `(audit) explicit --yes accepted for {what}; proceeding without interactive prompt.`
/// - non-TTY + no `--yes` → `(audit) implicit non-TTY session; refusing destructive {what} without --yes.` followed by bail!
///
/// Callers should `execute_nu_setup_with_installer` already follow this
/// pattern (src/nu/bootstrap.rs); this helper makes the same rule uniform
/// across the setup command surface.
pub fn require_tty_or_yes(yes: bool, what: &str) -> Result<()> {
    require_tty_or_yes_with_tty(yes, what, std::io::stdin().is_terminal())
}

/// Same as [`require_tty_or_yes`] but lets the caller inject the TTY decision.
pub fn require_tty_or_yes_with_tty(yes: bool, what: &str, is_tty: bool) -> Result<()> {
    if yes {
        eprintln!(
            "(audit) explicit --yes accepted for {what}; proceeding without interactive prompt."
        );
        return Ok(());
    }
    if !is_tty {
        eprintln!("(audit) implicit non-TTY session; refusing destructive {what} without --yes.");
        anyhow::bail!("Refusing destructive {what} in non-interactive session without --yes.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yes_flag_skips_prompt() {
        assert!(confirm_or_auto("Proceed?", true).unwrap());
    }

    #[test]
    fn confirm_or_bail_passes_on_yes() {
        assert!(confirm_or_bail("Proceed?", true, "Cancelled.").is_ok());
    }

    #[test]
    fn require_tty_or_yes_accepts_explicit_yes() {
        // Locks the `yes = true` polarity: returns Ok(()) regardless of TTY.
        assert!(require_tty_or_yes_with_tty(true, "test op", false).is_ok());
        assert!(require_tty_or_yes_with_tty(true, "test op", true).is_ok());
    }

    #[test]
    fn require_tty_or_yes_bails_on_non_tty_without_yes() {
        let err = require_tty_or_yes_with_tty(false, "off-path registration", false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("non-interactive"), "got: {err}");
    }

    #[test]
    fn confirm_or_auto_passes_on_non_tty() {
        // Non-TTY without --yes must auto-confirm (idempotent in CI/scripts).
        assert!(confirm_or_auto_with_tty("Proceed?", false, false).unwrap());
    }

    #[test]
    fn confirm_or_auto_uses_yes_flag() {
        assert!(confirm_or_auto_with_tty("Proceed?", true, false).unwrap());
        assert!(confirm_or_auto_with_tty("Proceed?", true, true).unwrap());
    }
}
