//! Download and install an official Nushell release binary under the Numan root.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::core::integrity;
use crate::core::platform::{Arch, Env, Os, Platform};
use crate::install::download::download_file;
use crate::install::extract::{extract_archive, ArchiveFormat, ExtractConfig};
use crate::nu::paths::validate_nushell_binary;
use crate::nu::version_manager;
use crate::state::lockfile::{Lockfile, LockfileEntry, BUNDLED_NU_ORIGIN};
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
#[cfg(unix)]
use crate::util::atomic::write_bytes_atomic;
#[cfg(unix)]
use crate::util::fs_safety::assert_not_symlink;

const RELEASES_LATEST: &str = "https://api.github.com/repos/nushell/nushell/releases/latest";
const RELEASES_TAGS_BASE: &str = "https://api.github.com/repos/nushell/nushell/releases/tags/";
const USER_AGENT: &str = "numan-cli (https://github.com/numan-cli/numan)";

/// Official Nushell release archives (nu + bundled plugins) exceeded 256 MiB
/// uncompressed as of 0.114.1 (~279 MiB on linux-gnu). Cap with headroom; we
/// also filter to the `nu` binary so plugin payloads are not extracted.
const NU_RELEASE_MAX_UNCOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;

/// Snapshot established Numan state before a `setup nu` install/PATH mutation.
fn snapshot_before_nu_setup(root: &Path, context: &'static str) -> Result<()> {
    create_snapshot(
        root,
        SnapshotReason::PreMutation,
        SnapshotTrigger::Install,
        None,
        None,
    )
    .context(context)?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    /// GitHub release asset digest, when present (e.g. `sha256:…`).
    #[serde(default)]
    digest: Option<String>,
}

pub fn managed_nu_dir(root: &Path) -> PathBuf {
    root.join("tools").join("nushell")
}

pub fn managed_nu_binary(root: &Path) -> PathBuf {
    managed_nu_dir(root).join(nu_binary_name())
}

fn nu_binary_name() -> &'static str {
    if cfg!(windows) {
        "nu.exe"
    } else {
        "nu"
    }
}

fn nu_release_extract_config(minimal: bool) -> ExtractConfig {
    ExtractConfig {
        // When minimal is true, filter to just the shell binary (old behavior).
        // When minimal is false, extract everything (nu + bundled plugins).
        include: if minimal {
            Some(vec![format!("**/{}", nu_binary_name())])
        } else {
            None
        },
        max_uncompressed_bytes: Some(NU_RELEASE_MAX_UNCOMPRESSED_BYTES),
        ..ExtractConfig::default()
    }
}

pub fn release_asset_suffix(platform: &Platform) -> Result<&'static str> {
    match (platform.os, platform.arch, platform.env) {
        (Os::Windows, Arch::X86_64, Env::Msvc) => Ok("x86_64-pc-windows-msvc.zip"),
        (Os::Windows, Arch::Aarch64, Env::Msvc) => Ok("aarch64-pc-windows-msvc.zip"),
        (Os::Linux, Arch::X86_64, Env::Gnu) => Ok("x86_64-unknown-linux-gnu.tar.gz"),
        (Os::Linux, Arch::X86_64, Env::Musl) => Ok("x86_64-unknown-linux-musl.tar.gz"),
        (Os::Linux, Arch::Aarch64, Env::Gnu) => Ok("aarch64-unknown-linux-gnu.tar.gz"),
        (Os::Linux, Arch::Aarch64, Env::Musl) => Ok("aarch64-unknown-linux-musl.tar.gz"),
        (Os::Macos, Arch::X86_64, Env::Darwin) => Ok("x86_64-apple-darwin.tar.gz"),
        (Os::Macos, Arch::Aarch64, Env::Darwin) => Ok("aarch64-apple-darwin.tar.gz"),
        _ => bail!(
            "No official Nushell release archive is published for platform triple '{}'. \
             Install Nushell manually from https://www.nushell.sh/book/installation.html",
            platform.triple
        ),
    }
}

fn select_release_asset<'a>(
    release: &'a GitHubRelease,
    platform: &Platform,
) -> Result<&'a GitHubAsset> {
    let suffix = release_asset_suffix(platform)?;
    let expected = format!("nu-{}-{suffix}", release.tag_name);
    release
        .assets
        .iter()
        .find(|a| a.name == expected)
        .with_context(|| {
            format!(
                "Release {} has no asset named '{expected}'. \
                 Install Nushell manually from https://www.nushell.sh/book/installation.html",
                release.tag_name
            )
        })
}

fn fetch_latest_release(client: &reqwest::blocking::Client) -> Result<GitHubRelease> {
    fetch_release_url(client, RELEASES_LATEST)
}

fn normalize_release_tag(version: &str) -> String {
    version.trim().trim_start_matches('v').to_string()
}

fn fetch_release_by_tag(
    client: &reqwest::blocking::Client,
    version: &str,
) -> Result<GitHubRelease> {
    let tag = normalize_release_tag(version);
    let url = format!("{RELEASES_TAGS_BASE}{tag}");
    fetch_release_url(client, &url).with_context(|| {
        format!(
            "Failed to find Nushell release tag '{tag}'. \
             Check https://github.com/nushell/nushell/releases for published versions."
        )
    })
}

fn fetch_release_url(client: &reqwest::blocking::Client, url: &str) -> Result<GitHubRelease> {
    let response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .context("Failed to query Nushell releases on GitHub")?;

    if !response.status().is_success() {
        bail!(
            "Failed to query Nushell releases: HTTP {}",
            response.status()
        );
    }

    let body = response
        .text()
        .context("Failed to read Nushell release metadata from GitHub")?;
    serde_json::from_str::<GitHubRelease>(&body)
        .context("Failed to parse Nushell release metadata from GitHub")
}

fn archive_format_for_url(url: &str) -> Result<ArchiveFormat> {
    ArchiveFormat::from_url(url)
        .with_context(|| format!("Unsupported Nushell release archive format for '{url}'"))
}

