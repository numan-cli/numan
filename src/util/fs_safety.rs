//! Filesystem safety helpers for Numan's managed-file operations.
//!
//! ## Mutation lock
//!
//! [`MutationLock`] wraps an advisory exclusive OS lock on
//! `$NUMAN_ROOT/state/mutation.lock`.  Acquiring it serializes all
//! state-mutating Numan commands (install, activate, deactivate, init
//! --refresh with active modules) against concurrent invocations of the same
//! binary on the same root.  The lock is released automatically when the
//! guard is dropped.
//!
//! Enforces three classes of invariant:
//!
//! 1. **Symlink / reparse-point detection** — Numan must never follow a
//!    symlink or Windows reparse point when writing to or reading from paths
//!    it manages.  Any such path causes an immediate error.
//!
//! 2. **Root containment** — Relative paths built from registry or lockfile
//!    metadata must resolve to a canonical path that remains under the
//!    canonical Numan root.  A `..` traversal or absolute component in a
//!    registry-supplied path must be rejected before any I/O.
//!
//! 3. **Managed-file ownership guard** — Before Numan overwrites or deletes
//!    `numan.nu`, it verifies that the file and its parent directory are
//!    regular (non-symlink) paths that Numan itself wrote, identified by the
//!    ownership-marker header.

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::path::{Component, Path, PathBuf};

// ── Mutation lock ─────────────────────────────────────────────────────────────

/// RAII guard holding an exclusive advisory lock on
/// `$NUMAN_ROOT/state/mutation.lock`.
///
/// Obtain with [`acquire_mutation_lock`].  The lock is released when this
/// value is dropped (the underlying file descriptor is closed).
///
/// The lock is **advisory**: it serializes well-behaved Numan processes but
/// does not prevent a foreign process from writing to the same files.
pub struct MutationLock {
    // The RwLockWriteGuard holds the OS lock as long as it is alive.
    // Wrapping in a Box keeps the guard's address stable (fd-lock ties the
    // lock to the file's memory address via raw-pointer tricks internally).
    _guard: Box<fd_lock::RwLockWriteGuard<'static, File>>,
    // We also keep the RwLock alive so the guard's lifetime is satisfied.
    _lock: Box<fd_lock::RwLock<File>>,
}

/// Acquire the root-scoped exclusive mutation lock.
///
/// Creates `$NUMAN_ROOT/state/mutation.lock` if absent, then attempts a
/// **non-blocking** exclusive lock.  Returns an error immediately if another
/// process already holds the lock:
///
/// ```text
/// Another Numan mutation is already in progress for this root.
/// Wait for it to finish, then retry.
/// ```
///
/// The returned [`MutationLock`] releases the lock when dropped.
pub fn acquire_mutation_lock(root: &Path) -> Result<MutationLock> {
    let state_dir = root.join("state");
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("Failed to create state directory '{}'", state_dir.display()))?;

    let lock_path = state_dir.join("mutation.lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("Failed to open mutation lock '{}'", lock_path.display()))?;

    // Box the RwLock so its address is stable before we take the write guard.
    let mut lock_box: Box<fd_lock::RwLock<File>> = Box::new(fd_lock::RwLock::new(file));

    // SAFETY: we extend the lifetime to 'static only for the purpose of
    // storing the guard alongside the lock in the same struct.  Both are
    // dropped together when MutationLock is dropped, and neither escapes.
    let lock_ref: &'static mut fd_lock::RwLock<File> =
        unsafe { &mut *(lock_box.as_mut() as *mut fd_lock::RwLock<File>) };

    let guard = lock_ref.try_write().map_err(|_| {
        anyhow::anyhow!(
            "Another Numan mutation is already in progress for this root.\n\
             Wait for it to finish, then retry."
        )
    })?;

    Ok(MutationLock {
        _guard: Box::new(guard),
        _lock: lock_box,
    })
}

