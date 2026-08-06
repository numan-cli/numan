//! Self-update the `numan` binary from GitHub Releases (`numan update --self`).

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::core::integrity;
use crate::core::platform::{Arch, Env, Os, Platform};
use crate::install::download::download_file;
use crate::install::extract::{extract_archive, ArchiveFormat, ExtractConfig};

const RELEASES_LATEST: &str = "https://api.github.com/repos/tonythethompson/numan/releases/latest";
const USER_AGENT: &str = "numan-cli (https://github.com/tonythethompson/numan)";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Ed25519 public key (standard base64) that must sign `SHA256SUMS` for
/// `numan update --self`. The matching 32-byte seed is stored only as the
/// GitHub Actions secret `NUMAN_RELEASE_SIGNING_KEY` (see docs/RELEASING.md).
pub const RELEASE_SUMS_PUBLIC_KEY_B64: &str = "ZyxTCLZyE1xDNnxiHmkSlUe8Y1IIvFoT+XR/+PgVcpw=";

/// How this `numan` binary was installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Homebrew,
    Winget,
    Cargo,
    Standalone,
}

impl InstallMethod {
    pub fn upgrade_hint(self) -> Option<&'static str> {
        match self {
            InstallMethod::Homebrew => Some("brew upgrade numan"),
            InstallMethod::Winget => Some("winget upgrade tonythethompson.numan"),
            InstallMethod::Cargo => Some("cargo install --locked --force numan-cli"),
            InstallMethod::Standalone => None,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            InstallMethod::Homebrew => "Homebrew",
            InstallMethod::Winget => "winget",
            InstallMethod::Cargo => "cargo",
            InstallMethod::Standalone => "standalone",
        }
    }
}

/// Detect install channel from the running executable path (and its canonical target).
pub fn detect_install_method(exe: &Path) -> InstallMethod {
    let mut candidates = vec![normalize_path_str(exe)];
    if let Ok(canon) = std::fs::canonicalize(exe) {
        let n = normalize_path_str(&canon);
        if n != candidates[0] {
            candidates.push(n);
        }
    }

    for path in &candidates {
        if path.contains("/cellar/numan/")
            || path.contains("/opt/homebrew/")
            || path.contains("/home/linuxbrew/.linuxbrew/")
            || path.contains("/.linuxbrew/")
            || path.contains("/usr/local/cellar/numan/")
        {
            return InstallMethod::Homebrew;
        }
        if path.contains("/microsoft/winget/packages/")
            || path.contains("/microsoft/winget/links/")
            || path.contains("/winget/packages/")
            || path.contains("/winget/links/")
        {
            return InstallMethod::Winget;
        }
        if path.contains("/.cargo/bin/") {
            return InstallMethod::Cargo;
        }
    }

    InstallMethod::Standalone
}

fn normalize_path_str(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

/// Release archive name for this platform (must match `.github/workflows/release.yml`).
pub fn release_asset_name(version: &str, platform: &Platform) -> Result<String> {
    let version = version.trim().trim_start_matches('v');
    let (triple, ext) = match (platform.os, platform.arch, platform.env) {
        (Os::Linux, Arch::X86_64, Env::Gnu) => ("x86_64-unknown-linux-gnu", "tar.gz"),
        (Os::Windows, Arch::X86_64, Env::Msvc) => ("x86_64-pc-windows-msvc", "zip"),
        (Os::Macos, Arch::Aarch64, Env::Darwin) => ("aarch64-apple-darwin", "tar.gz"),
        _ => bail!(
            "No GitHub Release asset is published for platform triple '{}'. \
             Install or upgrade via Homebrew, winget, or cargo instead. \
             Supported self-update targets: x86_64-unknown-linux-gnu, \
             x86_64-pc-windows-msvc, aarch64-apple-darwin.",
            platform.triple
        ),
    };
    Ok(format!("numan-{version}-{triple}.{ext}"))
}

fn numan_binary_name() -> &'static str {
    if cfg!(windows) {
        "numan.exe"
    } else {
        "numan"
    }
}

/// Parse GNU `sha256sum` / SHA256SUMS lines into `filename -> hex hash`.
pub fn parse_sha256sums(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (hash, rest) = match line.split_once("  ").or_else(|| line.split_once('\t')) {
            Some(pair) => pair,
            None => match line.split_once(' ') {
                Some(pair) => pair,
                None => continue,
            },
        };
        let hash = hash.trim();
        if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let name = rest.trim().trim_start_matches('*').trim();
        if name.is_empty() {
            continue;
        }
        // Prefer basename if a path was recorded.
        let name = Path::new(name)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(name);
        map.insert(name.to_string(), hash.to_ascii_lowercase());
    }
    map
}

