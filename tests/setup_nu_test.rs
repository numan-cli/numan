//! `numan setup nu` integration tests.
//!
//! Tests that invoke a real Nushell binary are marked `#[ignore]` and run in the
//! Real-Nu acceptance CI job (`cargo test -- --ignored`).

use clap::Parser;
use numan_cli::cli::Cli;
use numan_cli::cmd::setup::{execute_nu, NuAction, NuSetupArgs};
use numan_cli::core::platform::Platform;
use numan_cli::nu::bootstrap::{self, install_from_archive, NuSetupOptions};
use numan_cli::nu::paths::{find_nu_executable_with_root, validate_nushell_binary};
use std::io::Write;
use std::path::PathBuf;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

#[test]
fn managed_nu_is_discovered_after_install() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let zip_path = root.join("nu-test.zip");

    {
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        let inner = if cfg!(windows) {
            "nu-0.0.0-test/nu.exe"
        } else {
            "nu-0.0.0-test/nu"
        };
        zip.start_file(inner, options).unwrap();
        zip.write_all(b"fake nu binary").unwrap();
        zip.finish().unwrap();
    }

    install_from_archive(&zip_path, root, "0.0.0-test").unwrap();

    let resolved = find_nu_executable_with_root(root).unwrap();
    let expected = bootstrap::managed_nu_binary(root);
    assert_eq!(
        std::fs::canonicalize(&resolved).unwrap(),
        std::fs::canonicalize(&expected).unwrap(),
    );
}

#[test]
fn setup_nu_uses_injected_installer_without_network() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let platform = Platform::detect();

    let installer = |install_root: &std::path::Path, _platform: &Platform| {
        let binary = bootstrap::managed_nu_binary(install_root);
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"fake nu").unwrap();
        Ok(binary)
    };

    bootstrap::execute_nu_setup_with_installer(
        root,
        &platform,
        &NuSetupOptions {
            yes: true,
            force: false,
            skip_path: true,
            version: None,
            caller_consented_destructive: false,
        },
        installer,
    )
    .unwrap();

    assert!(bootstrap::managed_nu_binary(root).is_file());
}

#[test]
fn execute_nu_command_wraps_installer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Pre-install managed binary so execute_nu short-circuits without network.
    let binary = bootstrap::managed_nu_binary(root);
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"fake nu").unwrap();

    execute_nu(&NuSetupArgs::install(None, false, true, true), root).unwrap();
}

/// Return the first runnable Nushell binary on `$PATH` (or `/usr/local/bin/nu` on Unix).
fn runnable_nu_on_path() -> Option<PathBuf> {
    let nu_name = if cfg!(windows) { "nu.exe" } else { "nu" };
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(nu_name))
                .collect()
        })
        .unwrap_or_default();
    if cfg!(unix) {
        candidates.push(PathBuf::from("/usr/local/bin/nu"));
    }
    candidates
        .into_iter()
        .filter(|p| p.is_file())
        .find(|p| validate_nushell_binary(p).is_ok())
}

#[test]
#[ignore = "requires real Nu binary on $PATH — run in platform acceptance job"]
fn setup_nu_use_existing_registers_binary_without_download() {
    let Some(nu_source) = runnable_nu_on_path() else {
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let existing_dir = dir.path().join("existing-nu");
    std::fs::create_dir_all(&existing_dir).unwrap();
    let existing = existing_dir.join(if cfg!(windows) { "nu.exe" } else { "nu" });
    std::fs::copy(&nu_source, &existing).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&existing).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&existing, perms).unwrap();
    }

    execute_nu(&NuSetupArgs::use_existing(existing.clone(), true), root).unwrap();

    assert!(
        !bootstrap::managed_nu_binary(root).is_file(),
        "use-existing should not install a managed copy under NUMAN_ROOT"
    );

    let path_var = std::env::var("PATH").unwrap();
    let parent = existing
        .canonicalize()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let parent_str = parent.to_string_lossy().replace("\\\\?\\", "");
    let path_contains = std::env::split_paths(&path_var)
        .any(|part| part.to_string_lossy().eq_ignore_ascii_case(&parent_str));
    assert!(
        path_contains,
        "PATH should contain the existing Nu directory after use-existing"
    );
}

