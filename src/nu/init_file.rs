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

/// Ownership header written at the top of every generated init file.
pub const INIT_FILE_HEADER: &str = "# Generated and managed by Numan. Do not edit by hand.\n# Regenerated on every activate/deactivate/rollback.\n";

/// Render the contents of `numan_init.nu` for the given Numan root and Nu paths.
///
/// When a vendor-autoload directory is available, the file sources the managed
/// `numan.nu` there. Otherwise it is just the ownership header (a no-op file),
/// so the single `source` line in `config.nu` always resolves.
pub fn render_init_file(nu_paths: &crate::nu::paths::NuPaths) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(INIT_FILE_HEADER);
    if let Some(vendor_dir) = &nu_paths.vendor_autoload_dir {
        let managed = std::path::Path::new(vendor_dir).join("numan.nu");
        // Only source the managed autoload file when it actually exists, so a
        // fully-deactivated state never references a missing file.
        if managed.is_file() {
            let path_str = format!("{}/numan.nu", vendor_dir);
            let escaped = path_str.replace('\\', "\\\\").replace('"', "\\\"");
            out.push_str(&format!("source \"{}\"\n", escaped));
        }
    }
    out.into_bytes()
}

/// Atomically (re)generate `<root>/nu_state/numan_init.nu`.
pub fn regenerate_init_file(root: &Path, nu_paths: &crate::nu::paths::NuPaths) -> Result<()> {
    let path = root.join("nu_state").join("numan_init.nu");
    let content = render_init_file(nu_paths);
    write_bytes_atomic(&path, &content)
        .with_context(|| format!("Failed to regenerate '{}'", path.display()))
}

/// Expected content for drift detection (must match `regenerate_init_file`).
pub fn expected_init_content(nu_paths: &crate::nu::paths::NuPaths) -> Vec<u8> {
    render_init_file(nu_paths)
}

/// Marker comment used to make the config.nu source-line insertion idempotent.
pub const CONFIG_SOURCE_MARKER: &str = "# Numan (managed) — do not edit";

/// Build the single source line that config.nu must contain.
pub fn config_source_line(init_file_path: &Path) -> String {
    let escaped = init_file_path
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("source \"{}\"  {}", escaped, CONFIG_SOURCE_MARKER)
}

/// Returns true when `config.nu` already contains the Numan managed source line.
pub fn config_source_line_present(config_path: &Path) -> bool {
    std::fs::read_to_string(config_path)
        .map(|c| c.contains(CONFIG_SOURCE_MARKER))
        .unwrap_or(false)
}

/// Idempotently insert the single `source` line into `config.nu`.
///
/// Returns `Ok(true)` when the file was changed, `Ok(false)` when it already
/// contained the managed line. Creates the file if it does not exist.
pub fn ensure_config_source_line(config_path: &Path, init_file_path: &Path) -> Result<bool> {
    let line = config_source_line(init_file_path);
    if config_path.exists() {
        let content = std::fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read '{}'", config_path.display()))?;
        if content.contains(CONFIG_SOURCE_MARKER) {
            return Ok(false);
        }
        let mut new_content = content;
        if !new_content.ends_with('\n') && !new_content.is_empty() {
            new_content.push('\n');
        }
        if !new_content.is_empty() {
            new_content.push('\n');
        }
        new_content.push_str(&line);
        new_content.push('\n');
        crate::util::atomic::write_bytes_atomic(config_path, new_content.as_bytes())
            .with_context(|| format!("Failed to write '{}'", config_path.display()))?;
        Ok(true)
    } else {
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create '{}'", parent.display()))?;
        }
        let content = format!("{}\n", line);
        crate::util::atomic::write_bytes_atomic(config_path, content.as_bytes())
            .with_context(|| format!("Failed to write '{}'", config_path.display()))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nu::paths::NuPaths;

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

    #[test]
    fn renders_source_line_when_managed_file_present() {
        let dir = tempfile::tempdir().unwrap();
        let vendor = dir.path().join("vendor").join("autoload");
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::write(vendor.join("numan.nu"), b"# managed\n").unwrap();
        let vendor_str = vendor.to_string_lossy().into_owned();
        let paths = make_paths(Some(&vendor_str));
        let content = String::from_utf8(render_init_file(&paths)).unwrap();
        assert!(content.starts_with("# Generated and managed by Numan."));
        let path_str = format!("{}/numan.nu", vendor_str);
        // Escape like production does: host paths may contain \ (Windows).
        // The dedicated escaping test below guards the chain itself.
        let escaped = path_str.replace('\\', "\\\\").replace('"', "\\\"");
        assert!(content.contains(&format!("source \"{}\"", escaped)));
    }

    #[test]
    fn render_init_file_escapes_backslashes_and_quotes() {
        // Verify Nu string-literal escaping with a path containing \ and ".
        // We test config_source_line directly because render_init_file requires
        // actual filesystem paths that are valid on the host OS.
        let raw = "C:\\Users\\test\"dir\\";
        let init_path = std::path::Path::new(raw);
        let line = config_source_line(init_path);
        // Backslashes must be doubled and double-quotes must be backslash-escaped.
        assert!(line.contains("\\\\"), "backslash not doubled in: {line}");
        assert!(line.contains("\\\""), "double-quote not escaped in: {line}");
        assert!(line.contains(CONFIG_SOURCE_MARKER));
    }

    #[test]
    fn renders_header_only_when_no_vendor_dir() {
        let paths = make_paths(None);
        let content = String::from_utf8(render_init_file(&paths)).unwrap();
        assert!(content.starts_with("# Generated and managed by Numan."));
        assert!(!content.contains("source "));
    }

    #[test]
    fn regenerate_creates_file_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let vendor = root.join("vendor").join("autoload");
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::write(vendor.join("numan.nu"), b"# managed\n").unwrap();
        std::fs::create_dir_all(root.join("nu_state")).unwrap();
        let vendor_str = vendor.to_string_lossy().into_owned();
        let paths = make_paths(Some(&vendor_str));
        regenerate_init_file(root, &paths).unwrap();
        let read = std::fs::read_to_string(root.join("nu_state/numan_init.nu")).unwrap();
        let path_str = format!("{}/numan.nu", vendor_str);
        let escaped = path_str.replace('\\', "\\\\").replace('"', "\\\"");
        assert!(read.contains(&format!("source \"{}\"", escaped)));
    }
}
