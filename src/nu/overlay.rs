use anyhow::{Context, Result};
use std::path::Path;

use crate::util::atomic::write_bytes_atomic;
use crate::util::fs_safety::assert_managed_file_owned;

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
    if path.symlink_metadata().is_ok() {
        assert_managed_file_owned(&path)?;
    }
    let content = render_overlay_shell(root)?;
    write_bytes_atomic(&path, content.as_bytes())
        .context("Failed to write Numan overlay shell integration")
}
