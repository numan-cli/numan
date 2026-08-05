//! Detect Numan binaries installed via different package managers and gate
//! cross-channel installs behind an interactive uninstall prompt.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// How a `numan` binary on disk was likely installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstallChannel {
    Cargo,
    Winget,
    Homebrew,
    ReleaseArchive,
    Unknown,
}

impl InstallChannel {
    pub fn label(self) -> &'static str {
        match self {
            InstallChannel::Cargo => "cargo",
            InstallChannel::Winget => "winget",
            InstallChannel::Homebrew => "homebrew",
            InstallChannel::ReleaseArchive => "release archive",
            InstallChannel::Unknown => "unknown",
        }
    }

    pub fn uninstall_hint(self) -> Option<&'static str> {
        match self {
            InstallChannel::Cargo => Some("cargo uninstall numan-cli"),
            InstallChannel::Winget => Some("winget uninstall tonythethompson.numan"),
            InstallChannel::Homebrew => Some("brew uninstall numan"),
            InstallChannel::ReleaseArchive | InstallChannel::Unknown => None,
        }
    }
}

/// One discovered `numan` binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredInstall {
    pub path: PathBuf,
    pub channel: InstallChannel,
}

/// Classify a binary path by install channel heuristics.
pub fn classify_binary_path(path: &Path) -> InstallChannel {
    let normalized = path.to_string_lossy().replace('\\', "/").to_ascii_lowercase();

    if normalized.contains("/.cargo/bin/numan")
        || normalized.ends_with("/.cargo/bin/numan.exe")
    {
        return InstallChannel::Cargo;
    }

    if normalized.contains("/microsoft/winget/")
        || normalized.contains("/winget/packages/")
        || (normalized.contains("winget") && normalized.contains("numan"))
    {
        return InstallChannel::Winget;
    }

    if normalized.contains("/cellar/numan/")
        || normalized.contains("/homebrew/numan/")
        || normalized.contains("/linuxbrew/numan/")
    {
        return InstallChannel::Homebrew;
    }

    if normalized.contains("/numan-")
        && normalized
            .split('/')
            .any(|part| part.starts_with("numan-") && part.len() > "numan-".len())
    {
        return InstallChannel::ReleaseArchive;
    }

    InstallChannel::Unknown
}

/// Discover `numan` binaries on PATH and in known package-manager locations.
pub fn discover_installations() -> Vec<DiscoveredInstall> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for dir in path_directories() {
        push_if_numan(&dir, &mut seen, &mut out);
    }

    for candidate in known_install_candidates() {
        if candidate.is_file() {
            push_install(candidate, &mut seen, &mut out);
        }
    }

    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn push_if_numan(dir: &Path, seen: &mut HashSet<PathBuf>, out: &mut Vec<DiscoveredInstall>) {
    let candidates = [
        dir.join("numan"),
        dir.join("numan.exe"),
    ];
    for candidate in candidates {
        if candidate.is_file() {
            push_install(candidate, seen, out);
        }
    }
}

fn push_install(path: PathBuf, seen: &mut HashSet<PathBuf>, out: &mut Vec<DiscoveredInstall>) {
    let canonical = path.canonicalize().unwrap_or(path);
    if seen.insert(canonical.clone()) {
        let channel = classify_binary_path(&canonical);
        out.push(DiscoveredInstall {
            path: canonical,
            channel,
        });
    }
}

fn path_directories() -> Vec<PathBuf> {
    let key = if cfg!(windows) { "Path" } else { "PATH" };
    std::env::var_os(key)
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default()
}

