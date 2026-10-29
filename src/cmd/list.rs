use crate::nu::hooks::generate_hook_autoload_content;
use crate::nu::paths::NuPaths;
use crate::nupm_compat::schema::NUPM_IMPORT_ORIGIN;
use crate::state::hook_profile::HookProfile;
use crate::state::lockfile::{Lockfile, BUNDLED_NU_ORIGIN};
use anyhow::Result;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

pub fn execute(root: &Path) -> Result<()> {
    let mut stdout = std::io::stdout();
    execute_to(root, &mut stdout)
}

pub(crate) fn execute_to(root: &Path, out: &mut dyn Write) -> Result<()> {
    let lockfile = Lockfile::load(root)?;
    let nu_paths = NuPaths::load(root).ok();
    let active_hook_packages = nu_paths.as_ref().and_then(|paths| {
        let profile = HookProfile::load(root).ok()?;
        let managed_path = profile.managed_file_path.as_deref()?;
        let profile_path = HookProfile::profile_path(&root.canonicalize().ok()?);
        let expected_content = generate_hook_autoload_content(&profile, &profile_path).ok()?;
        if profile.nu_executable.as_deref() != Some(&paths.nu_executable)
            || profile.nu_executable_sha256.as_deref() != Some(&paths.nu_executable_hash)
            || profile.nu_version.as_deref() != Some(&paths.nu_version)
            || std::fs::read_to_string(managed_path).ok()? != expected_content
        {
            return None;
        }
        Some(profile)
    });

    if lockfile.is_empty() {
        writeln!(out, "No packages installed.")?;
        return Ok(());
    }

    writeln!(out, "Installed packages ({}):\n", lockfile.packages.len())?;

    for (id, entry) in &lockfile.packages {
        let status = match &nu_paths {
            Some(p)
                if active_hook_packages.as_ref().is_some_and(|profile| {
                    entry.hook_activation.as_ref().is_some_and(|activation| {
                        let profile_ids: BTreeSet<&str> = profile
                            .bindings
                            .iter()
                            .filter(|binding| binding.package_id == *id)
                            .map(|binding| binding.id.as_str())
                            .collect();
                        let activation_ids: BTreeSet<&str> =
                            activation.binding_ids.iter().map(String::as_str).collect();
                        !profile_ids.is_empty()
                            && profile_ids == activation_ids
                            && activation.nu_executable_sha256 == p.nu_executable_hash
                            && activation.nu_version == p.nu_version
                    })
                }) =>
            {
                "hooks enabled"
            }
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
    use crate::state::lockfile::{HookActivation, LockfileEntry, PluginActivation};

    fn base_entry(version: &str, package_type: &str) -> LockfileEntry {
        LockfileEntry {
            hook_activation: None,
            hook_bindings: None,
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
            overlay: None,
            script_activation: None,
            script_entry_points: None,
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

    #[test]
    fn hooks_enabled_requires_matching_profile_and_managed_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let vendor = root.join("vendor/autoload");
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::create_dir_all(root.join("nu_state")).unwrap();
        let nu_executable = root.join("nu");
        std::fs::write(&nu_executable, "fixture").unwrap();
        let paths = NuPaths {
            nu_executable: nu_executable.to_string_lossy().into_owned(),
            nu_version: "0.116.1".into(),
            plugin_registry_path: root
                .join("plugins/registry.msgpackz")
                .to_string_lossy()
                .into_owned(),
            nu_executable_hash: "hash".into(),
            platform: "test".into(),
            data_dir: None,
            vendor_autoload_dirs: vec![],
            vendor_autoload_dir: Some(vendor.to_string_lossy().into_owned()),
        };
        paths.save(root).unwrap();

        let profile = HookProfile {
            bindings: vec![crate::state::hook_profile::HookBindingRecord {
                id: "owner/pkg/prompt".into(),
                event: "pre_prompt".into(),
                handler: root
                    .join("package/prompt.nu")
                    .to_string_lossy()
                    .into_owned(),
                filter: None,
                package_id: "owner/pkg".into(),
                activated_at: "now".into(),
            }],
            nu_executable: Some(paths.nu_executable.clone()),
            nu_executable_sha256: Some(paths.nu_executable_hash.clone()),
            nu_version: Some(paths.nu_version.clone()),
            vendor_autoload_dir: Some(vendor.to_string_lossy().into_owned()),
            managed_file_path: Some(vendor.join("numan-hooks.nu").to_string_lossy().into_owned()),
        };
        profile.save(root).unwrap();
        std::fs::write(
            profile.managed_file_path.as_ref().unwrap(),
            generate_hook_autoload_content(
                &profile,
                &HookProfile::profile_path(&root.canonicalize().unwrap()),
            )
            .unwrap(),
        )
        .unwrap();

        let mut entry = base_entry("1.0.0", "plugin");
        entry.hook_activation = Some(HookActivation {
            binding_ids: vec!["owner/pkg/prompt".into()],
            nu_executable_sha256: "hash".into(),
            nu_version: "0.116.1".into(),
            activated_at: "now".into(),
        });
        let mut lock = Lockfile::empty();
        lock.packages.insert("owner/pkg".into(), entry);
        lock.save(root).unwrap();

        let mut out = Vec::new();
        execute_to(root, &mut out).unwrap();
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("[plugin]  hooks enabled"));

        std::fs::write(profile.managed_file_path.as_ref().unwrap(), "stale").unwrap();
        let mut out = Vec::new();
        execute_to(root, &mut out).unwrap();
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("[plugin]  installed"));
    }
}
