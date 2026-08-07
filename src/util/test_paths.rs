//! Helpers for restoring process-wide state after a test.
//!
//! Available to crate unit tests and `tests/` integration tests. Prefer wrapping
//! any PATH mutation with [`PathRestoreGuard`] so concurrent tests cannot race
//! on the process-global environment and so a developer shell is not left with
//! a poisoned PATH after the test binary exits.
//!
//! On Windows, [`PathRestoreGuard`] also snapshots and restores the **User**
//! PATH registry value (distinguishing absent `$null` from an explicit empty
//! string). Production `numan setup nu use` / doctor off-PATH repair call
//! [`crate::nu::bootstrap::persist_path_dir`], which writes the User PATH via
//! PowerShell; the test-only `NUMAN_TEST_NO_PERSIST_USER_PATH` flag blocks those
//! writes while the guard is held, and Drop restores the snapshotted User PATH
//! if anything still mutated it.

use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

/// Serializes every PATH snapshot/restore so concurrent tests cannot
/// race through the process-global environment.
static PATH_MUTEX: Mutex<()> = Mutex::new(());

/// RAII guard that snapshots the process PATH on construction and
/// restores it on drop. Acquires a shared process-wide mutex so callers
/// never need a separate `Mutex` for PATH serialization.
///
/// On Windows, also snapshots/restores the User PATH environment variable
/// stored in the registry (what `persist_path_dir` mutates).
///
/// Use this around any test that mutates PATH so real-Nu runs from a
/// developer terminal are not poisoned by the test process, and so
/// parallel ignored acceptance tests cannot overwrite each other's PATH.
///
/// # Example
///
/// Prefer a non-doctest fence (`text`): Real-Nu acceptance runs
/// `cargo test -- --ignored`, which also executes rustdoc examples marked
/// `ignore`.
///
/// ```text
/// let _path_guard = PathRestoreGuard::new();
/// // mutate PATH...
/// // drop restores original process PATH (and Windows User PATH)
/// ```
pub struct PathRestoreGuard {
    original: Option<OsString>,
    /// Pre-existing `NUMAN_TEST_NO_PERSIST_USER_PATH` value (or `None` if absent).
    previous_no_persist: Option<OsString>,
    #[cfg(windows)]
    original_user_path: Option<WindowsUserPathSnapshot>,
    _lock: MutexGuard<'static, ()>,
}

/// Windows User PATH registry snapshot distinguishing absent vs empty.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
enum WindowsUserPathSnapshot {
    /// `[Environment]::GetEnvironmentVariable('Path','User')` returned `$null`.
    Absent,
    /// Present registry value, including the empty string.
    Value(OsString),
}