/// Strip a leading `v` and parse as semver.
pub fn parse_release_version(tag: &str) -> Result<semver::Version> {
    let cleaned = tag.trim().trim_start_matches('v');
    semver::Version::parse(cleaned).with_context(|| format!("Invalid release version '{tag}'"))
}

pub fn is_newer_than(latest: &semver::Version, current: &str) -> Result<bool> {
    let current = parse_release_version(current)?;
    Ok(latest > &current)
}

fn require_https(url: &str, what: &str) -> Result<()> {
    if !url.starts_with("https://") {
        bail!("{what} URL must use https (got '{url}')");
    }
    Ok(())
}

/// Verify a detached base64 Ed25519 signature over exact `SHA256SUMS` bytes.
pub fn verify_sha256sums_signature(
    sums_bytes: &[u8],
    signature_b64: &str,
    public_key_b64: &str,
) -> Result<()> {
    let key_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        public_key_b64.trim(),
    )
    .context("Invalid base64 release public key")?;
    if key_bytes.len() != 32 {
        bail!(
            "Release public key must be 32 bytes, got {}",
            key_bytes.len()
        );
    }
    let mut key_array = [0u8; 32];
    key_array.copy_from_slice(&key_bytes);
    let verifying_key =
        VerifyingKey::from_bytes(&key_array).context("Invalid Ed25519 release public key")?;

    let sig_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        signature_b64.trim(),
    )
    .context("Invalid base64 SHA256SUMS signature")?;
    if sig_bytes.len() != 64 {
        bail!(
            "Ed25519 SHA256SUMS signature must be 64 bytes, got {}",
            sig_bytes.len()
        );
    }
    let mut sig_array = [0u8; 64];
    sig_array.copy_from_slice(&sig_bytes);
    let signature = Signature::from_bytes(&sig_array);

    verifying_key.verify(sums_bytes, &signature).context(
        "SHA256SUMS signature verification failed. The checksum file may have been \
             tampered with, or this release was not signed with the Numan release key.",
    )?;
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
}

/// Injectable GitHub release client (tests supply fakes; production uses HTTP).
pub trait ReleaseClient {
    fn fetch_latest(&self) -> Result<(String, Vec<(String, String)>)>;
    fn download(&self, url: &str, dest: &Path) -> Result<()>;
}

pub struct HttpReleaseClient;

impl ReleaseClient for HttpReleaseClient {
    fn fetch_latest(&self) -> Result<(String, Vec<(String, String)>)> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .context("Failed to build HTTP client for self-update")?;
        let response = client
            .get(RELEASES_LATEST)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .send()
            .context("Failed to query numan releases on GitHub")?;
        if !response.status().is_success() {
            bail!("Failed to query numan releases: HTTP {}", response.status());
        }
        let body = response
            .text()
            .context("Failed to read numan release metadata from GitHub")?;
        let release: GitHubRelease = serde_json::from_str(&body)
            .context("Failed to parse numan release metadata from GitHub")?;
        let assets = release
            .assets
            .into_iter()
            .map(|a| (a.name, a.browser_download_url))
            .collect();
        Ok((release.tag_name, assets))
    }

    fn download(&self, url: &str, dest: &Path) -> Result<()> {
        download_file(url, dest)
    }
}

/// Run `numan update --self` (optionally `--check`).
pub fn execute(check: bool, verbose: bool) -> Result<()> {
    let exe = std::env::current_exe().context("Failed to resolve current numan executable path")?;
    execute_with_client(
        &HttpReleaseClient,
        &exe,
        check,
        verbose,
        CURRENT_VERSION,
        RELEASE_SUMS_PUBLIC_KEY_B64,
    )
}