pub fn locate_extracted_nu_binary(extract_root: &Path) -> Result<PathBuf> {
    let direct = extract_root.join(nu_binary_name());
    if direct.is_file() {
        return Ok(direct);
    }

    for entry in std::fs::read_dir(extract_root).with_context(|| {
        format!(
            "Failed to read extracted Nushell directory '{}'",
            extract_root.display()
        )
    })? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let candidate = path.join(nu_binary_name());
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    bail!(
        "Could not find '{}' in extracted Nushell archive under '{}'",
        nu_binary_name(),
        extract_root.display()
    )
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .with_context(|| format!("Failed to read permissions for '{}'", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).with_context(|| {
        format!(
            "Failed to mark Nushell binary executable at '{}'",
            path.display()
        )
    })?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

pub fn install_from_archive(
    archive_path: &Path,
    root: &Path,
    version: &str,
    minimal: bool,
) -> Result<PathBuf> {
    let format = archive_format_for_url(
        archive_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("archive"),
    )?;

    let extract_root = root.join("tools/.nushell-extract");
    if extract_root.exists() {
        std::fs::remove_dir_all(&extract_root)?;
    }
    std::fs::create_dir_all(&extract_root)?;
    let _extract_cleanup = ExtractCleanup(extract_root.clone());

    extract_archive(
        archive_path,
        &extract_root,
        &nu_release_extract_config(minimal),
        format,
    )
    .with_context(|| format!("Failed to extract '{}'", archive_path.display()))?;

    let source = locate_extracted_nu_binary(&extract_root)?;
    // Install into the VERSIONED layout (`<root>/tools/nushell/<version>/<bin>`)
    // so `numan use list` / `numan use latest` — which scan
    // `<root>/tools/nushell/<version>/` for installed versions — see the
    // freshly installed release. The legacy single-binary path
    // (`<root>/tools/nushell/<bin>`) is migration-only now: it must not be
    // produced by new installs, otherwise `list_installed_versions` reports
    // `[]` right after `numan setup nu` succeeds.
    let normalized = version_manager::normalize_version(version)
        .with_context(|| format!("Invalid Nu version '{}' for installation", version))?;
    let dest_dir = version_manager::version_install_dir(root, &normalized);
    std::fs::create_dir_all(&dest_dir).with_context(|| {
        format!(
            "Failed to create managed Nushell directory '{}'",
            dest_dir.display()
        )
    })?;
    let dest = version_manager::version_binary(root, &normalized);

    // Copy the nu binary
    std::fs::copy(&source, &dest).with_context(|| {
        format!(
            "Failed to copy Nushell binary from '{}' to '{}'",
            source.display(),
            dest.display()
        )
    })?;
    make_executable(&dest)?;

    // When not in minimal mode, copy all other extracted files (plugins etc.)
    // into the versioned directory alongside the nu binary.
    if !minimal {
        let extract_subdir = source.parent().unwrap_or(&extract_root);
        copy_extracted_files(extract_subdir, &dest_dir, &source)?;
    }

    // Keep the legacy VERSION marker for backwards compat with tooling that
    // greps for it, but under the versioned dir so it never shadows a sibling
    // version's marker. Write the normalized version so `detect_legacy_version`
    // can parse it cleanly without having to strip a `v`-prefix.
    let version_file = dest_dir.join("VERSION");
    std::fs::write(&version_file, normalized.as_bytes()).with_context(|| {
        format!(
            "Failed to write VERSION metadata to '{}'",
            version_file.display()
        )
    })?;
    Ok(dest)
}

/// Copy extracted `nu_plugin_*` files from `src_dir` into `dest_dir`, skipping
/// `skip_file` (which has already been copied as the nu binary). Only files
/// whose name starts with `nu_plugin_` are copied; other archive contents
/// (README, LICENSE, etc.) are intentionally excluded to keep the version
/// directory clean.
fn copy_extracted_files(src_dir: &Path, dest_dir: &Path, skip_file: &Path) -> Result<()> {
    let entries = std::fs::read_dir(src_dir)
        .with_context(|| format!("Failed to read extracted directory '{}'", src_dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        // Skip the nu binary (already copied) and directories
        if path == skip_file || !path.is_file() {
            continue;
        }
        let file_name = match path.file_name() {
            Some(n) => n,
            None => continue,
        };
        // Only copy plugin binaries (nu_plugin_*); skip non-plugin files
        // like README.txt, LICENSE, etc.
        if !file_name.to_string_lossy().starts_with("nu_plugin_") {
            continue;
        }
        let dest_path = dest_dir.join(file_name);
        std::fs::copy(&path, &dest_path).with_context(|| {
            format!(
                "Failed to copy '{}' to '{}'",
                path.display(),
                dest_path.display()
            )
        })?;
        make_executable(&dest_path)?;
    }
    Ok(())
}

struct ExtractCleanup(PathBuf);

impl Drop for ExtractCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Format current timestamp as a zero-padded Unix-seconds string (matches the
/// install-transaction lockfile timestamp shape).
fn now_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs:016}")
}

/// Scan the installed version directory for `nu_plugin_*` binaries bundled
/// with the official Nushell release and record them in the lockfile with the
/// `bundled:nu` origin. These plugins are already on disk, so no registry
/// install flow is needed; they become discoverable and activatable via
/// `numan activate`. Not auto-activated.
///
/// The version directory (`tools/nushell/<version>/`) is scanned; each
/// `nu_plugin_*` file is hashed and written as a `type = "plugin"`,
/// `source = "binary"` lockfile entry keyed as `nushell/<plugin_name>`.
fn discover_bundled_plugins(root: &Path, version_dir: &Path, nu_version: &str) -> Result<()> {
    let entries = match std::fs::read_dir(version_dir) {
        Ok(e) => e,
        // No directory means nothing to discover (e.g. minimal install path).
        Err(_) => return Ok(()),
    };

    let mut discovered: Vec<(String, LockfileEntry)> = Vec::new();
    let payload_path = version_dir
        .strip_prefix(root)
        .with_context(|| {
            format!(
                "Version directory '{}' is not under root '{}'",
                version_dir.display(),
                root.display()
            )
        })?
        .to_string_lossy()
        .replace('\\', "/");

    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("nu_plugin_") {
            continue;
        }
        if !entry.file_type()?.is_file() {
            continue;
        }

        // Strip a platform suffix (`.exe` on Windows) then the `nu_plugin_`
        // prefix to derive the plugin short name.
        let stripped = name.strip_suffix(".exe").unwrap_or(&name);
        let plugin_name = match stripped.strip_prefix("nu_plugin_") {
            Some(n) if !n.is_empty() => n,
            _ => continue,
        };
        let package_id = format!("nushell/{plugin_name}");

        let bytes = std::fs::read(entry.path()).with_context(|| {
            format!(
                "Failed to read bundled plugin binary '{}'",
                entry.path().display()
            )
        })?;
        let sha = integrity::compute_sha256(&bytes);

        let lockfile_entry = LockfileEntry {
            version: nu_version.to_string(),
            package_type: "plugin".to_string(),
            source: "binary".to_string(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: Some(name.clone()),
            archive_root: None,
            include: None,
            entry: None,
            installed_at: now_timestamp(),
            nu_version_at_install: Some(nu_version.to_string()),
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
            payload_path: payload_path.clone(),
            revision_id: None,
            payload_sha256: None,
            executable_sha256: Some(sha),
            selection_reason: None,
            origin: Some(BUNDLED_NU_ORIGIN.to_string()),
            module_activation: None,
            module_import_mode: None,
            locked_dependencies: std::collections::BTreeMap::new(),
        };
        discovered.push((package_id, lockfile_entry));
    }

    if discovered.is_empty() {
        return Ok(());
    }

    let mut lockfile = Lockfile::load(root)?;
    for (package_id, entry) in discovered {
        // Skip entries that already exist with a non-bundled origin.
        // This respects a user's explicit registry install over automatic
        // bundled extraction (e.g. "nushell/polars" installed from the
        // registry should not be silently overwritten).
        if let Some(existing) = lockfile.packages.get(&package_id) {
            if existing.origin.as_deref() != Some(BUNDLED_NU_ORIGIN) {
                continue;
            }
        }
        lockfile.packages.insert(package_id, entry);
    }
    lockfile.save(root)?;
    Ok(())
}

fn verify_downloaded_archive(path: &Path, asset: &GitHubAsset) -> Result<()> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("Failed to read downloaded archive '{}'", path.display()))?;
    if bytes.len() as u64 != asset.size {
        bail!(
            "Downloaded archive size mismatch for '{}': expected {} bytes, got {} bytes",
            asset.name,
            asset.size,
            bytes.len()
        );
    }

    if let Some(digest) = asset.digest.as_deref() {
        let expected = digest
            .strip_prefix("sha256:")
            .with_context(|| format!("Unsupported digest format for '{}': {digest}", asset.name))?;
        let computed = integrity::compute_sha256(&bytes);
        if !computed.eq_ignore_ascii_case(expected) {
            bail!(
                "Downloaded archive checksum mismatch for '{}': expected {expected}, got {computed}",
                asset.name
            );
        }
    }

    Ok(())
}

pub fn install_latest(root: &Path, platform: &Platform, minimal: bool) -> Result<PathBuf> {
    install_release(root, platform, None, minimal)
}

/// Download and install a specific Nushell release tag (e.g. `0.113.1`).
pub fn install_version(
    root: &Path,
    platform: &Platform,
    version: &str,
    minimal: bool,
) -> Result<PathBuf> {
    install_release(root, platform, Some(version), minimal)
}

fn install_release(
    root: &Path,
    platform: &Platform,
    version: Option<&str>,
    minimal: bool,
) -> Result<PathBuf> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .user_agent(USER_AGENT)
        .build()
        .context("Failed to build HTTP client for Nushell release download")?;
    let release = match version {
        Some(v) => fetch_release_by_tag(&client, v)?,
        None => fetch_latest_release(&client)?,
    };
    install_from_github_release(root, platform, &release, minimal)
}

/// Install from an already-fetched GitHub release (avoids a second API call
/// when the caller resolved the tag via `fetch_latest_release`).
fn install_from_github_release(
    root: &Path,
    platform: &Platform,
    release: &GitHubRelease,
    minimal: bool,
) -> Result<PathBuf> {
    let asset = select_release_asset(release, platform)?;

    let cache_dir = root.join("tools/.cache");
    std::fs::create_dir_all(&cache_dir)?;
    let archive_path = cache_dir.join(&asset.name);

    println!("Downloading Nushell {} ({})…", release.tag_name, asset.name);
    download_file(&asset.browser_download_url, &archive_path)?;
    verify_downloaded_archive(&archive_path, asset)?;

    let installed = install_from_archive(&archive_path, root, &release.tag_name, minimal)?;
    validate_nushell_binary(&installed).with_context(|| {
        format!(
            "Installed Nushell binary at '{}' failed validation",
            installed.display()
        )
    })?;
    println!(
        "Installed Nushell {} to '{}'.",
        release.tag_name,
        installed.display()
    );
    println!(
        "Next: run '{}' then re-activate packages you still want on this Nu.",
        crate::util::hints::CMD_INIT_REFRESH
    );
    Ok(installed)
}

pub fn prepend_process_path(dir: &Path) -> Result<()> {
    let dir = normalize_path_entry(dir);
    let dir_str = dir
        .to_str()
        .with_context(|| format!("PATH entry '{}' is not valid UTF-8", dir.display()))?;
    let current = std::env::var("PATH").unwrap_or_default();
    if path_list_contains(&current, dir_str) {
        return Ok(());
    }
    #[cfg(windows)]
    let separator = ";";
    #[cfg(not(windows))]
    let separator = ":";
    std::env::set_var("PATH", format!("{dir_str}{separator}{current}"));
    Ok(())
}

#[cfg(windows)]
const VERBATIM_PATH_PREFIX: &str = "\\\\?\\";

