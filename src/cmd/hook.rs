use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::core::package::{HookBinding, ModuleImportMode};
use crate::nu::autoload::{
    replace_managed_file, resolve_entry, validate_candidate, write_candidate, CandidateRunner,
    NuCandidateRunner,
};
use crate::nu::hooks::{generate_hook_autoload_content, is_allowed_event};
use crate::nu::paths::NuPaths;
use crate::state::hook_profile::{HookBindingRecord, HookProfile};
use crate::state::lockfile::{HookActivation, Lockfile};
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
use crate::util::format_timestamp;
use crate::util::fs_safety::{acquire_mutation_lock, assert_managed_file_owned};
use crate::util::output::println;

const HOOK_AUTOLOAD_FILE: &str = "numan-hooks.nu";

#[derive(Subcommand, Debug)]
pub enum HookCommands {
    /// Enable declared hooks for installed packages
    Enable(HookEnableArgs),
    /// Disable declared hooks for packages, or all hooks when no IDs are given
    Disable(HookDisableArgs),
    /// Show active hook bindings
    List,
}

#[derive(Args, Debug)]
pub struct HookEnableArgs {
    /// Package IDs (owner/name); omit to enable all installed inactive hook packages
    pub packages: Vec<String>,
}

#[derive(Args, Debug)]
pub struct HookDisableArgs {
    /// Package IDs (owner/name); omit to disable all active hook packages
    pub packages: Vec<String>,
}

pub fn execute(command: HookCommands, root: &Path) -> Result<()> {
    match command {
        HookCommands::Enable(args) => enable(&args, root, None),
        HookCommands::Disable(args) => disable(&args, root, None),
        HookCommands::List => list(root),
    }
}

