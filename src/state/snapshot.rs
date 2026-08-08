//! Immutable activation snapshots for Numan-managed state.
//!
//! Snapshots are stored under `<root>/state/snapshots/<uuid-v7>/` and are
//! immutable after creation. Each snapshot captures the authoritative lockfile,
//! the managed module-autoload projection (including exact `numan.nu` content),
//! nupm-import provenance, and the Nu path cache (`nu_state/paths.json`).
//! Payloads are never duplicated; snapshots reference immutable payload
//! directories by path and a computed revision hash.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::core::integrity::compute_sha256;
use crate::nu::paths::NuPaths;
use crate::state::activation_profile::ActivationProfile;
use crate::state::autoload_journal::PendingAutoload;
#[cfg(test)]
use crate::state::autoload_journal::SCHEMA_VERSION as AUTOLOAD_SCHEMA_VERSION;
use crate::state::autoload_state::AutoloadState;
use crate::state::lifecycle_journal::PendingLifecycle;
#[cfg(test)]
use crate::state::lockfile::LockfileEntry;
use crate::state::lockfile::{compute_revision_id, Lockfile};
use crate::state::nupm_import::NupmImportsFile;
use crate::util::atomic::write_json_atomic;
use crate::util::fs_safety::is_symlink_or_reparse;

