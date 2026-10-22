//! Explicit session and project activation for Nu overlay packages.

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use std::path::{Path, PathBuf};

use crate::core::package::ModuleImportMode;
use crate::nu::autoload::resolve_entry;
use crate::state::lockfile::Lockfile;
use crate::state::overlay_state::{OverlayRecord, OverlayScope, OverlayState};
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
use crate::util::format_timestamp;
use crate::util::fs_safety::acquire_mutation_lock;
use crate::util::output::{eprintln, println};

#[derive(Subcommand, Debug)]
pub enum OverlayCommands {
    /// Validate and record an overlay, then emit a caller-shell command.
    Use(OverlayUseArgs),
    /// Remove a Numan-owned activation record, then emit the caller-shell hide command.
    Hide(OverlayHideArgs),
    /// List overlays recorded by Numan.
    List,
}

#[derive(Args, Debug)]
#[command(after_help = "Copy the emitted Nushell statement into the caller shell to activate it.")]
pub struct OverlayUseArgs {
    /// Installed package ID (owner/name).
    pub package: String,
    /// Record this activation as project-scoped under the given root.
    #[arg(long, value_name = "PATH")]
    pub project_root: Option<PathBuf>,
}

#[derive(Args, Debug)]
#[command(after_help = "Copy the emitted Nushell statement into the caller shell to hide it.")]
pub struct OverlayHideArgs {
    /// Overlay name or installed package ID.
    pub target: String,
}

pub fn execute(command: OverlayCommands, root: &Path) -> Result<()> {
    match command {
        OverlayCommands::Use(args) => use_overlay(&args, root),
        OverlayCommands::Hide(args) => hide_overlay(&args, root),
        OverlayCommands::List => list_overlays(root),
    }
}

fn use_overlay(args: &OverlayUseArgs, root: &Path) -> Result<()> {
    let _lock = acquire_mutation_lock(root)?;
    let lockfile = Lockfile::load(root)?;
    let package = lockfile
        .packages
        .get(&args.package)
        .with_context(|| format!("Package '{}' is not installed", args.package))?;
    let spec = package
        .overlay
        .as_ref()
        .with_context(|| format!("Package '{}' is not an overlay package", args.package))?;
    if package.package_type != "module" {
        bail!("Overlay package '{}' must have type 'module'", args.package);
    }
    if spec.name.trim().is_empty() {
        bail!(
            "Overlay package '{}' has an empty overlay name",
            args.package
        );
    }
    if package.module_activation.is_some() {
        bail!(
            "Package '{}' is already persistently active as a module; deactivate it before using it as an overlay",
            args.package
        );
    }
    if !package.locked_dependencies.is_empty() {
        bail!(
            "Overlay package '{}' has dependencies that Numan cannot resolve yet",
            args.package
        );
    }
    let entry = package
        .entry
        .as_deref()
        .context("Overlay package is missing its entry path")?;
    let resolved = resolve_entry(
        root,
        &package.payload_path,
        entry,
        ModuleImportMode::Module,
        &args.package,
    )?;

    let (scope, project_root) = match &args.project_root {
        Some(path) => {
            let canonical = path
                .canonicalize()
                .with_context(|| format!("Failed to resolve project root '{}'", path.display()))?;
            if !canonical.is_dir() {
                bail!("Project root '{}' is not a directory", canonical.display());
            }
            (
                OverlayScope::Project,
                Some(canonical.to_string_lossy().into_owned()),
            )
        }
        None => (OverlayScope::Session, None),
    };

    let entry_path = resolved.absolute_path.to_string_lossy().into_owned();
    let mut state = OverlayState::load(root)?;
    if let Some(existing) = state
        .overlays
        .iter()
        .find(|record| record.package_id == args.package)
    {
        if existing.entry_path != entry_path
            || existing.overlay_name != spec.name
            || existing.scope != scope
            || existing.project_root != project_root
        {
            bail!(
                "Overlay package '{}' is already recorded as '{}' at {}; run `numan overlay hide {}` before using it again",
                args.package,
                existing.overlay_name,
                existing.entry_path,
                args.package
            );
        }
        eprintln!("Overlay '{}' is already recorded as active.", spec.name);
        println!("{}", nu_use_command(&resolved.absolute_path, &spec.name));
        return Ok(());
    }
    let requested_root = project_root.as_deref();
    let mut active_exports: Vec<String> = Vec::new();
    for record in state.overlays.iter().filter(|record| match record.scope {
        OverlayScope::Session => true,
        OverlayScope::Project => {
            requested_root.is_none_or(|root| record.project_root.as_deref() == Some(root))
        }
    }) {
        let active = lockfile
            .packages
            .get(&record.package_id)
            .and_then(|entry| entry.overlay.as_ref())
            .with_context(|| {
                format!(
                    "Recorded overlay '{}' ({}) cannot be checked for export conflicts; run `numan overlay hide {}` first",
                    record.overlay_name, record.package_id, record.package_id
                )
            })?;
        active_exports.extend(active.exports.iter().cloned());
    }
    if let Some(conflict) = spec
        .exports
        .iter()
        .find(|export| active_exports.contains(export))
    {
        bail!(
            "Overlay '{}' exports '{}' which conflicts with an active overlay",
            spec.name,
            conflict
        );
    }
    if state
        .overlays
        .iter()
        .any(|record| record.overlay_name == spec.name)
    {
        bail!("Overlay name '{}' is already active", spec.name);
    }

    create_snapshot(
        root,
        SnapshotReason::PreMutation,
        SnapshotTrigger::Activate,
        None,
        None,
    )?;
    state.overlays.push(OverlayRecord {
        package_id: args.package.clone(),
        overlay_name: spec.name.clone(),
        entry_path,
        scope,
        project_root,
        activated_at: format_timestamp(),
    });
    state.save(root)?;

    eprintln!(
        "Recorded overlay '{}' metadata. Run the emitted statement in the current Nushell shell.",
        spec.name
    );
    println!("{}", nu_use_command(&resolved.absolute_path, &spec.name));
    Ok(())
}

