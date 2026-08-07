//! Helpers for restoring process-wide state after a test.
//!
//! Available to crate unit tests and `tests/` integration tests. Prefer wrapping
//! any PATH mutation with [`PathRestoreGuard`] so concurrent tests cannot race
//! on the process-global environment and so a developer shell is not left with
//! a poisoned PATH after the test binary exits.
//!
//! On Windows, [`PathRestoreGuard`] also snapshots and restores the **User**
//! PATH registry value. Production `numan setup nu use` / doctor off-PATH
//! repair call [`crate::nu::bootstrap::persist_path_dir`], which writes the
//! User PATH via PowerShell; restoring only `std::env::var_os("PATH")` is not
//! enough and previously left tempfile fixture dirs (e.g. `...\Temp\.tmp*\off`)
//! permanently on developer machines after `cargo test -- --ignored`.

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
    #[cfg(windows)]
    original_user_path: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl PathRestoreGuard {
    pub fn new() -> Self {
        let lock = PATH_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        #[cfg(windows)]
        {
            // Block persistent User PATH writes for the duration of the guard.
            // Production `persist_path_dir` checks this; without it, ignored
            // acceptance tests permanently pollute developer User PATH with
            // tempfile fixture dirs.
            std::env::set_var("NUMAN_TEST_NO_PERSIST_USER_PATH", "1");
        }
        #[cfg(unix)]
        {
            // Same for Unix shell-profile PATH appends from off-path registration.
            std::env::set_var("NUMAN_TEST_NO_PERSIST_USER_PATH", "1");
        }
        Self {
            original: std::env::var_os("PATH"),
            #[cfg(windows)]
            original_user_path: read_windows_user_path().or_else(|| {
                eprintln!(
                    "PathRestoreGuard: warning: could not snapshot Windows User PATH; \
                     restore-on-drop may be skipped"
                );
                None
            }),
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
        std::env::remove_var("NUMAN_TEST_NO_PERSIST_USER_PATH");
        #[cfg(windows)]
        {
            if let Some(user_path) = self.original_user_path.as_ref() {
                if let Err(err) = write_windows_user_path(user_path) {
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

#[cfg(windows)]
fn read_windows_user_path() -> Option<OsString> {
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Environment]::GetEnvironmentVariable('Path', 'User')",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout);
    // PowerShell prints $null as empty; preserve empty string as Some("") so
    // restore can clear a previously-empty User PATH.
    Some(OsString::from(value.trim_end_matches(['\r', '\n'])))
}

#[cfg(windows)]
fn write_windows_user_path(value: &std::ffi::OsStr) -> anyhow::Result<()> {
    use anyhow::{bail, Context};
    let value_str = value
        .to_str()
        .context("Windows User PATH snapshot is not valid UTF-8")?;
    // Pass via env to avoid PowerShell injection from PATH contents.
    let output = std::process::Command::new("powershell")
        .env("NUMAN_RESTORE_USER_PATH", value_str)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Environment]::SetEnvironmentVariable('Path', $env:NUMAN_RESTORE_USER_PATH, 'User')",
        ])
        .output()
        .context("Failed to invoke PowerShell to restore user PATH")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to restore user PATH: {stderr}");
    }
    Ok(())
}
