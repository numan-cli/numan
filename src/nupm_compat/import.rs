use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tempfile::TempDir;

use crate::core::integrity::compute_sha256;
use crate::core::package::{ModuleImportMode, ScopedId};
use crate::nu::autoload::{
    generate_autoload_content, resolve_entry, validate_candidate, CandidateRunner,
};
use crate::nupm_compat::classify::{classify_source_root, NupmCompatibility};
use crate::nupm_compat::discovery::{resolve_nupm_home, NupmHomeResolution};
use crate::nupm_compat::metadata::read_metadata_limited;
use crate::nupm_compat::schema::{
    MODULE_ENTRY, NUPM_IMPORT_ORIGIN, NUPM_IMPORT_SELECTION_REASON, NUPM_TRUST_LEVEL,
};
use crate::nupm_compat::walk::{
    check_module_tree_safe, check_path_chain_safe, check_path_chain_safe_within, find_package_root,
};
use crate::state::lifecycle_journal::{
    check_stale_journal, LifecycleOp, LifecycleStage, PendingLifecycle,
};
use crate::state::lockfile::{compute_revision_id, Lockfile, LockfileEntry};
use crate::state::nupm_import::{NupmImportRecord, NupmImportsFile};
use crate::util::atomic::write_bytes_atomic;
use crate::util::format_timestamp;
use crate::util::fs_safety::{acquire_mutation_lock, assert_not_symlink, MutationLock};
use crate::util::hints::{self, CMD_INIT, CMD_INIT_REFRESH, CMD_NUPM_INSPECT};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportResult {
    pub package_id: String,
    pub version: String,
    pub payload_path: String,
    pub revision_id: String,
    pub reimported: bool,
    pub skipped_unchanged: bool,
    pub old_revision_id: Option<String>,
    pub old_payload_path: Option<String>,
    pub old_source_payload_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportManifestResult {
    pub imports: Vec<ImportResult>,
}

#[derive(Debug, Clone, Deserialize)]
struct ImportManifestFile {
    imports: Vec<ManifestImportEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct ManifestImportEntry {
    source: String,
    #[serde(rename = "as")]
    target: String,
}

struct ResolvedImport {
    package_id: String,
    target: ScopedId,
    package_root: PathBuf,
    parsed_name: String,
    parsed_version: String,
    module_src: PathBuf,
    metadata_path: PathBuf,
    metadata_sha256: String,
    source_payload_sha256: String,
    reimported: bool,
    old_revision_id: Option<String>,
    old_payload_path: Option<String>,
    old_source_payload_sha256: Option<String>,
}

struct StagedImport {
    resolved: ResolvedImport,
    _staging: TempDir,
    staging_rel: String,
}

struct PromotedImport {
    resolved: ResolvedImport,
    payload_rel: String,
    revision_id: String,
}

pub fn import_module(
    root: &Path,
    source_path: &Path,
    target: &ScopedId,
    yes: bool,
) -> Result<ImportResult> {
    let nu_paths = crate::nu::paths::NuPaths::load(root).with_context(|| {
        format!(
            "Nu paths are not configured. {}",
            hints::run_then(CMD_INIT, CMD_INIT_REFRESH)
        )
    })?;
    let runner = crate::nu::autoload::NuCandidateRunner::new(&nu_paths.nu_executable);
    import_module_with_runner(root, source_path, target, yes, &runner)
}

pub fn import_module_with_runner(
    root: &Path,
    source_path: &Path,
    target: &ScopedId,
    yes: bool,
    runner: &dyn CandidateRunner,
) -> Result<ImportResult> {
    let _lock = acquire_mutation_lock(root)?;
    import_module_with_runner_under_lock(root, source_path, target, yes, runner, &_lock)
}

/// Import while the caller already holds the root mutation lock.
///
/// Mirrors `activate::execute_under_lock`: the caller keeps one lock across an
/// install→import→activate sequence (e.g. `numan migrate`) so nested
/// non-blocking [`acquire_mutation_lock`] does not self-deadlock.
pub fn import_module_with_runner_under_lock(
    root: &Path,
    source_path: &Path,
    target: &ScopedId,
    yes: bool,
    runner: &dyn CandidateRunner,
    _lock: &MutationLock,
) -> Result<ImportResult> {
    ensure_no_stale_journal(root)?;

    let resolved = resolve_single_import(root, source_path, target, yes)?;
    if resolved.reimported && yes {
        if let Some(ref old_hash) = resolved.old_source_payload_sha256 {
            if old_hash == &resolved.source_payload_sha256 {
                let payload_path = resolved
                    .old_payload_path
                    .clone()
                    .filter(|p| !p.is_empty())
                    .with_context(|| {
                        format!(
                            "Cannot skip unchanged re-import of '{}': lockfile entry is missing payload_path",
                            resolved.package_id
                        )
                    })?;
                let revision_id = resolved
                    .old_revision_id
                    .clone()
                    .filter(|r| !r.is_empty())
                    .with_context(|| {
                        format!(
                            "Cannot skip unchanged re-import of '{}': lockfile entry is missing revision_id",
                            resolved.package_id
                        )
                    })?;
                return Ok(ImportResult {
                    package_id: resolved.package_id.clone(),
                    version: resolved.parsed_version.clone(),
                    payload_path,
                    revision_id,
                    reimported: true,
                    skipped_unchanged: true,
                    old_revision_id: resolved.old_revision_id.clone(),
                    old_payload_path: resolved.old_payload_path.clone(),
                    old_source_payload_sha256: resolved.old_source_payload_sha256.clone(),
                });
            }
        }
    }

    let mut journal = begin_import_journal(
        root,
        LifecycleOp::NupmImport,
        &resolved.package_id,
        &[],
        &resolved.package_root,
        &resolved.metadata_sha256,
    )?;

    let staged = match stage_import(root, &resolved, runner) {
        Ok(s) => s,
        Err(e) => {
            let _ = PendingLifecycle::clear(root);
            return Err(e);
        }
    };
    journal.stage = LifecycleStage::PayloadsStaged;
    journal.staging_dir = Some(staged.staging_rel.clone());
    journal.save(root)?;

    let promoted = match promote_import(root, &staged) {
        Ok(p) => p,
        Err(e) => {
            cleanup_staging_dir(root, &staged.staging_rel);
            let _ = PendingLifecycle::clear(root);
            return Err(e);
        }
    };
    journal.stage = LifecycleStage::PayloadsPromoted;
    journal.staging_dir = None;
    journal.promoted_payload_path = Some(promoted.payload_rel.clone());
    journal.save(root)?;

    commit_imports(root, std::slice::from_ref(&promoted), &mut journal)?;
    Ok(ImportResult {
        package_id: resolved.package_id,
        version: resolved.parsed_version,
        payload_path: promoted.payload_rel,
        revision_id: promoted.revision_id,
        reimported: resolved.reimported,
        skipped_unchanged: false,
        old_revision_id: resolved.old_revision_id,
        old_payload_path: resolved.old_payload_path,
        old_source_payload_sha256: resolved.old_source_payload_sha256,
    })
}

pub fn import_manifest_with_runner(
    root: &Path,
    manifest_path: &Path,
    nupm_home: Option<&Path>,
    yes: bool,
    runner: &dyn CandidateRunner,
) -> Result<ImportManifestResult> {
    let _lock = acquire_mutation_lock(root)?;
    ensure_no_stale_journal(root)?;

    let home = match resolve_nupm_home(nupm_home)? {
        NupmHomeResolution::Found(p) => p,
        NupmHomeResolution::NotConfigured => {
            bail!(
                "No nupm home was supplied.\n\n\
                 Pass --nupm-home <path> or set NUPM_HOME for manifest import.\n\
                 Numan will not guess nupm's installation location."
            );
        }
    };

    let manifest_text = std::fs::read_to_string(manifest_path).with_context(|| {
        format!(
            "Failed to read import manifest '{}'",
            manifest_path.display()
        )
    })?;
    let manifest: ImportManifestFile = toml::from_str(&manifest_text)
        .with_context(|| format!("Invalid import manifest '{}'", manifest_path.display()))?;
    if manifest.imports.is_empty() {
        bail!("Import manifest contains no [[imports]] entries.");
    }

    let mut resolved_list = Vec::with_capacity(manifest.imports.len());
    for entry in &manifest.imports {
        let source = home.join(&entry.source);
        let target = ScopedId::parse(&entry.target)?;
        let resolved = resolve_single_import(root, &source, &target, yes)?;
        resolved_list.push(resolved);
    }

    let package_ids: Vec<String> = resolved_list.iter().map(|r| r.package_id.clone()).collect();
    let batch_label = package_ids.join(",");

    let mut journal = begin_import_journal(
        root,
        LifecycleOp::NupmImportManifest,
        &batch_label,
        &package_ids,
        &home,
        "",
    )?;

    let mut staged_list = Vec::new();
    for resolved in resolved_list {
        match stage_import(root, &resolved, runner) {
            Ok(staged) => {
                journal.batch_staging_dirs.push(staged.staging_rel.clone());
                staged_list.push(staged);
            }
            Err(e) => {
                for s in &staged_list {
                    cleanup_staging_dir(root, &s.staging_rel);
                }
                let _ = PendingLifecycle::clear(root);
                return Err(e);
            }
        }
    }
    journal.stage = LifecycleStage::PayloadsStaged;
    journal.save(root)?;

    let mut promoted_list = Vec::new();
    for staged in staged_list {
        match promote_import(root, &staged) {
            Ok(promoted) => promoted_list.push(promoted),
            Err(e) => {
                for s in journal.batch_staging_dirs.iter() {
                    cleanup_staging_dir(root, s);
                }
                let _ = PendingLifecycle::clear(root);
                return Err(e);
            }
        }
    }
    journal.stage = LifecycleStage::PayloadsPromoted;
    journal.batch_staging_dirs.clear();
    journal.save(root)?;

    commit_imports(root, &promoted_list, &mut journal)?;
    let mut imports = Vec::new();
    for promoted in promoted_list {
        let resolved = promoted.resolved;
        imports.push(ImportResult {
            package_id: resolved.package_id,
            version: resolved.parsed_version,
            payload_path: promoted.payload_rel,
            revision_id: promoted.revision_id,
            reimported: resolved.reimported,
            skipped_unchanged: false,
            old_revision_id: resolved.old_revision_id,
            old_payload_path: resolved.old_payload_path,
            old_source_payload_sha256: resolved.old_source_payload_sha256,
        });
    }
    Ok(ImportManifestResult { imports })
}

fn ensure_no_stale_journal(root: &Path) -> Result<()> {
    if let Some(journal) = check_stale_journal(root)? {
        let op = lifecycle_op_label(&journal.op);
        bail!(
            "A previous '{op}' operation on '{}' was interrupted.\n\
             Complete or clear the lifecycle journal, then retry.",
            journal.package_id
        );
    }
    Ok(())
}

fn begin_import_journal(
    root: &Path,
    op: LifecycleOp,
    package_id: &str,
    batch_package_ids: &[String],
    nupm_source_path: &Path,
    metadata_sha256: &str,
) -> Result<PendingLifecycle> {
    let journal = PendingLifecycle {
        op,
        package_id: package_id.to_string(),
        stage: LifecycleStage::Prepared,
        orphan_payload_path: None,
        from_version: None,
        to_version: None,
        nupm_source_path: Some(nupm_source_path.display().to_string()),
        nupm_metadata_sha256: if metadata_sha256.is_empty() {
            None
        } else {
            Some(metadata_sha256.to_string())
        },
        staging_dir: None,
        promoted_payload_path: None,
        batch_package_ids: batch_package_ids.to_vec(),
        batch_staging_dirs: Vec::new(),
        target_snapshot_id: None,
        pre_rollback_snapshot_id: None,
        needs_reactivate: false,
    };
    journal.save(root)?;
    Ok(journal)
}

fn resolve_single_import(
    root: &Path,
    source_path: &Path,
    target: &ScopedId,
    yes: bool,
) -> Result<ResolvedImport> {
    let package_root = resolve_package_root(source_path)?;
    let (compat, parsed_opt) = classify_source_root(&package_root)?;
    if compat != NupmCompatibility::ImportableModule {
        if compat == NupmCompatibility::UnsafeFilesystemLayout {
            if let Some(ref p) = parsed_opt {
                let module_src = package_root.join(&p.name);
                check_module_tree_safe(&module_src)?;
                check_path_chain_safe_within(&package_root, &module_src)?;
            }
            check_path_chain_safe(&package_root)?;
        }
        bail!(
            "Package at '{}' is not import-eligible ({compat:?}). {}",
            package_root.display(),
            hints::run(CMD_NUPM_INSPECT)
        );
    }
    let parsed = parsed_opt.with_context(|| {
        format!(
            "No supported metadata found for '{}'",
            package_root.display()
        )
    })?;

    let package_id = target.to_string();
    let lockfile = Lockfile::load(root)?;
    let mut reimported = false;
    let mut old_revision_id = None;
    let mut old_payload_path = None;
    let mut old_source_payload_sha256 = None;

    if let Some(existing) = lockfile.packages.get(&package_id) {
        if existing.origin.as_deref() != Some(NUPM_IMPORT_ORIGIN) {
            bail!(
                "Cannot import as '{package_id}': package is already installed from {}.\n\
                 {}",
                existing.origin.as_deref().unwrap_or("registry"),
                hints::run(&hints::remove_pkg(&package_id))
            );
        }
        reimported = true;
        old_revision_id = existing.revision_id.clone();
        old_payload_path = Some(existing.payload_path.clone());
        if !yes {
            bail!("Re-import of '{package_id}' requires --yes.");
        }
        let imports = NupmImportsFile::load(root)?;
        if let Some(record) = imports.imports.get(&package_id) {
            old_source_payload_sha256 = Some(record.source_payload_sha256.clone());
        }
    } else if !yes {
        print_consent(&package_root, target, &parsed.name, &parsed.version);
        bail!("Aborted. Pass --yes to confirm import.");
    }

    let module_src = package_root.join(&parsed.name);
    check_module_tree_safe(&module_src)?;

    let metadata_path = package_root.join(crate::nupm_compat::schema::METADATA_FILENAME);
    let metadata_bytes = read_metadata_limited(&metadata_path)?;
    let metadata_sha256 = compute_sha256(&metadata_bytes);

    let source_payload_sha256 = compute_revision_id(&module_src)
        .with_context(|| format!("Failed to hash source payload at {}", module_src.display()))?;

    Ok(ResolvedImport {
        package_id,
        target: target.clone(),
        package_root,
        parsed_name: parsed.name,
        parsed_version: parsed.version,
        module_src,
        metadata_path,
        metadata_sha256,
        source_payload_sha256,
        reimported,
        old_revision_id,
        old_payload_path,
        old_source_payload_sha256,
    })
}

fn stage_import(
    root: &Path,
    resolved: &ResolvedImport,
    runner: &dyn CandidateRunner,
) -> Result<StagedImport> {
    let parent_dir = root
        .join("packages")
        .join("modules")
        .join(&resolved.target.owner)
        .join(&resolved.target.name);
    std::fs::create_dir_all(&parent_dir).with_context(|| {
        format!(
            "Failed to create package directory '{}'",
            parent_dir.display()
        )
    })?;

    let staging =
        tempfile::tempdir_in(&parent_dir).context("Failed to create import staging dir")?;
    copy_module_payload(&resolved.module_src, staging.path())?;

    let staging_rel = rel_path_from_root(root, staging.path())?;
    validate_staged_module(
        root,
        &staging_rel,
        &resolved.package_id,
        staging.path(),
        runner,
    )?;

    Ok(StagedImport {
        resolved: resolved.clone_for_stage(),
        _staging: staging,
        staging_rel,
    })
}

fn promote_import(root: &Path, staged: &StagedImport) -> Result<PromotedImport> {
    let resolved = &staged.resolved;
    let revision_id =
        compute_revision_id(root.join(&staged.staging_rel).as_path()).with_context(|| {
            format!(
                "Failed to compute revision id for staged payload at {}",
                staged.staging_rel
            )
        })?;
    let sha_prefix = revision_id[..8.min(revision_id.len())].to_string();
    let version_dir = format!("{}-{sha_prefix}", resolved.parsed_version);
    let parent_dir = root
        .join("packages")
        .join("modules")
        .join(&resolved.target.owner)
        .join(&resolved.target.name);
    let install_dir = parent_dir.join(&version_dir);
    if install_dir.exists() {
        std::fs::remove_dir_all(&install_dir).with_context(|| {
            format!(
                "Failed to remove existing payload at {}",
                install_dir.display()
            )
        })?;
    }
    std::fs::rename(root.join(&staged.staging_rel), &install_dir).with_context(|| {
        format!(
            "Failed to promote staged payload to {}",
            install_dir.display()
        )
    })?;

    let payload_rel = rel_path_from_root(root, &install_dir)?;
    Ok(PromotedImport {
        resolved: resolved.clone_for_stage(),
        payload_rel,
        revision_id,
    })
}

fn commit_imports(
    root: &Path,
    promoted: &[PromotedImport],
    journal: &mut PendingLifecycle,
) -> Result<()> {
    let mut lockfile = Lockfile::load(root)?;
    crate::state::snapshot::create_snapshot(
        root,
        crate::state::snapshot::SnapshotReason::PreMutation,
        match journal.op {
            LifecycleOp::NupmImportManifest => {
                crate::state::snapshot::SnapshotTrigger::NupmImportManifest
            }
            _ => crate::state::snapshot::SnapshotTrigger::NupmImport,
        },
        None,
        None,
    )?;

    let installed_at = format_timestamp();
    let mut imports = NupmImportsFile::load(root)?;

    for item in promoted {
        let resolved = &item.resolved;
        let entry = LockfileEntry {
            hook_activation: None,
            hook_bindings: None,
            version: resolved.parsed_version.clone(),
            package_type: "module".to_string(),
            source: "nupm".to_string(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: None,
            archive_root: None,
            include: None,
            entry: Some(MODULE_ENTRY.to_string()),
            installed_at: installed_at.clone(),
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
            payload_path: item.payload_rel.clone(),
            revision_id: Some(item.revision_id.clone()),
            payload_sha256: Some(item.revision_id.clone()),
            executable_sha256: None,
            selection_reason: Some(NUPM_IMPORT_SELECTION_REASON.to_string()),
            origin: Some(NUPM_IMPORT_ORIGIN.to_string()),
            module_activation: None,
            module_import_mode: Some(ModuleImportMode::Module),
            overlay: None,
            locked_dependencies: BTreeMap::new(),
        };
        lockfile.packages.insert(resolved.package_id.clone(), entry);

        imports.upsert(
            &resolved.package_id,
            NupmImportRecord {
                trust_level: NUPM_TRUST_LEVEL.to_string(),
                nupm_source_path: resolved.package_root.display().to_string(),
                nupm_metadata_path: resolved.metadata_path.display().to_string(),
                nupm_metadata_sha256: resolved.metadata_sha256.clone(),
                source_payload_sha256: resolved.source_payload_sha256.clone(),
                imported_payload_sha256: item.revision_id.clone(),
                observed_git_remote: None,
                observed_git_commit: None,
                imported_at: installed_at.clone(),
            },
        );
    }

    lockfile.generated_at = installed_at;
    lockfile.save(root)?;
    imports.save(root)?;

    journal.stage = LifecycleStage::SelectionCommitted;
    journal.save(root)?;
    PendingLifecycle::clear(root)?;
    Ok(())
}

impl ResolvedImport {
    fn clone_for_stage(&self) -> Self {
        Self {
            package_id: self.package_id.clone(),
            target: self.target.clone(),
            package_root: self.package_root.clone(),
            parsed_name: self.parsed_name.clone(),
            parsed_version: self.parsed_version.clone(),
            module_src: self.module_src.clone(),
            metadata_path: self.metadata_path.clone(),
            metadata_sha256: self.metadata_sha256.clone(),
            source_payload_sha256: self.source_payload_sha256.clone(),
            reimported: self.reimported,
            old_revision_id: self.old_revision_id.clone(),
            old_payload_path: self.old_payload_path.clone(),
            old_source_payload_sha256: self.old_source_payload_sha256.clone(),
        }
    }
}

fn cleanup_staging_dir(root: &Path, staging_rel: &str) {
    let path = root.join(staging_rel);
    let _ = std::fs::remove_dir_all(path);
}

fn lifecycle_op_label(op: &LifecycleOp) -> &'static str {
    match op {
        LifecycleOp::Update => "update",
        LifecycleOp::Remove => "remove",
        LifecycleOp::NupmImport => "nupm import",
        LifecycleOp::NupmImportManifest => "nupm manifest import",
        LifecycleOp::Rollback => "snapshot rollback",
    }
}

fn resolve_package_root(source_path: &Path) -> Result<PathBuf> {
    find_package_root(source_path)?
        .with_context(|| format!("No nupm.nuon found for path '{}'", source_path.display()))
}

fn rel_path_from_root(root: &Path, path: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(root)
        .with_context(|| {
            format!(
                "Path '{}' is not under numan root '{}'",
                path.display(),
                root.display()
            )
        })?
        .to_string_lossy()
        .replace('\\', "/"))
}

fn print_consent(package_root: &Path, target: &ScopedId, declared_name: &str, version: &str) {
    eprintln!("Local nupm module import\n");
    eprintln!("  Source path:       {}", package_root.display());
    eprintln!("  Target Numan ID:   {target}");
    eprintln!("  Declared name:     {declared_name}");
    eprintln!("  Version:           {version}");
    eprintln!("  Package type:      module");
    eprintln!("  Entry point:       {declared_name}/{MODULE_ENTRY}");
    eprintln!("  Trust level:       {NUPM_TRUST_LEVEL}");
    eprintln!("  Build scripts:     not executed");
    eprintln!("  Activation:        not performed");
    eprintln!();
}

pub fn copy_module_payload(source_module_dir: &Path, dest_dir: &Path) -> Result<()> {
    check_module_tree_safe(source_module_dir)?;
    std::fs::create_dir_all(dest_dir).with_context(|| {
        format!(
            "Failed to create import destination '{}'",
            dest_dir.display()
        )
    })?;
    copy_regular_tree(source_module_dir, dest_dir)
}

fn copy_regular_tree(from: &Path, to: &Path) -> Result<()> {
    let mut stack = vec![(from.to_path_buf(), to.to_path_buf())];
    while let Some((src_dir, dst_dir)) = stack.pop() {
        for entry in std::fs::read_dir(&src_dir).with_context(|| {
            format!(
                "Failed to read module tree directory '{}'",
                src_dir.display()
            )
        })? {
            let entry = entry.with_context(|| {
                format!(
                    "Failed to read module tree entry under '{}'",
                    src_dir.display()
                )
            })?;
            let file_type = entry.file_type().with_context(|| {
                format!("Failed to read file type for '{}'", entry.path().display())
            })?;
            let name = entry.file_name();
            let src_path = src_dir.join(&name);
            let dst_path = dst_dir.join(&name);
            if file_type.is_dir() {
                std::fs::create_dir_all(&dst_path)?;
                stack.push((src_path, dst_path));
            } else if file_type.is_file() {
                assert_not_symlink(&src_path, "import source file")?;
                if let Some(parent) = dst_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&src_path, &dst_path).with_context(|| {
                    format!(
                        "Failed to copy '{}' to '{}'",
                        src_path.display(),
                        dst_path.display()
                    )
                })?;
            } else {
                bail!(
                    "Unsafe filesystem layout: module tree '{}' contains non-regular file '{}'",
                    from.display(),
                    src_path.display()
                );
            }
        }
    }
    Ok(())
}

