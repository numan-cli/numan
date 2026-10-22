//! Single generated `numan_init.nu` bootstrap file (issue #32).
//!
//! Numan owns one file that the user sources from `config.nu` exactly once:
//!
//! ```text
//! <root>/nu_state/numan_init.nu
//! ```
//!
//! It is a thin aggregator over the existing per-kind generated autoload files
//! (e.g. `<vendor-autoload-dir>/numan.nu`). It is regenerated atomically on
//! every activate/deactivate/rollback so `config.nu` never needs further edits.

use anyhow::{Context, Result};
use std::path::Path;

use crate::util::atomic::write_bytes_atomic;
use crate::util::fs_safety::{assert_managed_file_owned, assert_not_symlink};

/// Ownership header written at the top of every generated init file.
pub const INIT_FILE_HEADER: &str = "# Generated and managed by Numan. Do not edit by hand.\n# Regenerated on every activate/deactivate/rollback.\n";

/// Escape a path for embedding in a double-quoted Nushell string.
///
/// Nushell treats `\` as an escape character in double-quoted strings, so
/// Windows backslashes are normalized to forward slashes (valid on Windows)
/// and any literal double quote is escaped.
fn nu_escape_path(path: &str) -> String {
    path.replace('\\', "/").replace('"', "\\\"")
}

/// Render the contents of `numan_init.nu` for the given Numan root and Nu paths.
///
/// When a vendor-autoload directory is available and Numan owns the managed
/// `numan.nu` there, the file sources it. Otherwise it is just the ownership
/// header (a no-op file), so the single `source` line in `config.nu` always
/// resolves. An unowned or symlinked `numan.nu` is never sourced: `numan init`
/// wires this file into `config.nu`, so it must only ever execute Numan-owned
/// content.
pub fn render_init_file(nu_paths: &crate::nu::paths::NuPaths) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(INIT_FILE_HEADER);
    if let Some(vendor_dir) = &nu_paths.vendor_autoload_dir {
        let managed = std::path::Path::new(vendor_dir).join("numan.nu");
        // Only source the managed autoload file when Numan owns it (a regular
        // file beginning with the ownership marker, not a symlink). A missing
        // file means the fully-deactivated state — never reference it.
        if assert_managed_file_owned(&managed).is_ok() {
            let escaped = nu_escape_path(vendor_dir);
            out.push_str(&format!("source \"{escaped}/numan.nu\"\n"));
        }
    }
    out.into_bytes()
}

/// Atomically (re)generate `<root>/nu_state/numan_init.nu`.
pub fn regenerate_init_file(root: &Path, nu_paths: &crate::nu::paths::NuPaths) -> Result<()> {
    crate::nu::overlay::regenerate(root)?;
    let path = root.join("nu_state").join("numan_init.nu");
    let content = expected_init_content(root, nu_paths);
    write_bytes_atomic(&path, &content)
        .with_context(|| format!("Failed to regenerate '{}'", path.display()))
}

/// Expected content for drift detection (must match `regenerate_init_file`).
pub fn expected_init_content(root: &Path, nu_paths: &crate::nu::paths::NuPaths) -> Vec<u8> {
    let mut content = render_init_file(nu_paths);
    let path = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .join("nu_state/numan_overlay.nu");
    if let Ok(literal) = crate::cmd::overlay::nu_literal(&path.to_string_lossy()) {
        content.extend_from_slice(format!("source {literal}\n").as_bytes());
    }
    content
}

/// Marker comment used to make the config.nu source-line insertion idempotent.
pub const CONFIG_SOURCE_MARKER: &str = "# Numan (managed) — do not edit";

/// Build the single source line that config.nu must contain.
pub fn config_source_line(init_file_path: &Path) -> String {
    format!(
        "source \"{}\"  {}",
        nu_escape_path(&init_file_path.to_string_lossy()),
        CONFIG_SOURCE_MARKER
    )
}

/// Returns true when `config.nu` already contains the exact Numan managed
/// source line for `init_file_path`.
///
/// The line is matched in full (not just by marker) so a stale line pointing at
/// an old `NUMAN_ROOT` is not reported as current.
pub fn config_source_line_present(config_path: &Path, init_file_path: &Path) -> bool {
    let expected = config_source_line(init_file_path);
    std::fs::read_to_string(config_path)
        .map(|c| c.lines().any(|l| l.trim_end() == expected.trim_end()))
        .unwrap_or(false)
}

