//! Test-only helpers for restoring process-wide state after a test.
//!
//! `#[cfg(test)]`-gated so this module is never compiled into release
//! builds; the only ways to reach it are via dev-dependency test crates
//! or `cfg(test)` lib tests.
#![cfg(test)]

use std::ffi::OsString;

/// RAII guard that snapshots the process PATH on construction and
/// restores it on drop. Use this around any test that mutates PATH so
/// real-Nu runs from a developer terminal are not poisoned by the test
/// process.
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
}

impl PathRestoreGuard {
    pub fn new() -> Self {
        Self {
            original: std::env::var_os("PATH"),
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