// ---------------------------------------------------------------------------
// CLI-parse tests: verify the subcommand tree resolves as expected
// ---------------------------------------------------------------------------

fn parse_nu_args(argv: &[&str]) -> NuSetupArgs {
    let mut full: Vec<&str> = vec!["numan", "setup", "nu"];
    full.extend_from_slice(argv);
    let cli = Cli::try_parse_from(&full).unwrap();
    match cli.command {
        numan_cli::cli::Commands::Setup(numan_cli::cmd::setup::SetupCommands::Nu(args)) => args,
        _ => panic!("expected Setup(Nu) variant"),
    }
}

#[test]
fn cli_parse_bare_install() {
    let args = parse_nu_args(&[]);
    assert!(args.action.is_none());
    assert!(args.version.is_none());
    assert!(!args.force);
    assert!(!args.yes);
}

#[test]
fn cli_parse_pinned_version() {
    let args = parse_nu_args(&["0.113.1"]);
    assert!(args.action.is_none());
    assert_eq!(args.version.as_deref(), Some("0.113.1"));
}

#[test]
fn cli_parse_remove_subcommand() {
    let args = parse_nu_args(&["remove"]);
    assert!(matches!(args.action, Some(NuAction::Remove)));
}

#[test]
fn cli_parse_path_subcommand() {
    let args = parse_nu_args(&["path"]);
    assert!(matches!(args.action, Some(NuAction::Path)));
}

#[test]
fn cli_parse_use_subcommand() {
    let args = parse_nu_args(&["use", "/usr/bin/nu"]);
    match &args.action {
        Some(NuAction::Use { path }) => assert_eq!(path, &PathBuf::from("/usr/bin/nu")),
        other => panic!("expected Use, got {other:?}"),
    }
}

#[test]
fn cli_parse_backward_compat_remove_flag() {
    let args = parse_nu_args(&["--remove", "--yes"]);
    assert!(args.remove);
    assert!(args.yes);
    assert!(args.action.is_none());
}

#[test]
fn cli_parse_backward_compat_use_path_flag() {
    let args = parse_nu_args(&["--use-path"]);
    assert!(args.use_path);
    assert!(args.action.is_none());
}

#[test]
fn cli_parse_backward_compat_use_existing_flag() {
    let args = parse_nu_args(&["--use-existing", "C:\\nu.exe"]);
    assert_eq!(
        args.use_existing.as_deref(),
        Some(std::path::Path::new("C:\\nu.exe"))
    );
    assert!(args.action.is_none());
}

#[test]
fn setup_nu_rejects_use_existing_with_skip_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let existing = root.join("nu");
    std::fs::write(&existing, b"fake nu").unwrap();

    let args = NuSetupArgs::use_existing_for_test(existing, true, true);
    let err = execute_nu(&args, root).unwrap_err();
    assert!(
        err.to_string()
            .contains("cannot be combined with --skip-path"),
        "unexpected error: {err}"
    );
}

#[test]
fn cli_parse_rejects_version_with_subcommand() {
    let full = ["numan", "setup", "nu", "remove", "0.113.1"];
    assert!(
        Cli::try_parse_from(full).is_err(),
        "a version must not be accepted alongside an action subcommand"
    );
}

#[test]
fn setup_nu_rejects_legacy_use_existing_with_skip_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let existing = root.join("nu");
    std::fs::write(&existing, b"fake nu").unwrap();

    let mut args = NuSetupArgs::install(None, false, true, true);
    args.use_existing = Some(existing);

    let err = execute_nu(&args, root).unwrap_err();
    assert!(
        err.to_string().contains("--skip-path"),
        "unexpected error: {err}"
    );
}
