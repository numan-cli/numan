//! Tool presets and GitHub release binary installer for CLI shell integrations.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::core::platform::{Arch, Env, Os, Platform};
use crate::install::download::download_file;
use crate::install::extract::{extract_archive, ArchiveFormat, ExtractConfig};
use crate::nu::bootstrap::{persist_path_dir, prepend_process_path};

const USER_AGENT: &str = "numan-cli (https://github.com/tonythethompson/numan)";

#[derive(Debug, Clone)]
pub struct ToolPreset {
    pub name: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
    pub init_command: &'static str,
    pub binary_name: &'static str,
    pub github_repo: &'static str,
    pub is_direct_binary: bool,
}

pub const KNOWN_TOOLS: &[ToolPreset] = &[
    ToolPreset {
        name: "starship",
        display_name: "Starship",
        description: "The minimal, blazing-fast, and infinitely customizable prompt for any shell!",
        init_command: "starship init nu",
        binary_name: "starship",
        github_repo: "starship/starship",
        is_direct_binary: false,
    },
    ToolPreset {
        name: "zoxide",
        display_name: "Zoxide",
        description: "A smarter cd command for your terminal",
        init_command: "zoxide init nushell",
        binary_name: "zoxide",
        github_repo: "ajeetdsouza/zoxide",
        is_direct_binary: false,
    },
    ToolPreset {
        name: "carapace",
        display_name: "Carapace",
        description: "Multi-shell multi-command completion generator",
        init_command: "carapace _carapace nushell",
        binary_name: "carapace",
        github_repo: "carapace-sh/carapace-bin",
        is_direct_binary: false,
    },
    ToolPreset {
        name: "atuin",
        display_name: "Atuin",
        description: "Magical shell history across terminals and machines",
        init_command: "atuin init nu",
        binary_name: "atuin",
        github_repo: "atuinsh/atuin",
        is_direct_binary: false,
    },
    ToolPreset {
        name: "mise",
        display_name: "Mise",
        description: "Polyglot dev tool manager, environment variables, and task runner",
        init_command: "mise activate nu",
        binary_name: "mise",
        github_repo: "jdx/mise",
        is_direct_binary: false,
    },
    ToolPreset {
        name: "direnv",
        display_name: "Direnv",
        description: "Unclutter your .profile and load directory environment variables",
        init_command: "direnv hook nu",
        binary_name: "direnv",
        github_repo: "direnv/direnv",
        is_direct_binary: true,
    },
    ToolPreset {
        name: "oh-my-posh",
        display_name: "Oh My Posh",
        description: "A prompt theme engine for any shell",
        init_command: "oh-my-posh init nu",
        binary_name: "oh-my-posh",
        github_repo: "JanDeDobbeleer/oh-my-posh",
        is_direct_binary: true,
    },
];

pub fn find_preset(name: &str) -> Option<&'static ToolPreset> {
    let lower = name.to_ascii_lowercase();
    KNOWN_TOOLS.iter().find(|t| t.name == lower)
}

pub fn tools_bin_dir(root: &Path) -> PathBuf {
    root.join("tools").join("bin")
}