/// Test seam: inject release client, executable path, version, and sums pubkey.
pub fn execute_with_client(
    client: &dyn ReleaseClient,
    exe: &Path,
    check: bool,
    verbose: bool,
    current_version: &str,
    sums_public_key_b64: &str,
) -> Result<()> {
    let method = detect_install_method(exe);

    if let Some(hint) = method.upgrade_hint() {
        if check {
            // Report whether a newer release exists, then point at the package manager.
            let (tag, _assets) = client
                .fetch_latest()
                .context("Failed to fetch latest numan release")?;
            let latest = parse_release_version(&tag)?;
            if !is_newer_than(&latest, current_version)? {
                println!("numan is up to date ({current_version}).");
                println!(
                    "This install is managed by {}; use that tool if you need to reinstall.",
                    method.display_name()
                );
                return Ok(());
            }
            println!("Update available: {current_version} → {latest}");
            println!(
                "This numan binary looks like a {} install.",
                method.display_name()
            );
            println!("Upgrade with:");
            println!("  {hint}");
            return Ok(());
        }
        println!(
            "This numan binary looks like a {} install.",
            method.display_name()
        );
        println!("Upgrade with:");
        println!("  {hint}");
        return Ok(());
    }

    let platform = Platform::detect();
    if verbose {
        eprintln!(
            "Self-update: current={current_version} platform={}",
            platform.triple
        );
    }

    let (tag, assets) = client
        .fetch_latest()
        .context("Failed to fetch latest numan release")?;
    let latest = parse_release_version(&tag)?;

    if !is_newer_than(&latest, current_version)? {
        println!("numan is up to date ({current_version}).");
        return Ok(());
    }

    if check {
        println!("Update available: {current_version} → {latest}");
        println!("Run `numan update --self` to install.");
        return Ok(());
    }

    // Asset naming is only needed on the apply path so --check works on every
    // detected platform (including triples without published release archives).
    let asset_name = release_asset_name(&latest.to_string(), &platform)?;

    let asset_url = assets
        .iter()
        .find(|(name, _)| name == &asset_name)
        .map(|(_, url)| url.as_str())
        .with_context(|| {
            format!(
                "Release {tag} has no asset named '{asset_name}'. \
                 Check https://github.com/tonythethompson/numan/releases"
            )
        })?;
    let sums_url = assets
        .iter()
        .find(|(name, _)| name == "SHA256SUMS")
        .map(|(_, url)| url.as_str())
        .context(
            "Release is missing SHA256SUMS. Refusing to self-update without checksum verification.",
        )?;
    let sig_url = assets
        .iter()
        .find(|(name, _)| name == "SHA256SUMS.sig")
        .map(|(_, url)| url.as_str())
        .context(
            "Release is missing SHA256SUMS.sig. Refusing to self-update without an \
             independently signed checksum file.",
        )?;
    require_https(asset_url, "Release asset")?;
    require_https(sums_url, "SHA256SUMS")?;
    require_https(sig_url, "SHA256SUMS.sig")?;

    let temp = tempfile::tempdir().context("Failed to create temp dir for self-update")?;
    let archive_path = temp.path().join(&asset_name);
    let sums_path = temp.path().join("SHA256SUMS");
    let sig_path = temp.path().join("SHA256SUMS.sig");

    println!("Downloading {asset_name}...");
    client
        .download(asset_url, &archive_path)
        .with_context(|| format!("Failed to download {asset_name}"))?;
    client
        .download(sums_url, &sums_path)
        .context("Failed to download SHA256SUMS")?;
    client
        .download(sig_url, &sig_path)
        .context("Failed to download SHA256SUMS.sig")?;

    let sums_bytes = std::fs::read(&sums_path).context("Failed to read SHA256SUMS")?;
    let sig_b64 = std::fs::read_to_string(&sig_path).context("Failed to read SHA256SUMS.sig")?;
    verify_sha256sums_signature(&sums_bytes, &sig_b64, sums_public_key_b64)?;

    let sums_text = String::from_utf8(sums_bytes).context("SHA256SUMS is not valid UTF-8")?;
    let sums = parse_sha256sums(&sums_text);
    let expected = sums.get(&asset_name).with_context(|| {
        format!("SHA256SUMS does not list '{asset_name}'. Refusing to install.")
    })?;
    integrity::verify_and_report(&archive_path, expected, &asset_name)?;

    let new_bytes = extract_numan_binary(&archive_path, &asset_name, temp.path())?;
    let dest = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    replace_binary(&dest, &new_bytes)?;

    println!("Updated numan: {current_version} → {latest}");
    Ok(())
}

fn extract_numan_binary(archive_path: &Path, asset_name: &str, work: &Path) -> Result<Vec<u8>> {
    let format = ArchiveFormat::from_url(asset_name)
        .with_context(|| format!("Unsupported self-update archive format for '{asset_name}'"))?;
    let extract_root = work.join("extract");
    std::fs::create_dir_all(&extract_root)?;
    extract_archive(
        archive_path,
        &extract_root,
        &ExtractConfig {
            max_uncompressed_bytes: Some(64 * 1024 * 1024),
            ..ExtractConfig::default()
        },
        format,
    )
    .with_context(|| format!("Failed to extract '{}'", archive_path.display()))?;

    let bin = locate_extracted_numan(&extract_root)?;
    std::fs::read(&bin).with_context(|| format!("Failed to read extracted '{}'", bin.display()))
}

