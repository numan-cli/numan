//! Helpers for restoring process-wide state after a test.
//!
//! Available to crate unit tests and `tests/` integration tests. Prefer wrapping
//! any PATH mutation with [`PathRestoreGuard`] so concurrent tests cannot race
//! on the process-global environment and so a developer shell is not left with
//! a poisoned PATH after the test binary exits.
//!
//! On Windows, the guard prevents test-triggered persistent User PATH writes.
//! Production `numan setup nu use` / doctor off-PATH repair call
//! [`crate::nu::bootstrap::persist_path_dir`], which writes the User PATH via
//! PowerShell; the test-only environment flag prevents those writes during
//! guarded tests.

use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

/// Serializes every PATH snapshot/restore so concurrent tests cannot
/// race through the process-global environment.
static PATH_MUTEX: Mutex<()> = Mutex::new(());

/// RAII guard that snapshots the process PATH on construction and
/// restores it on drop. Acquires a shared process-wide mutex so callers
/// never need a separate `Mutex` for PATH serialization.
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
/// // drop restores the original process PATH
/// ```
pub struct PathRestoreGuard {
    original: Option<OsString>,
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
    }
}

impl Default for PathRestoreGuard {
    fn default() -> Self {
        Self::new()
    }
}

