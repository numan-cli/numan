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
    /// Pre-existing `NUMAN_TEST_NO_PERSIST_USER_PATH` value (or `None` if absent).
    previous_no_persist: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
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
    }
}

impl Default for PathRestoreGuard {
    fn default() -> Self {
        Self::new()
    }
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
}