fn hide_overlay(args: &OverlayHideArgs, root: &Path) -> Result<()> {
    let _lock = acquire_mutation_lock(root)?;
    let mut state = OverlayState::load(root)?;
    let matches: Vec<usize> = state
        .overlays
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            record.package_id == args.target || record.overlay_name == args.target
        })
        .map(|(index, _)| index)
        .collect();
    if matches.is_empty() {
        bail!(
            "No Numan-owned overlay '{}' is recorded as active",
            args.target
        );
    }
    if matches.len() > 1 {
        bail!(
            "Overlay target '{}' is ambiguous; use a package ID",
            args.target
        );
    }

    create_snapshot(
        root,
        SnapshotReason::PreMutation,
        SnapshotTrigger::Deactivate,
        None,
        None,
    )?;
    let record = state.overlays.remove(matches[0]);
    state.save(root)?;
    eprintln!(
        "Removed Numan's activation record for '{}'. Run the emitted statement in the current Nushell shell.",
        record.overlay_name
    );
    println!("overlay hide {}", nu_word(&record.overlay_name));
    Ok(())
}

fn list_overlays(root: &Path) -> Result<()> {
    let mut out = std::io::stdout();
    list_overlays_to(root, &mut out)
}

pub(crate) fn list_overlays_to(root: &Path, out: &mut dyn std::io::Write) -> Result<()> {
    let state = OverlayState::load(root)?;
    if state.overlays.is_empty() {
        writeln!(out, "No active overlays recorded.")?;
        return Ok(());
    }
    for record in state.overlays {
        let scope = match record.scope {
            OverlayScope::Session => "session".to_string(),
            OverlayScope::Project => {
                format!("project:{}", record.project_root.as_deref().unwrap_or("?"))
            }
        };
        writeln!(
            out,
            "{} ({}) [{}]",
            record.overlay_name, record.package_id, scope
        )?;
    }
    Ok(())
}

fn nu_use_command(path: &Path, name: &str) -> String {
    format!(
        "overlay use {} as {}",
        nu_word(&path.to_string_lossy()),
        nu_word(name)
    )
}

