use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, ValueEnum};
use clap_complete::{generate, Shell};
use clap_complete_nushell::Nushell;

use crate::cli::Cli;

/// Install (default) or print shell completion scripts
#[derive(clap::Parser)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
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
        println!("Add to $PROFILE (once): . {}", path.display());
    }
    Ok(())
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
    let home = dirs::home_dir().context("Could not resolve home directory")?;
    Ok(match shell {
        CompletionShell::Bash => home
            .join(".local")
            .join("share")
            .join("bash-completion")
            .join("completions")
            .join("numan"),
        CompletionShell::Zsh => home.join(".zfunc").join("_numan"),
        CompletionShell::Fish => home
            .join(".config")
            .join("fish")
            .join("completions")
            .join("numan.fish"),
        CompletionShell::PowerShell => home.join(".numan").join("completions.ps1"),
        CompletionShell::Nushell => {
            let data = dirs::data_dir().context("Could not resolve data directory")?;
            data.join("nushell")
                .join("vendor")
                .join("autoload")
                .join("numan-completions.nu")
        }
    })
}

/// Write the completion script to `path`, creating parent directories as needed.
pub fn install_to(shell: CompletionShell, path: &Path) -> Result<()> {
    if path.file_name().is_none_or(|name| name.is_empty()) {
        bail!("completion install path must be a file path");
    }
    let script = generate_script(shell)?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create completions directory {}",
                    parent.display()
                )
            })?;
        }
    }
    fs::write(path, script.as_bytes())
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
mkdir -p ~/.config/fish/completions
numan completions fish --print > ~/.config/fish/completions/numan.fish
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
        assert!(print_hint(CompletionShell::Fish).contains("mkdir -p ~/.config/fish/completions"));
        assert!(
            print_hint(CompletionShell::Nushell).contains("vendor/autoload/numan-completions.nu")
        );
        assert!(print_hint(CompletionShell::Nushell).contains("numan completions nushell --print"));
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
        let written = fs::read_to_string(&path).expect("read");
        assert!(written.contains("_numan"));
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