/// Schema version for `snapshot.json`.
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Snapshot manifest — the authoritative header for a committed snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub schema_version: u32,
    pub id: String,
    pub created_at: String,
    pub reason: SnapshotReason,
    pub trigger: SnapshotTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<SnapshotRelation>,
    pub numan_root: String,
    pub platform: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nu_identity: Option<SnapshotNuIdentity>,
    pub sidecar_digests: SidecarDigests,
    pub payload_revisions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotNuIdentity {
    pub nu_version: String,
    pub nu_executable_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotReason {
    PreMutation,
    PreRollback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotTrigger {
    Install,
    Update,
    Remove,
    Activate,
    Deactivate,
    NupmImport,
    NupmImportManifest,
    Rollback,
    /// `numan doctor` default repair pass.
    Doctor,
    /// `numan init --refresh` (paths / activation identity rewrite).
    Init,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotRelation {
    PreRollbackOf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarDigests {
    pub lockfile_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autoload_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imports_sha256: Option<String>,
    /// Digest of the `paths.json` sidecar (`SnapshotPaths`). `None` on legacy
    /// snapshots that predate paths capture; those must not mutate live paths
    /// on rollback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths_sha256: Option<String>,
    /// Digest of the `activation-profile.json` sidecar. `None` on legacy
    /// snapshots that predate activation-profile capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_profile_sha256: Option<String>,
}

/// Captured Nu path-cache sidecar (`nu_state/paths.json`).
///
/// Distinguished from "not captured" (legacy snapshots with no `paths.json`
/// sidecar): `Absent` means the root was uninitialized at snapshot time and
/// rollback should delete a later-created cache. `nu_identity` on the manifest
/// remains a convenience field (hash + version only); this sidecar is
/// authoritative for cache restore.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotPaths {
    Absent,
    Present(NuPaths),
}

/// Captured module-autoload projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotAutoload {
    pub schema_version: u32,
    pub projection: ManagedAutoloadProjection,
    pub state_sidecar: SnapshotSidecar<AutoloadState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedAutoloadProjection {
    NotConfigured,
    Absent {
        managed_file_path: String,
    },
    Present {
        managed_file_path: String,
        content: String,
        sha256: String,
        active_module_ids: Vec<String>,
        nu_executable_sha256: String,
        nu_version: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotSidecar<T> {
    Absent,
    Present {
        content: String,
        sha256: String,
        value: T,
    },
}

/// Loaded snapshot with all sidecars verified.
///
/// `paths` is `None` for legacy snapshots that never captured the Nu path
/// cache. New snapshots always record [`SnapshotPaths::Absent`] or
/// [`SnapshotPaths::Present`].
///
/// `activation_profile` is `None` for legacy snapshots that never captured
/// the activation profile. New snapshots always record
/// [`SnapshotSidecar::Absent`] or [`SnapshotSidecar::Present`].
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub manifest: SnapshotManifest,
    pub lockfile: Lockfile,
    pub autoload: SnapshotAutoload,
    pub imports: Option<NupmImportsFile>,
    pub paths: Option<SnapshotPaths>,
    pub activation_profile: Option<SnapshotSidecar<ActivationProfile>>,
}

/// A legacy timestamp-only snapshot directory.
#[derive(Debug, Clone)]
pub struct LegacySnapshot {
    pub path: PathBuf,
    pub lockfile: Lockfile,
}

/// Snapshot store paths.
fn snapshots_dir(root: &Path) -> PathBuf {
    root.join("state/snapshots")
}

fn staging_dir(root: &Path) -> PathBuf {
    snapshots_dir(root).join(".staging")
}

fn snapshot_dir(root: &Path, id: &str) -> PathBuf {
    snapshots_dir(root).join(id)
}

fn manifest_path(root: &Path, id: &str) -> PathBuf {
    snapshot_dir(root, id).join("snapshot.json")
}

/// Generate a new collision-resistant, time-sortable snapshot ID.
pub fn generate_snapshot_id() -> String {
    Uuid::now_v7().to_string()
}

/// Validate that `id` is a UUIDv7 string.
pub fn validate_snapshot_id(id: &str) -> Result<()> {
    let uuid = Uuid::parse_str(id).with_context(|| format!("'{}' is not a valid UUID", id))?;
    if uuid.get_version_num() != 7 {
        bail!("'{}' is not a UUIDv7 snapshot ID", id);
    }
    Ok(())
}

/// Create an immutable snapshot of the current Numan-owned state.
///
/// Snapshot creation is atomic: files are written to a staging directory and
/// then renamed into place. If any payload referenced by the lockfile is missing
/// or its revision hash cannot be computed, creation fails.
pub fn create_snapshot(
    root: &Path,
    reason: SnapshotReason,
    trigger: SnapshotTrigger,
    related_snapshot_id: Option<String>,
    relation: Option<SnapshotRelation>,
) -> Result<SnapshotManifest> {
    let id = generate_snapshot_id();
    let created_at = crate::util::format_timestamp();
    let numan_root = canonical_root_string(root)?;
    let platform = crate::core::platform::Platform::detect().triple;

    let lockfile = Lockfile::load(root)?;
    let nu_identity = load_nu_identity(root).ok();
    let autoload = capture_autoload(root, &nu_identity)?;
    let imports = load_imports_sidecar(root);
    let paths = capture_paths_sidecar(root)?;
    let payload_revisions = compute_payload_revisions(root, &lockfile)?;
    let activation_profile = capture_activation_profile_sidecar(root)?;

    let sidecar_digests = SidecarDigests {
        lockfile_sha256: sha256_json(&lockfile)?,
        autoload_sha256: Some(sha256_json(&autoload)?),
        imports_sha256: imports.as_ref().and_then(|i| sha256_json(i).ok()),
        paths_sha256: Some(sha256_json(&paths)?),
        activation_profile_sha256: activation_profile
            .as_ref()
            .and_then(|p| sha256_json(p).ok()),
    };

    let manifest = SnapshotManifest {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        id: id.clone(),
        created_at,
        reason,
        trigger,
        related_snapshot_id,
        relation,
        numan_root,
        platform,
        nu_identity,
        sidecar_digests,
        payload_revisions,
    };

    let stage = staging_dir(root).join(&id);
    std::fs::create_dir_all(&stage).with_context(|| {
        format!(
            "Failed to create snapshot staging dir '{}'",
            stage.display()
        )
    })?;

    write_json_atomic(&stage.join("snapshot.json"), &manifest)?;
    write_json_atomic(&stage.join("lockfile.json"), &lockfile)?;
    write_json_atomic(&stage.join("autoload.json"), &autoload)?;
    if let Some(ref imports) = imports {
        write_json_atomic(&stage.join("imports.json"), imports)?;
    }
    write_json_atomic(&stage.join("paths.json"), &paths)?;
    if let Some(ref profile) = activation_profile {
        write_json_atomic(&stage.join("activation-profile.json"), profile)?;
    }

    let dest = snapshot_dir(root, &id);
    std::fs::rename(&stage, &dest).with_context(|| {
        format!(
            "Failed to publish snapshot from '{}' to '{}'",
            stage.display(),
            dest.display()
        )
    })?;

    Ok(manifest)
}

/// Load a snapshot by ID, verifying sidecar digests against the manifest.
pub fn load_snapshot(root: &Path, id: &str) -> Result<Snapshot> {
    validate_snapshot_id(id)?;
    let dir = snapshot_dir(root, id);
    if !dir.exists() {
        bail!("Snapshot '{}' does not exist", id);
    }
    if is_symlink_or_reparse(&dir)? {
        bail!("Snapshot '{}' path is a symlink or reparse point", id);
    }

    let manifest: SnapshotManifest = read_json(&dir.join("snapshot.json"))?;
    if manifest.id != id {
        bail!(
            "Snapshot manifest ID mismatch: expected '{}', found '{}'",
            id,
            manifest.id
        );
    }

    let lockfile: Lockfile = read_json(&dir.join("lockfile.json"))?;
    verify_digest(
        &lockfile,
        &manifest.sidecar_digests.lockfile_sha256,
        "lockfile",
    )?;

    let autoload: SnapshotAutoload = read_json(&dir.join("autoload.json"))?;
    if let Some(expected) = manifest.sidecar_digests.autoload_sha256.as_deref() {
        verify_digest(&autoload, expected, "autoload")?;
    }

    let imports_path = dir.join("imports.json");
    let imports = if imports_path.exists() {
        let imports: NupmImportsFile = read_json(&imports_path)?;
        if let Some(expected) = manifest.sidecar_digests.imports_sha256.as_deref() {
            verify_digest(&imports, expected, "imports")?;
        }
        Some(imports)
    } else {
        None
    };

    // Legacy snapshots omit the paths digest; treat them as NotCaptured (`None`)
    // so rollback leaves the live Nu path cache alone. When a digest is present,
    // the paths.json sidecar is required; do not silently degrade to legacy.
    let paths = if let Some(expected) = manifest.sidecar_digests.paths_sha256.as_deref() {
        let paths_path = dir.join("paths.json");
        if !paths_path.exists() {
            bail!(
                "Snapshot '{}' declares a paths sidecar digest but paths.json is missing. \
                 Refuse to load an incomplete snapshot; re-create the snapshot or restore \
                 the sidecar file.",
                id
            );
        }
        let paths: SnapshotPaths = read_json(&paths_path)?;
        verify_digest(&paths, expected, "paths")?;
        Some(paths)
    } else {
        None
    };

    // Legacy snapshots omit the activation-profile digest; treat them as
    // NotCaptured (`None`) so rollback leaves the live profile alone.
    let activation_profile = if let Some(expected) = manifest
        .sidecar_digests
        .activation_profile_sha256
        .as_deref()
    {
        let profile_path = dir.join("activation-profile.json");
        if !profile_path.exists() {
            bail!(
                "Snapshot '{}' declares an activation-profile sidecar digest but \
                 activation-profile.json is missing. Refuse to load an incomplete snapshot; \
                 re-create the snapshot or restore the sidecar file.",
                id
            );
        }
        let profile: ActivationProfile = read_json(&profile_path)?;
        verify_digest(&profile, expected, "activation-profile")?;
        Some(SnapshotSidecar::Present {
            content: std::fs::read_to_string(&profile_path)
                .with_context(|| format!("Failed to read '{}'", profile_path.display()))?,
            sha256: expected.to_string(),
            value: profile,
        })
    } else {
        None
    };

    Ok(Snapshot {
        manifest,
        lockfile,
        autoload,
        imports,
        paths,
        activation_profile,
    })
}

/// Load only the manifest for a snapshot.
pub fn load_manifest(root: &Path, id: &str) -> Result<SnapshotManifest> {
    validate_snapshot_id(id)?;
    let path = manifest_path(root, id);
    read_json(&path)
}

/// List all committed snapshot manifests, sorted by creation time (UUIDv7 order).
pub fn list_snapshots(root: &Path) -> Result<Vec<SnapshotManifest>> {
    let dir = snapshots_dir(root);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut result = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .with_context(|| format!("Failed to read snapshots dir '{}'", dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        if is_symlink_or_reparse(&entry.path())? {
            continue;
        }
        if let Ok(manifest) = load_manifest(root, &name_str) {
            result.push(manifest);
        }
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(result)
}

/// Delete a snapshot directory, refusing if it is referenced by an in-flight
/// transaction or if the path is a symlink/reparse point.
pub fn delete_snapshot(root: &Path, id: &str) -> Result<()> {
    validate_snapshot_id(id)?;

    if let Some(journal) = PendingLifecycle::load(root)? {
        if journal.target_snapshot_id.as_deref() == Some(id)
            || journal.pre_rollback_snapshot_id.as_deref() == Some(id)
        {
            bail!(
                "Snapshot '{}' is referenced by an in-flight rollback journal. \
                 Complete or abort the rollback before deleting.",
                id
            );
        }
    }

    if let Some(journal) = PendingAutoload::load(root)? {
        if journal.pre_mutation_snapshot_id.as_deref() == Some(id) {
            bail!(
                "Snapshot '{}' is referenced by an in-flight autoload journal. \
                 Complete or abort the activation/deactivation before deleting.",
                id
            );
        }
    }

    let dir = snapshot_dir(root, id);
    if !dir.exists() {
        bail!("Snapshot '{}' does not exist", id);
    }
    if is_symlink_or_reparse(&dir)? {
        bail!(
            "Snapshot '{}' path is a symlink or reparse point; refusing to delete",
            id
        );
    }

    std::fs::remove_dir_all(&dir)
        .with_context(|| format!("Failed to delete snapshot directory '{}'", dir.display()))?;
    Ok(())
}

/// Verify that every payload referenced by the provided lockfile exists and
/// matches the revision hash recorded in `payload_revisions`.
///
/// Returns a list of remediation strings for any mismatches; an empty list means
/// all payloads verify.
pub fn verify_payloads(
    root: &Path,
    lockfile: &Lockfile,
    payload_revisions: &BTreeMap<String, String>,
) -> Result<Vec<String>> {
    let mut errors = Vec::new();
    for (package_id, entry) in &lockfile.packages {
        let payload_dir = root.join(entry.payload_path());
        if !payload_dir.exists() {
            errors.push(format!(
                "{}: payload directory missing: {}",
                package_id,
                payload_dir.display()
            ));
            continue;
        }
        let expected = match payload_revisions.get(package_id) {
            Some(r) => r,
            None => {
                errors.push(format!(
                    "{}: no recorded revision in snapshot manifest",
                    package_id
                ));
                continue;
            }
        };
        let actual = match compute_revision_id(&payload_dir) {
            Some(r) => r,
            None => {
                errors.push(format!(
                    "{}: could not compute revision for {}",
                    package_id,
                    payload_dir.display()
                ));
                continue;
            }
        };
        if actual != *expected {
            errors.push(format!(
                "{}: revision mismatch (expected {}, actual {})",
                package_id, expected, actual
            ));
        }
    }
    Ok(errors)
}

/// Find legacy timestamp-only snapshots under `root/snapshots/`.
pub fn find_legacy_snapshots(root: &Path) -> Result<Vec<LegacySnapshot>> {
    let legacy_dir = root.join("snapshots");
    if !legacy_dir.exists() {
        return Ok(Vec::new());
    }

    let mut result = Vec::new();
    for entry in std::fs::read_dir(&legacy_dir).with_context(|| {
        format!(
            "Failed to read legacy snapshots dir '{}'",
            legacy_dir.display()
        )
    })? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let lock_path = path.join("lockfile.json");
        if !lock_path.exists() {
            continue;
        }
        let lockfile: Lockfile = match read_json(&lock_path) {
            Ok(l) => l,
            Err(_) => continue,
        };
        result.push(LegacySnapshot { path, lockfile });
    }
    Ok(result)
}

/// Return payload paths referenced by legacy snapshots.
pub fn legacy_snapshot_payload_paths(root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for snap in find_legacy_snapshots(root)? {
        for entry in snap.lockfile.packages.values() {
            paths.push(root.join(entry.payload_path()));
        }
    }
    Ok(paths)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn canonical_root_string(root: &Path) -> Result<String> {
    let canonical = root
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize Numan root '{}'", root.display()))?;
    Ok(canonical.to_string_lossy().to_string())
}

fn load_nu_identity(root: &Path) -> Result<SnapshotNuIdentity> {
    let nu_paths = NuPaths::load(root)?;
    Ok(SnapshotNuIdentity {
        nu_version: nu_paths.nu_version,
        nu_executable_sha256: nu_paths.nu_executable_hash,
    })
}

fn capture_autoload(
    root: &Path,
    nu_identity: &Option<SnapshotNuIdentity>,
) -> Result<SnapshotAutoload> {
    let nu_paths = match NuPaths::load(root) {
        Ok(p) => p,
        Err(_) => {
            return Ok(SnapshotAutoload {
                schema_version: SNAPSHOT_SCHEMA_VERSION,
                projection: ManagedAutoloadProjection::NotConfigured,
                state_sidecar: SnapshotSidecar::Absent,
            });
        }
    };

    let vendor_dir = match nu_paths.vendor_autoload_dir {
        Some(d) => d,
        None => {
            return Ok(SnapshotAutoload {
                schema_version: SNAPSHOT_SCHEMA_VERSION,
                projection: ManagedAutoloadProjection::NotConfigured,
                state_sidecar: SnapshotSidecar::Absent,
            });
        }
    };

    let managed_file_path = format!("{vendor_dir}/numan.nu");
    let managed_path = Path::new(&managed_file_path);

    let state_sidecar = match AutoloadState::load(root)? {
        Some(state) => {
            let content = serde_json::to_string_pretty(&state)?;
            SnapshotSidecar::Present {
                sha256: compute_sha256(content.as_bytes()),
                content,
                value: state,
            }
        }
        None => SnapshotSidecar::Absent,
    };

    if !managed_path.exists() {
        return Ok(SnapshotAutoload {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            projection: ManagedAutoloadProjection::Absent { managed_file_path },
            state_sidecar,
        });
    }

    let content = std::fs::read_to_string(managed_path)
        .with_context(|| format!("Failed to read managed file '{}'", managed_path.display()))?;
    let sha256 = compute_sha256(content.as_bytes());

    let active_module_ids = if let Some(ref identity) = nu_identity {
        AutoloadState::active_module_ids_from_lockfile(
            &Lockfile::load(root)?,
            &identity.nu_executable_sha256,
            &identity.nu_version,
            &vendor_dir,
            &managed_file_path,
        )
    } else {
        Vec::new()
    };

    let nu_executable_sha256 = nu_paths.nu_executable_hash;
    let nu_version = nu_paths.nu_version;

    Ok(SnapshotAutoload {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        projection: ManagedAutoloadProjection::Present {
            managed_file_path,
            content,
            sha256,
            active_module_ids,
            nu_executable_sha256,
            nu_version,
        },
        state_sidecar,
    })
}

fn load_imports_sidecar(root: &Path) -> Option<NupmImportsFile> {
    let path = root.join("state/nupm-imports.json");
    if !path.exists() {
        return None;
    }
    NupmImportsFile::load(root).ok()
}

fn capture_paths_sidecar(root: &Path) -> Result<SnapshotPaths> {
    let paths_path = root.join("nu_state/paths.json");
    if !paths_path.exists() {
        return Ok(SnapshotPaths::Absent);
    }

    let paths = NuPaths::load(root).with_context(|| {
        format!(
            "Failed to load paths sidecar '{}'; refusing to create snapshot",
            paths_path.display()
        )
    })?;
    Ok(SnapshotPaths::Present(paths))
}

/// Capture the activation profile as a snapshot sidecar.
///
/// Returns `None` when the profile file does not exist (legacy/uninitialized
/// roots), so the snapshot omits the sidecar and rollback leaves the live
/// profile untouched (matching legacy behavior).
fn capture_activation_profile_sidecar(root: &Path) -> Result<Option<ActivationProfile>> {
    let path = ActivationProfile::profile_path(root);
    if !path.is_file() {
        return Ok(None);
    }
    ActivationProfile::load(root).with_context(|| {
        format!(
            "Failed to load activation profile '{}'; refusing to create snapshot",
            path.display()
        )
    })
}

fn compute_payload_revisions(root: &Path, lockfile: &Lockfile) -> Result<BTreeMap<String, String>> {
    let mut revisions = BTreeMap::new();
    for (package_id, entry) in &lockfile.packages {
        let payload_dir = root.join(entry.payload_path());
        let revision = compute_revision_id(&payload_dir).with_context(|| {
            format!(
                "Snapshot failed: cannot compute revision for payload '{}' of {}. \
                 Run 'numan gc' to clean up orphaned payloads, or reinstall the package.",
                payload_dir.display(),
                package_id
            )
        })?;
        revisions.insert(package_id.clone(), revision);
    }
    Ok(revisions)
}

fn sha256_json<T: Serialize>(value: &T) -> Result<String> {
    let content = serde_json::to_string(value).context("Failed to serialize value")?;
    Ok(compute_sha256(content.as_bytes()))
}

fn read_json<T>(path: &Path) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read '{}'", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("Failed to parse '{}'", path.display()))
}

fn verify_digest<T: Serialize>(value: &T, expected: &str, label: &str) -> Result<()> {
    let actual = sha256_json(value)?;
    if actual != expected {
        bail!(
            "Snapshot {} sidecar digest mismatch: expected {}, actual {}",
            label,
            expected,
            actual
        );
    }
    Ok(())
}

/// Count active modules from a captured autoload projection.
pub fn count_active_modules(autoload: &SnapshotAutoload) -> usize {
    match &autoload.projection {
        ManagedAutoloadProjection::Present {
            active_module_ids, ..
        } => active_module_ids.len(),
        _ => 0,
    }
}

/// Count active plugin entries whose stored activation record matches the given
/// Nu identity. The plugin registry path is not compared here because the
/// snapshot identity does not carry it; this is acceptable for a display count.
pub fn count_active_plugins(lockfile: &Lockfile, nu_identity: &SnapshotNuIdentity) -> usize {
    lockfile
        .packages
        .values()
        .filter(|e| {
            e.package_type == "plugin"
                && e.activation.as_ref().is_some_and(|a| {
                    a.nu_executable_sha256 == nu_identity.nu_executable_sha256
                        && a.nu_version == nu_identity.nu_version
                })
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn empty_lockfile_entry() -> LockfileEntry {
        LockfileEntry {
            version: "1.0.0".to_string(),
            package_type: "module".to_string(),
            source: "archive".to_string(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: None,
            archive_root: None,
            include: None,
            entry: Some("mod.nu".to_string()),
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
            payload_path: "packages/modules/owner/pkg/1.0.0-abc12345".to_string(),
            revision_id: None,
            payload_sha256: None,
            executable_sha256: None,
            selection_reason: None,
            origin: None,
            module_activation: None,
            module_import_mode: None,
            locked_dependencies: BTreeMap::new(),
        }
    }

    #[test]
    fn generate_and_validate_uuidv7() {
        let id = generate_snapshot_id();
        validate_snapshot_id(&id).unwrap();
    }

    #[test]
    fn invalid_uuid_rejected() {
        assert!(validate_snapshot_id("not-a-uuid").is_err());
    }

    #[test]
    fn uuid_v4_rejected() {
        let v4 = Uuid::new_v4().to_string();
        assert!(validate_snapshot_id(&v4).is_err());
    }

    #[test]
    fn create_empty_snapshot_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("state")).unwrap();

        let manifest = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();

        let loaded = load_snapshot(root, &manifest.id).unwrap();
        assert_eq!(loaded.manifest.id, manifest.id);
        assert!(loaded.lockfile.is_empty());
        assert!(loaded.imports.is_none());
    }

    #[test]
    fn create_snapshot_captures_payload_revisions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("state")).unwrap();

        // Create a payload directory.
        let payload = root.join("packages/modules/owner/pkg/1.0.0-abc12345");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("mod.nu"), "# module").unwrap();

        let mut lockfile = Lockfile::empty();
        lockfile
            .packages
            .insert("owner/pkg".to_string(), empty_lockfile_entry());
        lockfile.save(root).unwrap();

        let manifest = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();

        assert_eq!(manifest.payload_revisions.len(), 1);
        let loaded = load_snapshot(root, &manifest.id).unwrap();
        assert_eq!(
            loaded.manifest.payload_revisions,
            manifest.payload_revisions
        );
        let errors =
            verify_payloads(root, &loaded.lockfile, &loaded.manifest.payload_revisions).unwrap();
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
    }

    #[test]
    fn create_snapshot_fails_on_missing_payload() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("state")).unwrap();

        let mut lockfile = Lockfile::empty();
        lockfile
            .packages
            .insert("owner/pkg".to_string(), empty_lockfile_entry());
        lockfile.save(root).unwrap();

        assert!(create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .is_err());
    }

    #[test]
    fn list_and_delete_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("state")).unwrap();

        let m1 = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();
        let m2 = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Update,
            None,
            None,
        )
        .unwrap();

        let list = list_snapshots(root).unwrap();
        assert_eq!(list.len(), 2);

        delete_snapshot(root, &m1.id).unwrap();
        let list = list_snapshots(root).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, m2.id);
    }

    #[test]
    fn delete_rejects_journal_referenced_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("state")).unwrap();

        let m = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();

        let journal = PendingAutoload {
            schema_version: AUTOLOAD_SCHEMA_VERSION,
            operation: crate::state::autoload_journal::AutoloadOperation::Activate,
            stage: crate::state::autoload_journal::AutoloadStage::Prepared,
            nu_executable_sha256: "abc".to_string(),
            nu_version: "0.113.1".to_string(),
            vendor_autoload_dir: "/nu/vendor".to_string(),
            managed_file_path: "/nu/vendor/numan.nu".to_string(),
            previous_file_exists: false,
            previous_file_sha256: None,
            desired_file_exists: true,
            candidate_sha256: None,
            previous_active_module_ids: Vec::new(),
            desired_active_module_ids: vec!["owner/pkg".to_string()],
            targeted_module_ids: vec!["owner/pkg".to_string()],
            created_at: "0".to_string(),
            pre_mutation_snapshot_id: Some(m.id.clone()),
        };
        journal.save(root).unwrap();

        assert!(delete_snapshot(root, &m.id).is_err());
    }

    #[test]
    fn delete_rejects_symlink_snapshot_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("state/snapshots")).unwrap();

        let real = root.join("state/snapshots/real");
        std::fs::create_dir_all(&real).unwrap();
        let link = root.join("state/snapshots/018ff000-0000-7fff-0000-000000000001");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real, &link).unwrap();

        assert!(delete_snapshot(root, "018ff000-0000-7fff-0000-000000000001").is_err());
    }

    fn fake_nu_paths(root: &Path, hash: &str, version: &str) -> NuPaths {
        NuPaths {
            nu_executable: root.join("fake-nu").to_string_lossy().to_string(),
            nu_version: version.to_string(),
            plugin_registry_path: root.join("plugin.msgpackz").to_string_lossy().to_string(),
            nu_executable_hash: hash.to_string(),
            platform: "x86_64-linux-gnu".to_string(),
            data_dir: None,
            vendor_autoload_dirs: Vec::new(),
            vendor_autoload_dir: None,
        }
    }

    #[test]
    fn create_snapshot_captures_paths_sidecar_and_verifies_digest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("state")).unwrap();

        let paths = fake_nu_paths(root, "hash-aaa", "0.113.1");
        paths.save(root).unwrap();

        let manifest = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();

        assert!(
            manifest.sidecar_digests.paths_sha256.is_some(),
            "paths digest must be recorded when paths.json exists"
        );

        let loaded = load_snapshot(root, &manifest.id).unwrap();
        match &loaded.paths {
            Some(SnapshotPaths::Present(p)) => {
                assert_eq!(p.nu_executable_hash, "hash-aaa");
                assert_eq!(p.nu_version, "0.113.1");
            }
            other => panic!("expected Present paths sidecar, got {other:?}"),
        }
        assert_eq!(
            loaded.manifest.sidecar_digests.paths_sha256,
            manifest.sidecar_digests.paths_sha256
        );
    }

    #[test]
    fn create_snapshot_records_paths_absent_when_uninitialized() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("state")).unwrap();

        let manifest = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();

        assert!(
            manifest.sidecar_digests.paths_sha256.is_some(),
            "new snapshots must record an explicit paths capture (Absent)"
        );
        let loaded = load_snapshot(root, &manifest.id).unwrap();
        assert!(matches!(loaded.paths, Some(SnapshotPaths::Absent)));
    }

    #[test]
    fn create_snapshot_fails_when_existing_paths_json_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::create_dir_all(root.join("nu_state")).unwrap();
        // Present but unreadable as NuPaths: must not be recorded as Absent.
        std::fs::write(root.join("nu_state/paths.json"), "{ not-json").unwrap();

        let err = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Failed to load paths sidecar") || msg.contains("paths"),
            "expected paths load failure, got: {msg}"
        );
    }

    #[test]
    fn load_snapshot_rejects_missing_paths_sidecar_when_digest_present() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("state")).unwrap();

        let manifest = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();
        assert!(manifest.sidecar_digests.paths_sha256.is_some());

        let snap_dir = root.join(format!("state/snapshots/{}", manifest.id));
        std::fs::remove_file(snap_dir.join("paths.json")).unwrap();

        let err = load_snapshot(root, &manifest.id).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("incomplete snapshot") || msg.contains("paths.json is missing"),
            "expected missing sidecar rejection, got: {msg}"
        );
    }

    #[test]
    fn old_snapshot_without_paths_sidecar_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("state")).unwrap();

        let manifest = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();

        // Simulate a pre-paths-sidecar snapshot: drop the sidecar file and clear
        // the digest field (as #[serde(default)] would yield on old manifests).
        let snap_dir = root.join(format!("state/snapshots/{}", manifest.id));
        let paths_sidecar = snap_dir.join("paths.json");
        if paths_sidecar.exists() {
            std::fs::remove_file(&paths_sidecar).unwrap();
        }
        let mut manifest = load_manifest(root, &manifest.id).unwrap();
        manifest.sidecar_digests.paths_sha256 = None;
        write_json_atomic(&snap_dir.join("snapshot.json"), &manifest).unwrap();

        let loaded = load_snapshot(root, &manifest.id).unwrap();
        assert!(
            loaded.paths.is_none(),
            "legacy snapshots without paths.json must load as NotCaptured (None)"
        );
    }
}