/// Strip Windows extended-length prefixes so PATH entries round-trip through `std::env::var("PATH")`.
fn normalize_path_entry_str(entry: &str) -> String {
    #[cfg(windows)]
    {
        if let Some(stripped) = entry.strip_prefix(VERBATIM_PATH_PREFIX) {
            return stripped.to_string();
        }
    }
    entry.to_string()
}

fn normalize_path_entry(path: &Path) -> PathBuf {
    PathBuf::from(normalize_path_entry_str(&path.to_string_lossy()))
}

fn path_list_contains(path_var: &str, entry: &str) -> bool {
    let entry_str = normalize_path_entry_str(entry);
    if cfg!(windows) {
        path_var
            .split(';')
            .any(|part| normalize_path_entry_str(part.trim()).eq_ignore_ascii_case(&entry_str))
    } else {
        path_var
            .split(':')
            .any(|part| normalize_path_entry_str(part.trim()).eq_ignore_ascii_case(&entry_str))
    }
}

pub fn persist_user_path(binary: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let dir = binary.parent().with_context(|| {
            format!(
                "Installed Nushell binary '{}' has no parent directory",
                binary.display()
            )
        })?;
        persist_path_dir(dir)
    }
    #[cfg(unix)]
    {
        // Same test harness skip as `persist_path_dir_*`: PathRestoreGuard sets
        // this so ignored acceptance tests cannot leave a dangling
        // `~/.local/bin/nu` or shell-profile export after a tempfile fixture.
        if std::env::var_os("NUMAN_TEST_NO_PERSIST_USER_PATH").is_some() {
            return Ok(());
        }
        // Match Windows `persist_path_dir` temp refuse: never durable-link a
        // tempfile-rooted binary into `~/.local/bin/nu`.
        if path_is_under_temp_dir(binary) {
            bail!(
                "Refusing to add temporary directory '{}' to the user PATH. \
                 Install or register a stable Nushell location instead.",
                binary.display()
            );
        }
        persist_user_path_unix(binary)?;
        ensure_local_bin_on_path()?;
        Ok(())
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = binary;
        Ok(())
    }
}

/// Add a directory to the user PATH persistently (Windows user PATH or Unix shell profile).
pub fn persist_path_dir(dir: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        persist_path_dir_windows(dir)
    }
    #[cfg(unix)]
    {
        persist_path_dir_unix(dir)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Resolve the directory to prepend/persist for `--use-existing`.
///
/// When the user passes a relative path with a parent (including symlinked
/// Homebrew-style bins), keep that parent. For bare filenames such as `nu`,
/// `input.parent()` is empty even though `canonicalize()` resolves correctly.
fn path_parent_for_registration(input: &Path, resolved: &Path) -> Result<PathBuf> {
    if let Some(parent) = input.parent() {
        if !parent.as_os_str().is_empty() {
            return Ok(parent
                .canonicalize()
                .unwrap_or_else(|_| parent.to_path_buf()));
        }
    }

    resolved
        .parent()
        .with_context(|| {
            format!(
                "Nushell binary '{}' has no parent directory",
                resolved.display()
            )
        })
        .map(|parent| parent.to_path_buf())
}

/// Re-export for callers that historically imported this from bootstrap.
pub use crate::util::confirm::hoisted_audit_message;

/// Register an existing Nushell binary: prepend its directory to PATH and persist when allowed.
pub fn register_existing_nu(binary: &Path, options: &NuSetupOptions) -> Result<PathBuf> {
    let input = binary.to_path_buf();
    let resolved = input
        .canonicalize()
        .with_context(|| format!("Failed to resolve Nushell binary '{}'", binary.display()))?;
    if !resolved.is_file() {
        bail!(
            "'{}' is not an executable file. Pass the path to an existing nu binary.",
            binary.display()
        );
    }

    validate_nushell_binary(&resolved)
        .with_context(|| format!("'{}' is not a runnable Nushell binary", binary.display()))?;

    let parent = path_parent_for_registration(input.as_path(), &resolved)?;
    let parent = normalize_path_entry(&parent);

    if options.caller_consented_destructive {
        // Audit trail for the hoisted consent: the caller (typically
        // `execute_use_path` / `execute_use_existing`) already collected
        // the destructive-step consent (managed-tree deletion + PATH add)
        // via `require_tty_or_yes` + `confirm_or_bail` before reaching here.
        // Direct callers (CLI subcommand and doctor repair) leave the flag
        // `false` and the inner prompt continues to fire, preserving
        // backward-compatible UX.
        eprintln!("{}", crate::util::confirm::hoisted_audit_message(&parent));
    } else {
        // Fail closed on non-TTY without `--yes`. `confirm_or_bail` alone
        // would auto-confirm and mutate PATH / active-version state.
        let is_tty = options
            .is_tty
            .unwrap_or_else(|| std::io::stdin().is_terminal());
        crate::util::confirm::require_tty_or_yes_with_tty(
            options.yes,
            "off-path Nu PATH registration",
            is_tty,
        )?;
        println!(
            "This will add '{}' to your user PATH so Nushell can be found.",
            parent.display()
        );
        crate::util::confirm::confirm_or_bail(
            "Proceed?",
            options.yes,
            "Nushell PATH setup cancelled.",
        )?;
    }

    // Refuse temporary roots before any process-global PATH mutation so a
    // later `persist_path_dir` refusal cannot leave PATH half-updated.
    // Unconditional on `skip_path`: even a session-only registration must not
    // put a temp dir on the process PATH. `persist_path_dir` short-circuits
    // under `NUMAN_TEST_NO_PERSIST_USER_PATH` (set by `PathRestoreGuard`), so
    // the half-update risk does not apply and ignored acceptance tests may
    // stage binaries under tempfile roots.
    if std::env::var_os("NUMAN_TEST_NO_PERSIST_USER_PATH").is_none()
        && path_is_under_temp_dir(&parent)
    {
        bail!(
            "Refusing to add temporary directory '{}' to the user PATH. \
             Install or register a stable Nushell location instead.",
            parent.display()
        );
    }

    prepend_process_path(&parent)?;
    if !options.skip_path {
        persist_path_dir(&parent)?;
        #[cfg(windows)]
        println!(
            "Added '{}' to your user PATH. Open a new terminal for PATH changes to apply everywhere.",
            parent.display()
        );
        #[cfg(unix)]
        println!(
            "Appended '{}' to your shell profile PATH. Restart your shell or open a new terminal.",
            parent.display()
        );
    } else {
        println!(
            "Skipped persistent PATH update. This session can use '{}'.",
            resolved.display()
        );
    }

    println!();
    println!("Next steps:");
    println!("  numan init");
    println!("  numan doctor");
    Ok(resolved)
}

#[cfg(windows)]
fn persist_path_dir_windows(dir: &Path) -> Result<()> {
    // Test harness sets this while PathRestoreGuard is held so ignored
    // acceptance tests cannot permanently pollute the developer User PATH.
    if std::env::var_os("NUMAN_TEST_NO_PERSIST_USER_PATH").is_some() {
        return Ok(());
    }
    let dir = normalize_path_entry(dir);
    // Refuse tempfile roots: test fixtures and one-off extracts must not land
    // on the durable User PATH (seen as Temp\.tmp*\off / existing-nu leaks).
    if path_is_under_temp_dir(&dir) {
        bail!(
            "Refusing to add temporary directory '{}' to the user PATH. \
             Install or register a stable Nushell location instead.",
            dir.display()
        );
    }
    let dir_str = dir
        .to_str()
        .with_context(|| format!("PATH entry '{}' is not valid UTF-8", dir.display()))?;
    let script = r#"$dir = $env:NUMAN_PATH_ENTRY; $current = [Environment]::GetEnvironmentVariable('Path', 'User'); if ($null -eq $current) { $current = '' }; if ($current.Split(';') -notcontains $dir) { [Environment]::SetEnvironmentVariable('Path', ($dir + ';' + $current), 'User') }"#;
    let output = std::process::Command::new("powershell")
        .env("NUMAN_PATH_ENTRY", dir_str)
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .context("Failed to invoke PowerShell to update user PATH")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to update user PATH: {stderr}");
    }
    Ok(())
}

fn path_is_under_temp_dir(dir: &Path) -> bool {
    path_is_under_temp_dir_with(dir, &std::env::temp_dir())
}

/// Returns true when `dir` sits under `temp_raw`, failing closed if either
/// path cannot be canonicalized (lexical `starts_with` fallback).
fn path_is_under_temp_dir_with(dir: &Path, temp_raw: &Path) -> bool {
    let Ok(temp) = temp_raw.canonicalize() else {
        // Fail closed: an unresolvable temp root must still refuse lexical
        // children (same fallback as an uncanonicalizable `dir`).
        return match dir.canonicalize() {
            Ok(d) => d.starts_with(temp_raw),
            Err(_) => dir.starts_with(temp_raw),
        };
    };
    let Ok(dir) = dir.canonicalize() else {
        // If the dir vanished, still treat literal temp prefixes as unsafe.
        return dir.starts_with(temp_raw);
    };
    dir.starts_with(&temp)
}