/// Idempotently insert the single `source` line into `config.nu`.
///
/// Returns `Ok(true)` when the file was changed, `Ok(false)` when it already
/// contained the exact managed line. Creates the file if it does not exist, and
/// replaces any stale managed line (e.g. pointing at another `NUMAN_ROOT`).
/// Refuses to write through a symlinked `config.nu` so dotfile-managed
/// configurations are never silently replaced.
pub fn ensure_config_source_line(config_path: &Path, init_file_path: &Path) -> Result<bool> {
    assert_not_symlink(config_path, "config.nu")?;
    let line = config_source_line(init_file_path);

    if config_path.exists() {
        let content = std::fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read '{}'", config_path.display()))?;
        let managed_lines: Vec<&str> = content
            .lines()
            .filter(|l| l.contains(CONFIG_SOURCE_MARKER))
            .collect();
        if managed_lines.len() == 1 && managed_lines[0].trim_end() == line.trim_end() {
            return Ok(false);
        }

        // Rebuild: drop every managed line (stale or misplaced) and append the
        // exact line for this root.
        let mut new_content = String::new();
        for l in content.lines() {
            if !l.contains(CONFIG_SOURCE_MARKER) {
                new_content.push_str(l);
                new_content.push('\n');
            }
        }
        if !new_content.is_empty() && !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push_str(&line);
        new_content.push('\n');
        write_bytes_atomic(config_path, new_content.as_bytes())
            .with_context(|| format!("Failed to write '{}'", config_path.display()))?;
        Ok(true)
    } else {
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create '{}'", parent.display()))?;
        }
        let content = format!("{}\n", line);
        write_bytes_atomic(config_path, content.as_bytes())
            .with_context(|| format!("Failed to write '{}'", config_path.display()))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nu::paths::NuPaths;
    use crate::util::fs_safety::OWNERSHIP_MARKER;

    fn make_paths(vendor: Option<&str>) -> NuPaths {
        NuPaths {
            nu_executable: "/usr/bin/nu".to_string(),
            nu_version: "0.114.1".to_string(),
            plugin_registry_path: "/tmp/plugins.msgpackz".to_string(),
            nu_executable_hash: "abc".to_string(),
            platform: "x86_64-unknown-linux-gnu".to_string(),
            data_dir: None,
            vendor_autoload_dirs: vec![],
            vendor_autoload_dir: vendor.map(str::to_string),
        }
    }

    fn write_owned_managed_file(vendor: &Path) {
        std::fs::create_dir_all(vendor).unwrap();
        std::fs::write(
            vendor.join("numan.nu"),
            format!("{}# managed\n", OWNERSHIP_MARKER),
        )
        .unwrap();
    }

    #[test]
    fn renders_source_line_when_managed_file_owned() {
        let dir = tempfile::tempdir().unwrap();
        let vendor = dir.path().join("vendor").join("autoload");
        write_owned_managed_file(&vendor);
        let vendor_str = vendor.to_string_lossy().into_owned();
        let paths = make_paths(Some(&vendor_str));
        let content = String::from_utf8(render_init_file(&paths)).unwrap();
        assert!(content.starts_with("# Generated and managed by Numan."));
        let expected = vendor_str.replace('\\', "/");
        assert!(content.contains(&format!("source \"{expected}/numan.nu\"")));
    }

    #[test]
    fn omits_source_line_when_managed_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let vendor = dir.path().join("vendor").join("autoload");
        std::fs::create_dir_all(&vendor).unwrap();
        let vendor_str = vendor.to_string_lossy().into_owned();
        let paths = make_paths(Some(&vendor_str));
        let content = String::from_utf8(render_init_file(&paths)).unwrap();
        assert!(content.starts_with("# Generated and managed by Numan."));
        assert!(
            !content.contains("source "),
            "deactivated state must not reference a missing autoload file: {content}"
        );
    }

    #[test]
    fn omits_source_line_when_managed_file_unowned() {
        let dir = tempfile::tempdir().unwrap();
        let vendor = dir.path().join("vendor").join("autoload");
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::write(vendor.join("numan.nu"), b"# not owned by numan\n").unwrap();
        let vendor_str = vendor.to_string_lossy().into_owned();
        let paths = make_paths(Some(&vendor_str));
        let content = String::from_utf8(render_init_file(&paths)).unwrap();
        assert!(
            !content.contains("source "),
            "unowned autoload content must never be sourced: {content}"
        );
    }

    #[test]
    fn renders_header_only_when_no_vendor_dir() {
        let paths = make_paths(None);
        let content = String::from_utf8(render_init_file(&paths)).unwrap();
        assert!(content.starts_with("# Generated and managed by Numan."));
        assert!(!content.contains("source "));
    }

    #[test]
    fn render_escapes_windows_backslashes() {
        let dir = tempfile::tempdir().unwrap();
        // On Linux, backslash is a valid filename character — use it to prove
        // that backslashes never leak into the emitted `source` statement.
        let vendor = dir.path().join("vendor\\autoload");
        write_owned_managed_file(&vendor);
        let vendor_str = vendor.to_string_lossy().into_owned();
        let paths = make_paths(Some(&vendor_str));
        let content = String::from_utf8(render_init_file(&paths)).unwrap();
        let expected = vendor_str.replace('\\', "/");
        assert!(content.contains(&format!("source \"{expected}/numan.nu\"\n")));
        assert!(!content.contains("source \"vendor\\"));
    }

    #[test]
    fn regenerate_creates_file_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let vendor = root.join("vendor").join("autoload");
        write_owned_managed_file(&vendor);
        std::fs::create_dir_all(root.join("nu_state")).unwrap();
        let vendor_str = vendor.to_string_lossy().into_owned();
        let paths = make_paths(Some(&vendor_str));
        regenerate_init_file(root, &paths).unwrap();
        let read = std::fs::read_to_string(root.join("nu_state/numan_init.nu")).unwrap();
        let expected = vendor_str.replace('\\', "/");
        assert!(read.contains(&format!("source \"{expected}/numan.nu\"")));
    }

    #[test]
    fn config_source_line_escapes_windows_paths() {
        let line = config_source_line(Path::new("C:\\Users\\dev\\nu_state\\numan_init.nu"));
        assert_eq!(
            line,
            format!("source \"C:/Users/dev/nu_state/numan_init.nu\"  {CONFIG_SOURCE_MARKER}")
        );
    }

    #[test]
    fn ensure_config_source_line_creates_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.nu");
        let init = dir.path().join("nu_state/numan_init.nu");
        assert!(ensure_config_source_line(&config, &init).unwrap());
        let content = std::fs::read_to_string(&config).unwrap();
        assert!(content.contains(&config_source_line(&init)));
        assert!(config_source_line_present(&config, &init));
    }

    #[test]
    fn ensure_config_source_line_appends_when_marker_absent() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.nu");
        std::fs::write(&config, "let-env EDITOR = 'vim'\n").unwrap();
        let init = dir.path().join("nu_state/numan_init.nu");
        assert!(ensure_config_source_line(&config, &init).unwrap());
        let content = std::fs::read_to_string(&config).unwrap();
        assert!(content.contains(&config_source_line(&init)));
        assert!(content.contains("let-env EDITOR"));
    }

    #[test]
    fn ensure_config_source_line_noop_when_exact_line_present() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.nu");
        let init = dir.path().join("nu_state/numan_init.nu");
        let line = config_source_line(&init);
        std::fs::write(&config, format!("{}\n", line)).unwrap();
        assert!(!ensure_config_source_line(&config, &init).unwrap());
    }

    #[test]
    fn ensure_config_source_line_replaces_stale_marker_line() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.nu");
        let stale = dir.path().join("old_root/nu_state/numan_init.nu");
        let current = dir.path().join("new_root/nu_state/numan_init.nu");
        std::fs::write(&config, format!("{}\n", config_source_line(&stale))).unwrap();
        assert!(ensure_config_source_line(&config, &current).unwrap());
        let content = std::fs::read_to_string(&config).unwrap();
        assert!(content.contains(&config_source_line(&current)));
        assert!(!content.contains(&config_source_line(&stale)));
        assert!(!config_source_line_present(&config, &stale));
        assert!(config_source_line_present(&config, &current));
    }

    #[test]
    fn config_source_line_present_requires_exact_line() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.nu");
        let other_root = dir.path().join("other_root/nu_state/numan_init.nu");
        let this_root = dir.path().join("this_root/nu_state/numan_init.nu");
        // Stale marker line from another root must NOT be treated as current.
        std::fs::write(&config, format!("{}\n", config_source_line(&other_root))).unwrap();
        assert!(!config_source_line_present(&config, &this_root));
        assert!(config_source_line_present(&config, &other_root));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_config_source_line_refuses_symlinked_config() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real_config.nu");
        std::fs::write(&target, "let-env EDITOR = 'vim'\n").unwrap();
        let config = dir.path().join("config.nu");
        symlink(&target, &config).unwrap();
        let init = dir.path().join("nu_state/numan_init.nu");
        let err = ensure_config_source_line(&config, &init)
            .expect_err("must refuse to write through a symlinked config.nu");
        assert!(err.to_string().contains("symlink"));
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "let-env EDITOR = 'vim'\n"
        );
    }
}