/// Acquire the root mutation lock, run a destructive setup subcommand, and
/// release the lock on return.
///
/// Every destructive setup entry point (install, off-path registration,
/// PATH-Nu registration, managed removal, `numan setup loader`, and
/// derive/active/upgrade via `numan use`) flows through this helper so the
/// lock boundary has exactly one source of truth — closing PR #69's WCr
/// (`setup_family_mutation_lock`) and ensuring that a concurrent `numan use`,
/// `numan install`, or `numan doctor --fix` cannot interleave filesystem
/// mutations on the same root.
///
/// `what` is a short human-readable label (e.g. `"Nushell install"`,
/// `"off-path Nu registration"`, `"managed Nushell removal"`); it lands
/// in the audit log alongside the `(audit)` prefix so safe-batch automation
/// can grep one consistent shape across the whole destructive setup
/// surface:
///
/// ```text
/// (audit) setup mutation lock acquired for {what} on '{root}'.
/// ```
///
/// The closure runs while the lock is held; returns the closure's
/// `Result<T>` verbatim. The lock is released on Drop at the end of this
/// function's scope (also on panic — RAII).
///
/// Use [`crate::util::confirm::require_tty_or_yes`] for non-TTY / `--yes`
/// gating; this helper does **not** add TTY checks.
pub fn setup_subcommand_lock<T, F>(root: &Path, what: &str, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let _lock = acquire_mutation_lock(root).with_context(|| {
        format!(
            "Refusing destructive '{what}' on '{}': another Numan mutation is already in progress.",
            root.display()
        )
    })?;
    eprintln!(
        "(audit) setup mutation lock acquired for {what} on '{}'.",
        root.display()
    );
    f()
}

// ── Ownership marker ──────────────────────────────────────────────────────────

/// The exact two-line UTF-8 prefix that every Numan-generated autoload file
/// must begin with.  Checked before any overwrite or delete of `numan.nu`.
pub const OWNERSHIP_MARKER: &str =
    "# Generated and managed by Numan. Do not edit.\n# Numan autoload schema: 1\n";

// ── Symlink / reparse-point detection ─────────────────────────────────────────

/// Returns `true` when `path` itself is a symlink or Windows reparse point.
///
/// On Unix, `symlink_metadata` reports `is_symlink()` directly.
/// On Windows, the same call also returns `true` for NTFS reparse points
/// (junctions and symbolic links) because the Rust standard library maps
/// `FILE_FLAG_OPEN_REPARSE_POINT` for `symlink_metadata`.
pub fn is_symlink_or_reparse(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.file_type().is_symlink()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => {
            Err(e).with_context(|| format!("Failed to read metadata for '{}'", path.display()))
        }
    }
}

/// Asserts that `path` is not a symlink or reparse point.
///
/// Returns an error with a descriptive message if the check fails.
pub fn assert_not_symlink(path: &Path, label: &str) -> Result<()> {
    if is_symlink_or_reparse(path)? {
        bail!(
            "Numan safety check failed: {} '{}' is a symlink or reparse point. \
             Numan will not operate on symlinked paths.",
            label,
            path.display()
        );
    }
    Ok(())
}

// ── Root containment ──────────────────────────────────────────────────────────

/// Returns `true` when `candidate` is relative and contains no `..`,
/// root component, or Windows drive/UNC prefix.
///
/// Does **not** perform I/O; the check is purely lexical.
pub fn is_safe_relative_path(candidate: &Path) -> bool {
    if candidate.is_absolute() {
        return false;
    }
    for component in candidate.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            // ParentDir (..), RootDir (/), Prefix (C:, \\server\share)
            _ => return false,
        }
    }
    true
}

/// Resolves `relative` against `root` and asserts the result is contained
/// within the canonical `root` directory.
///
/// `relative` must pass [`is_safe_relative_path`] first; then the joined
/// canonical path must begin with the canonical root prefix.
///
/// Returns the canonical joined path on success.
pub fn assert_contained(root: &Path, relative: &Path) -> Result<PathBuf> {
    if !is_safe_relative_path(relative) {
        bail!(
            "Path '{}' is not a safe relative path: must be relative, \
             contain no '..', no root component, and no platform prefix.",
            relative.display()
        );
    }

    let joined = root.join(relative);

    // Canonicalize root first (it must exist).
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize root '{}'", root.display()))?;

    // Canonicalize the joined path only if it exists; otherwise append the
    // relative components onto the *canonical* root. Lexically normalizing
    // `root.join(relative)` would keep pre-canonicalize prefixes (e.g. macOS
    // `/var` vs `/private/var`) and spuriously fail the `starts_with` check
    // for not-yet-created paths such as `tools/nushell/<version>/`.
    let canonical_joined = if joined.exists() {
        joined
            .canonicalize()
            .with_context(|| format!("Failed to canonicalize '{}'", joined.display()))?
    } else {
        let mut out = canonical_root.clone();
        for component in relative.components() {
            match component {
                Component::CurDir => {}
                Component::Normal(c) => out.push(c),
                Component::ParentDir => {
                    // Should not occur after is_safe_relative_path; defensive.
                    out.pop();
                }
                // RootDir / Prefix already rejected by is_safe_relative_path.
                _ => {}
            }
        }
        out
    };

    if !canonical_joined.starts_with(&canonical_root) {
        bail!(
            "Path containment violation: '{}' resolves outside of root '{}'.",
            relative.display(),
            root.display()
        );
    }

    Ok(canonical_joined)
}