fn enable(args: &HookEnableArgs, root: &Path, runner: Option<&dyn CandidateRunner>) -> Result<()> {
    let _lock = acquire_mutation_lock(root)?;
    let nu_paths = NuPaths::load(root)?;
    nu_paths.validate_drift()?;
    if runner.is_none() {
        nu_paths.validate_vendor_drift_live()?;
    }
    let vendor_dir = nu_paths
        .vendor_autoload_dir
        .as_deref()
        .context("No safe Nushell vendor-autoload directory. Run `numan init --refresh` first.")?;
    let managed_path = Path::new(vendor_dir).join(HOOK_AUTOLOAD_FILE);
    if managed_path.exists() {
        assert_managed_file_owned(&managed_path)?;
    }

    let mut lockfile = Lockfile::load(root)?;
    let mut profile = HookProfile::load(root)?;
    validate_profile_identity(&profile, &nu_paths)?;
    let active_ids: BTreeSet<String> = profile
        .bindings
        .iter()
        .map(|binding| binding.package_id.clone())
        .collect();

    let requested = if args.packages.is_empty() {
        lockfile
            .packages
            .iter()
            .filter(|(id, entry)| entry.hook_bindings.is_some() && !active_ids.contains(*id))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>()
    } else {
        args.packages
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    if requested.is_empty() {
        if profile.bindings.is_empty() {
            println!("No inactive hook packages to enable.");
            return Ok(());
        }
        set_profile_identity(&mut profile, &nu_paths, &managed_path);
        commit_profile(
            root,
            &nu_paths,
            &profile,
            &mut lockfile,
            runner,
            SnapshotTrigger::Activate,
        )?;
        println!("Reconciled the generated hook integration.");
        return Ok(());
    }

    let mut active_binding_ids: BTreeSet<String> = profile
        .bindings
        .iter()
        .map(|binding| binding.id.clone())
        .collect();
    let mut targets = Vec::new();
    for package_id in &requested {
        let entry = lockfile
            .packages
            .get(package_id)
            .with_context(|| format!("Package '{package_id}' is not installed."))?;
        let declarations = entry.hook_bindings.as_ref().with_context(|| {
            format!("Package '{package_id}' has no nu-hook activation metadata.")
        })?;
        if !entry.locked_dependencies.is_empty() {
            bail!("Hook package '{package_id}' has unresolved dependencies; Numan will not enable its hooks.");
        }
        if active_ids.contains(package_id) {
            println!("✓ {package_id} hooks are already enabled (skipped)");
            continue;
        }
        validate_declarations(package_id, declarations, &active_binding_ids)?;
        let mut resolved = Vec::new();
        for binding in declarations {
            if !active_binding_ids.insert(binding.id.clone()) {
                bail!(
                    "Hook binding ID '{}' is already active or declared by another requested package.",
                    binding.id
                );
            }
            let handler = resolve_entry(
                root,
                &entry.payload_path,
                &binding.handler,
                ModuleImportMode::Module,
                &format!("{package_id}/{}", binding.id),
            )?;
            resolved.push(HookBindingRecord {
                id: binding.id.clone(),
                event: binding.event.clone(),
                handler: handler.absolute_path.to_string_lossy().into_owned(),
                filter: binding.filter.clone(),
                package_id: package_id.clone(),
                activated_at: format_timestamp(),
            });
        }
        targets.push((package_id.clone(), resolved));
    }
    if targets.is_empty() {
        if !profile.bindings.is_empty() {
            set_profile_identity(&mut profile, &nu_paths, &managed_path);
            commit_profile(
                root,
                &nu_paths,
                &profile,
                &mut lockfile,
                runner,
                SnapshotTrigger::Activate,
            )?;
            println!("Reconciled the generated hook integration.");
        }
        return Ok(());
    }

    let enabled_binding_count: usize = targets.iter().map(|(_, bindings)| bindings.len()).sum();
    let activated_at = format_timestamp();
    for (package_id, bindings) in targets {
        let ids = bindings.iter().map(|binding| binding.id.clone()).collect();
        profile.bindings.extend(bindings);
        let entry = lockfile
            .packages
            .get_mut(&package_id)
            .expect("validated above");
        entry.hook_activation = Some(HookActivation {
            binding_ids: ids,
            nu_executable_sha256: nu_paths.nu_executable_hash.clone(),
            nu_version: nu_paths.nu_version.clone(),
            activated_at: activated_at.clone(),
        });
    }
    profile
        .bindings
        .sort_by(|left, right| left.id.cmp(&right.id));
    set_profile_identity(&mut profile, &nu_paths, &managed_path);
    commit_profile(
        root,
        &nu_paths,
        &profile,
        &mut lockfile,
        runner,
        SnapshotTrigger::Activate,
    )?;
    println!(
        "Enabled {} hook binding(s). Hooks load in new Nushell sessions.",
        enabled_binding_count
    );
    Ok(())
}

fn disable(
    args: &HookDisableArgs,
    root: &Path,
    runner: Option<&dyn CandidateRunner>,
) -> Result<()> {
    let _lock = acquire_mutation_lock(root)?;
    let nu_paths = NuPaths::load(root)?;
    nu_paths.validate_drift()?;
    let mut lockfile = Lockfile::load(root)?;
    let mut profile = HookProfile::load(root)?;
    if profile.bindings.is_empty() {
        let stale_packages: Vec<String> = lockfile
            .packages
            .iter()
            .filter(|(_, entry)| entry.hook_activation.is_some())
            .map(|(package_id, _)| package_id.clone())
            .collect();
        let managed_path = profile
            .managed_file_path
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| {
                nu_paths
                    .vendor_autoload_dir
                    .as_deref()
                    .map(|dir| Path::new(dir).join(HOOK_AUTOLOAD_FILE))
            });
        let has_stale_file = managed_path.as_ref().is_some_and(|path| path.exists());
        if stale_packages.is_empty() && !has_stale_file {
            println!("No active hooks.");
            return Ok(());
        }
        for package_id in &stale_packages {
            if let Some(entry) = lockfile.packages.get_mut(package_id) {
                entry.hook_activation = None;
            }
        }
        if let Some(managed_path) = managed_path {
            if managed_path.exists() {
                assert_managed_file_owned(&managed_path)?;
            }
            set_profile_identity(&mut profile, &nu_paths, &managed_path);
            commit_profile(
                root,
                &nu_paths,
                &profile,
                &mut lockfile,
                runner,
                SnapshotTrigger::Deactivate,
            )?;
        } else {
            create_snapshot(
                root,
                SnapshotReason::PreMutation,
                SnapshotTrigger::Deactivate,
                None,
                None,
            )?;
            lockfile.save(root)?;
            HookProfile::delete(root)?;
        }
        if stale_packages.is_empty() {
            println!("Reconciled stale hook integration file.");
        } else {
            println!(
                "Reconciled {} stale hook activation record(s).",
                stale_packages.len()
            );
        }
        return Ok(());
    }

    let requested: BTreeSet<String> = args.packages.iter().cloned().collect();
    let disabled: BTreeSet<String> = if requested.is_empty() {
        profile
            .bindings
            .iter()
            .map(|binding| binding.package_id.clone())
            .collect()
    } else {
        requested
    };
    let existing: BTreeSet<String> = profile
        .bindings
        .iter()
        .map(|binding| binding.package_id.clone())
        .collect();
    if let Some(package_id) = disabled.difference(&existing).next() {
        bail!("Package '{package_id}' has no active Numan hook bindings.");
    }
    profile
        .bindings
        .retain(|binding| !disabled.contains(&binding.package_id));
    for package_id in &disabled {
        if let Some(entry) = lockfile.packages.get_mut(package_id) {
            entry.hook_activation = None;
        }
    }

    let managed_path = profile
        .managed_file_path
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| {
            nu_paths
                .vendor_autoload_dir
                .as_deref()
                .map(|dir| Path::new(dir).join(HOOK_AUTOLOAD_FILE))
        })
        .context("No recorded hook integration path; run `numan init --refresh` first.")?;
    if profile.nu_executable_sha256.is_none() {
        set_profile_identity(&mut profile, &nu_paths, &managed_path);
    }
    if managed_path.exists() {
        assert_managed_file_owned(&managed_path)?;
    }
    commit_profile(
        root,
        &nu_paths,
        &profile,
        &mut lockfile,
        runner,
        SnapshotTrigger::Deactivate,
    )?;
    println!(
        "Disabled hooks for {} package(s). Start a new Nushell session to apply.",
        disabled.len()
    );
    Ok(())
}

