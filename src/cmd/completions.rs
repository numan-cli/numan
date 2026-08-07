use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, ValueEnum};
use clap_complete::{generate, Shell};
use clap_complete_nushell::Nushell;

use crate::cli::Cli;
use crate::util::atomic::write_bytes_atomic;
use crate::util::fs_safety::{assert_managed_file_owned, assert_not_symlink, OWNERSHIP_MARKER};

/// Install (default) or print shell completion scripts
#[derive(clap::Parser)]
pub struct CompletionsArgs {
    /// Shell to install completions for
    #[arg(value_enum)]
    pub shell: CompletionShell,

    /// Print the script on stdout instead of installing it
    /// (pipe-safe; copy-ready redirect hints go to stderr)
    #[arg(long)]
    pub print: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Fish,
    Zsh,
    #[value(name = "powershell")]
    PowerShell,
    /// Nushell (`nu` is accepted as an alias)
    #[value(name = "nushell", alias = "nu")]
    Nushell,
}

pub fn execute(args: &CompletionsArgs) -> Result<()> {
    if args.print {
        let script = generate_script(args.shell)?;
        print!("{script}");
        // stderr so redirects / pipes stay script-only
        eprint!("{}", print_hint(args.shell));
        return Ok(());
    }

    let path = default_install_path(args.shell)?;
    install_to(args.shell, &path)?;
    println!(
        "Installed {} completions to {}",
        shell_label(args.shell),
        path.display()
    );
    if matches!(args.shell, CompletionShell::PowerShell) {
        println!(
            "Add to $PROFILE (once): . {}",
            powershell_single_quote(&path)
        );
    }
    Ok(())
}

/// Quote a path for a copy-pasteable PowerShell single-quoted string.
///
/// Always wraps in `'…'` and doubles embedded `'` as `''`, so spaces and
/// quotes in home paths cannot break the `. path` instruction.
fn powershell_single_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

fn shell_label(shell: CompletionShell) -> &'static str {
    match shell {
        CompletionShell::Bash => "bash",
        CompletionShell::Fish => "fish",
        CompletionShell::Zsh => "zsh",
        CompletionShell::PowerShell => "powershell",
        CompletionShell::Nushell => "nushell",
    }
}

/// Canonical install path for `numan completions <shell>`.
pub fn default_install_path(shell: CompletionShell) -> Result<PathBuf> {
    Ok(match shell {
        CompletionShell::Bash => require_home_dir()?
            .join(".local")
            .join("share")
            .join("bash-completion")
            .join("completions")
            .join("numan"),
        CompletionShell::Zsh => require_home_dir()?.join(".zfunc").join("_numan"),
        CompletionShell::Fish => fish_config_home()?
            .join("fish")
            .join("completions")
            .join("numan.fish"),
        CompletionShell::PowerShell => require_home_dir()?.join(".numan").join("completions.ps1"),
        CompletionShell::Nushell => dirs::data_dir()
            .context("Could not resolve data directory")?
            .join("nushell")
            .join("vendor")
            .join("autoload")
            .join("numan-completions.nu"),
    })
}

/// Fish config root: `$XDG_CONFIG_HOME` when set, else `~/.config`.
///
/// Matches Fish's discovery path. Do not use [`dirs::config_dir`] here: on
/// Windows that resolves to `%APPDATA%`, which Fish does not use by default.
fn fish_config_home() -> Result<PathBuf> {
    Ok(fish_config_home_with(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        &require_home_dir()?,
    ))
}

fn fish_config_home_with(xdg_config_home: Option<&OsStr>, home: &Path) -> PathBuf {
    match xdg_config_home {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => home.join(".config"),
    }
}

fn require_home_dir() -> Result<PathBuf> {
    dirs::home_dir().context("Could not resolve home directory")
}

