use std::process::{Command, Output};

fn run(args: &[&str], escape: &str) -> (tempfile::TempDir, Output) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("not-created");
    let output = Command::new(env!("CARGO_BIN_EXE_numan"))
        .arg("--root")
        .arg(&root)
        .args(args)
        .env("NUMAN_NO_TUI", escape)
        .output()
        .unwrap();
    assert!(
        !root.exists(),
        "launch/help/rejection must not initialize a root"
    );
    (temp, output)
}

#[test]
fn bare_noninteractive_shows_help_and_preserves_root() {
    let (_, output) = run(&[], "0");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
    assert!(
        !output.stdout.contains(&0x1b),
        "no terminal escape sequences in help"
    );
}

#[test]
fn escape_hatch_precedes_feature_and_tty_checks() {
    let (_, output) = run(&["tui"], "1");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("NUMAN_NO_TUI=1"));
    let (_, output) = run(&[], "1");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
}

#[test]
fn explicit_noninteractive_launch_explains_the_relevant_boundary() {
    let (_, output) = run(&["tui"], "0");
    assert!(!output.status.success());
    let expected = if cfg!(feature = "tui") {
        "interactive stdin and stdout"
    } else {
        "no TUI support"
    };
    assert!(String::from_utf8_lossy(&output.stderr).contains(expected));
}

#[test]
fn help_version_invalid_arguments_and_existing_commands_keep_dispatch() {
    for args in [&["--help"][..], &["--version"][..]] {
        let (_, output) = run(args, "0");
        assert!(output.status.success());
    }
    let (_, output) = run(&["invalid-command"], "0");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand"));
    let (_, output) = run(&["completions", "nushell", "--print"], "0");
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Focus:"));
}
