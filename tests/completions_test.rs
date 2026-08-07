//! Shell completion generation tests.

use numan_cli::cmd::completions::{generate_script, CompletionShell};

#[test]
fn bash_completions_include_core_commands() {
    let script = generate_script(CompletionShell::Bash).expect("generate bash");
    for needle in [
        "numan",
        "init",
        "install",
        "activate",
        "try",
        "completions",
        "nupm",
    ] {
        assert!(
            script.contains(needle),
            "bash completion script missing '{needle}'"
        );
    }
}

#[test]
fn all_completion_shells_generate_non_empty_output() {
    for shell in [
        CompletionShell::Bash,
        CompletionShell::Fish,
        CompletionShell::Zsh,
        CompletionShell::PowerShell,
        CompletionShell::Nushell,
    ] {
        let script = generate_script(shell).expect("generate");
        assert!(
            !script.is_empty(),
            "{shell:?} completion script should not be empty"
        );
    }
}

#[test]
fn nushell_completions_include_core_commands() {
    let script = generate_script(CompletionShell::Nushell).expect("generate nushell");
    for needle in [
        "numan",
        "init",
        "install",
        "activate",
        "completions",
        "nupm",
        "module completions",
    ] {
        assert!(
            script.contains(needle),
            "nushell completion script missing '{needle}'"
        );
    }
}

#[test]
fn powershell_completions_can_append_to_existing_profile() {
    let script = generate_script(CompletionShell::PowerShell).expect("generate powershell");
    assert!(
        !script
            .lines()
            .any(|line| line.trim_start().starts_with("using ")),
        "PowerShell completions must not require top-of-script `using` directives"
    );
    assert!(script.contains("Register-ArgumentCompleter"));
    assert!(script.contains("[System.Management.Automation.CompletionResult]::"));
    assert!(script.contains("numan"));
}

#[test]
fn print_hint_is_ready_to_copy() {
    use numan_cli::cmd::completions::print_hint;

    let hint = print_hint(CompletionShell::PowerShell);
    assert!(hint.contains("Add-Content -Encoding utf8 $PROFILE"));
    assert!(hint.contains("numan completions powershell --print"));
    assert!(
        !generate_script(CompletionShell::PowerShell)
            .expect("generate")
            .contains("Add-Content"),
        "hint must stay on stderr / separate from script stdout"
    );
}

#[test]
fn install_to_creates_parent_dirs_for_each_shell() {
    use numan_cli::cmd::completions::install_to;
    use std::fs;

    let dir = tempfile::tempdir().expect("tempdir");
    let cases = [
        (CompletionShell::Bash, "bash/completions/numan"),
        (CompletionShell::Zsh, "zsh/.zfunc/_numan"),
        (CompletionShell::Fish, "fish/completions/numan.fish"),
        (CompletionShell::PowerShell, "ps/.numan/completions.ps1"),
        (
            CompletionShell::Nushell,
            "nu/vendor/autoload/numan-completions.nu",
        ),
    ];
    for (shell, rel) in cases {
        let path = dir.path().join(rel);
        install_to(shell, &path).unwrap_or_else(|e| panic!("install {shell:?}: {e}"));
        assert!(path.is_file(), "{shell:?} missing at {}", path.display());
        assert!(!fs::read_to_string(&path).expect("read").is_empty());
    }
}