fn locate_extracted_numan(extract_root: &Path) -> Result<PathBuf> {
    let name = numan_binary_name();
    let direct = extract_root.join(name);
    if direct.is_file() {
        return Ok(direct);
    }
    for entry in std::fs::read_dir(extract_root).with_context(|| {
        format!(
            "Failed to read extracted archive directory '{}'",
            extract_root.display()
        )
    })? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let candidate = path.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    bail!(
        "Could not find '{}' in extracted self-update archive under '{}'",
        name,
        extract_root.display()
    )
}

fn replace_binary(dest: &Path, new_bytes: &[u8]) -> Result<()> {
    if new_bytes.is_empty() {
        bail!("Extracted numan binary is empty");
    }

    #[cfg(windows)]
    {
        replace_binary_windows(dest, new_bytes)
    }
    #[cfg(unix)]
    {
        replace_binary_unix(dest, new_bytes)
    }
}

#[cfg(unix)]
fn replace_binary_unix(dest: &Path, new_bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    // Preserve existing mode bits (group/other), ensure owner-execute.
    // Apply mode on the staged temp file BEFORE renaming into place so a
    // permission failure never leaves dest replaced by a non-executable inode.
    let mode = std::fs::metadata(dest)
        .map(|m| m.permissions().mode())
        .unwrap_or(0o755)
        | 0o100;

    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create parent directory for '{}'", dest.display()))?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("Failed to create temp file in '{}'", parent.display()))?;
    staged
        .write_all(new_bytes)
        .context("Failed to write staged numan binary")?;
    staged
        .flush()
        .context("Failed to flush staged numan binary")?;

    let mut perms = std::fs::metadata(staged.path())
        .with_context(|| {
            format!(
                "Failed to read permissions for staged binary '{}'",
                staged.path().display()
            )
        })?
        .permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(staged.path(), perms).with_context(|| {
        format!(
            "Failed to set permissions on staged numan at '{}'",
            staged.path().display()
        )
    })?;

    staged.persist(dest).map_err(|e| {
        anyhow::anyhow!(
            "Failed to replace numan binary at '{}': {}",
            dest.display(),
            e.error
        )
    })?;
    Ok(())
}