pub fn binary_file_name(base: &str) -> String {
    if cfg!(windows) && !base.ends_with(".exe") {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// Search PATH and `$NUMAN_ROOT/tools/bin` for an executable binary.
pub fn find_binary_on_path(base_name: &str, root: Option<&Path>) -> Option<PathBuf> {
    let target = binary_file_name(base_name);

    if let Some(r) = root {
        let in_tools = tools_bin_dir(r).join(&target);
        if in_tools.is_file() {
            return Some(in_tools);
        }
    }

    let path_var = std::env::var("PATH").unwrap_or_default();
    #[cfg(windows)]
    let separator = ';';
    #[cfg(not(windows))]
    let separator = ':';

    for dir in path_var.split(separator) {
        let trimmed = dir.trim();
        if trimmed.is_empty() {
            continue;
        }
        let candidate = Path::new(trimmed).join(&target);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
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
    #[serde(default)]
    #[allow(dead_code)]
    size: u64,
}

#[allow(unreachable_patterns)]
fn matches_tool_asset(tool: &ToolPreset, asset_name: &str, platform: &Platform) -> bool {
    let name = asset_name.to_ascii_lowercase();

    match tool.name {
        "starship" => match (platform.os, platform.arch) {
            (Os::Windows, Arch::X86_64) => name.contains("x86_64-pc-windows-msvc.zip"),
            (Os::Windows, Arch::Aarch64) => name.contains("aarch64-pc-windows-msvc.zip"),
            (Os::Linux, Arch::X86_64) => {
                let libc = match platform.env {
                    Env::Musl => "musl",
                    _ => "gnu",
                };
                name.contains(&format!("x86_64-unknown-linux-{libc}")) && name.ends_with(".tar.gz")
            }
            (Os::Linux, Arch::Aarch64) => {
                let libc = match platform.env {
                    Env::Musl => "musl",
                    _ => "gnu",
                };
                name.contains(&format!("aarch64-unknown-linux-{libc}")) && name.ends_with(".tar.gz")
            }
            (Os::Macos, Arch::X86_64) => name.contains("x86_64-apple-darwin.tar.gz"),
            (Os::Macos, Arch::Aarch64) => name.contains("aarch64-apple-darwin.tar.gz"),
            _ => false,
        },
        "zoxide" => match (platform.os, platform.arch) {
            (Os::Windows, Arch::X86_64) => name.contains("x86_64-pc-windows-msvc.zip"),
            (Os::Linux, Arch::X86_64) => {
                let libc = match platform.env {
                    Env::Musl => "musl",
                    _ => "gnu",
                };
                name.contains(&format!("x86_64-unknown-linux-{libc}")) && name.ends_with(".tar.gz")
            }
            (Os::Linux, Arch::Aarch64) => {
                let libc = match platform.env {
                    Env::Musl => "musl",
                    _ => "gnu",
                };
                name.contains(&format!("aarch64-unknown-linux-{libc}")) && name.ends_with(".tar.gz")
            }
            (Os::Macos, Arch::X86_64) => name.contains("x86_64-apple-darwin.tar.gz"),
            (Os::Macos, Arch::Aarch64) => name.contains("aarch64-apple-darwin.tar.gz"),
            _ => false,
        },
        "carapace" => match (platform.os, platform.arch) {
            (Os::Windows, Arch::X86_64) => name.contains("windows_amd64.zip"),
            (Os::Windows, Arch::Aarch64) => name.contains("windows_arm64.zip"),
            (Os::Linux, Arch::X86_64) => name.contains("linux_amd64.tar.gz"),
            (Os::Linux, Arch::Aarch64) => name.contains("linux_arm64.tar.gz"),
            (Os::Macos, Arch::X86_64) => name.contains("darwin_amd64.tar.gz"),
            (Os::Macos, Arch::Aarch64) => name.contains("darwin_arm64.tar.gz"),
            _ => false,
        },
        "atuin" => match (platform.os, platform.arch) {
            (Os::Windows, Arch::X86_64) => name.contains("x86_64-pc-windows-msvc.zip"),
            (Os::Linux, Arch::X86_64) => {
                let libc = match platform.env {
                    Env::Musl => "musl",
                    _ => "gnu",
                };
                name.contains(&format!("x86_64-unknown-linux-{libc}")) && name.ends_with(".tar.gz")
            }
            (Os::Linux, Arch::Aarch64) => {
                let libc = match platform.env {
                    Env::Musl => "musl",
                    _ => "gnu",
                };
                name.contains(&format!("aarch64-unknown-linux-{libc}")) && name.ends_with(".tar.gz")
            }
            (Os::Macos, Arch::X86_64) => name.contains("x86_64-apple-darwin.tar.gz"),
            (Os::Macos, Arch::Aarch64) => name.contains("aarch64-apple-darwin.tar.gz"),
            _ => false,
        },
        "mise" => match (platform.os, platform.arch) {
            (Os::Windows, Arch::X86_64) => name.contains("win-x64.zip"),
            (Os::Windows, Arch::Aarch64) => name.contains("win-arm64.zip"),
            (Os::Linux, Arch::X86_64) => name.contains("linux-x64.tar.gz"),
            (Os::Linux, Arch::Aarch64) => name.contains("linux-arm64.tar.gz"),
            (Os::Macos, Arch::X86_64) => name.contains("macos-x64.tar.gz"),
            (Os::Macos, Arch::Aarch64) => name.contains("macos-arm64.tar.gz"),
            _ => false,
        },
        "direnv" => match (platform.os, platform.arch) {
            (Os::Windows, Arch::X86_64) => name.contains("windows-amd64"),
            (Os::Windows, Arch::Aarch64) => name.contains("windows-arm64"),
            (Os::Linux, Arch::X86_64) => name == "direnv.linux-amd64",
            (Os::Linux, Arch::Aarch64) => name == "direnv.linux-arm64",
            (Os::Macos, Arch::X86_64) => name == "direnv.darwin-amd64",
            (Os::Macos, Arch::Aarch64) => name == "direnv.darwin-arm64",
            _ => false,
        },
        "oh-my-posh" => match (platform.os, platform.arch) {
            (Os::Windows, Arch::X86_64) => name == "posh-windows-amd64.exe",
            (Os::Windows, Arch::Aarch64) => name == "posh-windows-arm64.exe",
            (Os::Linux, Arch::X86_64) => name == "posh-linux-amd64",
            (Os::Linux, Arch::Aarch64) => name == "posh-linux-arm64",
            (Os::Macos, Arch::X86_64) => name == "posh-darwin-amd64",
            (Os::Macos, Arch::Aarch64) => name == "posh-darwin-arm64",
            _ => false,
        },
        _ => false,
    }
}

fn fetch_latest_release(repo: &str) -> Result<GitHubRelease> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent(USER_AGENT)
        .build()
        .context("Failed to build HTTP client for tool download")?;

    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let mut request = client
        .get(&url)
        .header("Accept", "application/vnd.github+json");

    // Authenticate when a GitHub token is available for higher rate limits.
    if let Ok(token) = std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN")) {
        if !token.is_empty() {
            request = request.bearer_auth(&token);
        }
    }

    let response = request
        .send()
        .with_context(|| format!("Failed to fetch release metadata for {repo}"))?;

    if response.status().as_u16() == 403 || response.status().as_u16() == 429 {
        bail!(
            "GitHub API rate limit exceeded while fetching releases for {repo}. \
             Set GITHUB_TOKEN or GH_TOKEN to authenticate and increase the limit."
        );
    }

    if !response.status().is_success() {
        bail!(
            "Failed to fetch release for {repo}: HTTP {}",
            response.status()
        );
    }

    let text = response.text()?;
    serde_json::from_str::<GitHubRelease>(&text)
        .with_context(|| format!("Failed to parse release JSON for {repo}"))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .with_context(|| format!("Failed to read metadata for '{}'", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("Failed to mark '{}' executable", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn find_extracted_binary(extract_dir: &Path, expected_name: &str) -> Result<PathBuf> {
    let bin_target = binary_file_name(expected_name);
    let direct = extract_dir.join(&bin_target);
    if direct.is_file() {
        return Ok(direct);
    }

    // Search 2 levels deep
    for entry in std::fs::read_dir(extract_dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_file() && p.file_name().and_then(|n| n.to_str()) == Some(&bin_target) {
            return Ok(p);
        }
        if p.is_dir() {
            for sub in std::fs::read_dir(&p)? {
                let sub = sub?;
                let sub_p = sub.path();
                if sub_p.is_file()
                    && sub_p.file_name().and_then(|n| n.to_str()) == Some(&bin_target)
                {
                    return Ok(sub_p);
                }
            }
        }
    }

    bail!(
        "Could not find '{}' in extracted archive at '{}'",
        bin_target,
        extract_dir.display()
    )
}

/// Download and install a tool preset from GitHub into `$NUMAN_ROOT/tools/bin`.
pub fn download_and_install_tool(
    tool: &ToolPreset,
    root: &Path,
    platform: &Platform,
) -> Result<PathBuf> {
    let release = fetch_latest_release(tool.github_repo)?;
    let asset = release
        .assets
        .iter()
        .find(|a| matches_tool_asset(tool, &a.name, platform))
        .with_context(|| {
            format!(
                "No release asset found for {} ({}) on platform {}",
                tool.display_name, release.tag_name, platform.triple
            )
        })?;

    let cache_dir = root.join("tools").join(".cache");
    std::fs::create_dir_all(&cache_dir)?;
    let sanitized_name = asset
        .name
        .chars()
        .filter(|c| *c != '/' && *c != '\\' && *c != ':')
        .collect::<String>();
    if sanitized_name.is_empty() {
        bail!(
            "Release asset name for {} is empty after sanitization",
            tool.display_name
        );
    }
    let download_dest = cache_dir.join(&sanitized_name);

    println!(
        "Downloading {} {} ({})…",
        tool.display_name, release.tag_name, asset.name
    );
    download_file(&asset.browser_download_url, &download_dest)?;

    // Validate downloaded file size when the release metadata provides one.
    if asset.size > 0 {
        let actual = std::fs::metadata(&download_dest)
            .with_context(|| format!("Failed to stat downloaded '{}'", download_dest.display()))?
            .len();
        if actual != asset.size {
            bail!(
                "Downloaded {} asset '{}' has {} bytes but release metadata reports {} bytes",
                tool.display_name,
                asset.name,
                actual,
                asset.size
            );
        }
    }

    let bin_dir = tools_bin_dir(root);
    std::fs::create_dir_all(&bin_dir)?;
    let final_dest = bin_dir.join(binary_file_name(tool.binary_name));

    if tool.is_direct_binary {
        std::fs::copy(&download_dest, &final_dest).with_context(|| {
            format!(
                "Failed to copy {} to {}",
                download_dest.display(),
                final_dest.display()
            )
        })?;
    } else {
        let extract_dir = cache_dir.join(format!(".extract-{}", tool.name));
        if extract_dir.exists() {
            let _ = std::fs::remove_dir_all(&extract_dir);
        }
        std::fs::create_dir_all(&extract_dir)?;

        let format = ArchiveFormat::from_url(&asset.name).with_context(|| {
            format!("Unsupported archive format for tool asset '{}'", asset.name)
        })?;

        extract_archive(
            &download_dest,
            &extract_dir,
            &ExtractConfig::default(),
            format,
        )?;

        let extracted_bin = find_extracted_binary(&extract_dir, tool.binary_name)?;
        std::fs::copy(&extracted_bin, &final_dest).with_context(|| {
            format!(
                "Failed to copy extracted binary from '{}' to '{}'",
                extracted_bin.display(),
                final_dest.display()
            )
        })?;

        let _ = std::fs::remove_dir_all(&extract_dir);
    }

    make_executable(&final_dest)?;

    // Add tools bin to PATH
    prepend_process_path(&bin_dir)?;
    if let Err(err) = persist_path_dir(&bin_dir) {
        eprintln!(
            "Warning: failed to persist '{}' on PATH: {err:#}. \
             Add it manually to keep {} available in new shells.",
            bin_dir.display(),
            tool.binary_name
        );
    }

    println!(
        "Installed {} {} to '{}'.",
        tool.display_name,
        release.tag_name,
        final_dest.display()
    );

    Ok(final_dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_preset() {
        assert!(find_preset("starship").is_some());
        assert!(find_preset("STARSHIP").is_some());
        assert!(find_preset("zoxide").is_some());
        assert!(find_preset("carapace").is_some());
        assert!(find_preset("atuin").is_some());
        assert!(find_preset("mise").is_some());
        assert!(find_preset("direnv").is_some());
        assert!(find_preset("oh-my-posh").is_some());
        assert!(find_preset("unknown-tool").is_none());
    }

    #[test]
    fn test_asset_matching_starship() {
        let linux_x64_gnu = Platform {
            triple: "x86_64-unknown-linux-gnu".to_string(),
            os: Os::Linux,
            arch: Arch::X86_64,
            env: crate::core::platform::Env::Gnu,
        };
        let linux_x64_musl = Platform {
            triple: "x86_64-unknown-linux-musl".to_string(),
            os: Os::Linux,
            arch: Arch::X86_64,
            env: crate::core::platform::Env::Musl,
        };
        let win_x64 = Platform {
            triple: "x86_64-pc-windows-msvc".to_string(),
            os: Os::Windows,
            arch: Arch::X86_64,
            env: crate::core::platform::Env::Msvc,
        };
        let starship = find_preset("starship").unwrap();

        // GNU Linux matches gnu asset
        assert!(matches_tool_asset(
            starship,
            "starship-x86_64-unknown-linux-gnu.tar.gz",
            &linux_x64_gnu
        ));
        // GNU Linux rejects musl asset
        assert!(!matches_tool_asset(
            starship,
            "starship-x86_64-unknown-linux-musl.tar.gz",
            &linux_x64_gnu
        ));
        // Musl Linux matches musl asset
        assert!(matches_tool_asset(
            starship,
            "starship-x86_64-unknown-linux-musl.tar.gz",
            &linux_x64_musl
        ));
        // Musl Linux rejects gnu asset
        assert!(!matches_tool_asset(
            starship,
            "starship-x86_64-unknown-linux-gnu.tar.gz",
            &linux_x64_musl
        ));
        assert!(matches_tool_asset(
            starship,
            "starship-x86_64-pc-windows-msvc.zip",
            &win_x64
        ));
        assert!(!matches_tool_asset(
            starship,
            "starship-x86_64-apple-darwin.tar.gz",
            &win_x64
        ));
    }

    #[test]
    fn test_asset_matching_direnv_exact_names() {
        let linux_x64 = Platform {
            triple: "x86_64-unknown-linux-gnu".to_string(),
            os: Os::Linux,
            arch: Arch::X86_64,
            env: crate::core::platform::Env::Gnu,
        };
        let darwin_arm64 = Platform {
            triple: "aarch64-apple-darwin".to_string(),
            os: Os::Macos,
            arch: Arch::Aarch64,
            env: crate::core::platform::Env::Darwin,
        };
        let direnv = find_preset("direnv").unwrap();

        assert!(matches_tool_asset(direnv, "direnv.linux-amd64", &linux_x64));
        assert!(!matches_tool_asset(
            direnv,
            "direnv.linux-amd64.tar.gz",
            &linux_x64
        ));
        assert!(matches_tool_asset(
            direnv,
            "direnv.darwin-arm64",
            &darwin_arm64
        ));
    }

    #[test]
    fn test_asset_matching_oh_my_posh_exact_names() {
        let linux_x64 = Platform {
            triple: "x86_64-unknown-linux-gnu".to_string(),
            os: Os::Linux,
            arch: Arch::X86_64,
            env: crate::core::platform::Env::Gnu,
        };
        let win_x64 = Platform {
            triple: "x86_64-pc-windows-msvc".to_string(),
            os: Os::Windows,
            arch: Arch::X86_64,
            env: crate::core::platform::Env::Msvc,
        };
        let omp = find_preset("oh-my-posh").unwrap();

        assert!(matches_tool_asset(omp, "posh-linux-amd64", &linux_x64));
        assert!(!matches_tool_asset(omp, "posh-linux-amd64.exe", &linux_x64));
        assert!(matches_tool_asset(omp, "posh-windows-amd64.exe", &win_x64));
    }

    #[test]
    fn test_asset_matching_empty_assets_yields_no_match() {
        let linux_x64 = Platform {
            triple: "x86_64-unknown-linux-gnu".to_string(),
            os: Os::Linux,
            arch: Arch::X86_64,
            env: crate::core::platform::Env::Gnu,
        };
        let starship = find_preset("starship").unwrap();
        let empty: Vec<String> = vec![];
        assert!(empty
            .iter()
            .find(|a| matches_tool_asset(starship, a, &linux_x64))
            .is_none());
    }
}
