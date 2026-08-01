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
    if yes {
        return Ok(true);
    }

    if !std::io::stdin().is_terminal() {
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
}
