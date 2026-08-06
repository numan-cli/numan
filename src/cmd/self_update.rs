//! Self-update the `numan` binary from GitHub Releases (`numan update --self`).

use anyhow::{bail, Context, Result};
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
            InstallMethod::Cargo => Some("cargo install numan-cli"),
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
    execute_with_client(&HttpReleaseClient, check, verbose, CURRENT_VERSION)
}

/// Test seam: inject release client and current version string.
pub fn execute_with_client(
    client: &dyn ReleaseClient,
    check: bool,
    verbose: bool,
    current_version: &str,
) -> Result<()> {
    let exe = std::env::current_exe().context("Failed to resolve current numan executable path")?;
    let method = detect_install_method(&exe);

    if let Some(hint) = method.upgrade_hint() {
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
    let asset_name = release_asset_name(&latest.to_string(), &platform)?;

    if !is_newer_than(&latest, current_version)? {
        println!("numan is up to date ({current_version}).");
        return Ok(());
    }

    if check {
        println!("Update available: {current_version} → {latest}");
        println!("Run `numan update --self` to install.");
        return Ok(());
    }

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

    let temp = tempfile::tempdir().context("Failed to create temp dir for self-update")?;
    let archive_path = temp.path().join(&asset_name);
    let sums_path = temp.path().join("SHA256SUMS");

    println!("Downloading {asset_name}...");
    client
        .download(asset_url, &archive_path)
        .with_context(|| format!("Failed to download {asset_name}"))?;
    client
        .download(sums_url, &sums_path)
        .context("Failed to download SHA256SUMS")?;

    let sums_text = std::fs::read_to_string(&sums_path).context("Failed to read SHA256SUMS")?;
    let sums = parse_sha256sums(&sums_text);
    let expected = sums.get(&asset_name).with_context(|| {
        format!("SHA256SUMS does not list '{asset_name}'. Refusing to install.")
    })?;
    integrity::verify_and_report(&archive_path, expected, &asset_name)?;

    let new_bytes = extract_numan_binary(&archive_path, &asset_name, temp.path())?;
    let dest = std::fs::canonicalize(&exe).unwrap_or(exe);
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
    #[cfg(not(windows))]
    {
        replace_binary_unix(dest, new_bytes)
    }
}

#[cfg(unix)]
fn replace_binary_unix(dest: &Path, new_bytes: &[u8]) -> Result<()> {
    use crate::util::atomic::write_bytes_atomic;
    write_bytes_atomic(dest, new_bytes)
        .with_context(|| format!("Failed to replace numan binary at '{}'", dest.display()))?;
    make_executable(dest)?;
    Ok(())
}

#[cfg(windows)]
fn replace_binary_windows(dest: &Path, new_bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    // Running executables cannot be overwritten in place on Windows. Move the
    // current binary aside, write the new one, then best-effort delete the old.
    let backup = dest.with_extension("exe.old");
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(dest, &backup).with_context(|| {
        format!(
            "Failed to move running numan aside to '{}'",
            backup.display()
        )
    })?;
    let write_result = (|| -> Result<()> {
        let parent = dest.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent)
            .with_context(|| format!("Failed to create temp file in '{}'", parent.display()))?;
        tmp.write_all(new_bytes)
            .context("Failed to write new numan binary")?;
        tmp.flush().context("Failed to flush new numan binary")?;
        tmp.persist(dest).map_err(|e| {
            anyhow::anyhow!(
                "Failed to install new numan at '{}': {}",
                dest.display(),
                e.error
            )
        })?;
        Ok(())
    })();
    if let Err(e) = write_result {
        // Attempt to restore the previous binary.
        let _ = std::fs::rename(&backup, dest);
        return Err(e);
    }
    let _ = std::fs::remove_file(&backup);
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .with_context(|| format!("Failed to read permissions for '{}'", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("Failed to mark numan executable at '{}'", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

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
            detect_install_method(Path::new("/usr/local/bin/numan")),
            InstallMethod::Standalone
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
    fn is_newer_compares_semver() {
        let latest = parse_release_version("v0.2.1").unwrap();
        assert!(is_newer_than(&latest, "0.2.0").unwrap());
        assert!(!is_newer_than(&latest, "0.2.1").unwrap());
        assert!(!is_newer_than(&latest, "0.3.0").unwrap());
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

    #[test]
    fn check_reports_update_without_download() {
        // Only exercised when this test binary is detected as standalone.
        let exe = std::env::current_exe().unwrap();
        if detect_install_method(&exe) != InstallMethod::Standalone {
            return;
        }
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
        execute_with_client(&client, true, false, "0.2.0").unwrap();
        assert!(
            client.downloads.lock().unwrap().is_empty(),
            "check must not download"
        );
    }

    #[test]
    fn check_up_to_date_skips_download() {
        let exe = std::env::current_exe().unwrap();
        if detect_install_method(&exe) != InstallMethod::Standalone {
            return;
        }
        let client = FakeClient {
            tag: "v0.2.0".into(),
            assets: vec![],
            downloads: Mutex::new(Vec::new()),
            files: HashMap::new(),
        };
        execute_with_client(&client, true, false, "0.2.0").unwrap();
        assert!(client.downloads.lock().unwrap().is_empty());
    }
}
