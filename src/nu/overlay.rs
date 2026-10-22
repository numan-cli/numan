use anyhow::{Context, Result};
use std::path::Path;

use crate::util::atomic::write_bytes_atomic;
use crate::util::fs_safety::{assert_managed_file_owned, assert_not_symlink};

pub fn render_overlay_shell(root: &Path) -> Result<String> {
    let root = root.canonicalize()?;
    let literal = crate::cmd::overlay::nu_literal(&root.to_string_lossy())?;
    Ok(format!(
        "{}{}",
        crate::util::fs_safety::OWNERSHIP_MARKER,
        include_str!("overlay_shell.nu").replace("@ROOT@", &literal)
    ))
}

pub fn regenerate(root: &Path) -> Result<()> {
    let path = root.join("nu_state/numan_overlay.nu");
    assert_not_symlink(&root.join("nu_state"), "overlay state directory")?;
    if path.symlink_metadata().is_ok() {
        assert_managed_file_owned(&path)?;
    }
    let content = render_overlay_shell(root)?;
    write_bytes_atomic(&path, content.as_bytes())
        .context("Failed to write Numan overlay shell integration")
}

pub fn is_current(root: &Path) -> bool {
    let path = root.join("nu_state/numan_overlay.nu");
    assert_managed_file_owned(&path).is_ok()
        && render_overlay_shell(root).ok().is_some_and(|expected| {
            std::fs::read_to_string(path).ok().as_deref() == Some(expected.as_str())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_overlay_is_current_but_changed_content_is_not() {
        let root = tempfile::tempdir().unwrap();
        regenerate(root.path()).unwrap();
        assert!(is_current(root.path()));
        std::fs::write(root.path().join("nu_state/numan_overlay.nu"), "changed").unwrap();
        assert!(!is_current(root.path()));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_parent_and_owned_symlinked_file() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("nu_state")).unwrap();
        assert!(regenerate(root.path()).is_err());
        assert!(!outside.path().join("numan_overlay.nu").exists());
        std::fs::remove_file(root.path().join("nu_state")).unwrap();
        std::fs::create_dir(root.path().join("nu_state")).unwrap();
        let external = outside.path().join("overlay.nu");
        std::fs::write(&external, render_overlay_shell(root.path()).unwrap()).unwrap();
        std::os::unix::fs::symlink(&external, root.path().join("nu_state/numan_overlay.nu"))
            .unwrap();
        assert!(!is_current(root.path()));
        assert!(regenerate(root.path()).is_err());
    }
}