fn known_install_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(home) = home_dir() {
        candidates.push(home.join(".cargo").join("bin").join(if cfg!(windows) {
            "numan.exe"
        } else {
            "numan"
        }));
    }

    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let winget_packages = PathBuf::from(local).join("Microsoft").join("WinGet").join("Packages");
            candidates.extend(find_numan_under(&winget_packages, 4));
        }
    }

    if cfg!(target_os = "macos") {
        candidates.push(PathBuf::from("/opt/homebrew/Cellar/numan"));
        candidates.push(PathBuf::from("/usr/local/Cellar/numan"));
        candidates.extend(find_numan_under(&PathBuf::from("/opt/homebrew/Cellar/numan"), 3));
        candidates.extend(find_numan_under(&PathBuf::from("/usr/local/Cellar/numan"), 3));
    }

    if cfg!(target_os = "linux") {
        if let Some(home) = home_dir() {
            let linuxbrew = home.join(".linuxbrew/Cellar/numan");
            candidates.extend(find_numan_under(&linuxbrew, 3));
        }
        candidates.extend(find_numan_under(&PathBuf::from("/home/linuxbrew/.linuxbrew/Cellar/numan"), 3));
    }

    candidates
}

fn find_numan_under(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    if !root.exists() {
        return Vec::new();
    }
    let mut found = Vec::new();
    walk_for_numan(root, 0, max_depth, &mut found);
    found
}

fn walk_for_numan(dir: &Path, depth: usize, max_depth: usize, found: &mut Vec<PathBuf>) {
    if depth > max_depth {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_for_numan(&path, depth + 1, max_depth, found);
        } else if is_numan_binary(&path) {
            found.push(path);
        }
    }
}