fn nu_word(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::overlay_state::OverlayState;
    use tempfile::TempDir;

    fn overlay_fixture() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let payload = root.join("packages/modules/owner/tool/1");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(
            payload.join("overlay.nu"),
            "export def hello [] { 'hello' }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("lockfile"),
            r#"{
              "version": 2,
              "generated_at": "",
              "nu_version": "",
              "platform": "",
              "packages": {
                "owner/tool": {
                  "version": "1.0.0",
                  "type": "module",
                  "source": "archive",
                  "installed_at": "now",
                  "payload_path": "packages/modules/owner/tool/1",
                  "entry": "overlay.nu",
                  "overlay": {"name": "tooling", "exports": ["hello"]}
                }
              }
            }"#,
        )
        .unwrap();
        (dir, root)
    }

    #[test]
    fn nu_command_quotes_paths_and_names() {
        assert_eq!(
            nu_use_command(
                Path::new(r#"C:\Numan Root\pkg\overlay.nu"#),
                "weather tools"
            ),
            r#"overlay use "C:\\Numan Root\\pkg\\overlay.nu" as "weather tools""#
        );
    }

    #[test]
    fn use_records_valid_overlay_and_creates_snapshot() {
        let (_dir, root) = overlay_fixture();
        use_overlay(
            &OverlayUseArgs {
                package: "owner/tool".to_string(),
                project_root: None,
            },
            &root,
        )
        .unwrap();

        let state = OverlayState::load(&root).unwrap();
        assert_eq!(state.overlays.len(), 1);
        assert_eq!(state.overlays[0].overlay_name, "tooling");
        assert_eq!(state.overlays[0].scope, OverlayScope::Session);
        assert!(root.join("state/snapshots").is_dir());
    }

    #[test]
    fn hide_removes_only_matching_record_and_creates_snapshot() {
        let (_dir, root) = overlay_fixture();
        use_overlay(
            &OverlayUseArgs {
                package: "owner/tool".to_string(),
                project_root: None,
            },
            &root,
        )
        .unwrap();
        hide_overlay(
            &OverlayHideArgs {
                target: "tooling".to_string(),
            },
            &root,
        )
        .unwrap();

        assert!(OverlayState::load(&root).unwrap().overlays.is_empty());
        let snapshot_count = std::fs::read_dir(root.join("state/snapshots"))
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_name() != ".staging")
            .count();
        assert_eq!(snapshot_count, 2);
    }

    #[test]
    fn use_rejects_conflicting_exports_without_mutation() {
        let (_dir, root) = overlay_fixture();
        let other_payload = root.join("packages/modules/other/package/1");
        std::fs::create_dir_all(&other_payload).unwrap();
        std::fs::write(
            other_payload.join("overlay.nu"),
            "export def hello [] { 'other' }\n",
        )
        .unwrap();
        let lock_path = root.join("lockfile");
        let mut lock_json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&lock_path).unwrap()).unwrap();
        lock_json["packages"]["other/package"] = serde_json::json!({
            "version": "1.0.0",
            "type": "module",
            "source": "archive",
            "installed_at": "now",
            "payload_path": "packages/modules/other/package/1",
            "entry": "overlay.nu",
            "overlay": {"name": "other", "exports": ["hello"]}
        });
        std::fs::write(&lock_path, serde_json::to_vec_pretty(&lock_json).unwrap()).unwrap();
        let mut state = OverlayState::default();
        state.overlays.push(OverlayRecord {
            package_id: "other/package".to_string(),
            overlay_name: "other".to_string(),
            entry_path: "ignored".to_string(),
            scope: OverlayScope::Session,
            project_root: None,
            activated_at: "now".to_string(),
        });
        state.save(&root).unwrap();
        let err = use_overlay(
            &OverlayUseArgs {
                package: "owner/tool".to_string(),
                project_root: None,
            },
            &root,
        )
        .unwrap_err();
        assert!(err.to_string().contains("conflicts with an active overlay"));
        assert_eq!(OverlayState::load(&root).unwrap().overlays.len(), 1);
    }

    #[test]
    fn use_refuses_when_mutation_lock_is_held() {
        let (_dir, root) = overlay_fixture();
        let _guard = crate::util::fs_safety::acquire_mutation_lock(&root).unwrap();
        let err = use_overlay(
            &OverlayUseArgs {
                package: "owner/tool".to_string(),
                project_root: None,
            },
            &root,
        )
        .unwrap_err();
        assert!(err.to_string().contains("mutation is already in progress"));
        assert!(OverlayState::load(&root).unwrap().overlays.is_empty());
    }

    #[test]
    fn use_refuses_stale_record_for_same_package() {
        let (_dir, root) = overlay_fixture();
        use_overlay(
            &OverlayUseArgs {
                package: "owner/tool".to_string(),
                project_root: None,
            },
            &root,
        )
        .unwrap();
        let mut state = OverlayState::load(&root).unwrap();
        state.overlays[0].overlay_name = "stale".to_string();
        state.save(&root).unwrap();

        let err = use_overlay(
            &OverlayUseArgs {
                package: "owner/tool".to_string(),
                project_root: None,
            },
            &root,
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("run `numan overlay hide owner/tool` before using it again"));
        assert_eq!(
            OverlayState::load(&root).unwrap().overlays[0].overlay_name,
            "stale"
        );
    }

    #[test]
    fn snapshot_rollback_restores_overlay_records() {
        let (_dir, root) = overlay_fixture();
        use_overlay(
            &OverlayUseArgs {
                package: "owner/tool".to_string(),
                project_root: None,
            },
            &root,
        )
        .unwrap();
        hide_overlay(
            &OverlayHideArgs {
                target: "tooling".to_string(),
            },
            &root,
        )
        .unwrap();
        let snapshot = crate::state::snapshot::list_snapshots(&root)
            .unwrap()
            .into_iter()
            .last()
            .unwrap();

        crate::state::rollback::rollback_to_snapshot(
            &root,
            &snapshot.id,
            &crate::nu::autoload::FakeCandidateRunner {
                should_succeed: true,
                error_msg: String::new(),
            },
        )
        .unwrap();
        let restored = OverlayState::load(&root).unwrap();
        assert_eq!(restored.overlays.len(), 1);
        assert_eq!(restored.overlays[0].overlay_name, "tooling");
    }
}
