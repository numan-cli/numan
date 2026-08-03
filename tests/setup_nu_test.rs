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
use numan_cli::nu::version_manager;
use numan_cli::util::test_paths::PathRestoreGuard;
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
    // Discovery keys off the active marker; `install_from_archive` alone does
    // not write it (the setup flow does). Mirror what `numan setup nu` does
    // so discovery can resolve the freshly installed versioned binary.
    version_manager::write_active_version(root, "0.0.0-test").unwrap();

    let resolved = find_nu_executable_with_root(root).unwrap();
    let expected = version_manager::version_binary(root, "0.0.0-test");
    assert_eq!(
        std::fs::canonicalize(&resolved).unwrap(),
        std::fs::canonicalize(&expected).unwrap(),
        "installed binary must live in the versioned layout \
         (<root>/tools/nushell/<version>/<bin>)",
    );
}

#[test]
fn setup_nu_uses_injected_installer_without_network() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let platform = Platform::detect();

    let installer = |install_root: &std::path::Path, _platform: &Platform| {
        // PR69 Srm: the injected installer must write the VERSIONED layout,
        // exactly like the real network installer now does.
        let binary = version_manager::version_binary(install_root, "0.113.1");
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
            version: Some("0.113.1".to_string()),
            caller_consented_destructive: false,
            is_tty: None,
        },
        installer,
    )
    .unwrap();

    // The versioned binary must exist and be marked active (PR67 round-3
    // requirement: fake-installer test covering both the active marker and
    // the resulting `numan use list` state).
    assert!(
        version_manager::version_binary(root, "0.113.1").is_file(),
        "versioned binary must exist after injected install"
    );
    let active = version_manager::read_active_version(root).unwrap().unwrap();
    assert_eq!(
        active.version, "0.113.1",
        "freshly installed version must be active"
    );
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
    // Serialize PATH mutation with sibling ignored tests in this binary.
    let _path_guard = PathRestoreGuard::new();

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

    execute_nu(
        &NuSetupArgs::use_existing(existing.clone(), true, false),
        root,
    )
    .unwrap();

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
    let path_contains = std::env::split_paths(&path_var).any(|part| {
        let part_str = part.to_string_lossy().replace("\\\\?\\", "");
        if part_str.eq_ignore_ascii_case(&parent_str) {
            return true;
        }
        // macOS temp dirs often appear as /var/... vs /private/var/...;
        // compare canonical forms when the entry still exists on disk.
        part.canonicalize()
            .map(|canon| canon == parent)
            .unwrap_or(false)
    });
    assert!(
        path_contains,
        "PATH should contain the existing Nu directory after use-existing; PATH={path_var}"
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
    assert!(matches!(args.action, Some(NuAction::Path { .. })));
}

#[test]
fn cli_parse_use_subcommand() {
    let args = parse_nu_args(&["use", "/usr/bin/nu"]);
    match &args.action {
        Some(NuAction::Use { path, force: false }) => {
            assert_eq!(path, &PathBuf::from("/usr/bin/nu"))
        }
        other => panic!("expected Use {{ path, force: false }}, got {other:?}"),
    }
}