fn list(root: &Path) -> Result<()> {
    let mut out = std::io::stdout();
    list_to(root, &mut out)
}

pub(crate) fn list_to(root: &Path, out: &mut dyn std::io::Write) -> Result<()> {
    let profile = HookProfile::load(root)?;
    if profile.bindings.is_empty() {
        writeln!(out, "No active hooks.")?;
        return Ok(());
    }
    for binding in profile.bindings {
        writeln!(
            out,
            "{}\t{}\t{}\t{}",
            binding.package_id, binding.id, binding.event, binding.handler
        )?;
    }
    Ok(())
}

fn validate_profile_identity(profile: &HookProfile, nu_paths: &NuPaths) -> Result<()> {
    if !profile.bindings.is_empty()
        && (profile.nu_executable_sha256.as_deref() != Some(&nu_paths.nu_executable_hash)
            || profile.nu_version.as_deref() != Some(&nu_paths.nu_version))
    {
        bail!(
            "Active hook profile belongs to a different Nushell identity. Run `numan hook disable` before enabling hooks for this Nu version."
        );
    }
    Ok(())
}

fn set_profile_identity(profile: &mut HookProfile, nu_paths: &NuPaths, managed_path: &Path) {
    profile.nu_executable = Some(nu_paths.nu_executable.clone());
    profile.nu_executable_sha256 = Some(nu_paths.nu_executable_hash.clone());
    profile.nu_version = Some(nu_paths.nu_version.clone());
    profile.vendor_autoload_dir = nu_paths.vendor_autoload_dir.clone();
    profile.managed_file_path = Some(managed_path.to_string_lossy().into_owned());
}

fn validate_declarations(
    package_id: &str,
    declarations: &[HookBinding],
    active_ids: &BTreeSet<String>,
) -> Result<()> {
    if declarations.is_empty() {
        bail!("Hook package '{package_id}' declares no bindings.");
    }
    let mut package_ids = BTreeSet::new();
    for binding in declarations {
        if binding.id.trim().is_empty()
            || binding
                .id
                .chars()
                .any(|character| matches!(character, '/' | '\\'))
        {
            bail!(
                "Hook package '{package_id}' has an invalid binding ID '{}'.",
                binding.id
            );
        }
        if !package_ids.insert(binding.id.clone()) || active_ids.contains(&binding.id) {
            bail!(
                "Hook binding ID '{}' is already active or duplicated.",
                binding.id
            );
        }
        if !is_allowed_event(&binding.event) {
            bail!(
                "Hook '{}' declares unsupported event '{}'.",
                binding.id,
                binding.event
            );
        }
        if binding.handler.trim().is_empty() {
            bail!("Hook '{}' has an empty handler path.", binding.id);
        }
        if binding
            .filter
            .as_deref()
            .is_some_and(|filter| filter.trim().is_empty())
        {
            bail!("Hook '{}' has an empty filter.", binding.id);
        }
        if binding.event == "env_change" && binding.filter.is_none() {
            bail!(
                "env_change hook '{}' must set filter to the environment variable name it watches.",
                binding.id
            );
        }
    }
    Ok(())
}