#[cfg(windows)]
fn replace_binary_windows(dest: &Path, new_bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    // Stage the full replacement first, then move the running binary aside.
    // That way a write failure never leaves the destination missing.
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create parent directory for '{}'", dest.display()))?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("Failed to create temp file in '{}'", parent.display()))?;
    staged
        .write_all(new_bytes)
        .context("Failed to write new numan binary")?;
    staged.flush().context("Failed to flush new numan binary")?;

    let backup = dest.with_extension("exe.old");
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(dest, &backup).with_context(|| {
        format!(
            "Failed to move running numan aside to '{}'",
            backup.display()
        )
    })?;

    match staged.persist(dest) {
        Ok(_) => {
            let _ = std::fs::remove_file(&backup);
            Ok(())
        }
        Err(e) => match std::fs::rename(&backup, dest) {
            Ok(()) => Err(anyhow::anyhow!(
                "Failed to install new numan at '{}': {}",
                dest.display(),
                e.error
            )),
            Err(restore_err) => Err(anyhow::anyhow!(
                "Failed to install new numan at '{}': {}. \
                 Also failed to restore the previous binary from '{}': {}. \
                 Manually rename that backup back to '{}' to recover.",
                dest.display(),
                e.error,
                backup.display(),
                restore_err,
                dest.display()
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const STANDALONE_EXE: &str = "/usr/local/bin/numan";

    #[test]
    fn detect_homebrew_cellar() {
        assert_eq!(
            detect_install_method(Path::new("/opt/homebrew/Cellar/numan/0.2.0/bin/numan")),
            InstallMethod::Homebrew
        );
    }

    #[test]
    fn detect_homebrew_bin() {
        assert_eq!(
            detect_install_method(Path::new("/opt/homebrew/bin/numan")),
            InstallMethod::Homebrew
        );
    }

    #[test]
    fn detect_winget_packages() {
        assert_eq!(
            detect_install_method(Path::new(
                r"C:\Users\me\AppData\Local\Microsoft\WinGet\Packages\tonythethompson.numan_1.0.0\numan.exe"
            )),
            InstallMethod::Winget
        );
    }

    #[test]
    fn detect_cargo_bin() {
        assert_eq!(
            detect_install_method(Path::new("/home/me/.cargo/bin/numan")),
            InstallMethod::Cargo
        );
    }

    #[test]
    fn detect_standalone() {
        assert_eq!(
            detect_install_method(Path::new(STANDALONE_EXE)),
            InstallMethod::Standalone
        );
    }

    #[test]
    fn cargo_upgrade_hint_uses_locked_force() {
        assert_eq!(
            InstallMethod::Cargo.upgrade_hint(),
            Some("cargo install --locked --force numan-cli")
        );
    }

    #[test]
    fn release_asset_name_linux_gnu() {
        let p = Platform {
            os: Os::Linux,
            arch: Arch::X86_64,
            env: Env::Gnu,
            triple: "x86_64-unknown-linux-gnu".into(),
        };
        assert_eq!(
            release_asset_name("0.2.0", &p).unwrap(),
            "numan-0.2.0-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(
            release_asset_name("v0.2.0", &p).unwrap(),
            "numan-0.2.0-x86_64-unknown-linux-gnu.tar.gz"
        );
    }

    #[test]
    fn release_asset_name_windows() {
        let p = Platform {
            os: Os::Windows,
            arch: Arch::X86_64,
            env: Env::Msvc,
            triple: "x86_64-pc-windows-msvc".into(),
        };
        assert_eq!(
            release_asset_name("0.2.0", &p).unwrap(),
            "numan-0.2.0-x86_64-pc-windows-msvc.zip"
        );
    }

    #[test]
    fn release_asset_name_macos_arm() {
        let p = Platform {
            os: Os::Macos,
            arch: Arch::Aarch64,
            env: Env::Darwin,
            triple: "aarch64-apple-darwin".into(),
        };
        assert_eq!(
            release_asset_name("0.2.0", &p).unwrap(),
            "numan-0.2.0-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn release_asset_name_rejects_unsupported() {
        let p = Platform {
            os: Os::Macos,
            arch: Arch::X86_64,
            env: Env::Darwin,
            triple: "x86_64-apple-darwin".into(),
        };
        let err = release_asset_name("0.2.0", &p).unwrap_err().to_string();
        assert!(err.contains("No GitHub Release asset"), "{err}");
    }

    #[test]
    fn parse_sha256sums_gnu_format() {
        let text = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  numan-0.2.0-x86_64-unknown-linux-gnu.tar.gz
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  numan-0.2.0-x86_64-pc-windows-msvc.zip
";
        let map = parse_sha256sums(text);
        assert_eq!(
            map.get("numan-0.2.0-x86_64-unknown-linux-gnu.tar.gz")
                .unwrap(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(
            map.get("numan-0.2.0-x86_64-pc-windows-msvc.zip").unwrap(),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
    }

    #[test]
    fn parse_sha256sums_binary_mode_star() {
        let text =
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa *numan-0.2.0.tar.gz\n";
        let map = parse_sha256sums(text);
        assert_eq!(
            map.get("numan-0.2.0.tar.gz").unwrap(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[test]
    fn parse_sha256sums_rejects_short_and_non_hex() {
        let text = "\
abcd  short.tar.gz
notahex64charshere!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!  bad.tar.gz
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  good.tar.gz
";
        let map = parse_sha256sums(text);
        assert!(map.get("short.tar.gz").is_none());
        assert!(map.get("bad.tar.gz").is_none());
        assert!(map.get("good.tar.gz").is_some());
    }

    #[test]
    fn is_newer_compares_semver() {
        let latest = parse_release_version("v0.2.1").unwrap();
        assert!(is_newer_than(&latest, "0.2.0").unwrap());
        assert!(!is_newer_than(&latest, "0.2.1").unwrap());
        assert!(!is_newer_than(&latest, "0.3.0").unwrap());
    }

    #[test]
    fn parse_release_version_rejects_malformed() {
        let err = parse_release_version("not-a-version")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Invalid release version"), "{err}");
    }

    #[test]
    fn require_https_rejects_http() {
        let err = require_https("http://example.test/a", "asset")
            .unwrap_err()
            .to_string();
        assert!(err.contains("must use https"), "{err}");
    }

    #[test]
    fn locate_extracted_numan_at_root() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join(numan_binary_name());
        std::fs::write(&bin, b"bin").unwrap();
        assert_eq!(locate_extracted_numan(dir.path()).unwrap(), bin);
    }

    #[test]
    fn locate_extracted_numan_one_dir_deep() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("numan-0.2.0-x86_64-unknown-linux-gnu");
        std::fs::create_dir_all(&nested).unwrap();
        let bin = nested.join(numan_binary_name());
        std::fs::write(&bin, b"bin").unwrap();
        assert_eq!(locate_extracted_numan(dir.path()).unwrap(), bin);
    }

    #[test]
    fn locate_extracted_numan_absent() {
        let dir = tempfile::tempdir().unwrap();
        let err = locate_extracted_numan(dir.path()).unwrap_err().to_string();
        assert!(err.contains("Could not find"), "{err}");
    }

    #[test]
    fn replace_binary_rejects_empty_without_modifying_target() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(numan_binary_name());
        std::fs::write(&dest, b"old-bytes").unwrap();
        let err = replace_binary(&dest, b"").unwrap_err().to_string();
        assert!(err.contains("empty"), "{err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"old-bytes");
    }

    #[cfg(unix)]
    #[test]
    fn replace_binary_unix_preserves_mode_and_owner_execute() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(numan_binary_name());
        std::fs::write(&dest, b"old").unwrap();
        let mut perms = std::fs::metadata(&dest).unwrap().permissions();
        perms.set_mode(0o640);
        std::fs::set_permissions(&dest, perms).unwrap();

        replace_binary(&dest, b"new-binary").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new-binary");
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o740, "mode={mode:#o}");
    }

    struct FakeClient {
        tag: String,
        assets: Vec<(String, String)>,
        downloads: Mutex<Vec<String>>,
        files: HashMap<String, Vec<u8>>,
    }

    impl ReleaseClient for FakeClient {
        fn fetch_latest(&self) -> Result<(String, Vec<(String, String)>)> {
            Ok((self.tag.clone(), self.assets.clone()))
        }

        fn download(&self, url: &str, dest: &Path) -> Result<()> {
            self.downloads.lock().unwrap().push(url.to_string());
            let data = self
                .files
                .get(url)
                .with_context(|| format!("fake missing file for {url}"))?;
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(dest, data)?;
            Ok(())
        }
    }

    fn platform_asset_name(version: &str) -> Option<String> {
        release_asset_name(version, &Platform::detect()).ok()
    }

    fn test_signing_keypair() -> (String, ed25519_dalek::SigningKey) {
        use ed25519_dalek::SigningKey;
        use rand_core::OsRng;
        let signing_key = SigningKey::generate(&mut OsRng);
        let pub_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signing_key.verifying_key().as_bytes(),
        );
        (pub_b64, signing_key)
    }

    fn sign_sums(signing_key: &ed25519_dalek::SigningKey, sums: &[u8]) -> String {
        use ed25519_dalek::Signer;
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signing_key.sign(sums).to_bytes(),
        )
    }

    #[test]
    fn check_reports_update_without_download() {
        let client = FakeClient {
            tag: "v9.9.9".into(),
            assets: vec![
                (
                    "numan-9.9.9-x86_64-unknown-linux-gnu.tar.gz".into(),
                    "https://example.test/archive".into(),
                ),
                ("SHA256SUMS".into(), "https://example.test/sums".into()),
            ],
            downloads: Mutex::new(Vec::new()),
            files: HashMap::new(),
        };
        execute_with_client(
            &client,
            Path::new(STANDALONE_EXE),
            true,
            false,
            "0.2.0",
            "unused",
        )
        .unwrap();
        assert!(
            client.downloads.lock().unwrap().is_empty(),
            "check must not download"
        );
    }

    #[test]
    fn check_up_to_date_skips_download() {
        let client = FakeClient {
            tag: "v0.2.0".into(),
            assets: vec![],
            downloads: Mutex::new(Vec::new()),
            files: HashMap::new(),
        };
        execute_with_client(
            &client,
            Path::new(STANDALONE_EXE),
            true,
            false,
            "0.2.0",
            "unused",
        )
        .unwrap();
        assert!(client.downloads.lock().unwrap().is_empty());
    }

    #[test]
    fn managed_install_prints_hint_without_fetch() {
        let client = FakeClient {
            tag: "v9.9.9".into(),
            assets: vec![],
            downloads: Mutex::new(Vec::new()),
            files: HashMap::new(),
        };
        execute_with_client(
            &client,
            Path::new("/opt/homebrew/bin/numan"),
            false,
            false,
            "0.2.0",
            "unused",
        )
        .unwrap();
        assert!(client.downloads.lock().unwrap().is_empty());
    }

    #[test]
    fn managed_install_check_reports_update_then_hint() {
        let client = FakeClient {
            tag: "v9.9.9".into(),
            assets: vec![],
            downloads: Mutex::new(Vec::new()),
            files: HashMap::new(),
        };
        execute_with_client(
            &client,
            Path::new("/opt/homebrew/bin/numan"),
            true,
            false,
            "0.2.0",
            "unused",
        )
        .unwrap();
        // check fetches release metadata but must not download assets
        assert!(client.downloads.lock().unwrap().is_empty());
    }

    #[test]
    fn managed_install_check_reports_up_to_date() {
        let client = FakeClient {
            tag: "v0.2.0".into(),
            assets: vec![],
            downloads: Mutex::new(Vec::new()),
            files: HashMap::new(),
        };
        execute_with_client(
            &client,
            Path::new("/home/me/.cargo/bin/numan"),
            true,
            false,
            "0.2.0",
            "unused",
        )
        .unwrap();
        assert!(client.downloads.lock().unwrap().is_empty());
    }

    #[test]
    fn apply_rejects_missing_platform_asset() {
        let Some(_) = platform_asset_name("9.9.9") else {
            // Unsupported host triple: apply path correctly fails at asset naming.
            let client = FakeClient {
                tag: "v9.9.9".into(),
                assets: vec![("SHA256SUMS".into(), "https://example.test/sums".into())],
                downloads: Mutex::new(Vec::new()),
                files: HashMap::new(),
            };
            let err = execute_with_client(
                &client,
                Path::new(STANDALONE_EXE),
                false,
                false,
                "0.2.0",
                "unused",
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("No GitHub Release asset"), "{err}");
            return;
        };

        let client = FakeClient {
            tag: "v9.9.9".into(),
            assets: vec![("SHA256SUMS".into(), "https://example.test/sums".into())],
            downloads: Mutex::new(Vec::new()),
            files: HashMap::new(),
        };
        let err = execute_with_client(
            &client,
            Path::new(STANDALONE_EXE),
            false,
            false,
            "0.2.0",
            "unused",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("has no asset named"), "{err}");
        assert!(client.downloads.lock().unwrap().is_empty());
    }

    #[test]
    fn apply_rejects_missing_sha256sums_asset() {
        let Some(asset_name) = platform_asset_name("9.9.9") else {
            return;
        };
        let client = FakeClient {
            tag: "v9.9.9".into(),
            assets: vec![(asset_name, "https://example.test/archive".into())],
            downloads: Mutex::new(Vec::new()),
            files: HashMap::new(),
        };
        let err = execute_with_client(
            &client,
            Path::new(STANDALONE_EXE),
            false,
            false,
            "0.2.0",
            "unused",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("missing SHA256SUMS"), "{err}");
    }

    #[test]
    fn apply_rejects_missing_sha256sums_sig() {
        let Some(asset_name) = platform_asset_name("9.9.9") else {
            return;
        };
        let client = FakeClient {
            tag: "v9.9.9".into(),
            assets: vec![
                (asset_name, "https://example.test/archive".into()),
                ("SHA256SUMS".into(), "https://example.test/sums".into()),
            ],
            downloads: Mutex::new(Vec::new()),
            files: HashMap::new(),
        };
        let err = execute_with_client(
            &client,
            Path::new(STANDALONE_EXE),
            false,
            false,
            "0.2.0",
            "unused",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("missing SHA256SUMS.sig"), "{err}");
    }

    #[test]
    fn apply_rejects_asset_absent_from_sha256sums() {
        let Some(asset_name) = platform_asset_name("9.9.9") else {
            return;
        };
        let (pub_b64, signing_key) = test_signing_keypair();
        let archive_url = "https://example.test/archive";
        let sums_url = "https://example.test/sums";
        let sig_url = "https://example.test/sums.sig";
        let sums =
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  other.tar.gz\n"
                .to_vec();
        let sig = sign_sums(&signing_key, &sums);
        let mut files = HashMap::new();
        files.insert(archive_url.to_string(), b"archive-bytes".to_vec());
        files.insert(sums_url.to_string(), sums);
        files.insert(sig_url.to_string(), sig.into_bytes());
        let client = FakeClient {
            tag: "v9.9.9".into(),
            assets: vec![
                (asset_name.clone(), archive_url.into()),
                ("SHA256SUMS".into(), sums_url.into()),
                ("SHA256SUMS.sig".into(), sig_url.into()),
            ],
            downloads: Mutex::new(Vec::new()),
            files,
        };
        let dir = tempfile::tempdir().unwrap();
        let fake_exe = dir.path().join(numan_binary_name());
        std::fs::write(&fake_exe, b"old").unwrap();
        let err = execute_with_client(&client, &fake_exe, false, false, "0.2.0", &pub_b64)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("SHA256SUMS does not list") || err.contains(&asset_name),
            "{err}"
        );
        assert_eq!(std::fs::read(&fake_exe).unwrap(), b"old");
    }

    #[test]
    fn apply_rejects_checksum_mismatch() {
        let Some(asset_name) = platform_asset_name("9.9.9") else {
            return;
        };
        let (pub_b64, signing_key) = test_signing_keypair();
        let archive_url = "https://example.test/archive";
        let sums_url = "https://example.test/sums";
        let sig_url = "https://example.test/sums.sig";
        let wrong = "0000000000000000000000000000000000000000000000000000000000000000";
        let sums = format!("{wrong}  {asset_name}\n").into_bytes();
        let sig = sign_sums(&signing_key, &sums);
        let mut files = HashMap::new();
        files.insert(archive_url.to_string(), b"archive-bytes".to_vec());
        files.insert(sums_url.to_string(), sums);
        files.insert(sig_url.to_string(), sig.into_bytes());
        let client = FakeClient {
            tag: "v9.9.9".into(),
            assets: vec![
                (asset_name, archive_url.into()),
                ("SHA256SUMS".into(), sums_url.into()),
                ("SHA256SUMS.sig".into(), sig_url.into()),
            ],
            downloads: Mutex::new(Vec::new()),
            files,
        };
        let dir = tempfile::tempdir().unwrap();
        let fake_exe = dir.path().join(numan_binary_name());
        std::fs::write(&fake_exe, b"old").unwrap();
        let err = execute_with_client(&client, &fake_exe, false, false, "0.2.0", &pub_b64)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Integrity check failed"), "{err}");
        assert_eq!(std::fs::read(&fake_exe).unwrap(), b"old");
    }

    #[test]
    fn apply_rejects_bad_sums_signature() {
        let Some(asset_name) = platform_asset_name("9.9.9") else {
            return;
        };
        let (pub_b64, _signing_key) = test_signing_keypair();
        let (_other_pub, other_key) = test_signing_keypair();
        let archive_url = "https://example.test/archive";
        let sums_url = "https://example.test/sums";
        let sig_url = "https://example.test/sums.sig";
        let sums = format!(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  {asset_name}\n"
        )
        .into_bytes();
        // Sign with a different key than the injected verifying key.
        let sig = sign_sums(&other_key, &sums);
        let mut files = HashMap::new();
        files.insert(archive_url.to_string(), b"archive-bytes".to_vec());
        files.insert(sums_url.to_string(), sums);
        files.insert(sig_url.to_string(), sig.into_bytes());
        let client = FakeClient {
            tag: "v9.9.9".into(),
            assets: vec![
                (asset_name, archive_url.into()),
                ("SHA256SUMS".into(), sums_url.into()),
                ("SHA256SUMS.sig".into(), sig_url.into()),
            ],
            downloads: Mutex::new(Vec::new()),
            files,
        };
        let err = execute_with_client(
            &client,
            Path::new(STANDALONE_EXE),
            false,
            false,
            "0.2.0",
            &pub_b64,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("signature verification failed"), "{err}");
    }

    #[test]
    fn apply_rejects_non_https_asset_url() {
        let Some(asset_name) = platform_asset_name("9.9.9") else {
            return;
        };
        let client = FakeClient {
            tag: "v9.9.9".into(),
            assets: vec![
                (asset_name, "http://example.test/archive".into()),
                ("SHA256SUMS".into(), "https://example.test/sums".into()),
                (
                    "SHA256SUMS.sig".into(),
                    "https://example.test/sums.sig".into(),
                ),
            ],
            downloads: Mutex::new(Vec::new()),
            files: HashMap::new(),
        };
        let err = execute_with_client(
            &client,
            Path::new(STANDALONE_EXE),
            false,
            false,
            "0.2.0",
            "unused",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("must use https"), "{err}");
        assert!(client.downloads.lock().unwrap().is_empty());
    }

    #[test]
    fn verify_sha256sums_signature_round_trip() {
        let (pub_b64, signing_key) = test_signing_keypair();
        let sums = b"deadbeef  numan.tar.gz\n";
        let sig = sign_sums(&signing_key, sums);
        verify_sha256sums_signature(sums, &sig, &pub_b64).unwrap();
        let err = verify_sha256sums_signature(b"tampered", &sig, &pub_b64)
            .unwrap_err()
            .to_string();
        assert!(err.contains("signature verification failed"), "{err}");
    }
}
