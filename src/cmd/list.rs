use crate::nu::paths::NuPaths;
use crate::nupm_compat::schema::NUPM_IMPORT_ORIGIN;
use crate::state::lockfile::{Lockfile, BUNDLED_NU_ORIGIN};
use anyhow::Result;
use std::io::Write;
use std::path::Path;

pub fn execute(root: &Path) -> Result<()> {
    let mut stdout = std::io::stdout();
    execute_to(root, &mut stdout)
}

fn execute_to(root: &Path, out: &mut dyn Write) -> Result<()> {
    let lockfile = Lockfile::load(root)?;
    let nu_paths = NuPaths::load(root).ok();

    if lockfile.is_empty() {
        writeln!(out, "No packages installed.")?;
        return Ok(());
    }

    writeln!(out, "Installed packages ({}):\n", lockfile.packages.len())?;

    for (id, entry) in &lockfile.packages {
        let status = match &nu_paths {
            Some(p)
                if entry.is_active_for(
                    &p.nu_executable_hash,
                    &p.nu_version,
                    &p.plugin_registry_path,
                ) =>
            {
                "activated"
            }
            _ => "installed",
        };
        let origin_tag = if entry.origin.as_deref() == Some(NUPM_IMPORT_ORIGIN) {
            " (nupm import)"
        } else if entry.origin.as_deref() == Some(BUNDLED_NU_ORIGIN) {
            " (bundled with Nu)"
        } else {
            ""
        };
        writeln!(
            out,
            "  {}  v{}  [{}]  {}{}",
            id, entry.version, entry.package_type, status, origin_tag
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::lockfile::{LockfileEntry, PluginActivation};

    fn base_entry(version: &str, package_type: &str) -> LockfileEntry {
        LockfileEntry {
            version: version.to_string(),
            package_type: package_type.to_string(),
            source: "binary".to_string(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: None,
            archive_root: None,
            include: None,
            entry: None,
            installed_at: "0".to_string(),
            nu_version_at_install: None,
            activation: None,
            registry_url: None,
            registry_revision: None,
            index_sha256: None,
            signing_key_fingerprint: None,
            git_url: None,
            git_rev: None,
            cargo_name: None,
            cargo_lock_sha256: None,
            built_sha256: None,
            payload_path: String::new(),
            revision_id: None,
            payload_sha256: None,
            executable_sha256: None,
            selection_reason: None,
            origin: None,
            module_activation: None,
            module_import_mode: None,
            locked_dependencies: Default::default(),
        }
    }

    #[test]
    fn execute_empty_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        Lockfile::empty().save(dir.path()).unwrap();
        let mut out = Vec::new();
        execute_to(dir.path(), &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "No packages installed.\n");
    }

    #[test]
    fn execute_one_package() {
        let dir = tempfile::tempdir().unwrap();
        let mut lock = Lockfile::empty();
        lock.packages
            .insert("owner/pkg".to_string(), base_entry("1.0.0", "plugin"));
        lock.save(dir.path()).unwrap();
        let mut out = Vec::new();
        execute_to(dir.path(), &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("Installed packages (1):"));
        assert!(s.contains("owner/pkg  v1.0.0  [plugin]  installed"));
    }

    #[test]
    fn execute_multiple_packages_with_one_active() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let mut lock = Lockfile::empty();
        let mut active = base_entry("1.0.0", "plugin");
        active.activation = Some(PluginActivation {
            plugin_registry_path: "/path/to/plugins.msgpackz".to_string(),
            nu_executable_sha256: "abc123".to_string(),
            nu_version: "0.113.1".to_string(),
            activated_at: "0".to_string(),
        });
        lock.packages.insert("owner/active".to_string(), active);
        lock.packages
            .insert("owner/inactive".to_string(), base_entry("2.0.0", "module"));
        lock.save(root).unwrap();

        std::fs::create_dir_all(root.join("nu_state")).unwrap();
        let nu_paths = NuPaths {
            nu_executable: "/usr/bin/nu".to_string(),
            nu_version: "0.113.1".to_string(),
            plugin_registry_path: "/path/to/plugins.msgpackz".to_string(),
            nu_executable_hash: "abc123".to_string(),
            platform: "x86_64-unknown-linux-gnu".to_string(),
            data_dir: None,
            vendor_autoload_dirs: vec![],
            vendor_autoload_dir: None,
        };
        nu_paths.save(root).unwrap();

        let mut out = Vec::new();
        execute_to(root, &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("Installed packages (2):"));
        assert!(s.contains("owner/active  v1.0.0  [plugin]  activated"));
        assert!(s.contains("owner/inactive  v2.0.0  [module]  installed"));
    }
}