fn commit_profile(
    root: &Path,
    nu_paths: &NuPaths,
    profile: &HookProfile,
    lockfile: &mut Lockfile,
    runner: Option<&dyn CandidateRunner>,
    trigger: SnapshotTrigger,
) -> Result<()> {
    let managed_path = PathBuf::from(
        profile
            .managed_file_path
            .as_deref()
            .context("Missing managed hook path")?,
    );
    let profile_path = HookProfile::profile_path(&root.canonicalize()?);
    let content = generate_hook_autoload_content(profile, &profile_path)?;
    // Snapshot before creating the temporary candidate so a snapshot failure
    // cannot strand a candidate file in the vendor-autoload directory.
    create_snapshot(root, SnapshotReason::PreMutation, trigger, None, None)?;
    let candidate = write_candidate(&managed_path, &content)?;
    let real_runner;
    let runner = if let Some(runner) = runner {
        runner
    } else {
        real_runner = NuCandidateRunner::new(&nu_paths.nu_executable);
        &real_runner
    };
    let packages: Vec<&str> = profile
        .bindings
        .iter()
        .map(|binding| binding.package_id.as_str())
        .collect();
    validate_candidate(&candidate, runner, &packages)?;

    // Lockfile is a projection of the profile. Save it first; the owned Nu file
    // checks exact profile equality and stays inert during any interrupted gap.
    lockfile.save(root)?;
    profile.save(root)?;
    replace_managed_file(&managed_path, &candidate)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::lockfile::LockfileEntry;
    use anyhow::Result;
    use std::collections::BTreeMap;

    struct AcceptRunner;

    impl CandidateRunner for AcceptRunner {
        fn run(&self, candidate: &Path) -> Result<()> {
            assert!(std::fs::read_to_string(candidate)?
                .starts_with(crate::util::fs_safety::OWNERSHIP_MARKER));
            Ok(())
        }
    }

    #[test]
    fn hook_declarations_require_unique_supported_ids_and_event_selectors() {
        let base = HookBinding {
            id: "pre-prompt".into(),
            event: "pre_prompt".into(),
            handler: "hooks/pre_prompt.nu".into(),
            filter: None,
        };
        validate_declarations("owner/tool", &[base.clone()], &BTreeSet::new()).unwrap();
        assert!(validate_declarations(
            "owner/tool",
            &[base.clone(), base.clone()],
            &BTreeSet::new()
        )
        .is_err());
        let mut env_change = base.clone();
        env_change.event = "env_change".into();
        assert!(validate_declarations("owner/tool", &[env_change], &BTreeSet::new()).is_err());
        let mut unknown = base;
        unknown.event = "command_not_found".into();
        assert!(validate_declarations("owner/tool", &[unknown], &BTreeSet::new()).is_err());
    }

    #[test]
    fn enable_targets_are_based_on_persisted_hook_metadata() {
        let mut entry = LockfileEntry {
            hook_activation: None,
            hook_bindings: Some(vec![HookBinding {
                id: "before-prompt".into(),
                event: "pre_prompt".into(),
                handler: "hooks/pre_prompt.nu".into(),
                filter: None,
            }]),
            ..test_entry()
        };
        assert!(entry.hook_bindings.take().is_some());
        assert!(entry.hook_activation.is_none());
    }

    #[test]
    fn enable_and_disable_persist_profile_and_owned_integration() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let nu_state = root.join("nu_state");
        let vendor = root.join("vendor/autoload");
        let plugin_dir = root.join("plugins");
        let payload = root.join("packages/modules/owner/tool");
        std::fs::create_dir_all(&nu_state).unwrap();
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::create_dir_all(payload.join("hooks")).unwrap();
        std::fs::write(payload.join("hooks/pre_prompt.nu"), "print 'ran'\n").unwrap();
        let fake_nu = root.join("nu.exe");
        std::fs::write(&fake_nu, "fixture nu binary").unwrap();
        let nu_hash = crate::core::integrity::compute_sha256(&std::fs::read(&fake_nu).unwrap());
        NuPaths {
            nu_executable: fake_nu.to_string_lossy().into_owned(),
            nu_version: "0.116.1".into(),
            plugin_registry_path: plugin_dir
                .join("registry.msgpackz")
                .to_string_lossy()
                .into_owned(),
            nu_executable_hash: nu_hash,
            platform: "test".into(),
            data_dir: Some(root.join("nu-data").to_string_lossy().into_owned()),
            vendor_autoload_dirs: vec![vendor.to_string_lossy().into_owned()],
            vendor_autoload_dir: Some(vendor.to_string_lossy().into_owned()),
        }
        .save(root)
        .unwrap();

        let mut lockfile = Lockfile::empty();
        let mut entry = test_entry();
        entry.payload_path = "packages/modules/owner/tool".into();
        entry.hook_bindings = Some(vec![HookBinding {
            id: "prompt".into(),
            event: "pre_prompt".into(),
            handler: "hooks/pre_prompt.nu".into(),
            filter: None,
        }]);
        lockfile.packages.insert("owner/tool".into(), entry);
        lockfile.save(root).unwrap();

        enable(
            &HookEnableArgs {
                packages: vec!["owner/tool".into(), "owner/tool".into()],
            },
            root,
            Some(&AcceptRunner),
        )
        .unwrap();
        let enabled = HookProfile::load(root).unwrap();
        assert_eq!(enabled.bindings.len(), 1);
        assert_eq!(enabled.bindings[0].event, "pre_prompt");
        assert!(enabled.bindings[0]
            .handler
            .replace('\\', "/")
            .ends_with("hooks/pre_prompt.nu"));
        assert!(Lockfile::load(root).unwrap().packages["owner/tool"]
            .hook_activation
            .is_some());
        let managed = vendor.join(HOOK_AUTOLOAD_FILE);
        assert!(std::fs::read_to_string(&managed)
            .unwrap()
            .contains("source r#'"));
        assert_eq!(
            HookProfile::load(root).unwrap().bindings.len(),
            1,
            "duplicate package arguments must not duplicate callbacks"
        );

        disable(
            &HookDisableArgs {
                packages: vec!["owner/tool".into()],
            },
            root,
            Some(&AcceptRunner),
        )
        .unwrap();
        assert!(HookProfile::load(root).unwrap().bindings.is_empty());
        assert!(Lockfile::load(root).unwrap().packages["owner/tool"]
            .hook_activation
            .is_none());
        assert!(!std::fs::read_to_string(managed)
            .unwrap()
            .contains("hooks/pre_prompt.nu"));
    }

    #[test]
    fn snapshot_failure_does_not_leave_candidate_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let vendor = root.join("vendor/autoload");
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::create_dir_all(root.join("nu_state")).unwrap();
        std::fs::write(HookProfile::profile_path(root), "invalid json").unwrap();
        let nu_executable = root.join("nu.exe");
        std::fs::write(&nu_executable, "fixture").unwrap();
        let nu_paths = NuPaths {
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
        let profile = HookProfile {
            managed_file_path: Some(
                vendor
                    .join(HOOK_AUTOLOAD_FILE)
                    .to_string_lossy()
                    .into_owned(),
            ),
            ..HookProfile::default()
        };
        let mut lockfile = Lockfile::empty();
        assert!(commit_profile(
            root,
            &nu_paths,
            &profile,
            &mut lockfile,
            Some(&AcceptRunner),
            SnapshotTrigger::Activate,
        )
        .is_err());
        assert!(std::fs::read_dir(vendor).unwrap().next().is_none());
    }

    fn test_entry() -> LockfileEntry {
        LockfileEntry {
            version: "1.0.0".into(),
            package_type: "module".into(),
            source: "archive".into(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: None,
            archive_root: None,
            include: None,
            entry: None,
            installed_at: "now".into(),
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
            payload_path: "packages/modules/owner/tool".into(),
            revision_id: Some("r".into()),
            payload_sha256: None,
            executable_sha256: None,
            selection_reason: None,
            origin: None,
            module_activation: None,
            hook_activation: None,
            hook_bindings: None,
            module_import_mode: None,
            overlay: None,
            script_activation: None,
            script_entry_points: None,
            locked_dependencies: BTreeMap::new(),
        }
    }
}
