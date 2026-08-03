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
/// `execute_nu_setup_with_installer` (src/nu/bootstrap.rs) already follows
/// this pattern; this helper makes the same rule uniform across the setup
/// command surface.
pub fn require_tty_or_yes(yes: bool, what: &str) -> Result<()> {
    require_tty_or_yes_with_seam(yes, what, std::io::stdin().is_terminal())
}

pub fn require_tty_or_yes_with_seam(yes: bool, what: &str, is_tty: bool) -> Result<()> {
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

    // ── require_tty_or_yes (all three branches via the seam) ─────────────
    // PR #69 WDr: every branch is now reachable from a unit test.

    #[test]
    fn require_tty_or_yes_accepts_explicit_yes_regardless_of_tty() {
        // Yes-skip is the polarity that doesn't depend on stdin. Lock it
        // across both TTY and non-TTY so a future regression that gates
        // --yes on TTY would fail here.
        assert!(require_tty_or_yes_with_seam(true, "test op", true).is_ok());
        assert!(require_tty_or_yes_with_seam(true, "test op", false).is_ok());
    }

    #[test]
    fn require_tty_or_yes_passes_on_tty_without_yes() {
        // The interactive case: TTY is true, the destructive step is
        // allowed to proceed (the caller is responsible for an
        // interactive prompt downstream, e.g. via confirm_or_bail).
        assert!(require_tty_or_yes_with_seam(false, "test op", true).is_ok());
    }

    #[test]
    fn require_tty_or_yes_bails_on_non_tty_without_yes() {
        // The CI / pipe case: non-TTY + no --yes must refuse the
        // destructive step. The audit eprintln text is the contract
        // safe-batch automation greps, so it's tested shape-equal.
        let err = require_tty_or_yes_with_seam(false, "off-path registration", false)
            .expect_err("must bail on non-TTY + no --yes");
        let msg = err.to_string();
        assert!(
            msg.contains("Refusing destructive off-path registration"),
            "bail message must include the `what` label: {msg}"
        );
    }

    #[test]
    fn confirm_or_auto_passes_on_non_tty() {
        assert!(confirm_or_auto_with_tty("Proceed?", false, false).unwrap());
    }

    #[test]
    fn confirm_or_auto_uses_yes_flag() {
        assert!(confirm_or_auto_with_tty("Proceed?", true, false).unwrap());
        assert!(confirm_or_auto_with_tty("Proceed?", true, true).unwrap());
    }
}
