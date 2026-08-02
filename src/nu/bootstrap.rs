//! Download and install an official Nushell release binary under the Numan root.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::core::integrity;
use crate::core::platform::{Arch, Env, Os, Platform};
use crate::install::download::download_file;
use crate::install::extract::{extract_archive, ArchiveFormat, ExtractConfig};
use crate::nu::paths::validate_nushell_binary;
use crate::nu::version_manager;
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
#[cfg(unix)]
use crate::util::atomic::write_bytes_atomic;
#[cfg(unix)]
use crate::util::fs_safety::assert_not_symlink;

const RELEASES_LATEST: &str = "https://api.github.com/repos/nushell/nushell/releases/latest";
const RELEASES_TAGS_BASE: &str = "https://api.github.com/repos/nushell/nushell/releases/tags/";
const USER_AGENT: &str = "numan-cli (https://github.com/tonythethompson/numan)";

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

pub fn install_from_archive(archive_path: &Path, root: &Path, version: &str) -> Result<PathBuf> {
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
        &ExtractConfig {
            max_uncompressed_bytes: Some(256 * 1024 * 1024),
            ..ExtractConfig::default()
        },
        format,
    )
    .with_context(|| format!("Failed to extract '{}'", archive_path.display()))?;

    let source = locate_extracted_nu_binary(&extract_root)?;
    // Write into the versioned layout (`<root>/tools/nushell/<version>/nu`) so
    // a single install action never clobbers another installed version. Install
    // is a pure payload write: the caller (`execute_nu_setup_with_installer`)
    // owns active-marker persistence; per AGENTS.md, only `activate`/`deactivate`
    // modify Nu integration state.
    let normalized = version_manager::normalize_version(version)
        .with_context(|| format!("Invalid Nu version '{}' for installation", version))?;
    let dest_dir = version_manager::version_install_dir(root, &normalized);
    std::fs::create_dir_all(&dest_dir).with_context(|| {
        format!(
            "Failed to create managed Nushell version directory '{}'",
            dest_dir.display()
        )
    })?;
    let dest = version_manager::version_binary(root, &normalized);

    std::fs::copy(&source, &dest).with_context(|| {
        format!(
            "Failed to copy Nushell binary from '{}' to '{}'",
            source.display(),
            dest.display()
        )
    })?;
    make_executable(&dest)?;
    std::fs::write(dest_dir.join("VERSION"), normalized.as_bytes())
        .with_context(|| format!("Failed to write VERSION file in '{}'", dest_dir.display()))?;
    Ok(dest)
}

struct ExtractCleanup(PathBuf);

impl Drop for ExtractCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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

pub fn install_latest(root: &Path, platform: &Platform) -> Result<PathBuf> {
    install_release(root, platform, None)
}

/// Download and install a specific Nushell release tag (e.g. `0.113.1`).
pub fn install_version(root: &Path, platform: &Platform, version: &str) -> Result<PathBuf> {
    install_release(root, platform, Some(version))
}

fn install_release(root: &Path, platform: &Platform, version: Option<&str>) -> Result<PathBuf> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .user_agent(USER_AGENT)
        .build()?;
    let release = match version {
        Some(v) => fetch_release_by_tag(&client, v)?,
        None => fetch_latest_release(&client)?,
    };
    let asset = select_release_asset(&release, platform)?;

    let cache_dir = root.join("tools/.cache");
    std::fs::create_dir_all(&cache_dir)?;
    let archive_path = cache_dir.join(&asset.name);

    println!("Downloading Nushell {} ({})…", release.tag_name, asset.name);
    download_file(&asset.browser_download_url, &archive_path)?;
    verify_downloaded_archive(&archive_path, asset)?;

    let installed = install_from_archive(&archive_path, root, &release.tag_name)?;
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