fn is_numan_binary(path: &Path) -> bool {
    match path.file_name().and_then(OsStr::to_str) {
        Some("numan") | Some("numan.exe") => true,
        _ => false,
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Conflicting installs for a new install via `channel`, excluding `exclude_paths`.
pub fn conflicting_installs(
    channel: InstallChannel,
    exclude_paths: &[PathBuf],
) -> Vec<DiscoveredInstall> {
    let excluded: HashSet<PathBuf> = exclude_paths
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect();

    discover_installations()
        .into_iter()
        .filter(|install| !excluded.contains(&install.path))
        .filter(|install| install.channel != channel)
        .collect()
}

/// Run the cargo-install guard. Returns process exit code.
pub fn run_cargo_install_guard() -> ExitCode {
    let install_root = std::env::var("CARGO_INSTALL_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_cargo_install_root());
    let target = install_root.join("bin").join(if cfg!(windows) {
        "numan.exe"
    } else {
        "numan"
    });

    run_install_guard(InstallChannel::Cargo, &[target])
}

/// Run the winget-install guard. Returns process exit code.
pub fn run_winget_install_guard() -> ExitCode {
    run_install_guard(InstallChannel::Winget, &[])
}

/// Run the homebrew-install guard. Returns process exit code.
pub fn run_homebrew_install_guard() -> ExitCode {
    run_install_guard(InstallChannel::Homebrew, &[])
}

fn default_cargo_install_root() -> PathBuf {
    if let Some(home) = home_dir() {
        return home.join(".cargo");
    }
    PathBuf::from(".cargo")
}

fn run_install_guard(channel: InstallChannel, exclude_paths: &[PathBuf]) -> ExitCode {
    if should_skip_guard() {
        return ExitCode::SUCCESS;
    }

    let conflicts = conflicting_installs(channel, exclude_paths);
    if conflicts.is_empty() {
        return ExitCode::SUCCESS;
    }

    let is_tty = std::io::stdin().is_terminal();
    print_conflict_banner(channel, &conflicts);

    if !is_tty {
        eprintln!(
            "Refusing {} install in non-interactive session while another channel is installed.",
            channel.label()
        );
        eprintln!("Remove the existing install first, or set NUMAN_SKIP_INSTALL_GUARD=1 to bypass.");
        return ExitCode::from(1);
    }

    let channel_labels: Vec<&str> = conflicts
        .iter()
        .map(|c| c.channel.label())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    print!(
        "Uninstall the existing {} install(s) before continuing? [y/N] ",
        channel_labels.join(", ")
    );
    let _ = std::io::stdout().flush();

    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        eprintln!("Failed to read confirmation; cancelling install.");
        return ExitCode::from(1);
    }

    if !input.trim().eq_ignore_ascii_case("y") {
        eprintln!("Install cancelled.");
        return ExitCode::from(1);
    }

    for conflict in &conflicts {
        if !uninstall_channel(conflict.channel) {
            eprintln!(
                "Could not uninstall {} install at {}.",
                conflict.channel.label(),
                conflict.path.display()
            );
            if let Some(hint) = conflict.channel.uninstall_hint() {
                eprintln!("Run manually: {hint}");
            }
            return ExitCode::from(1);
        }
    }

  let remaining = conflicting_installs(channel, exclude_paths);
    if !remaining.is_empty() {
        eprintln!("Existing install still detected after uninstall; cancelling install.");
        for install in remaining {
            eprintln!("  {} ({})", install.path.display(), install.channel.label());
        }
        return ExitCode::from(1);
    }

    ExitCode::SUCCESS
}

fn should_skip_guard() -> bool {
    std::env::var("NUMAN_SKIP_INSTALL_GUARD")
        .map(|v| v == "1")
        .unwrap_or(false)
        || std::env::var("CI").ok().as_deref() == Some("true")
        || std::env::var("GITHUB_ACTIONS").ok().as_deref() == Some("true")
}

fn print_conflict_banner(channel: InstallChannel, conflicts: &[DiscoveredInstall]) {
    eprintln!();
    eprintln!(
        "Another numan install was detected while installing via {}.",
        channel.label()
    );
    for conflict in conflicts {
        eprintln!(
            "  {} via {}",
            conflict.path.display(),
            conflict.channel.label()
        );
        if let Some(hint) = conflict.channel.uninstall_hint() {
            eprintln!("    uninstall: {hint}");
        }
    }
    eprintln!();
}

fn uninstall_channel(channel: InstallChannel) -> bool {
    match channel {
        InstallChannel::Cargo => run_command("cargo", &["uninstall", "numan-cli"]),
        InstallChannel::Winget => run_command(
            "winget",
            &[
                "uninstall",
                "--id",
                "tonythethompson.numan",
                "--exact",
                "--accept-source-agreements",
            ],
        ),
        InstallChannel::Homebrew => run_command("brew", &["uninstall", "numan"]),
        InstallChannel::ReleaseArchive | InstallChannel::Unknown => false,
    }
}

fn run_command(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_cargo_path() {
        let path = Path::new("/home/user/.cargo/bin/numan");
        assert_eq!(classify_binary_path(path), InstallChannel::Cargo);
    }

    #[test]
    fn classify_winget_path() {
        let path = Path::new(
            "C:/Users/x/AppData/Local/Microsoft/WinGet/Packages/foo/numan.exe",
        );
        assert_eq!(classify_binary_path(path), InstallChannel::Winget);
    }

    #[test]
    fn classify_homebrew_path() {
        let path = Path::new("/opt/homebrew/Cellar/numan/0.2.0/bin/numan");
        assert_eq!(classify_binary_path(path), InstallChannel::Homebrew);
    }

    #[test]
    fn classify_release_archive_path() {
        let path = Path::new("/tmp/numan-0.2.0-x86_64-pc-windows-msvc/numan.exe");
        assert_eq!(classify_binary_path(path), InstallChannel::ReleaseArchive);
    }

    #[test]
    fn conflicting_installs_excludes_paths() {
        let excluded = PathBuf::from("/tmp/excluded/numan");
        let installs = vec![
            DiscoveredInstall {
                path: excluded.clone(),
                channel: InstallChannel::Winget,
            },
            DiscoveredInstall {
                path: PathBuf::from("/tmp/other/numan"),
                channel: InstallChannel::Cargo,
            },
        ];
        let filtered = installs
            .into_iter()
            .filter(|install| !std::slice::from_ref(&excluded).contains(&install.path))
            .filter(|install| install.channel != InstallChannel::Winget)
            .collect::<Vec<_>>();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].channel, InstallChannel::Cargo);
    }
}