#[cfg(unix)]
fn shell_escape_for_double_quotes(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' | '"' | '$' | '`' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(unix)]
fn persist_path_dir_unix(dir: &Path) -> Result<()> {
    if std::env::var_os("NUMAN_TEST_NO_PERSIST_USER_PATH").is_some() {
        return Ok(());
    }
    if path_is_under_temp_dir(dir) {
        bail!(
            "Refusing to add temporary directory '{}' to the user PATH. \
             Install or register a stable Nushell location instead.",
            dir.display()
        );
    }
    let dir_str = dir
        .to_str()
        .with_context(|| format!("PATH entry '{}' is not valid UTF-8", dir.display()))?;
    let export_line = format!(
        r#"export PATH="{}:$PATH""#,
        shell_escape_for_double_quotes(dir_str)
    );
    append_shell_profile_line(&export_line, |content| content.contains(dir_str))
}

#[cfg(unix)]
fn persist_user_path_unix(binary: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;

    let home = dirs::home_dir().context("Could not resolve home directory for PATH setup")?;
    let local_bin = home.join(".local").join("bin");
    std::fs::create_dir_all(&local_bin)?;
    let link_path = local_bin.join("nu");
    let managed = binary.canonicalize().with_context(|| {
        format!(
            "Failed to resolve managed Nushell binary '{}'",
            binary.display()
        )
    })?;

    if link_path.exists() {
        if link_path.is_symlink() {
            let existing = std::fs::read_link(&link_path).with_context(|| {
                format!("Failed to read existing symlink '{}'", link_path.display())
            })?;
            let existing_resolved = if existing.is_absolute() {
                existing.canonicalize().ok()
            } else {
                link_path
                    .parent()
                    .and_then(|parent| parent.join(&existing).canonicalize().ok())
            };
            if existing_resolved.as_ref() == Some(&managed) {
                return Ok(());
            }
            bail!(
                "'{}' already points to another Nushell install ({}). \
                 Pass --skip-path to leave it unchanged.",
                link_path.display(),
                existing.display()
            );
        }
        bail!(
            "'{}' already exists and is not a symlink. \
             Pass --skip-path to leave it unchanged.",
            link_path.display()
        );
    }

    symlink(&managed, &link_path)
        .with_context(|| format!("Failed to symlink Nushell into '{}'", link_path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn ensure_local_bin_on_path() -> Result<()> {
    append_shell_profile_line(r##"export PATH="$HOME/.local/bin:$PATH""##, |content| {
        content.contains(".local/bin")
    })
}

#[cfg(unix)]
fn append_shell_profile_line(
    export_line: &str,
    already_present: impl Fn(&str) -> bool,
) -> Result<()> {
    let home = dirs::home_dir().context("Could not resolve home directory for PATH setup")?;
    for name in [".zshrc", ".bashrc", ".profile"] {
        let profile = home.join(name);
        if profile.exists() {
            assert_not_symlink(&profile, name)?;
        }
        if profile.is_file() {
            let content = std::fs::read_to_string(&profile)
                .with_context(|| format!("Failed to read shell profile '{}'", profile.display()))?;
            if already_present(&content) {
                return Ok(());
            }
            let updated = format!("{}\n{export_line}\n", content.trim_end());
            write_bytes_atomic(&profile, updated.as_bytes()).with_context(|| {
                format!("Failed to update shell profile '{}'", profile.display())
            })?;
            return Ok(());
        }
    }

    let profile = home.join(".profile");
    write_bytes_atomic(profile.as_path(), format!("{export_line}\n").as_bytes())
        .with_context(|| format!("Failed to create shell profile '{}'", profile.display()))?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NuSetupOptions {
    pub yes: bool,
    pub force: bool,
    pub skip_path: bool,
    /// When set, download this release tag instead of latest.
    pub version: Option<String>,
    /// When `true`, extract only the `nu` binary from the release archive,
    /// skipping bundled plugins. Default `false` extracts everything.
    pub minimal: bool,
    /// When `true`, the caller has already collected destructive-step consent
    /// (e.g. `cmd::setup::execute_use_path` / `execute_use_existing` already
    /// prompted for both the managed-tree deletion and the PATH add). The
    /// internal PATH-confirmation prompt in [`register_existing_nu`] is then
    /// suppressed and replaced with an audit log, so the user sees one prompt
    /// instead of two. Default `false` preserves the original two-prompt UX
    /// for direct callers (`numan setup nu use <path>`, hidden legacy flags,
    /// doctor off-PATH repair).
    pub caller_consented_destructive: bool,
    /// Override stdin TTY detection for the non-interactive guard (tests).
    /// `None` uses `stdin().is_terminal()`.
    pub is_tty: Option<bool>,
}

pub fn execute_nu_setup(
    root: &Path,
    platform: &Platform,
    options: &NuSetupOptions,
) -> Result<PathBuf> {
    if let Some(ref version) = options.version {
        let version = version.clone();
        let minimal = options.minimal;
        return execute_nu_setup_with_installer(root, platform, options, move |r, p| {
            install_version(r, p, &version, minimal)
        });
    }
    // Unpinned (latest) flow:
    // 1. Fail closed on non-TTY without `--yes` *before* any network so
    //    non-interactive callers never hit GitHub when they will be refused.
    // 2. Resolve the latest tag so the already-installed gate checks a real
    //    versioned dest and the install path marks active like a pinned run.
    // 3. Reuse the fetched release in the installer (no second API call).
    let is_tty = options
        .is_tty
        .unwrap_or_else(|| std::io::stdin().is_terminal());
    crate::util::confirm::require_tty_or_yes_with_tty(options.yes, "Nushell setup", is_tty)?;

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .user_agent(USER_AGENT)
        .build()
        .context("Failed to build HTTP client for latest Nushell release lookup")?;
    let release = fetch_latest_release(&client)
        .context("Failed to resolve latest Nushell release from GitHub")?;
    let tag = release.tag_name.clone();
    let pinned_opts = NuSetupOptions {
        version: Some(tag),
        ..options.clone()
    };
    let minimal = options.minimal;
    execute_nu_setup_with_installer(root, platform, &pinned_opts, move |r, p| {
        install_from_github_release(r, p, &release, minimal)
    })
}

pub fn execute_nu_setup_with_installer<F>(
    root: &Path,
    platform: &Platform,
    options: &NuSetupOptions,
    install: F,
) -> Result<PathBuf>
where
    F: FnOnce(&Path, &Platform) -> Result<PathBuf>,
{
    // Compute the normalized version once for pinned installs; reuse it
    // wherever the version is needed later (dest path, --yes active marker).
    let (dest, normalized_version) = match &options.version {
        Some(version) => {
            let normalized = version_manager::normalize_version(version)
                .with_context(|| format!("Failed to normalize requested version '{version}'"))?;
            let d = version_manager::version_binary(root, &normalized);
            (d, Some(normalized))
        }
        None => (managed_nu_binary(root), None),
    };
    let version_label = options
        .version
        .as_deref()
        .map(normalize_release_tag)
        .unwrap_or_else(|| "latest".to_string());

    // Already-installed gate: only short-circuit when the exact destination
    // binary exists. For pinned installs the dest is always a versioned path;
    // for the unpinned (latest) flow `execute_nu_setup` resolves the tag
    // first and passes it as a pinned version, so `dest` is also a versioned
    // path. The legacy `managed_nu_binary` placeholder is never a file in the
    // new versioned layout, so this gate is always accurate.
    if dest.is_file() && !options.force {
        // Resolve the effective installed binary so PATH prepend/persist
        // point at a real versioned file.
        let effective = {
            use version_manager::VersionManagerError;
            // Resolve active binary, tolerating only dangling-marker errors
            // (the binary is absent but the marker exists); other errors
            // (unreadable/malformed marker) must propagate.
            let from_active = match version_manager::active_nu_binary(root) {
                Ok(opt) => opt,
                Err(
                    VersionManagerError::DanglingActive { .. }
                    | VersionManagerError::DanglingActiveWithOffTree { .. },
                ) => None,
                Err(e) => {
                    return Err(anyhow::Error::from(e))
                        .context("Failed to read active Nu version for already-installed check")
                }
            };
            from_active
                .or_else(|| {
                    version_manager::latest_installed_version(root)
                        .ok()
                        .flatten()
                        .map(|v| version_manager::version_binary(root, &v))
                })
                .unwrap_or_else(|| dest.clone())
        };
        if options.yes && effective.is_file() {
            // PATH/marker mutation on the short-circuit path still needs a
            // PreMutation snapshot (same AGENTS.md boundary as install).
            snapshot_before_nu_setup(
                root,
                "Failed to create pre-mutation snapshot for existing `numan setup nu`",
            )?;
            let tools_dir = match effective.parent() {
                Some(parent) => parent.to_path_buf(),
                None => managed_nu_dir(root),
            };
            prepend_process_path(&tools_dir)?;
            if !options.skip_path {
                persist_user_path(&effective)?;
            }
            // Re-runs with `--yes` on an already-installed version still
            // record the version as active so `numan use list` stays
            // consistent.
            if let Some(ref normalized) = normalized_version {
                version_manager::write_active_version(root, normalized).with_context(|| {
                    format!("Failed to persist installed Nu version '{normalized}' as active")
                })?;
            }
            println!(
                "Nushell already installed at '{}' (unchanged).",
                effective.display()
            );
            return Ok(effective);
        }

        crate::util::confirm::confirm_or_bail(
            &format!(
                "Nushell is already installed at '{}'. Reinstall {version_label} release?",
                effective.display()
            ),
            false,
            "Nushell setup cancelled.",
        )?;
    }

    println!(
        "This will download the official Nushell {version_label} release for {} from GitHub.",
        platform.triple
    );
    // Refuse to proceed without explicit consent in non-interactive sessions.
    // Routes through `require_tty_or_yes` so the audit-grade eprintln output
    // is identical across every destructive setup entry — `cmd::setup::*`
    // (off-path registration, managed removal) and this download path share
    // one helper, one audit pattern, one source of truth. The pipe fallback
    // that `confirm_or_bail` would auto-promote is closed here instead.
    let is_tty = options
        .is_tty
        .unwrap_or_else(|| std::io::stdin().is_terminal());
    crate::util::confirm::require_tty_or_yes_with_tty(options.yes, "Nushell setup", is_tty)?;
    crate::util::confirm::confirm_or_bail("Proceed?", options.yes, "Nushell setup cancelled.")?;

    // Snapshot established state right before the download/install mutates the
    // filesystem. Same lifecycle boundary used by install/update/remove/activate
    // per AGENTS.md.
    snapshot_before_nu_setup(
        root,
        "Failed to create pre-install snapshot for `numan setup nu`",
    )?;

    let installed = install(root, platform)?;

    // Persist the freshly installed (pinned) version as the active version so
    // `numan use list` and downstream activation see it as the selected Nu.
    // Reuse the already-normalized version to avoid a second normalize call.
    if let Some(ref normalized) = normalized_version {
        version_manager::write_active_version(root, normalized).with_context(|| {
            format!(
                "Failed to persist installed Nu version '{}' as active",
                normalized
            )
        })?;

        // Discover bundled plugins when full extraction mode was used.
        if !options.minimal {
            let version_dir = version_manager::version_install_dir(root, normalized);
            discover_bundled_plugins(root, &version_dir, normalized)?;
        }
    }

    // With the versioned layout the binary lives at
    // `<root>/tools/nushell/<version>/<bin>`; prepend its parent directory
    // (not the bare `tools/nushell` root) so `nu` is actually findable on
    // the process PATH.
    let tools_dir = match installed.parent() {
        Some(parent) => parent.to_path_buf(),
        None => managed_nu_dir(root),
    };
    prepend_process_path(&tools_dir)?;
    if !options.skip_path {
        persist_user_path(&installed)?;
        #[cfg(windows)]
        println!(
            "Added '{}' to your user PATH. Open a new terminal for PATH changes to apply everywhere.",
            tools_dir.display()
        );
        #[cfg(unix)]
        println!(
            "Linked '{}' to ~/.local/bin/nu. Ensure ~/.local/bin is on your PATH.",
            installed.display()
        );
    } else {
        println!(
            "Skipped persistent PATH update. Numan will use '{}'.",
            installed.display()
        );
    }

    println!();
    println!("Next steps:");
    println!("  {}", crate::util::hints::CMD_INIT_REFRESH);
    println!("  numan doctor");
    println!(
        "  Re-activate packages you still want: {}",
        crate::util::hints::CMD_ACTIVATE
    );
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn fake_release() -> GitHubRelease {
        GitHubRelease {
            tag_name: "0.114.0".to_string(),
            assets: vec![GitHubAsset {
                name: "nu-0.114.0-x86_64-pc-windows-msvc.zip".to_string(),
                browser_download_url: "https://example.invalid/nu.zip".to_string(),
                size: 0,
                digest: None,
            }],
        }
    }

    #[test]
    fn select_release_asset_matches_platform_suffix() {
        let release = fake_release();
        let platform = Platform::detect();
        if platform.os != Os::Windows || platform.arch != Arch::X86_64 {
            return;
        }
        let asset = select_release_asset(&release, &platform).unwrap();
        assert!(asset.name.contains("x86_64-pc-windows-msvc.zip"));
    }

    #[test]
    fn normalize_release_tag_strips_v_prefix() {
        assert_eq!(normalize_release_tag("v0.113.1"), "0.113.1");
        assert_eq!(normalize_release_tag("0.113.1"), "0.113.1");
        assert_eq!(normalize_release_tag(" 0.114.0 "), "0.114.0");
    }

    #[test]
    fn select_release_asset_uses_tag_name_in_asset() {
        let release = GitHubRelease {
            tag_name: "0.113.1".to_string(),
            assets: vec![GitHubAsset {
                name: "nu-0.113.1-x86_64-unknown-linux-gnu.tar.gz".to_string(),
                browser_download_url: "https://example.invalid/nu.tar.gz".to_string(),
                size: 0,
                digest: None,
            }],
        };
        let platform = Platform {
            os: Os::Linux,
            arch: Arch::X86_64,
            env: Env::Gnu,
            triple: "x86_64-unknown-linux-gnu".to_string(),
        };
        let asset = select_release_asset(&release, &platform).unwrap();
        assert_eq!(asset.name, "nu-0.113.1-x86_64-unknown-linux-gnu.tar.gz");
    }

    #[test]
    fn path_parent_for_registration_uses_resolved_parent_for_bare_filename() {
        let dir = TempDir::new().unwrap();
        let nu_path = dir.path().join("nu");
        std::fs::write(&nu_path, b"fake").unwrap();
        let resolved = nu_path.canonicalize().unwrap();
        let parent = path_parent_for_registration(Path::new("nu"), &resolved).unwrap();
        assert_eq!(parent, resolved.parent().unwrap());
    }

    #[test]
    fn ensure_local_bin_export_line_is_well_quoted() {
        const EXPORT_LINE: &str = r##"export PATH="$HOME/.local/bin:$PATH""##;
        assert!(EXPORT_LINE.ends_with('"'));
        assert_eq!(EXPORT_LINE.matches('"').count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn shell_escape_for_double_quotes_escapes_metacharacters() {
        assert_eq!(
            shell_escape_for_double_quotes(r#"/opt/$HOME/bin"#),
            r#"/opt/\$HOME/bin"#
        );
    }

    #[cfg(windows)]
    #[test]
    fn normalize_path_entry_str_strips_verbatim_prefix() {
        let entry = format!("{VERBATIM_PATH_PREFIX}C:\\foo\\bin");
        assert_eq!(normalize_path_entry_str(&entry), "C:\\foo\\bin");
    }

    #[cfg(windows)]
    #[test]
    fn path_list_contains_matches_normalized_windows_paths() {
        let entry = format!("{VERBATIM_PATH_PREFIX}C:\\foo\\bin");
        let path_var = r"C:\foo\bin;C:\Windows";
        assert!(path_list_contains(path_var, &entry));
    }

    #[test]
    fn prepend_process_path_adds_canonical_dir() {
        use crate::util::test_paths::PathRestoreGuard;
        let _path_guard = PathRestoreGuard::new();
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("bin");
        std::fs::create_dir_all(&sub).unwrap();
        let canonical = std::fs::canonicalize(&sub).unwrap();
        let before = std::env::var("PATH").unwrap();
        prepend_process_path(&canonical).unwrap();
        let path_var = std::env::var("PATH").unwrap();
        let dir_str = normalize_path_entry(&canonical)
            .to_string_lossy()
            .into_owned();
        assert_ne!(
            before, path_var,
            "PATH should change when prepending a new directory"
        );
        assert!(
            path_list_contains(&path_var, &dir_str),
            "PATH should contain prepended directory; got PATH prefix: {}",
            {
                #[cfg(windows)]
                {
                    path_var.split(';').next().unwrap_or("")
                }
                #[cfg(not(windows))]
                {
                    path_var.split(':').next().unwrap_or("")
                }
            }
        );
    }

    #[test]
    fn install_from_zip_places_managed_binary() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let zip_path = root.join("nu-test.zip");

        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = ZipWriter::new(file);
            let options = SimpleFileOptions::default();
            let inner = format!("nu-0.0.0-test/{}", nu_binary_name());
            zip.start_file(&inner, options).unwrap();
            zip.write_all(b"fake nu binary").unwrap();
            zip.finish().unwrap();
        }

        let installed = install_from_archive(&zip_path, root, "0.0.0-test", false).unwrap();
        // Installs land in the VERSIONED layout, never the legacy
        // single-binary path (which is migration-only now).
        assert_eq!(
            installed,
            version_manager::version_binary(root, "0.0.0-test")
        );
        assert!(installed.is_file());
        assert!(
            !managed_nu_binary(root).exists(),
            "legacy single-binary path must not be produced by new installs"
        );
    }

    #[test]
    fn persist_path_dir_refuses_temp_directories() {
        use crate::util::test_paths::PathRestoreGuard;
        // Hold the PATH mutex so concurrent tests do not race on the env flag.
        let _guard = PathRestoreGuard::new();
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("off");
        std::fs::create_dir_all(&nested).unwrap();
        // Clear the test harness skip flag so we exercise the production refuse.
        std::env::remove_var("NUMAN_TEST_NO_PERSIST_USER_PATH");
        let err = persist_path_dir(&nested).expect_err("temp dirs must not be persisted");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("temporary directory") || msg.contains("Refusing"),
            "unexpected error: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn persist_user_path_honors_test_no_persist_flag() {
        use crate::util::test_paths::{HomeRestoreGuard, PathRestoreGuard};
        let _path_guard = PathRestoreGuard::new();
        let _home_guard = HomeRestoreGuard::new();
        let home = TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());

        let binary = home.path().join("fixture-nu");
        std::fs::write(&binary, b"fake").unwrap();
        persist_user_path(&binary).expect("flag must no-op durable Unix PATH writes");

        assert!(
            !home.path().join(".local").join("bin").join("nu").exists(),
            "must not create ~/.local/bin/nu while PathRestoreGuard is held"
        );
        // No shell-profile export either.
        for name in [".zshrc", ".bashrc", ".profile"] {
            assert!(
                !home.path().join(name).exists(),
                "must not create {name} while PathRestoreGuard is held"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn persist_user_path_refuses_temp_binaries_without_flag() {
        use crate::util::test_paths::PathRestoreGuard;
        let _guard = PathRestoreGuard::new();
        std::env::remove_var("NUMAN_TEST_NO_PERSIST_USER_PATH");
        let dir = TempDir::new().unwrap();
        let binary = dir.path().join("nu");
        std::fs::write(&binary, b"fake").unwrap();
        let err = persist_user_path(&binary).expect_err("temp binaries must not be persisted");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("temporary directory") || msg.contains("Refusing"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn path_is_under_temp_dir_fails_closed_when_temp_uncanonicalizable() {
        // A temp root that does not exist cannot be canonicalized; the helper
        // must still refuse lexical children (fail closed), not return false.
        let missing_temp =
            std::env::temp_dir().join(format!("numan-missing-temp-root-{}", std::process::id()));
        assert!(
            !missing_temp.exists(),
            "precondition: missing temp root must not exist"
        );
        let nested = missing_temp.join("off");
        assert!(
            path_is_under_temp_dir_with(&nested, &missing_temp),
            "lexical child of an uncanonicalizable temp root must be refused"
        );
        let outside = PathBuf::from(if cfg!(windows) {
            r"C:\Windows\System32"
        } else {
            "/usr/bin"
        });
        assert!(
            !path_is_under_temp_dir_with(&outside, &missing_temp),
            "unrelated paths must not match an uncanonicalizable temp root"
        );
    }

    #[test]
    fn nu_release_size_cap_exceeds_known_official_archive() {
        // Nu 0.114.1 x86_64-unknown-linux-gnu was ~279 MiB uncompressed and
        // tripped the previous 256 MiB bootstrap cap.
        assert!(
            NU_RELEASE_MAX_UNCOMPRESSED_BYTES > 279 * 1024 * 1024,
            "cap must clear known official Nu release sizes"
        );
        // minimal=true preserves the old filter behavior
        let cfg = nu_release_extract_config(true);
        assert_eq!(
            cfg.max_uncompressed_bytes,
            Some(NU_RELEASE_MAX_UNCOMPRESSED_BYTES)
        );
        assert_eq!(
            cfg.include.as_ref().unwrap(),
            &vec![format!("**/{}", nu_binary_name())]
        );
        // minimal=false extracts everything
        let cfg_full = nu_release_extract_config(false);
        assert_eq!(
            cfg_full.max_uncompressed_bytes,
            Some(NU_RELEASE_MAX_UNCOMPRESSED_BYTES)
        );
        assert!(cfg_full.include.is_none());
    }

    #[test]
    fn install_from_archive_minimal_skips_bundled_plugin_payloads() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let zip_path = root.join("nu-with-plugins.zip");

        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = ZipWriter::new(file);
            let options = SimpleFileOptions::default();
            let nu_inner = format!("nu-0.0.0-test/{}", nu_binary_name());
            zip.start_file(&nu_inner, options).unwrap();
            zip.write_all(b"fake nu binary").unwrap();
            zip.start_file("nu-0.0.0-test/nu_plugin_polars", options)
                .unwrap();
            zip.write_all(&vec![0u8; 64 * 1024]).unwrap();
            zip.finish().unwrap();
        }

        // Include filter must skip writing plugin payloads; bomb accounting
        // still charges them (covered in extract.rs).
        let extract_root = root.join("tools/.manual-extract");
        std::fs::create_dir_all(&extract_root).unwrap();
        let result = extract_archive(
            &zip_path,
            &extract_root,
            &ExtractConfig {
                include: Some(vec![format!("**/{}", nu_binary_name())]),
                ..ExtractConfig::default()
            },
            ArchiveFormat::Zip,
        )
        .expect("include-filtered extract must succeed under the default size cap");
        assert_eq!(result.files.len(), 1);
        assert!(result.files[0].ends_with(nu_binary_name()));
        assert!(
            !extract_root.join("nu-0.0.0-test/nu_plugin_polars").exists(),
            "bundled plugin must not be written to disk"
        );

        // With minimal=true, install_from_archive only extracts the nu binary
        let installed = install_from_archive(&zip_path, root, "0.0.0-plugins", true).unwrap();
        assert_eq!(
            std::fs::read(&installed).unwrap(),
            b"fake nu binary".as_slice()
        );
        // Plugin must NOT be present in the version directory
        let version_dir = version_manager::version_install_dir(root, "0.0.0-plugins");
        assert!(
            !version_dir.join("nu_plugin_polars").exists(),
            "minimal install must not extract bundled plugin"
        );
    }

    #[test]
    fn install_from_archive_full_extracts_bundled_plugins() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let zip_path = root.join("nu-with-plugins.zip");

        let plugin_content = vec![0xCAu8; 1024];
        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = ZipWriter::new(file);
            let options = SimpleFileOptions::default();
            let nu_inner = format!("nu-0.0.0-test/{}", nu_binary_name());
            zip.start_file(&nu_inner, options).unwrap();
            zip.write_all(b"fake nu binary").unwrap();
            zip.start_file("nu-0.0.0-test/nu_plugin_polars", options)
                .unwrap();
            zip.write_all(&plugin_content).unwrap();
            zip.start_file("nu-0.0.0-test/nu_plugin_formats", options)
                .unwrap();
            zip.write_all(b"formats content").unwrap();
            zip.finish().unwrap();
        }

        // With minimal=false (default), all files are extracted
        let installed = install_from_archive(&zip_path, root, "0.0.0-full", false).unwrap();
        assert_eq!(
            std::fs::read(&installed).unwrap(),
            b"fake nu binary".as_slice()
        );
        // Plugins must be present in the version directory
        let version_dir = version_manager::version_install_dir(root, "0.0.0-full");
        assert!(
            version_dir.join("nu_plugin_polars").exists(),
            "full install must extract bundled plugin polars"
        );
        assert!(
            version_dir.join("nu_plugin_formats").exists(),
            "full install must extract bundled plugin formats"
        );
        assert_eq!(
            std::fs::read(version_dir.join("nu_plugin_polars")).unwrap(),
            plugin_content
        );
    }

    #[test]
    fn install_from_archive_full_skips_non_plugin_files() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let zip_path = root.join("nu-with-extras.zip");

        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = ZipWriter::new(file);
            let options = SimpleFileOptions::default();
            let nu_inner = format!("nu-0.0.0-test/{}", nu_binary_name());
            zip.start_file(&nu_inner, options).unwrap();
            zip.write_all(b"fake nu binary").unwrap();
            zip.start_file("nu-0.0.0-test/nu_plugin_polars", options)
                .unwrap();
            zip.write_all(b"polars binary").unwrap();
            zip.start_file("nu-0.0.0-test/README.txt", options).unwrap();
            zip.write_all(b"readme content").unwrap();
            zip.start_file("nu-0.0.0-test/LICENSE", options).unwrap();
            zip.write_all(b"license content").unwrap();
            zip.finish().unwrap();
        }

        let installed = install_from_archive(&zip_path, root, "0.0.0-filter", false).unwrap();
        assert!(installed.is_file());

        let version_dir = version_manager::version_install_dir(root, "0.0.0-filter");
        // Plugin must be present
        assert!(
            version_dir.join("nu_plugin_polars").exists(),
            "plugin binary must be copied"
        );
        // Non-plugin files must NOT be present
        assert!(
            !version_dir.join("README.txt").exists(),
            "README.txt must not be copied to version directory"
        );
        assert!(
            !version_dir.join("LICENSE").exists(),
            "LICENSE must not be copied to version directory"
        );
    }

    #[test]
    fn discover_bundled_plugins_writes_lockfile_entries() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let zip_path = root.join("nu-with-plugins.zip");

        let plugin_content = b"fake polars binary";
        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = ZipWriter::new(file);
            let options = SimpleFileOptions::default();
            let nu_inner = format!("nu-0.0.0-test/{}", nu_binary_name());
            zip.start_file(&nu_inner, options).unwrap();
            zip.write_all(b"fake nu binary").unwrap();
            zip.start_file("nu-0.0.0-test/nu_plugin_polars", options)
                .unwrap();
            zip.write_all(plugin_content).unwrap();
            zip.start_file("nu-0.0.0-test/nu_plugin_query", options)
                .unwrap();
            zip.write_all(b"fake query binary").unwrap();
            zip.finish().unwrap();
        }

        // Full extraction
        install_from_archive(&zip_path, root, "0.114.0", false).unwrap();

        // Run discovery
        let version_dir = version_manager::version_install_dir(root, "0.114.0");
        discover_bundled_plugins(root, &version_dir, "0.114.0").unwrap();

        // Verify lockfile entries
        let lockfile = Lockfile::load(root).unwrap();
        let polars = lockfile
            .packages
            .get("nushell/polars")
            .expect("polars entry");
        assert_eq!(polars.package_type, "plugin");
        assert_eq!(polars.source, "binary");
        assert_eq!(polars.origin.as_deref(), Some(BUNDLED_NU_ORIGIN));
        assert_eq!(polars.executable_path.as_deref(), Some("nu_plugin_polars"));
        assert_eq!(polars.payload_path, "tools/nushell/0.114.0");
        assert_eq!(polars.version, "0.114.0");
        assert!(polars.executable_sha256.is_some());
        let expected_sha = integrity::compute_sha256(plugin_content);
        assert_eq!(
            polars.executable_sha256.as_deref(),
            Some(expected_sha.as_str())
        );

        let query = lockfile.packages.get("nushell/query").expect("query entry");
        assert_eq!(query.package_type, "plugin");
        assert_eq!(query.source, "binary");
        assert_eq!(query.origin.as_deref(), Some(BUNDLED_NU_ORIGIN));
        assert_eq!(query.executable_path.as_deref(), Some("nu_plugin_query"));
    }

    #[test]
    fn discover_bundled_plugins_skips_existing_registry_entry() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // Pre-populate a lockfile entry with a registry origin for nushell/polars
        let mut lockfile = Lockfile::load(root).unwrap();
        lockfile.packages.insert(
            "nushell/polars".to_string(),
            LockfileEntry {
                version: "0.114.0".to_string(),
                package_type: "plugin".to_string(),
                source: "binary".to_string(),
                target: None,
                artifact_url: Some("https://registry.example.com/polars.tar.gz".to_string()),
                artifact_sha256: Some("registry_sha256_value".to_string()),
                executable_path: Some("nu_plugin_polars".to_string()),
                archive_root: None,
                include: None,
                entry: None,
                installed_at: "0000000000000001".to_string(),
                nu_version_at_install: Some("0.114.0".to_string()),
                activation: None,
                registry_url: Some("https://registry.example.com".to_string()),
                registry_revision: Some("abc123".to_string()),
                index_sha256: None,
                signing_key_fingerprint: Some("fingerprint123".to_string()),
                git_url: None,
                git_rev: None,
                cargo_name: None,
                cargo_lock_sha256: None,
                built_sha256: None,
                payload_path: "packages/plugin/nushell/polars/0.114.0-abcd1234".to_string(),
                revision_id: None,
                payload_sha256: None,
                executable_sha256: Some("original_sha".to_string()),
                selection_reason: None,
                origin: Some("registry:official".to_string()),
                module_activation: None,
                module_import_mode: None,
                locked_dependencies: std::collections::BTreeMap::new(),
            },
        );
        lockfile.save(root).unwrap();

        // Create a version directory with a polars plugin binary
        let version_dir = version_manager::version_install_dir(root, "0.114.0");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join("nu_plugin_polars"), b"bundled polars").unwrap();
        std::fs::write(version_dir.join("nu_plugin_query"), b"bundled query").unwrap();

        // Run discovery - should skip polars (registry origin) but add query
        discover_bundled_plugins(root, &version_dir, "0.114.0").unwrap();

        // Verify nushell/polars was NOT overwritten
        let lockfile = Lockfile::load(root).unwrap();
        let polars = lockfile
            .packages
            .get("nushell/polars")
            .expect("polars entry must still exist");
        assert_eq!(
            polars.origin.as_deref(),
            Some("registry:official"),
            "registry origin must be preserved"
        );
        assert_eq!(
            polars.executable_sha256.as_deref(),
            Some("original_sha"),
            "original SHA must be preserved"
        );
        assert_eq!(
            polars.artifact_sha256.as_deref(),
            Some("registry_sha256_value"),
            "registry artifact SHA must be preserved"
        );
        assert_eq!(
            polars.signing_key_fingerprint.as_deref(),
            Some("fingerprint123"),
            "signing key fingerprint must be preserved"
        );

        // Verify nushell/query WAS added (no pre-existing entry)
        let query = lockfile
            .packages
            .get("nushell/query")
            .expect("query entry must be created");
        assert_eq!(query.origin.as_deref(), Some(BUNDLED_NU_ORIGIN));
        assert_eq!(query.executable_path.as_deref(), Some("nu_plugin_query"));
    }

    #[test]
    fn discover_bundled_plugins_overwrites_existing_bundled_entry() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // Pre-populate a lockfile entry with a bundled origin for nushell/polars
        let mut lockfile = Lockfile::load(root).unwrap();
        lockfile.packages.insert(
            "nushell/polars".to_string(),
            LockfileEntry {
                version: "0.113.0".to_string(),
                package_type: "plugin".to_string(),
                source: "binary".to_string(),
                target: None,
                artifact_url: None,
                artifact_sha256: None,
                executable_path: Some("nu_plugin_polars".to_string()),
                archive_root: None,
                include: None,
                entry: None,
                installed_at: "0000000000000001".to_string(),
                nu_version_at_install: Some("0.113.0".to_string()),
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
                payload_path: "tools/nushell/0.113.0".to_string(),
                revision_id: None,
                payload_sha256: None,
                executable_sha256: Some("old_bundled_sha".to_string()),
                selection_reason: None,
                origin: Some(BUNDLED_NU_ORIGIN.to_string()),
                module_activation: None,
                module_import_mode: None,
                locked_dependencies: std::collections::BTreeMap::new(),
            },
        );
        lockfile.save(root).unwrap();

        // Create a version directory with updated polars binary
        let version_dir = version_manager::version_install_dir(root, "0.114.0");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join("nu_plugin_polars"), b"newer polars").unwrap();

        // Run discovery - should update the existing bundled entry
        discover_bundled_plugins(root, &version_dir, "0.114.0").unwrap();

        // Verify nushell/polars WAS updated (same bundled origin)
        let lockfile = Lockfile::load(root).unwrap();
        let polars = lockfile
            .packages
            .get("nushell/polars")
            .expect("polars entry");
        assert_eq!(polars.origin.as_deref(), Some(BUNDLED_NU_ORIGIN));
        assert_eq!(polars.version, "0.114.0");
        let expected_sha = integrity::compute_sha256(b"newer polars");
        assert_eq!(
            polars.executable_sha256.as_deref(),
            Some(expected_sha.as_str())
        );
    }

    /// Manual smoke: `NUMAN_SMOKE_NU_ARCHIVE=/path/to/nu-*.tar.gz cargo test --lib \
    /// install_from_real_nu_release_archive_smoke -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn install_from_real_nu_release_archive_smoke() {
        let archive = match std::env::var_os("NUMAN_SMOKE_NU_ARCHIVE") {
            Some(p) => PathBuf::from(p),
            None => return,
        };
        assert!(
            archive.is_file(),
            "NUMAN_SMOKE_NU_ARCHIVE does not exist: {}",
            archive.display()
        );
        let dir = TempDir::new().unwrap();
        let installed = install_from_archive(&archive, dir.path(), "0.114.1", false).unwrap();
        assert!(installed.is_file());
        assert!(installed.ends_with(nu_binary_name()));
        // Must be the real shell binary, not a tiny plugin stub.
        assert!(
            std::fs::metadata(&installed).unwrap().len() > 1_000_000,
            "installed nu looks too small: {}",
            installed.display()
        );
    }

    #[test]
    fn execute_nu_setup_with_pinned_version_persists_active_marker() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let platform = Platform::detect();
        let options = NuSetupOptions {
            yes: true,
            force: false,
            skip_path: true,
            version: Some("0.113.1".to_string()),
            minimal: false,
            // Install path doesn't enter `register_existing_nu`, but the
            // initializer needs this field for the struct to compile.
            caller_consented_destructive: false,
            is_tty: None,
        };

        // Fake installer: write a versioned-style binary at tools/nushell/<v>/
        // and return its path. Mimics what `install_release` does after
        // `install_from_archive` succeeds.
        let result = execute_nu_setup_with_installer(root, &platform, &options, |r, _p| {
            let bin_dir = version_manager::version_install_dir(r, "0.113.1");
            std::fs::create_dir_all(&bin_dir).unwrap();
            let bin = version_manager::version_binary(r, "0.113.1");
            std::fs::write(&bin, b"fake nu").unwrap();
            Ok(bin)
        })
        .unwrap();

        assert!(result.is_file());

        // Active marker written by the orchestrator.
        let active = version_manager::read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");

        // And the resulting `numan use list` state includes the freshly installed
        // version.
        let listed = version_manager::list_installed_versions(root).unwrap();
        assert!(
            listed.contains(&"0.113.1".to_string()),
            "expected 0.113.1 in installed list, got: {:?}",
            listed
        );
    }

    /// When an older version (0.113.0) is already installed but the requested
    /// version (0.114.0) is not, the installer must be invoked for 0.114.0.
    #[test]
    fn setup_nu_yes_with_only_older_version_invokes_installer() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let platform = Platform::detect();

        // Pre-install an older version so `latest_installed_version` returns it.
        let old_bin_dir = version_manager::version_install_dir(root, "0.113.0");
        std::fs::create_dir_all(&old_bin_dir).unwrap();
        let old_bin = version_manager::version_binary(root, "0.113.0");
        std::fs::write(&old_bin, b"fake old nu").unwrap();

        let called = Arc::new(AtomicBool::new(false));
        let called_clone = Arc::clone(&called);

        let options = NuSetupOptions {
            yes: true,
            force: false,
            skip_path: true,
            version: Some("0.114.0".to_string()),
            minimal: false,
            caller_consented_destructive: false,
            is_tty: None,
        };

        // `dest` for 0.114.0 does not exist, so the short-circuit gate is not
        // entered and the installer closure must be called.
        let result = execute_nu_setup_with_installer(root, &platform, &options, move |r, _p| {
            called_clone.store(true, Ordering::SeqCst);
            let bin_dir = version_manager::version_install_dir(r, "0.114.0");
            std::fs::create_dir_all(&bin_dir).unwrap();
            let bin = version_manager::version_binary(r, "0.114.0");
            std::fs::write(&bin, b"fake new nu").unwrap();
            Ok(bin)
        });

        assert!(result.is_ok(), "setup should succeed: {:?}", result);
        assert!(
            called.load(Ordering::SeqCst),
            "installer must be called when the target version is not yet installed"
        );
    }

    /// A dangling active marker (pointing to a non-existent binary) must not
    /// prevent the already-installed short-circuit from working.  When the
    /// active marker is dangling but the requested version binary exists, `--yes`
    /// should succeed immediately without invoking the installer.
    #[test]
    fn latest_setup_short_circuit_ignores_dangling_active_marker() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let platform = Platform::detect();

        // Install 0.113.0 for real.
        let bin_dir = version_manager::version_install_dir(root, "0.113.0");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = version_manager::version_binary(root, "0.113.0");
        std::fs::write(&bin, b"fake nu").unwrap();

        // Write a dangling active marker pointing to 0.115.0 (never installed).
        version_manager::write_active_version(root, "0.115.0").unwrap();

        let called = Arc::new(AtomicBool::new(false));
        let called_clone = Arc::clone(&called);

        let options = NuSetupOptions {
            yes: true,
            force: false,
            skip_path: true,
            version: Some("0.113.0".to_string()),
            minimal: false,
            caller_consented_destructive: false,
            is_tty: None,
        };

        // `dest` for 0.113.0 exists, active marker is dangling → fall back to
        // latest_installed_version (0.113.0).  The `--yes` short-circuit should
        // fire and the installer closure must NOT be called.
        let result = execute_nu_setup_with_installer(root, &platform, &options, move |_r, _p| {
            called_clone.store(true, Ordering::SeqCst);
            panic!("installer must not be called when version is already installed");
        });

        assert!(
            result.is_ok(),
            "should short-circuit successfully: {:?}",
            result
        );
        assert!(
            !called.load(Ordering::SeqCst),
            "installer must not be called when the version is already installed"
        );
    }

    /// A malformed active-version marker (invalid JSON) must propagate as an
    /// error rather than being silently swallowed.
    #[test]
    fn corrupt_active_marker_propagates_error() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let platform = Platform::detect();

        // Install 0.113.0.
        let bin_dir = version_manager::version_install_dir(root, "0.113.0");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = version_manager::version_binary(root, "0.113.0");
        std::fs::write(&bin, b"fake nu").unwrap();

        // Write a corrupt (non-JSON) active marker.
        let nu_state_dir = root.join("nu_state");
        std::fs::create_dir_all(&nu_state_dir).unwrap();
        std::fs::write(nu_state_dir.join("active-version.json"), b"!!!not json!!!").unwrap();

        let options = NuSetupOptions {
            yes: true,
            force: false,
            skip_path: true,
            version: Some("0.113.0".to_string()),
            minimal: false,
            caller_consented_destructive: false,
            is_tty: None,
        };

        let result =
            execute_nu_setup_with_installer(root, &platform, &options, |_r, _p| unreachable!());
        assert!(
            result.is_err(),
            "corrupt active marker must produce an error, not be ignored"
        );
    }

    #[test]
    fn execute_nu_setup_refuses_non_tty_without_yes_and_skips_installer() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let platform = Platform::detect();
        let options = NuSetupOptions {
            yes: false,
            force: false,
            skip_path: true,
            version: Some("0.113.1".to_string()),
            minimal: false,
            caller_consented_destructive: false,
            is_tty: Some(false),
        };

        let err = execute_nu_setup_with_installer(root, &platform, &options, |_r, _p| {
            panic!("installer must not run when non-TTY guard refuses");
        })
        .expect_err("non-TTY without --yes must refuse before install");

        let msg = format!("{err:#}");
        assert!(
            msg.contains("non-interactive") || msg.contains("Refusing destructive"),
            "expected non-TTY refusal, got: {msg}"
        );
    }

    /// Unpinned (latest) setup must refuse non-TTY without `--yes` *before*
    /// any GitHub request. A failure that mentions DNS/HTTP would mean the
    /// network ran first.
    #[test]
    fn execute_nu_setup_unpinned_refuses_non_tty_without_yes_before_network() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let platform = Platform::detect();
        let options = NuSetupOptions {
            yes: false,
            force: false,
            skip_path: true,
            version: None,
            minimal: false,
            caller_consented_destructive: false,
            is_tty: Some(false),
        };

        let err = execute_nu_setup(root, &platform, &options)
            .expect_err("unpinned non-TTY without --yes must refuse before network");

        let msg = format!("{err:#}");
        assert!(
            msg.contains("non-interactive") || msg.contains("Refusing destructive"),
            "expected non-TTY refusal (not a network error), got: {msg}"
        );
        assert!(
            !msg.to_lowercase().contains("github")
                && !msg.to_lowercase().contains("http")
                && !msg.to_lowercase().contains("dns")
                && !msg.to_lowercase().contains("connect"),
            "refusal must not come from a network attempt: {msg}"
        );
    }

    #[test]
    fn register_existing_nu_refuses_non_tty_without_yes_before_path_mutation() {
        use crate::util::test_paths::PathRestoreGuard;

        // Serialize PATH mutations so concurrent tests cannot race on the
        // process-global environment. Acquire the guard before reading PATH
        // so the source scan is protected from concurrent PATH edits.
        let _path_guard = PathRestoreGuard::new();

        let nu_name = nu_binary_name();
        let Some(src) = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(nu_name))
                .find(|p| p.is_file() && validate_nushell_binary(p).is_ok())
        }) else {
            // Unit CI without Nu on PATH cannot exercise the post-validate gate.
            return;
        };

        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let existing_dir = dir.path().join("existing-nu");
        std::fs::create_dir_all(&existing_dir).unwrap();
        let existing = existing_dir.join(nu_name);
        std::fs::copy(&src, &existing).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&existing).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&existing, perms).unwrap();
        }

        // Run with an empty PATH to ensure the refusal happens before any
        // PATH or active-version mutation.
        std::env::set_var("PATH", "");
        let before_path = std::env::var_os("PATH");

        let options = NuSetupOptions {
            yes: false,
            force: false,
            skip_path: true,
            version: None,
            minimal: false,
            caller_consented_destructive: false,
            is_tty: Some(false),
        };

        let err = register_existing_nu(&existing, &options)
            .expect_err("non-TTY without --yes must refuse before PATH/active mutation");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("non-interactive") || msg.contains("Refusing destructive"),
            "expected non-TTY refusal, got: {msg}"
        );
        assert_eq!(
            std::env::var_os("PATH"),
            before_path,
            "PATH must be unchanged after refusal"
        );
        assert!(
            version_manager::read_active_version(root)
                .unwrap()
                .is_none(),
            "active-version marker must not be written after refusal"
        );
    }
}