/// Write the completion script to `path`, creating parent directories as needed.
///
/// Existing destinations must already carry [`OWNERSHIP_MARKER`]; foreign files
/// are refused. The written content always begins with the ownership header.
pub fn install_to(shell: CompletionShell, path: &Path) -> Result<()> {
    if path.file_name().is_none_or(|name| name.is_empty()) {
        bail!("completion install path must be a file path");
    }
    if let Some(parent) = path.parent() {
        if parent.as_os_str().is_empty() {
            bail!("completion install path must be a file path");
        }
        if parent.exists() {
            assert_not_symlink(parent, "completions directory")?;
        }
    }
    if path.exists() {
        assert_managed_file_owned(path)?;
    } else {
        assert_not_symlink(path, "completions file")?;
    }

    let script = format!("{}{}", OWNERSHIP_MARKER, generate_script(shell)?);
    write_bytes_atomic(path, script.as_bytes())
        .with_context(|| format!("Failed to write completions to {}", path.display()))?;
    Ok(())
}

/// Copy-ready redirect / pipe hints shown with `--print`.
///
/// Written to stderr after the script so stdout remains safe to pipe.
pub fn print_hint(shell: CompletionShell) -> String {
    match shell {
        CompletionShell::Bash => "\
# Prefer: numan completions bash
# Or redirect:
mkdir -p ~/.local/share/bash-completion/completions
numan completions bash --print > ~/.local/share/bash-completion/completions/numan
"
        .to_string(),
        CompletionShell::Zsh => "\
# Prefer: numan completions zsh
# Or redirect:
mkdir -p ~/.zfunc
numan completions zsh --print > ~/.zfunc/_numan
"
        .to_string(),
        CompletionShell::Fish => "\
# Prefer: numan completions fish
# Or redirect:
mkdir -p \"${XDG_CONFIG_HOME:-$HOME/.config}/fish/completions\"
numan completions fish --print > \"${XDG_CONFIG_HOME:-$HOME/.config}/fish/completions/numan.fish\"
"
        .to_string(),
        CompletionShell::PowerShell => "\
# Prefer: numan completions powershell  (writes ~/.numan/completions.ps1)
# Or append to your PowerShell profile:
numan completions powershell --print | Add-Content -Encoding utf8 $PROFILE
"
        .to_string(),
        CompletionShell::Nushell => "\
# Prefer: numan completions nushell
# Or manually:
mkdir --all ($nu.data-dir | path join vendor/autoload)
numan completions nushell --print | save -f ($nu.data-dir | path join vendor/autoload/numan-completions.nu)
"
        .to_string(),
    }
}

/// Generate a completion script for `shell`.
///
/// PowerShell output is rewritten so it can be appended to an existing
/// `$PROFILE` that already contains statements (see
/// [`make_powershell_profile_safe`]).
pub fn generate_script(shell: CompletionShell) -> Result<String> {
    let mut cmd = Cli::command();
    let mut buf = Vec::new();
    match shell {
        CompletionShell::Bash => generate(Shell::Bash, &mut cmd, "numan", &mut buf),
        CompletionShell::Fish => generate(Shell::Fish, &mut cmd, "numan", &mut buf),
        CompletionShell::Zsh => generate(Shell::Zsh, &mut cmd, "numan", &mut buf),
        CompletionShell::PowerShell => generate(Shell::PowerShell, &mut cmd, "numan", &mut buf),
        CompletionShell::Nushell => generate(Nushell, &mut cmd, "numan", &mut buf),
    }
    let script = String::from_utf8(buf).context("completion script was not valid UTF-8")?;
    Ok(match shell {
        CompletionShell::PowerShell => make_powershell_profile_safe(&script),
        _ => script,
    })
}

