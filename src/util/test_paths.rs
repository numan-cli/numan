//! Helpers for restoring process-wide PATH after a test.
//!
//! Gated with `#[cfg(test)]` so release builds never compile or export this
//! module. Integration tests under `tests/` keep a local copy of the guard.

#![cfg(test)]

use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

/// Serializes process-global PATH mutations across tests.
///
/// Held for the lifetime of every [`PathRestoreGuard`] (including through
/// `Drop` restoration) so concurrent tests cannot interleave `set_var` /
/// `remove_var` on `PATH`.
static PATH_ENV_LOCK: Mutex<()> = Mutex::new(());

/// RAII guard that snapshots the process PATH on construction and
/// restores it on drop. Use this around any test that mutates PATH so
/// real-Nu runs from a developer terminal are not poisoned by the test
/// process.
///
/// # Example
///
/// ```text
/// let _path_guard = PathRestoreGuard::new();
/// // mutate PATH for the test...
/// // drop restores original PATH (or unsets it if it was unset)
/// ```
#[must_use = "PathRestoreGuard restores PATH on drop; bind it to a variable"]
pub struct PathRestoreGuard {
    original: Option<OsString>,
    /// Held until drop completes so PATH restore is race-free.
    _lock: MutexGuard<'static, ()>,
}

impl PathRestoreGuard {
    pub fn new() -> Self {
        // Poisoned lock still allows progress: a panicking PATH test should
        // not permanently brick the suite.
        let lock = PATH_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    }
}

impl Default for PathRestoreGuard {
    fn default() -> Self {
        Self::new()
    }
}