#[test]
fn cli_parse_use_subcommand_with_force_flag() {
    let args = parse_nu_args(&["use", "--force", "/usr/bin/nu"]);
    match &args.action {
        Some(NuAction::Use { path, force: true }) => {
            assert_eq!(path, &PathBuf::from("/usr/bin/nu"))
        }
        other => panic!("expected Use {{ path, force: true }}, got {other:?}"),
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

    let args = NuSetupArgs {
        action: Some(numan_cli::cmd::setup::NuAction::Use {
            path: existing,
            force: false,
        }),
        version: None,
        force: false,
        skip_path: true,
        yes: true,
        remove: false,
        use_path: false,
        use_existing: None,
    };
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

/// Stage a writable copy of `nu` at `dst` (copies the host's Nu binary so
/// `validate_nushell_binary` succeeds without spawning a build).
#[cfg(unix)]
fn stage_fake_nu(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(src, dst).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(dst).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(dst, perms).unwrap();
}

#[cfg(windows)]
fn stage_fake_nu(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(src, dst).unwrap();
}

// PR spec-ambiguity fix: `numan setup nu use <path>` MUST refuse the
// destructive two-step flow (managed-tree delete + off-path registration)
// when a managed Nushell install already exists, unless the caller
// explicitly opts in with `--force`. The hint message names `--force`
// and `setup nu remove`, so CLI users have two clean recovery paths.
#[test]
#[ignore = "requires real Nu binary on $PATH — run in platform acceptance job"]
fn setup_nu_use_existing_refuses_when_managed_tree_present_without_force() {
    // Keep PATH reads consistent with sibling ignored tests that mutate PATH.
    let _path_guard = PathRestoreGuard::new();

    let Some(nu_source) = runnable_nu_on_path() else {
        return;
    };
    let bin_name = if cfg!(windows) { "nu.exe" } else { "nu" };

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Stage a managed Nushell install under NUMAN_ROOT.
    let managed = root.join("tools").join("nushell").join(bin_name);
    stage_fake_nu(&nu_source, &managed);
    assert!(managed.is_file(), "managed Nu must be on disk");

    // Stage the user-supplied off-path Nu.
    let off_path_dir = dir.path().join("off");
    let off_path = off_path_dir.join(bin_name);
    stage_fake_nu(&nu_source, &off_path);

    // Without --force: must refuse, naming --force in the hint.
    let err = execute_nu(
        &NuSetupArgs::use_existing(off_path.clone(), true, false),
        root,
    )
    .expect_err("expected refusal when managed tree exists and --force omitted");
    let msg = err.to_string();
    assert!(
        msg.contains("Refusing"),
        "hint must lead with the refusal, got: {msg}"
    );
    assert!(msg.contains("--force"), "hint must name --force: {msg}");
    // The gate names the managed-tree DIRECTORY (not the binary inside it),
    // mirroring what `bootstrap::managed_nu_dir(root)` returns at the prompt
    // site — so callers can grep exactly one path shape from CI logs.
    let managed_dir = managed.parent().unwrap();
    assert!(
        msg.contains(&managed_dir.display().to_string()),
        "hint must include managed tree directory: {msg}"
    );
    assert!(
        msg.contains(&off_path.display().to_string()),
        "hint must include off-path binary: {msg}"
    );
    assert!(
        msg.contains("setup nu remove"),
        "hint must name the alternate recovery path: {msg}"
    );

    // The managed Nu must still be on disk — the refusal preserves state.
    assert!(managed.is_file(), "managed Nu must survive the refusal");
}

/// With `--force`, the destructive two-step proceeds under the standard
/// merged confirm prompt. `yes=true` short-circuits the inner confirm so
/// the test runs without TTY interaction; in production, the user sees a
/// warn-and-confirm prompt shaped exactly like the existing destructive
/// prompt on `execute_use_path`.
#[test]
#[ignore = "requires real Nu binary on $PATH — run in platform acceptance job"]
fn setup_nu_use_existing_force_drops_managed_tree() {
    // Serialize PATH mutation with sibling ignored tests in this binary.
    let _path_guard = PathRestoreGuard::new();

    let Some(nu_source) = runnable_nu_on_path() else {
        return;
    };
    let bin_name = if cfg!(windows) { "nu.exe" } else { "nu" };

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let managed = root.join("tools").join("nushell").join(bin_name);
    stage_fake_nu(&nu_source, &managed);
    let off_path_dir = dir.path().join("off");
    let off_path = off_path_dir.join(bin_name);
    stage_fake_nu(&nu_source, &off_path);

    // With --force + yes: destructive two-step proceeds; pre-existing
    // managed Nu is replaced by the off-path binary.
    execute_nu(
        &NuSetupArgs::use_existing(off_path.clone(), true, true),
        root,
    )
    .unwrap();

    assert!(
        !managed.is_file(),
        "managed Nu must be deleted after force=true execution"
    );
}

/// Golden-string stability test for the hoisted-consent audit trail.
///
/// The literal text emitted when `register_existing_nu` runs with
/// `caller_consented_destructive` is part of the public contract: safe-batch
/// automation greps `(audit)` lines out of stderr to reason about which
/// consent decision was made. Any accidental copy-edit would silently break
/// that grep, so the literal string is pinned here. Update this test
/// deliberately and in the same commit as the helper's text change.
#[test]
fn register_existing_nu_audit_text_is_stable() {
    use numan_cli::util::confirm::hoisted_audit_message;
    use std::path::Path;

    let parent = Path::new("/usr/local/bin");
    let actual = hoisted_audit_message(parent);
    assert_eq!(
        actual,
        format!(
            "(audit) prompt hoisted; skipping internal PATH-confirmation prompt \
             for '{}' (caller has already gathered destructive-step consent).",
            parent.display()
        )
    );

    // Empty parent path: a corner case the future hoist surfaces (setup nu
    // remove, install <v> one-shot) might pass through. The helper must still
    // return a stable shape; an empty `display()` renders as `""`.
    let empty_parent = Path::new("");
    let empty = hoisted_audit_message(empty_parent);
    assert_eq!(
        empty,
        format!(
            "(audit) prompt hoisted; skipping internal PATH-confirmation prompt \
             for '{}' (caller has already gathered destructive-step consent).",
            empty_parent.display()
        )
    );
}