fn validate_staged_module(
    root: &Path,
    payload_rel: &str,
    package_id: &str,
    staging_dir: &Path,
    runner: &dyn CandidateRunner,
) -> Result<()> {
    let resolved = resolve_entry(
        root,
        payload_rel,
        MODULE_ENTRY,
        ModuleImportMode::Module,
        package_id,
    )?;
    let content = generate_autoload_content(&[resolved])?;
    let candidate = staging_dir.join(".numan-import-validate.candidate.nu");
    write_bytes_atomic(&candidate, content.as_bytes())?;
    let result = validate_candidate(&candidate, runner, &[package_id]);
    let _ = std::fs::remove_file(&candidate);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nu::autoload::FakeCandidateRunner;

    fn fixture(path: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nupm")
            .join(path)
    }

    #[test]
    fn copy_minimal_module_fixture() {
        let src = fixture("supported/minimal-module/minimal-module");
        let dir = tempfile::tempdir().unwrap();
        copy_module_payload(&src, dir.path()).unwrap();
        assert!(dir.path().join("mod.nu").is_file());
    }

    #[test]
    fn import_ineligible_package_rejected() {
        let root = tempfile::tempdir().unwrap();
        let source = fixture("rejected/script-type");
        let err = import_module_with_runner(
            root.path(),
            &source,
            &ScopedId::parse("o/n").unwrap(),
            true,
            &FakeCandidateRunner::success(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not import-eligible"));
    }

    #[test]
    fn import_requires_yes_for_new_package() {
        let root = tempfile::tempdir().unwrap();
        let source = fixture("supported/minimal-module");
        assert!(import_module_with_runner(
            root.path(),
            &source,
            &ScopedId::parse("o/n").unwrap(),
            false,
            &FakeCandidateRunner::success(),
        )
        .is_err());
    }

    #[test]
    fn import_success_writes_lockfile_and_provenance() {
        let root = tempfile::tempdir().unwrap();
        let source = fixture("supported/minimal-module");
        let result = import_module_with_runner(
            root.path(),
            &source,
            &ScopedId::parse("test/minimal").unwrap(),
            true,
            &FakeCandidateRunner::success(),
        )
        .unwrap();
        assert_eq!(result.package_id, "test/minimal");
        assert!(root
            .path()
            .join(&result.payload_path)
            .join("mod.nu")
            .is_file());
        let lockfile = Lockfile::load(root.path()).unwrap();
        let entry = lockfile.packages.get("test/minimal").unwrap();
        assert_eq!(entry.origin.as_deref(), Some(NUPM_IMPORT_ORIGIN));
        let imports = NupmImportsFile::load(root.path()).unwrap();
        assert!(imports.imports.contains_key("test/minimal"));
    }

    #[test]
    fn under_lock_variant_reuses_held_lock_without_self_deadlock() {
        // Regression test for the `numan migrate` deadlock: the command holds
        // the mutation lock for the whole migrate sequence, so nested imports
        // must go through the under-lock variant.
        let root = tempfile::tempdir().unwrap();
        let source = fixture("supported/minimal-module");
        let target = ScopedId::parse("test/underlock").unwrap();

        let lock = acquire_mutation_lock(root.path()).unwrap();

        // Re-acquiring the non-blocking lock while it is already held must fail.
        assert!(acquire_mutation_lock(root.path()).is_err());

        // The under-lock variant must succeed while the caller holds the lock.
        let result = import_module_with_runner_under_lock(
            root.path(),
            &source,
            &target,
            true,
            &FakeCandidateRunner::success(),
            &lock,
        )
        .unwrap();
        assert_eq!(result.package_id, "test/underlock");
        assert!(root
            .path()
            .join(&result.payload_path)
            .join("mod.nu")
            .is_file());
    }

    #[test]
    fn registry_collision_rejected() {
        let root = tempfile::tempdir().unwrap();
        let mut lockfile = Lockfile::empty();
        lockfile.packages.insert(
            "test/minimal".to_string(),
            LockfileEntry {
                version: "1.0.0".to_string(),
                package_type: "module".to_string(),
                source: "registry".to_string(),
                installed_at: "0".to_string(),
                payload_path: "packages/modules/test/minimal/1.0.0-abc".to_string(),
                origin: Some("registry:official".to_string()),
                ..default_entry()
            },
        );
        lockfile.save(root.path()).unwrap();

        let source = fixture("supported/minimal-module");
        let err = import_module_with_runner(
            root.path(),
            &source,
            &ScopedId::parse("test/minimal").unwrap(),
            true,
            &FakeCandidateRunner::success(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("already installed"));
    }

    #[cfg(unix)]
    #[test]
    fn import_rejects_symlink_ancestor_when_package_root_passed_directly() {
        use std::fs;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let real_pkg = dir.path().join("real_pkg");
        fs::create_dir_all(&real_pkg.join("minimal-module")).unwrap();
        fs::copy(
            fixture("supported/minimal-module/nupm.nuon"),
            real_pkg.join("nupm.nuon"),
        )
        .unwrap();
        fs::copy(
            fixture("supported/minimal-module/minimal-module/mod.nu"),
            real_pkg.join("minimal-module/mod.nu"),
        )
        .unwrap();
        let link = dir.path().join("link");
        symlink(&real_pkg, &link).unwrap();

        let root = tempfile::tempdir().unwrap();
        let err = import_module_with_runner(
            root.path(),
            &link,
            &ScopedId::parse("test/minimal").unwrap(),
            true,
            &FakeCandidateRunner::success(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("Unsafe filesystem layout"));
    }

    fn default_entry() -> LockfileEntry {
        LockfileEntry {
            hook_activation: None,
            hook_bindings: None,
            version: String::new(),
            package_type: String::new(),
            source: String::new(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: None,
            archive_root: None,
            include: None,
            entry: None,
            installed_at: String::new(),
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
            locked_dependencies: BTreeMap::new(),
        }
    }
}