impl PathRestoreGuard {
    pub fn new() -> Self {
        let lock = PATH_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Capture then set so Drop can restore a pre-existing value (or remove
        // only when the variable was originally absent).
        let previous_no_persist = std::env::var_os("NUMAN_TEST_NO_PERSIST_USER_PATH");
        // Block durable PATH writes for the duration of the guard.
        // Production `persist_path_dir` checks this; without it, ignored
        // acceptance tests permanently pollute developer User PATH / shell
        // profiles with tempfile fixture dirs.
        std::env::set_var("NUMAN_TEST_NO_PERSIST_USER_PATH", "1");
        Self {
            original: std::env::var_os("PATH"),
            previous_no_persist,
            #[cfg(windows)]
            original_user_path: match read_windows_user_path() {
                Ok(snapshot) => Some(snapshot),
                Err(err) => {
                    eprintln!(
                        "PathRestoreGuard: warning: could not snapshot Windows User PATH ({err}); \
                         restore-on-drop may be skipped"
                    );
                    None
                }
            },
            _lock: lock,
        }
    }
}

impl Drop for PathRestoreGuard {
    fn drop(&mut self) {
        match self.original.as_ref() {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
        match self.previous_no_persist.as_ref() {
            Some(prev) => std::env::set_var("NUMAN_TEST_NO_PERSIST_USER_PATH", prev),
            None => std::env::remove_var("NUMAN_TEST_NO_PERSIST_USER_PATH"),
        }
        #[cfg(windows)]
        {
            if let Some(snapshot) = self.original_user_path.as_ref() {
                if let Err(err) = write_windows_user_path(snapshot) {
                    // Never panic in Drop; surface the leak clearly on stderr.
                    eprintln!(
                        "PathRestoreGuard: failed to restore Windows User PATH: {err:#}\n\
                         Remove leftover Temp\\.tmp*\\off (or existing-nu) entries from \
                         System Properties → Environment Variables → User Path."
                    );
                }
            }
        }
    }
}

impl Default for PathRestoreGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// Snapshot wire format from PowerShell (unambiguous for `$null` vs `""`):
/// - `A` → User PATH absent (`$null`)
/// - `P` + standard Base64(UTF-8 bytes of value) → present value (incl. empty)
///
/// Length-prefixed UTF-8 slicing is unsafe here: PowerShell `$v.Length` is a
/// UTF-16 code-unit count, not a UTF-8 byte length.
#[cfg(windows)]
fn read_windows_user_path() -> Result<WindowsUserPathSnapshot, String> {
    let script = concat!(
        "$v = [Environment]::GetEnvironmentVariable('Path', 'User'); ",
        "if ($null -eq $v) { ",
        "Write-Output 'A' ",
        "} else { ",
        "$bytes = [Text.Encoding]::UTF8.GetBytes([string]$v); ",
        "Write-Output ('P' + [Convert]::ToBase64String($bytes)) ",
        "}"
    );
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .map_err(|e| format!("failed to invoke PowerShell: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "PowerShell User PATH read failed (status {}): {stderr}",
            output.status
        ));
    }
    parse_windows_user_path_stdout(&String::from_utf8_lossy(&output.stdout))
}

/// Parse the `A` / `P<base64>` snapshot line emitted by [`read_windows_user_path`].
#[cfg(any(windows, test))]
fn parse_windows_user_path_stdout(stdout: &str) -> Result<WindowsUserPathSnapshot, String> {
    let first = stdout
        .lines()
        .next()
        .unwrap_or("")
        .trim_end_matches(['\r', '\n', ' ', '\t']);
    if first == "A" {
        return Ok(WindowsUserPathSnapshot::Absent);
    }
    let Some(b64) = first.strip_prefix('P') else {
        return Err(format!(
            "unexpected PowerShell User PATH snapshot marker: {first:?}"
        ));
    };
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
        .map_err(|e| format!("invalid User PATH base64 payload: {e}"))?;
    let value = String::from_utf8(bytes)
        .map_err(|e| format!("User PATH snapshot is not valid UTF-8: {e}"))?;
    Ok(WindowsUserPathSnapshot::Value(OsString::from(value)))
}

#[cfg(windows)]
fn write_windows_user_path(snapshot: &WindowsUserPathSnapshot) -> anyhow::Result<()> {
    use anyhow::{bail, Context};
    let (script, maybe_env): (&str, Option<&str>) = match snapshot {
        // $null clears the User Path variable (absent), distinct from "".
        WindowsUserPathSnapshot::Absent => (
            "[Environment]::SetEnvironmentVariable('Path', $null, 'User')",
            None,
        ),
        WindowsUserPathSnapshot::Value(value) => {
            let value_str = value
                .to_str()
                .context("Windows User PATH snapshot is not valid UTF-8")?;
            (
                "[Environment]::SetEnvironmentVariable('Path', $env:NUMAN_RESTORE_USER_PATH, 'User')",
                Some(value_str),
            )
        }
    };
    let mut cmd = std::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    if let Some(value_str) = maybe_env {
        cmd.env("NUMAN_RESTORE_USER_PATH", value_str);
    }
    let output = cmd
        .output()
        .context("Failed to invoke PowerShell to restore user PATH")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to restore user PATH: {stderr}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Like [`PathRestoreGuard::new`], but installs `previous` under the PATH
    /// mutex before capturing so parallel tests cannot race the set/capture window.
    fn guard_with_previous_no_persist(previous: Option<&str>) -> PathRestoreGuard {
        let lock = PATH_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match previous {
            Some(v) => std::env::set_var("NUMAN_TEST_NO_PERSIST_USER_PATH", v),
            None => std::env::remove_var("NUMAN_TEST_NO_PERSIST_USER_PATH"),
        }
        let previous_no_persist = std::env::var_os("NUMAN_TEST_NO_PERSIST_USER_PATH");
        std::env::set_var("NUMAN_TEST_NO_PERSIST_USER_PATH", "1");
        PathRestoreGuard {
            original: std::env::var_os("PATH"),
            previous_no_persist,
            #[cfg(windows)]
            original_user_path: None,
            _lock: lock,
        }
    }

    #[test]
    fn path_restore_guard_preserves_no_persist_flag_across_drop() {
        {
            let guard = guard_with_previous_no_persist(Some("preexisting"));
            assert_eq!(
                std::env::var("NUMAN_TEST_NO_PERSIST_USER_PATH").as_deref(),
                Ok("1")
            );
            drop(guard);
            // Re-acquire so parallel PathRestoreGuard users cannot flip the flag
            // between Drop restore and our assertion.
            let _lock = PATH_MUTEX
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert_eq!(
                std::env::var("NUMAN_TEST_NO_PERSIST_USER_PATH").as_deref(),
                Ok("preexisting")
            );
            std::env::remove_var("NUMAN_TEST_NO_PERSIST_USER_PATH");
        }

        {
            let guard = guard_with_previous_no_persist(None);
            assert_eq!(
                std::env::var("NUMAN_TEST_NO_PERSIST_USER_PATH").as_deref(),
                Ok("1")
            );
            drop(guard);
            let _lock = PATH_MUTEX
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert!(
                std::env::var_os("NUMAN_TEST_NO_PERSIST_USER_PATH").is_none(),
                "flag must be removed when it was originally absent"
            );
        }
    }

    #[test]
    fn parse_windows_user_path_stdout_roundtrips_non_ascii() {
        // Regression: UTF-16 `.Length` must not be used as a UTF-8 byte index.
        // "café" is 4 UTF-16 code units but 5 UTF-8 bytes.
        let value = r"C:\Users\café\bin;D:\tools";
        let b64 =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, value.as_bytes());
        let parsed = parse_windows_user_path_stdout(&format!("P{b64}\r\n"))
            .expect("parse non-ASCII User PATH snapshot");
        assert_eq!(
            parsed,
            WindowsUserPathSnapshot::Value(OsString::from(value))
        );
        assert_eq!(
            parse_windows_user_path_stdout("A\r\n").unwrap(),
            WindowsUserPathSnapshot::Absent
        );
        assert_eq!(
            parse_windows_user_path_stdout("P\r\n").unwrap(),
            WindowsUserPathSnapshot::Value(OsString::from(""))
        );
    }
}

