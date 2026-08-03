//! Test-only helpers for restoring process-wide state after a test.
//!
//! `#[cfg(test)]`-gated at the module declaration in `src/util/mod.rs`, so
//! this module is never compiled into release builds. It is reachable only
//! from inline `cfg(test)` unit tests in this crate, not from integration
//! tests under `tests/` or from external crates.

use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

/// Serializes every PATH snapshot/restore so concurrent unit tests cannot
/// race through the process-global environment.
static PATH_MUTEX: Mutex<()> = Mutex::new(());

/// RAII guard that snapshots the process PATH on construction and
/// restores it on drop. Acquires a shared process-wide mutex so callers
/// never need a separate `Mutex` for PATH serialization.
///
/// Use this around any test that mutates PATH so real-Nu runs from a
/// developer terminal are not poisoned by the test process.
///
/// # Example
///
/// ```ignore
/// let _path_guard = PathRestoreGuard::new();
/// prepend_process_path_for_test("...");
/// // drop restores original PATH (or unsets it if it was unset)
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