/// Refuse migration/reconcile when the managed Nushell layout escapes `$NUMAN_ROOT`.
///
/// Checks:
/// 1. `tools` and `tools/nushell` are not themselves symlinks/reparse points
/// 2. When those paths exist, their canonical forms stay under the canonical root
///    (catches a symlinked *ancestor* such as `$NUMAN_ROOT/tools` → elsewhere,
///    where the leaf `nushell` directory is a real dir and a leaf-only symlink
///    check would miss the escape)
pub fn assert_managed_nushell_layout(root: &Path) -> Result<()> {
    let tools = root.join("tools");
    assert_not_symlink(&tools, "managed tools directory")?;
    let managed = tools.join("nushell");
    assert_not_symlink(&managed, "managed Nushell directory")?;
    if tools.exists() {
        let _ = assert_contained(root, Path::new("tools"))?;
    }
    if managed.exists() {
        let _ = assert_contained(root, Path::new("tools/nushell"))?;
    }
    Ok(())
}

// ── Managed-file ownership guard ──────────────────────────────────────────────

/// Verifies that `file_path` is a regular, non-symlink file that begins with
/// the Numan ownership marker.
///
/// This must be called before any overwrite or delete of `numan.nu`.
///
/// # Checks performed
///
/// 1. The **parent directory** of `file_path` is not a symlink or reparse point.
/// 2. `file_path` itself is not a symlink or reparse point.
/// 3. The file exists and begins with [`OWNERSHIP_MARKER`].
///
/// SHA-256 and autoload-state projection checks are the caller's responsibility
/// (they require external state not available here).
pub fn assert_managed_file_owned(file_path: &Path) -> Result<()> {
    // 1. Parent directory must not be a symlink.
    if let Some(parent) = file_path.parent() {
        assert_not_symlink(parent, "vendor-autoload directory")?;
    }

    // 2. The file itself must not be a symlink.
    assert_not_symlink(file_path, "managed file")?;

    // 3. File must begin with the ownership marker.
    let content = std::fs::read(file_path).with_context(|| {
        format!(
            "Failed to read managed file '{}' for ownership check",
            file_path.display()
        )
    })?;

    if !content.starts_with(OWNERSHIP_MARKER.as_bytes()) {
        bail!(
            "Numan managed-file drift detected.\n\n\
             numan.nu was changed, replaced, moved, or is no longer a Numan-owned regular \
             file. Numan will not overwrite or delete it automatically.\n\n\
             File: {}",
            file_path.display()
        );
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ── is_safe_relative_path ────────────────────────────────────────────────

    #[test]
    fn safe_relative_simple() {
        assert!(is_safe_relative_path(Path::new("foo/bar/baz.nu")));
    }

    #[test]
    fn safe_relative_single_component() {
        assert!(is_safe_relative_path(Path::new("mod.nu")));
    }

    #[test]
    fn rejects_absolute() {
        assert!(!is_safe_relative_path(Path::new("/absolute/path")));
    }

    #[test]
    fn rejects_parent_traversal() {
        assert!(!is_safe_relative_path(Path::new("../escape")));
    }

    #[test]
    fn rejects_embedded_parent_traversal() {
        assert!(!is_safe_relative_path(Path::new("foo/../../etc/passwd")));
    }

    #[cfg(windows)]
    #[test]
    fn rejects_windows_prefix() {
        assert!(!is_safe_relative_path(Path::new("C:\\absolute")));
    }

    // ── assert_contained ─────────────────────────────────────────────────────

    #[test]
    fn contained_path_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Create the sub-path so canonicalize works.
        let sub = root.join("packages").join("mod.nu");
        std::fs::create_dir_all(sub.parent().unwrap()).unwrap();
        std::fs::write(&sub, b"").unwrap();

        let result = assert_contained(root, Path::new("packages/mod.nu")).unwrap();
        assert!(result.starts_with(root.canonicalize().unwrap()));
    }

    #[test]
    fn contained_nonexistent_path_uses_canonical_root_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Not-yet-created leaf must still pass under a (possibly
        // symlink-prefixed) temp root — e.g. macOS /var → /private/var.
        let result = assert_contained(root, Path::new("tools/nushell/0.113.1")).unwrap();
        let canonical_root = root.canonicalize().unwrap();
        assert!(
            result.starts_with(&canonical_root),
            "expected {} under {}",
            result.display(),
            canonical_root.display()
        );
        assert!(result.ends_with(Path::new("tools/nushell/0.113.1")));
    }

    #[test]
    fn rejects_parent_traversal_in_assert_contained() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let err = assert_contained(root, Path::new("../escape")).unwrap_err();
        assert!(err.to_string().contains("safe relative path"));
    }

    #[test]
    fn rejects_absolute_in_assert_contained() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let err = assert_contained(root, Path::new("/etc/passwd")).unwrap_err();
        assert!(err.to_string().contains("safe relative path"));
    }

    // ── assert_managed_file_owned ────────────────────────────────────────────

    #[test]
    fn owned_file_passes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("numan.nu");
        let mut f = std::fs::File::create(&file).unwrap();
        f.write_all(OWNERSHIP_MARKER.as_bytes()).unwrap();
        f.write_all(b"\nuse \"some/path.nu\"\n").unwrap();
        drop(f);
        assert_managed_file_owned(&file).unwrap();
    }

    #[test]
    fn unowned_file_fails_ownership_check() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("numan.nu");
        std::fs::write(&file, b"# Not a Numan file\n").unwrap();
        let err = assert_managed_file_owned(&file).unwrap_err();
        assert!(err.to_string().contains("managed-file drift"));
    }

    #[test]
    fn missing_file_fails_ownership_check() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nonexistent.nu");
        let err = assert_managed_file_owned(&file).unwrap_err();
        // Error comes from the read failure, not the marker check.
        assert!(err.to_string().contains("ownership check"));
    }

    // ── is_symlink_or_reparse ────────────────────────────────────────────────

    #[test]
    fn regular_file_is_not_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("regular.txt");
        std::fs::write(&file, b"hello").unwrap();
        assert!(!is_symlink_or_reparse(&file).unwrap());
    }

    #[test]
    fn nonexistent_path_is_not_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist");
        assert!(!is_symlink_or_reparse(&path).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn unix_symlink_detected() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&target, b"hello").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(is_symlink_or_reparse(&link).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn managed_nushell_layout_refuses_symlinked_tools_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(outside.join("nushell")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("tools")).unwrap();
        let err = assert_managed_nushell_layout(root).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("symlink") || msg.contains("reparse") || msg.contains("containment"),
            "got: {msg}"
        );
    }

    #[test]
    fn managed_nushell_layout_accepts_plain_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("tools/nushell")).unwrap();
        assert_managed_nushell_layout(root).unwrap();
    }

    // ── setup_subcommand_lock ───────────────────────────────────────────────

    #[test]
    fn setup_subcommand_lock_runs_closure_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let seen = setup_subcommand_lock(root, "test op", || Ok::<_, anyhow::Error>(42)).unwrap();
        assert_eq!(seen, 42);
        // Lock is released on Drop: a second acquisition after the first
        // closure returned must succeed.
        acquire_mutation_lock(root).expect("lock should be free after first closure returns");
    }

    #[test]
    fn setup_subcommand_lock_propagates_closure_error_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let err = setup_subcommand_lock::<(), _>(root, "test op", || {
            anyhow::bail!("closure failed");
        })
        .unwrap_err();
        assert!(err.to_string().contains("closure failed"));
        // Lock must still be released on the error path so a retry succeeds.
        acquire_mutation_lock(root).expect("lock should be free even on error");
    }

    #[test]
    fn setup_subcommand_lock_second_concurrent_call_fails() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Hold the lock outside the helper. A second attempt must fail fast
        // with the "another Numan mutation" message — this is the property
        // AGENTS.md relies on to serialize destructive setup across
        // concurrent invocations of the same binary on the same root.
        let _held = acquire_mutation_lock(root).expect("first acquire");
        let err = setup_subcommand_lock::<(), _>(root, "test op", || Ok(()))
            .expect_err("concurrent acquire must fail");
        assert!(
            err.to_string().contains("another Numan mutation"),
            "unexpected error: {err}"
        );
    }
}