#[cfg(all(test, windows))]
mod windows_user_path_tests {
    use super::*;

    #[test]
    fn read_windows_user_path_distinguishes_absent_and_empty() {
        // Snapshot+restore the developer's real User PATH around mutations.
        let _guard = PathRestoreGuard::new();

        write_windows_user_path(&WindowsUserPathSnapshot::Value(OsString::from("")))
            .expect("set empty User PATH");
        let empty = read_windows_user_path().expect("read empty User PATH");
        assert_eq!(
            empty,
            WindowsUserPathSnapshot::Value(OsString::from("")),
            "empty string must not be treated as absent"
        );

        let non_ascii = OsString::from(r"C:\Users\café\bin");
        write_windows_user_path(&WindowsUserPathSnapshot::Value(non_ascii.clone()))
            .expect("set non-ASCII User PATH");
        let roundtrip = read_windows_user_path().expect("read non-ASCII User PATH");
        assert_eq!(
            roundtrip,
            WindowsUserPathSnapshot::Value(non_ascii),
            "non-ASCII User PATH must roundtrip without truncation"
        );

        write_windows_user_path(&WindowsUserPathSnapshot::Absent).expect("clear User PATH");
        let absent = read_windows_user_path().expect("read absent User PATH");
        assert_eq!(
            absent,
            WindowsUserPathSnapshot::Absent,
            "absent ($null) must not be treated as empty string"
        );
    }
}