/// Audit-trail text for hoisted destructive-step consent.
///
/// Locked behind a pure helper so future hoist surfaces can call the same
/// formatter (`numan setup nu remove`, `numan setup nu install <v>` one-shot
/// swap, `numan doctor --fix` repair paths) without silently copy-editing the
/// text across four-ish call sites. The literal string is golden-tested in
/// `tests/setup_nu_test.rs::register_existing_nu_audit_text_is_stable` so any
/// accidental rename or punctuation drift fails CI immediately.
pub fn hoisted_audit_message(parent: &Path) -> String {
    format!(
        "(audit) prompt hoisted; skipping internal PATH-confirmation prompt \
         for '{}' (caller has already gathered destructive-step consent).",
        parent.display()
    )
}

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

    if options.caller_consented_destructive {
        // Audit trail for the hoisted consent: the caller (typically
        // `execute_use_path` / `execute_use_existing`) already collected
        // the destructive-step consent (managed-tree deletion + PATH add)
        // via its own `confirm_or_bail(...)` before reaching this point.
        // Direct callers (CLI subcommand and doctor --fix) leave the flag
        // `false` and the inner prompt continues to fire, preserving
        // backward-compatible UX.
        eprintln!("{}", hoisted_audit_message(&parent));
    } else {
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
    let dir = normalize_path_entry(dir);
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

#[derive(Debug, Clone)]
pub struct NuSetupOptions {
    pub yes: bool,
    pub force: bool,
    pub skip_path: bool,
    /// When set, download this release tag instead of latest.
    pub version: Option<String>,
    /// When `true`, the caller has already collected destructive-step consent
    /// (e.g. `cmd::setup::execute_use_path` / `execute_use_existing` already
    /// prompted for both the managed-tree deletion and the PATH add). The
    /// internal PATH-confirmation prompt in [`register_existing_nu`] is then
    /// suppressed and replaced with an audit log, so the user sees one prompt
    /// instead of two. Default `false` preserves the original two-prompt UX
    /// for direct callers (`numan setup nu use <path>`, hidden legacy flags,
    /// `numan doctor --fix`'s off-PATH repair).
    pub caller_consented_destructive: bool,
}

pub fn execute_nu_setup(
    root: &Path,
    platform: &Platform,
    options: &NuSetupOptions,
) -> Result<PathBuf> {
    if let Some(ref version) = options.version {
        let version = version.clone();
        return execute_nu_setup_with_installer(root, platform, options, move |r, p| {
            install_version(r, p, &version)
        });
    }
    execute_nu_setup_with_installer(root, platform, options, install_latest)
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
    let dest = managed_nu_binary(root);
    let version_label = options
        .version
        .as_deref()
        .map(normalize_release_tag)
        .unwrap_or_else(|| "latest".to_string());

    if dest.is_file() && !options.force {
        if options.yes {
            let tools_dir = managed_nu_dir(root);
            prepend_process_path(&tools_dir)?;
            if !options.skip_path {
                persist_user_path(&dest)?;
            }
            println!(
                "Nushell already installed at '{}' (unchanged).",
                dest.display()
            );
            return Ok(dest);
        }

        crate::util::confirm::confirm_or_bail(
            &format!(
                "Nushell is already installed at '{}'. Reinstall {version_label} release?",
                dest.display()
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
    crate::util::confirm::require_tty_or_yes(options.yes, "Nushell setup")?;
    crate::util::confirm::confirm_or_bail("Proceed?", options.yes, "Nushell setup cancelled.")?;

    // Snapshot established state right before the download/install mutates the
    // filesystem. Same lifecycle boundary used by install/update/remove/activate
    // per AGENTS.md.
    create_snapshot(
        root,
        SnapshotReason::PreMutation,
        SnapshotTrigger::Install,
        None,
        None,
    )
    .with_context(|| "Failed to create pre-install snapshot for `numan setup nu`")?;

    let installed = install(root, platform)?;

    // Persist the freshly installed (pinned) version as the active version so
    // `numan use list` and downstream activation see it as the selected Nu.
    // `latest` is left to `numan use latest` — the user explicitly resolves the
    // release tag there.
    if let Some(version) = &options.version {
        let pinned = version_manager::normalize_version(version)
            .with_context(|| format!("Failed to normalize requested version '{}'", version))?;
        version_manager::write_active_version(root, &pinned).with_context(|| {
            format!(
                "Failed to persist installed Nu version '{}' as active",
                pinned
            )
        })?;
    }

    let tools_dir = managed_nu_dir(root);
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
    println!("  Re-activate packages you still want: numan activate");
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

        let installed = install_from_archive(&zip_path, root, "0.0.0-test").unwrap();
        // install_from_archive must place the binary in the versioned layout
        // (`tools/nushell/<version>/nu`), not the legacy single-binary
        // `managed_nu_binary` location. Asserting against `version_binary`
        // here means the test would catch a regression that flips us back to
        // clobbering every installed version on every install.
        assert_eq!(
            installed,
            version_manager::version_binary(root, "0.0.0-test")
        );
        assert!(installed.is_file());
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
            // Install path doesn't enter `register_existing_nu`, but the
            // initializer needs this field for the struct to compile.
            caller_consented_destructive: false,
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
}