/// Rewrite clap_complete's PowerShell script so it can be appended to an
/// existing `$PROFILE` that already contains statements.
///
/// clap_complete emits `using namespace ...` directives. PowerShell requires
/// those at the top of a script, so pasting the raw output below other
/// profile content fails with:
/// `A 'using' statement must appear before any other statements in a script.`
fn make_powershell_profile_safe(script: &str) -> String {
    let mut out = String::with_capacity(script.len());
    for line in script.lines() {
        let trimmed = line.trim_start();
        if trimmed == "using namespace System.Management.Automation"
            || trimmed == "using namespace System.Management.Automation.Language"
        {
            continue;
        }
        // Replace short type names (dependent on the removed `using` lines)
        // with fully-qualified names. Replace `CompletionResultType` before
        // `CompletionResult` so the longer name is not partially rewritten.
        let rewritten = line
            .replace(
                "[StringConstantExpressionAst]",
                "[System.Management.Automation.Language.StringConstantExpressionAst]",
            )
            .replace(
                "[StringConstantType]",
                "[System.Management.Automation.Language.StringConstantType]",
            )
            .replace(
                "[CompletionResultType]",
                "[System.Management.Automation.CompletionResultType]",
            )
            .replace(
                "[CompletionResult]::",
                "[System.Management.Automation.CompletionResult]::",
            );
        out.push_str(&rewritten);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_script_is_profile_safe() {
        let script = generate_script(CompletionShell::PowerShell).expect("generate");
        assert!(
            !script.contains("using namespace"),
            "profile-safe script must not emit using namespace directives"
        );
        assert!(script.contains("Register-ArgumentCompleter"));
        assert!(
            script.contains("[System.Management.Automation.Language.StringConstantExpressionAst]")
        );
        assert!(script.contains("[System.Management.Automation.Language.StringConstantType]"));
        assert!(script.contains("[System.Management.Automation.CompletionResult]::"));
        assert!(script.contains("[System.Management.Automation.CompletionResultType]"));
        // Short names must not remain (would fail without `using`).
        assert!(!script.contains("[StringConstantExpressionAst]"));
        assert!(!script.contains("[StringConstantType]"));
        assert!(!script.contains("[CompletionResult]::"));
        assert!(!script.contains("[CompletionResultType]"));
    }

    #[test]
    fn print_hint_is_copy_ready_and_not_part_of_script() {
        let script = generate_script(CompletionShell::PowerShell).expect("generate");
        let hint = print_hint(CompletionShell::PowerShell);
        assert!(
            !script.contains("Add-Content"),
            "print hint must not be mixed into the completion script"
        );
        assert!(hint.contains("numan completions powershell"));
        assert!(hint.contains(
            "numan completions powershell --print | Add-Content -Encoding utf8 $PROFILE"
        ));
        assert!(print_hint(CompletionShell::Bash)
            .contains("mkdir -p ~/.local/share/bash-completion/completions"));
        assert!(print_hint(CompletionShell::Bash).contains("numan completions bash --print"));
        assert!(print_hint(CompletionShell::Zsh).contains("mkdir -p ~/.zfunc"));
        assert!(print_hint(CompletionShell::Fish)
            .contains("mkdir -p \"${XDG_CONFIG_HOME:-$HOME/.config}/fish/completions\""));
        assert!(
            print_hint(CompletionShell::Nushell).contains("vendor/autoload/numan-completions.nu")
        );
        assert!(print_hint(CompletionShell::Nushell).contains("numan completions nushell --print"));
    }

    #[test]
    fn powershell_single_quote_is_copy_paste_safe() {
        assert_eq!(
            powershell_single_quote(Path::new(r"C:\Users\Alice\.numan\completions.ps1")),
            r"'C:\Users\Alice\.numan\completions.ps1'"
        );
        assert_eq!(
            powershell_single_quote(Path::new(r"C:\Users\Alice Smith\.numan\completions.ps1")),
            r"'C:\Users\Alice Smith\.numan\completions.ps1'"
        );
        assert_eq!(
            powershell_single_quote(Path::new(r"C:\Users\O'Brien\.numan\completions.ps1")),
            r"'C:\Users\O''Brien\.numan\completions.ps1'"
        );
    }

    #[test]
    fn install_to_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir
            .path()
            .join("missing")
            .join("nested")
            .join("completions")
            .join("numan");
        assert!(!path.parent().unwrap().exists());
        install_to(CompletionShell::Bash, &path).expect("install_to");
        assert!(path.is_file());
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(written.starts_with(OWNERSHIP_MARKER));
        assert!(written.contains("_numan"));
    }

    #[test]
    fn install_to_refuses_foreign_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("numan");
        std::fs::write(&path, "# not owned by numan\n").expect("seed");
        let err = install_to(CompletionShell::Bash, &path).expect_err("foreign");
        assert!(
            err.to_string().contains("managed-file drift"),
            "unexpected error: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "# not owned by numan\n"
        );
    }

    #[test]
    fn install_to_overwrites_owned_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("numan");
        std::fs::write(&path, format!("{OWNERSHIP_MARKER}# stale\n")).expect("seed");
        install_to(CompletionShell::Bash, &path).expect("overwrite");
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(written.starts_with(OWNERSHIP_MARKER));
        assert!(written.contains("_numan"));
        assert!(!written.contains("# stale"));
    }

    #[test]
    fn fish_config_home_respects_xdg_config_home() {
        let home = Path::new("/home/alice");
        assert_eq!(
            fish_config_home_with(None, home),
            PathBuf::from("/home/alice/.config")
        );
        assert_eq!(
            fish_config_home_with(Some(OsStr::new("")), home),
            PathBuf::from("/home/alice/.config")
        );
        assert_eq!(
            fish_config_home_with(Some(OsStr::new("/xdg/config")), home),
            PathBuf::from("/xdg/config")
        );
    }

    #[test]
    fn default_install_paths_are_under_home_or_data() {
        let bash = default_install_path(CompletionShell::Bash).expect("bash path");
        assert!(
            bash.ends_with("bash-completion/completions/numan")
                || bash.ends_with("bash-completion\\completions\\numan")
        );
        let zsh = default_install_path(CompletionShell::Zsh).expect("zsh path");
        assert!(zsh.ends_with(".zfunc/_numan") || zsh.ends_with(".zfunc\\_numan"));
        let fish = default_install_path(CompletionShell::Fish).expect("fish path");
        assert!(
            fish.ends_with("fish/completions/numan.fish")
                || fish.ends_with("fish\\completions\\numan.fish")
        );
        let ps = default_install_path(CompletionShell::PowerShell).expect("ps path");
        assert!(ps.ends_with(".numan/completions.ps1") || ps.ends_with(".numan\\completions.ps1"));
        let nu = default_install_path(CompletionShell::Nushell).expect("nu path");
        assert!(
            nu.ends_with("nushell/vendor/autoload/numan-completions.nu")
                || nu.ends_with("nushell\\vendor\\autoload\\numan-completions.nu")
        );
    }

    #[test]
    fn nushell_completions_export_extern_commands() {
        let script = generate_script(CompletionShell::Nushell).expect("generate nushell");
        assert!(script.contains("module completions"));
        assert!(script.contains("export extern numan"));
        assert!(script.contains("export extern \"numan install\""));
        assert!(script.contains("export use completions *"));
        assert!(
            !script.contains("vendor/autoload"),
            "hint must not be in script"
        );
    }

    #[test]
    fn make_powershell_profile_safe_preserves_trailing_newline_and_body() {
        let raw = "\
using namespace System.Management.Automation
using namespace System.Management.Automation.Language

Register-ArgumentCompleter -Native -CommandName 'numan' -ScriptBlock {
    if ($element -isnot [StringConstantExpressionAst] -or
        $element.StringConstantType -ne [StringConstantType]::BareWord) { }
    [CompletionResult]::new('x', 'x', [CompletionResultType]::ParameterName, 'd')
}
";
        let safe = make_powershell_profile_safe(raw);
        assert!(!safe.contains("using namespace"));
        assert!(safe.contains("Register-ArgumentCompleter"));
        assert!(safe.ends_with('\n'));
        assert!(safe.contains(
            "[System.Management.Automation.CompletionResult]::new('x', 'x', [System.Management.Automation.CompletionResultType]::ParameterName, 'd')"
        ));
    }
}
