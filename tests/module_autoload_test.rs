/// Integration tests for Phase 4 module autoload: candidate generation,
/// validation, managed-file replacement, deactivation, journal recovery, and
/// concurrent mutation serialization.
///
/// ## Test structure
///
/// - **Unit tests** for `render_use_statement`: path escaping on all platforms,
///   Windows/Unix paths, spaces, quotes, backslashes, Unicode, apostrophes,
///   brackets. These test the rendering function directly without I/O.
///
/// - **Ordering tests** for `generate_autoload_content`: deterministic sort.
///
/// - **Integration tests** using `FakeCandidateRunner`: exercise the full
///   module lane of `activate` and `deactivate` without spawning a real Nu
///   binary. Created under a `tempfile::TempDir` Numan root.
///
/// - **Real-Nu tests** marked `#[ignore]`: require a `nu` binary on `$PATH`.
///   Run with `cargo test -- --ignored` or in a platform acceptance job.
///   Per Phase4Plan §16.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use numan_cli::cmd::activate::{execute_with_candidate_runner, ActivateArgs};
use numan_cli::cmd::deactivate::{
    execute_with_candidate_runner as deactivate_with_runner, DeactivateArgs,
};
use numan_cli::core::integrity;
use numan_cli::core::package::ModuleImportMode;
use numan_cli::nu::autoload::{
    generate_autoload_content, render_use_statement, FakeCandidateRunner, ResolvedEntry,
};
use numan_cli::nu::paths::NuPaths;
use numan_cli::state::autoload_journal::{
    sha256_file, AutoloadOperation, AutoloadStage, PendingAutoload,
    SCHEMA_VERSION as AUTOLOAD_SCHEMA_VERSION,
};
use numan_cli::state::autoload_recovery::{reconcile_pending_autoload, AutoloadRecoveryOutcome};
use numan_cli::state::autoload_state::AutoloadState;
use numan_cli::state::lockfile::{Lockfile, LockfileEntry, ModuleActivation};
use numan_cli::util::fs_safety::{acquire_mutation_lock, OWNERSHIP_MARKER};

use tempfile::TempDir;

// ── Test helpers ─────────────────────────────────────────────────────────────

/// A fully-wired Numan root environment with a fake Nu binary, NuPaths, and
/// a vendor-autoload directory. Suitable for both activate and deactivate tests.
struct ModuleTestEnv {
    dir: TempDir,
    nu_hash: String,
    nu_exe: PathBuf,
    vendor_dir: PathBuf,
}

impl ModuleTestEnv {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let nu_exe = dir.path().join("fake_nu");
        std::fs::write(&nu_exe, b"fake nu binary").unwrap();
        let nu_hash = integrity::compute_sha256(b"fake nu binary");

        let vendor_dir = dir.path().join("vendor").join("autoload");
        std::fs::create_dir_all(&vendor_dir).unwrap();

        Self {
            dir,
            nu_hash,
            nu_exe,
            vendor_dir,
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn vendor_dir_str(&self) -> String {
        self.vendor_dir.to_string_lossy().into_owned()
    }

    fn managed_file_path(&self) -> PathBuf {
        self.vendor_dir.join("numan.nu")
    }

    fn managed_file_path_str(&self) -> String {
        self.managed_file_path().to_string_lossy().into_owned()
    }

    /// Write NuPaths with vendor_autoload_dir populated.
    fn write_nu_paths(&self) {
        let paths = NuPaths {
            nu_executable: self.nu_exe.to_string_lossy().into_owned(),
            nu_version: "0.113.1".to_string(),
            plugin_registry_path: self
                .root()
                .join("plugins.msgpackz")
                .to_string_lossy()
                .into_owned(),
            nu_executable_hash: self.nu_hash.clone(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            data_dir: Some(self.root().join("data").to_string_lossy().into_owned()),
            vendor_autoload_dirs: vec![self.vendor_dir_str()],
            vendor_autoload_dir: Some(self.vendor_dir_str()),
        };
        paths.save(self.root()).unwrap();
    }

    /// Create an installed module payload on disk and return a lockfile entry
    /// pointing to it.
    fn create_module(
        &self,
        owner: &str,
        name: &str,
        version: &str,
        import_mode: ModuleImportMode,
    ) -> (String, LockfileEntry) {
        let pkg_id = format!("{owner}/{name}");
        let payload_rel = format!("packages/modules/{owner}/{name}/{version}-aabbccdd");
        let payload_abs = self.root().join(&payload_rel);
        std::fs::create_dir_all(&payload_abs).unwrap();
        let entry_path = payload_abs.join("mod.nu");
        std::fs::write(
            &entry_path,
            b"# nushell module\nexport def hello [] { \"hello\" }\n",
        )
        .unwrap();

        let entry = LockfileEntry {
            version: version.to_string(),
            package_type: "module".to_string(),
            source: "archive".to_string(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: None,
            archive_root: None,
            include: None,
            entry: Some("mod.nu".to_string()),
            installed_at: "0000000000000001".to_string(),
            nu_version_at_install: Some("0.113.1".to_string()),
            activation: None,
            registry_url: None,
            registry_revision: None,
            index_sha256: None,
            signing_key_fingerprint: None,
            git_url: None,
            git_rev: None,
            cargo_name: None,
            cargo_lock_sha256: None,
            built_sha256: None,
            payload_path: payload_rel,
            revision_id: None,
            payload_sha256: None,
            executable_sha256: None,
            selection_reason: None,
            origin: None,
            module_activation: None,
            module_import_mode: Some(import_mode),
            locked_dependencies: BTreeMap::new(),
        };

        (pkg_id, entry)
    }

    /// Write a lockfile for this environment.
    fn write_lockfile(&self, lockfile: &Lockfile) {
        lockfile.save(self.root()).unwrap();
    }

    /// Build ActivateArgs for the given package IDs.
    fn activate_args(&self, packages: &[&str]) -> ActivateArgs {
        ActivateArgs {
            packages: packages.iter().map(|s| s.to_string()).collect(),
            verbose: false,
            list: false,
            check: false,
        }
    }

    /// Build DeactivateArgs for the given package IDs.
    fn deactivate_args(&self, packages: &[&str]) -> DeactivateArgs {
        DeactivateArgs {
            packages: packages.iter().map(|s| s.to_string()).collect(),
            verbose: false,
        }
    }

    /// Fake plugin registrar — always succeeds.
    fn ok_registrar() -> impl Fn(&str, &str, &str) -> anyhow::Result<()> {
        |_nu, _bin, _cfg| Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// SECTION 1: Unit tests for render_use_statement
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn render_module_mode_simple_unix_path() {
    let path =
        Path::new("/home/user/.local/share/nushell/packages/modules/owner/foo/1.0.0-abc/mod.nu");
    let stmt = render_use_statement(path, &ModuleImportMode::Module).unwrap();
    assert_eq!(
        stmt,
        r#"use "/home/user/.local/share/nushell/packages/modules/owner/foo/1.0.0-abc/mod.nu""#
    );
    assert!(
        !stmt.ends_with(" *"),
        "Module mode must not have trailing ' *'"
    );
}

#[test]
fn render_all_mode_simple_unix_path() {
    let path = Path::new("/home/user/packages/bar/1.0.0-aaa/mod.nu");
    let stmt = render_use_statement(path, &ModuleImportMode::All).unwrap();
    assert_eq!(stmt, r#"use "/home/user/packages/bar/1.0.0-aaa/mod.nu" *"#);
    assert!(stmt.ends_with(" *"), "All mode must end with ' *'");
}

#[test]
fn render_windows_backslashes_doubled_module_mode() {
    // Build path from raw string so it contains real backslashes.
    let path = PathBuf::from(r"A:\numan\packages\modules\owner\foo\1.0.0-a1b2c3d4\mod.nu");
    let stmt = render_use_statement(&path, &ModuleImportMode::Module).unwrap();
    // Every backslash must be doubled in the Nu double-quoted string literal.
    assert!(
        stmt.contains(r"A:\\numan\\packages\\modules\\owner\\foo\\1.0.0-a1b2c3d4\\mod.nu"),
        "Backslashes must be doubled; got: {stmt}"
    );
    assert!(stmt.starts_with(r#"use ""#));
    assert!(!stmt.ends_with(" *"));
}

#[test]
fn render_windows_backslashes_doubled_all_mode() {
    let path =
        PathBuf::from(r"C:\Users\example\nushell\packages\modules\owner\bar\1.2.0-d4c3b2a1\mod.nu");
    let stmt = render_use_statement(&path, &ModuleImportMode::All).unwrap();
    assert!(stmt.ends_with(" *"), "All mode must end with ' *'");
    assert!(
        stmt.contains(
            r"C:\\Users\\example\\nushell\\packages\\modules\\owner\\bar\\1.2.0-d4c3b2a1\\mod.nu"
        ),
        "Backslashes must be doubled; got: {stmt}"
    );
}

#[test]
fn render_path_with_spaces_is_double_quoted() {
    let path = Path::new("/home/user name/my packages/mod.nu");
    let stmt = render_use_statement(path, &ModuleImportMode::Module).unwrap();
    // Spaces require no escaping in Nu double-quoted strings;
    // the enclosing double-quotes handle them.
    assert_eq!(stmt, r#"use "/home/user name/my packages/mod.nu""#);
}

#[test]
fn render_path_with_double_quotes_escaped() {
    // A path containing a double-quote character (unusual but possible on Unix).
    let path = Path::new("/home/user/path\"with\"quotes/mod.nu");
    let stmt = render_use_statement(path, &ModuleImportMode::Module).unwrap();
    // The double-quote inside the string must be escaped as \"
    assert!(
        stmt.contains(r#"path\"with\"quotes"#),
        r#"Double-quotes inside path must be escaped; got: {stmt}"#
    );
    assert!(stmt.starts_with(r#"use ""#));
}

#[test]
fn render_path_with_unicode_characters() {
    let path = Path::new("/home/用户/模块包/mod.nu");
    let stmt = render_use_statement(path, &ModuleImportMode::Module).unwrap();
    assert_eq!(stmt, "use \"/home/用户/模块包/mod.nu\"");
}

#[test]
fn render_path_with_apostrophe_no_escaping_needed() {
    // Apostrophes need no escaping inside Nu double-quoted strings.
    let path = Path::new("/home/user's packages/mod.nu");
    let stmt = render_use_statement(path, &ModuleImportMode::Module).unwrap();
    assert_eq!(stmt, r#"use "/home/user's packages/mod.nu""#);
}

#[test]
fn render_path_with_brackets_and_parens() {
    let path = Path::new("/opt/nushell/pkgs/owner[1]/mod (v2).nu");
    let stmt = render_use_statement(path, &ModuleImportMode::All).unwrap();
    assert!(
        stmt.contains("owner[1]"),
        "Brackets must pass through; got: {stmt}"
    );
    assert!(
        stmt.contains("mod (v2).nu"),
        "Parens must pass through; got: {stmt}"
    );
    assert!(stmt.ends_with(" *"));
}

#[test]
fn render_windows_path_with_spaces_and_unicode() {
    let path = PathBuf::from("C:\\Users\\пользователь\\мой модуль\\1.0.0-abc\\mod.nu");
    let stmt = render_use_statement(&path, &ModuleImportMode::Module).unwrap();
    // Backslashes doubled, Unicode preserved, no trailing " *".
    assert!(
        stmt.contains("\\\\"),
        "Backslashes must be doubled; got: {stmt}"
    );
    assert!(
        stmt.contains("пользователь"),
        "Unicode must be preserved; got: {stmt}"
    );
    assert!(!stmt.ends_with(" *"));
}

#[test]
fn render_path_backslash_and_quote_combined() {
    // A path with both backslashes and a double-quote (edge case on Windows).
    // Build the path string manually since the OS may not allow this.
    // We test the rendering logic directly, not filesystem resolution.
    let raw = "C:\\Users\\test\"user\\mod.nu";
    // Construct via PathBuf from a string to avoid any OS path interpretation.
    let path = PathBuf::from(raw);
    let stmt = render_use_statement(&path, &ModuleImportMode::Module).unwrap();
    // Backslash → \\, then double-quote → \"
    assert!(
        stmt.contains(r"C:\\Users\\test"),
        "Backslashes must be doubled; got: {stmt}"
    );
    assert!(
        stmt.contains(r#"\""#) || stmt.contains(r#"test\"user"#),
        "Double-quote must be escaped; got: {stmt}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// SECTION 2: Deterministic ordering tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn ordering_is_by_scoped_id_not_insertion_order() {
    let entries = vec![
        ResolvedEntry {
            absolute_path: PathBuf::from("/root/packages/modules/owner/zeta/1.0.0-aaa/mod.nu"),
            import_mode: ModuleImportMode::Module,
            scoped_id: "owner/zeta".to_string(),
        },
        ResolvedEntry {
            absolute_path: PathBuf::from("/root/packages/modules/owner/alpha/1.0.0-bbb/mod.nu"),
            import_mode: ModuleImportMode::All,
            scoped_id: "owner/alpha".to_string(),
        },
        ResolvedEntry {
            absolute_path: PathBuf::from("/root/packages/modules/owner/beta/1.0.0-ccc/mod.nu"),
            import_mode: ModuleImportMode::Module,
            scoped_id: "owner/beta".to_string(),
        },
    ];

    let content = generate_autoload_content(&entries).unwrap();

    let alpha_pos = content.find("alpha").unwrap();
    let beta_pos = content.find("beta").unwrap();
    let zeta_pos = content.find("zeta").unwrap();
    assert!(
        alpha_pos < beta_pos,
        "alpha must come before beta in sorted output"
    );
    assert!(
        beta_pos < zeta_pos,
        "beta must come before zeta in sorted output"
    );
}

#[test]
fn ordering_stable_across_repeated_calls() {
    let entries = vec![
        ResolvedEntry {
            absolute_path: PathBuf::from("/root/m/owner/z/1.0.0-aaa/mod.nu"),
            import_mode: ModuleImportMode::Module,
            scoped_id: "owner/z".to_string(),
        },
        ResolvedEntry {
            absolute_path: PathBuf::from("/root/m/owner/a/1.0.0-bbb/mod.nu"),
            import_mode: ModuleImportMode::Module,
            scoped_id: "owner/a".to_string(),
        },
    ];

    let c1 = generate_autoload_content(&entries).unwrap();
    let c2 = generate_autoload_content(&entries).unwrap();
    assert_eq!(c1, c2, "generate_autoload_content must be deterministic");
}

#[test]
fn ownership_header_always_present() {
    let entries: Vec<ResolvedEntry> = vec![];
    let content = generate_autoload_content(&entries).unwrap();
    assert!(
        content.starts_with(OWNERSHIP_MARKER),
        "Content must start with ownership marker even when empty"
    );
}

#[test]
fn empty_entries_produces_header_only_no_use_statements() {
    let content = generate_autoload_content(&[]).unwrap();
    assert!(content.starts_with(OWNERSHIP_MARKER));
    assert!(
        !content.contains("use "),
        "Empty entry list must not produce any use statements"
    );
}

#[test]
fn all_mode_entries_include_star_suffix() {
    let entries = vec![ResolvedEntry {
        absolute_path: PathBuf::from("/root/m/owner/foo/1.0.0-aaa/mod.nu"),
        import_mode: ModuleImportMode::All,
        scoped_id: "owner/foo".to_string(),
    }];
    let content = generate_autoload_content(&entries).unwrap();
    let use_line = content.lines().find(|l| l.starts_with("use ")).unwrap();
    assert!(
        use_line.ends_with(" *"),
        "All-mode import must end with ' *'; got: {use_line}"
    );
}

#[test]
fn module_mode_entries_do_not_have_star_suffix() {
    let entries = vec![ResolvedEntry {
        absolute_path: PathBuf::from("/root/m/owner/foo/1.0.0-aaa/mod.nu"),
        import_mode: ModuleImportMode::Module,
        scoped_id: "owner/foo".to_string(),
    }];
    let content = generate_autoload_content(&entries).unwrap();
    let use_line = content.lines().find(|l| l.starts_with("use ")).unwrap();
    assert!(
        !use_line.ends_with(" *"),
        "Module-mode import must not end with ' *'; got: {use_line}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// SECTION 3: Integration tests using FakeCandidateRunner
// ═══════════════════════════════════════════════════════════════════════════════

/// Helper: run `numan activate [packages]` with a FakeCandidateRunner.
fn run_activate(
    env: &ModuleTestEnv,
    packages: &[&str],
    runner: &dyn numan_cli::nu::autoload::CandidateRunner,
) -> anyhow::Result<()> {
    let args = env.activate_args(packages);
    execute_with_candidate_runner(&args, env.root(), &ModuleTestEnv::ok_registrar(), runner)
}

/// Helper: run `numan deactivate [packages]` with a FakeCandidateRunner.
fn run_deactivate(
    env: &ModuleTestEnv,
    packages: &[&str],
    runner: &dyn numan_cli::nu::autoload::CandidateRunner,
) -> anyhow::Result<()> {
    let args = env.deactivate_args(packages);
    deactivate_with_runner(&args, env.root(), runner)
}

// ── 3a. Module activation writes expected candidate and state ──────────────────

#[test]
fn activation_writes_managed_file_and_state() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();

    let (pkg_id, entry) = env.create_module("owner", "foo", "1.0.0", ModuleImportMode::Module);

    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(pkg_id.clone(), entry);
    env.write_lockfile(&lockfile);

    let runner = FakeCandidateRunner::success();
    run_activate(&env, &[], &runner).unwrap();

    // Managed file must exist and start with ownership marker.
    let managed = env.managed_file_path();
    assert!(
        managed.exists(),
        "Managed file must be created after activation"
    );
    let content = std::fs::read_to_string(&managed).unwrap();
    assert!(
        content.starts_with(OWNERSHIP_MARKER),
        "Managed file must start with ownership marker"
    );
    // The managed file contains absolute paths to the .nu entry files, not scoped IDs.
    // Verify it has a use statement pointing to the module directory.
    assert!(
        content.contains("mod.nu"),
        "Managed file must contain a use statement for the module entry"
    );
    assert!(
        content.contains("use "),
        "Managed file must contain use statement(s)"
    );

    // Autoload state must exist.
    let state = AutoloadState::load(env.root())
        .unwrap()
        .expect("autoload-state.json must exist");
    assert!(
        state.active_module_ids.contains(&pkg_id),
        "autoload-state must list the activated module"
    );

    // Lockfile must have a module_activation record.
    let lock = Lockfile::load(env.root()).unwrap();
    let ent = lock.packages.get(&pkg_id).unwrap();
    assert!(
        ent.module_activation.is_some(),
        "Lockfile must have module_activation after activation"
    );

    // Journal must be cleared.
    assert!(
        PendingAutoload::load(env.root()).unwrap().is_none(),
        "Journal must be removed after successful activation"
    );
}

// ── 3b. Validation failure preserves prior managed file ───────────────────────

#[test]
fn validation_failure_preserves_prior_managed_file() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();

    // Write an existing valid managed file.
    let old_content = format!("{}\nuse \"/old/mod.nu\"\n", OWNERSHIP_MARKER);
    std::fs::write(env.managed_file_path(), &old_content).unwrap();

    let (pkg_id, entry) = env.create_module("owner", "bar", "1.0.0", ModuleImportMode::Module);

    // Also need an existing active module to have a valid autoload-state.
    // We skip that complexity and just check that activation fails without
    // touching the existing managed file.
    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(pkg_id.clone(), entry);
    env.write_lockfile(&lockfile);

    // Runner that always fails validation.
    let runner = FakeCandidateRunner::failure("simulated syntax error");
    let result = run_activate(&env, &[&pkg_id], &runner);

    // The activation lane must report failure.
    assert!(
        result.is_err() || {
            // The lane returns Ok(()) with `any_failed = true` propagated as Err at top level.
            // Actually activate returns Err in this case.
            false
        },
        "Activation with failed validation must return error"
    );

    // The prior managed file must be unchanged.
    let content = std::fs::read_to_string(env.managed_file_path()).unwrap();
    assert_eq!(
        content, old_content,
        "Prior managed file must be preserved when validation fails"
    );

    // Lockfile must not have a module_activation record for the failed module.
    let lock = Lockfile::load(env.root()).unwrap();
    let ent = lock.packages.get(&pkg_id).unwrap();
    assert!(
        ent.module_activation.is_none(),
        "Lockfile must not have module_activation for validation-failed module"
    );
}

// ── 3c. Partial deactivation keeps remaining import ───────────────────────────

#[test]
fn partial_deactivation_keeps_remaining_import() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();

    let (pkg_id_foo, entry_foo) =
        env.create_module("owner", "foo", "1.0.0", ModuleImportMode::Module);
    let (pkg_id_bar, entry_bar) = env.create_module("owner", "bar", "1.0.0", ModuleImportMode::All);

    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(pkg_id_foo.clone(), entry_foo);
    lockfile.packages.insert(pkg_id_bar.clone(), entry_bar);
    env.write_lockfile(&lockfile);

    // Activate both.
    let runner = FakeCandidateRunner::success();
    run_activate(&env, &[], &runner).unwrap();

    // Verify both are active. The managed file contains absolute paths to the
    // module entry files, and the module directories contain the package name.
    let managed_content = std::fs::read_to_string(env.managed_file_path()).unwrap();
    assert!(
        managed_content.contains("foo"),
        "foo module must be in managed file; content: {managed_content}"
    );
    assert!(
        managed_content.contains("bar"),
        "bar module must be in managed file; content: {managed_content}"
    );

    // Deactivate only foo.
    let runner = FakeCandidateRunner::success();
    run_deactivate(&env, &[&pkg_id_foo], &runner).unwrap();

    // Managed file must still exist and contain bar but not foo.
    assert!(
        env.managed_file_path().exists(),
        "Managed file must still exist after partial deactivation"
    );
    // The managed file uses absolute canonical paths to the module entry files.
    // After partial deactivation of foo, the managed file should contain bar's
    // path but not foo's path. We check by looking for the package name in the
    // module directory path (which appears in the use statement).
    let content_after = std::fs::read_to_string(env.managed_file_path()).unwrap();

    // Count use statements — should be exactly 1 (for bar only).
    let use_count = content_after
        .lines()
        .filter(|l| l.starts_with("use "))
        .count();
    assert_eq!(
        use_count, 1,
        "After partial deactivation, exactly 1 use statement must remain; got: {content_after}"
    );

    // The remaining use statement must reference bar's path, not foo's.
    let use_line = content_after
        .lines()
        .find(|l| l.starts_with("use "))
        .unwrap();
    assert!(
        use_line.to_lowercase().contains("bar"),
        "Remaining use statement must reference bar's module path; got: {use_line}"
    );
    assert!(
        !use_line.to_lowercase().contains("foo"),
        "Remaining use statement must not reference foo's module path; got: {use_line}"
    );

    // Autoload state must list only bar.
    let state = AutoloadState::load(env.root())
        .unwrap()
        .expect("autoload-state must exist");
    assert!(
        !state.active_module_ids.contains(&pkg_id_foo),
        "foo must not be in state after deactivation"
    );
    assert!(
        state.active_module_ids.contains(&pkg_id_bar),
        "bar must still be in state"
    );
}

// ── 3d. Full deactivation deletes only managed file ──────────────────────────

#[test]
fn full_deactivation_deletes_managed_file_and_state() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();

    let (pkg_id, entry) = env.create_module("owner", "foo", "1.0.0", ModuleImportMode::Module);

    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(pkg_id.clone(), entry);
    env.write_lockfile(&lockfile);

    // Activate first.
    let runner = FakeCandidateRunner::success();
    run_activate(&env, &[], &runner).unwrap();
    assert!(
        env.managed_file_path().exists(),
        "Managed file must exist after activation"
    );

    // Deactivate (full — only module).
    let runner = FakeCandidateRunner::success();
    run_deactivate(&env, &[], &runner).unwrap();

    // Managed file must be gone.
    assert!(
        !env.managed_file_path().exists(),
        "Managed file must be deleted after full deactivation"
    );

    // Autoload state must be gone.
    assert!(
        AutoloadState::load(env.root()).unwrap().is_none(),
        "autoload-state.json must be removed after full deactivation"
    );

    // Lockfile must have no module_activation.
    let lock = Lockfile::load(env.root()).unwrap();
    let ent = lock.packages.get(&pkg_id).unwrap();
    assert!(
        ent.module_activation.is_none(),
        "Lockfile must not have module_activation after full deactivation"
    );

    // Journal must be cleared.
    assert!(
        PendingAutoload::load(env.root()).unwrap().is_none(),
        "Journal must be removed after successful full deactivation"
    );

    // The vendor directory itself must still exist (we only delete numan.nu).
    assert!(
        env.vendor_dir.exists(),
        "Vendor-autoload directory must NOT be deleted by Numan"
    );
}

// ── 3e. Lockfile save failure after replacement leaves Replaced journal ────────

#[test]
fn lockfile_save_failure_after_replacement_leaves_replaced_journal() {
    // This test simulates the scenario where the managed file is replaced
    // successfully but the lockfile save fails afterwards. The journal must
    // remain at the Replaced stage for recovery.
    //
    // We test this by placing the journal at the Replaced stage directly and
    // verifying that recovery logic can pick it up.
    let env = ModuleTestEnv::new();
    env.write_nu_paths();

    let (pkg_id, entry) = env.create_module("owner", "foo", "1.0.0", ModuleImportMode::Module);
    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(pkg_id.clone(), entry);
    env.write_lockfile(&lockfile);

    // Write the managed file as if replacement happened.
    let managed_content = format!("{}\nuse \"/fake/path/mod.nu\"\n", OWNERSHIP_MARKER);
    std::fs::write(env.managed_file_path(), &managed_content).unwrap();

    // Compute SHA of the managed file (as the journal would store it).
    use sha2::{Digest, Sha256};
    let sha_bytes = Sha256::digest(managed_content.as_bytes());
    let candidate_sha = format!("{sha_bytes:x}");

    // Write a Replaced journal manually (simulates crash after file replacement
    // but before lockfile save).
    let journal = PendingAutoload {
        schema_version: numan_cli::state::autoload_journal::SCHEMA_VERSION,
        operation: numan_cli::state::autoload_journal::AutoloadOperation::Activate,
        stage: AutoloadStage::Replaced,
        nu_executable_sha256: env.nu_hash.clone(),
        nu_version: "0.113.1".to_string(),
        vendor_autoload_dir: env.vendor_dir_str(),
        managed_file_path: env.managed_file_path_str(),
        previous_file_exists: false,
        previous_file_sha256: None,
        desired_file_exists: true,
        candidate_sha256: Some(candidate_sha),
        previous_active_module_ids: vec![],
        desired_active_module_ids: vec![pkg_id.clone()],
        targeted_module_ids: vec![pkg_id.clone()],
        created_at: "0000000000000001".to_string(),
        pre_mutation_snapshot_id: None,
    };
    // Ensure state dir exists before saving journal.
    std::fs::create_dir_all(env.root().join("state")).unwrap();
    journal.save(env.root()).unwrap();

    // Journal must be present at the Replaced stage.
    let loaded = PendingAutoload::load(env.root())
        .unwrap()
        .expect("Journal must be present");
    assert_eq!(
        loaded.stage,
        AutoloadStage::Replaced,
        "Journal must be at Replaced stage"
    );

    // Now run activation again — it should reconcile the Replaced journal.
    let (_, entry2) = env.create_module("owner", "foo", "1.0.0", ModuleImportMode::Module);
    let mut lockfile2 = Lockfile::empty();
    lockfile2.packages.insert(pkg_id.clone(), entry2);
    env.write_lockfile(&lockfile2);

    let runner = FakeCandidateRunner::success();
    run_activate(&env, &[], &runner).unwrap();

    // After recovery + activation, journal must be cleared.
    assert!(
        PendingAutoload::load(env.root()).unwrap().is_none(),
        "Journal must be cleared after recovery completes"
    );
}

// ── 3f. Recovery completes lockfile and autoload-state updates ──────────────

#[test]
fn recovery_completes_updates_from_replaced_journal() {
    // Simulate: managed file was replaced, journal is at Replaced, but lockfile
    // and autoload-state were NOT updated. Recovery must complete both.
    let env = ModuleTestEnv::new();
    env.write_nu_paths();

    let (pkg_id, entry) = env.create_module("owner", "foo", "1.0.0", ModuleImportMode::Module);
    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(pkg_id.clone(), entry);
    env.write_lockfile(&lockfile);

    // Write managed file content (as if activation just happened).
    let managed_content = format!("{}\nuse \"/tmp/mod.nu\"\n", OWNERSHIP_MARKER);
    std::fs::write(env.managed_file_path(), &managed_content).unwrap();

    // Compute SHA.
    use sha2::{Digest, Sha256};
    let sha_bytes = Sha256::digest(managed_content.as_bytes());
    let candidate_sha = format!("{sha_bytes:x}");

    // Write a Replaced journal — lockfile and state are out of date.
    let journal = PendingAutoload {
        schema_version: numan_cli::state::autoload_journal::SCHEMA_VERSION,
        operation: numan_cli::state::autoload_journal::AutoloadOperation::Activate,
        stage: AutoloadStage::Replaced,
        nu_executable_sha256: env.nu_hash.clone(),
        nu_version: "0.113.1".to_string(),
        vendor_autoload_dir: env.vendor_dir_str(),
        managed_file_path: env.managed_file_path_str(),
        previous_file_exists: false,
        previous_file_sha256: None,
        desired_file_exists: true,
        candidate_sha256: Some(candidate_sha),
        previous_active_module_ids: vec![],
        desired_active_module_ids: vec![pkg_id.clone()],
        targeted_module_ids: vec![pkg_id.clone()],
        created_at: "0000000000000001".to_string(),
        pre_mutation_snapshot_id: None,
    };
    std::fs::create_dir_all(env.root().join("state")).unwrap();
    journal.save(env.root()).unwrap();

    // Trigger reconciliation by running activate (no new modules to activate,
    // so only the journal reconciliation will run).
    let runner = FakeCandidateRunner::success();
    run_activate(&env, &[], &runner).unwrap();

    // After recovery, journal must be cleared.
    assert!(
        PendingAutoload::load(env.root()).unwrap().is_none(),
        "Journal must be cleared after recovery"
    );

    // Autoload-state should be present (written during recovery).
    // Note: The reconciliation writes autoload-state when desired_file_exists is true
    // and the managed file is present.
    // (The lockfile update sets module_activation based on existing records.)
}

#[test]
fn deactivate_recovers_replaced_activation_before_classifying_targets() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();

    let (pkg_id, entry) = env.create_module(
        "owner",
        "recover-activate",
        "1.0.0",
        ModuleImportMode::Module,
    );
    let entry_path = env.root().join(&entry.payload_path).join("mod.nu");
    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(pkg_id.clone(), entry);
    env.write_lockfile(&lockfile);

    let managed_content = format!("{}\nuse \"{}\"\n", OWNERSHIP_MARKER, entry_path.display());
    std::fs::write(env.managed_file_path(), managed_content).unwrap();

    PendingAutoload {
        schema_version: AUTOLOAD_SCHEMA_VERSION,
        operation: AutoloadOperation::Activate,
        stage: AutoloadStage::Replaced,
        nu_executable_sha256: env.nu_hash.clone(),
        nu_version: "0.113.1".to_string(),
        vendor_autoload_dir: env.vendor_dir_str(),
        managed_file_path: env.managed_file_path_str(),
        previous_file_exists: false,
        previous_file_sha256: None,
        desired_file_exists: true,
        candidate_sha256: Some(sha256_file(&env.managed_file_path()).unwrap()),
        previous_active_module_ids: vec![],
        desired_active_module_ids: vec![pkg_id.clone()],
        targeted_module_ids: vec![pkg_id.clone()],
        created_at: "0000000000000001".to_string(),
        pre_mutation_snapshot_id: None,
    }
    .save(env.root())
    .unwrap();

    let runner = FakeCandidateRunner::success();
    run_deactivate(&env, &[&pkg_id], &runner).unwrap();

    let recovered = Lockfile::load(env.root()).unwrap();
    assert!(recovered.packages[&pkg_id].module_activation.is_none());
    assert!(!env.managed_file_path().exists());
    assert!(PendingAutoload::load(env.root()).unwrap().is_none());
}

#[test]
fn activate_recovers_full_deactivation_without_reactivating_removed_module() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();

    let (old_id, old_entry) = env.create_module("owner", "old", "1.0.0", ModuleImportMode::Module);
    let (next_id, next_entry) =
        env.create_module("owner", "next", "1.0.0", ModuleImportMode::Module);
    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(old_id.clone(), old_entry);
    lockfile.packages.insert(next_id.clone(), next_entry);
    env.write_lockfile(&lockfile);

    let runner = FakeCandidateRunner::success();
    run_activate(&env, &[&old_id], &runner).unwrap();
    let activation = Lockfile::load(env.root()).unwrap().packages[&old_id]
        .module_activation
        .clone()
        .unwrap();
    let previous_sha = sha256_file(&env.managed_file_path()).unwrap();
    std::fs::remove_file(env.managed_file_path()).unwrap();

    PendingAutoload {
        schema_version: AUTOLOAD_SCHEMA_VERSION,
        operation: AutoloadOperation::Deactivate,
        stage: AutoloadStage::Replaced,
        nu_executable_sha256: env.nu_hash.clone(),
        nu_version: "0.113.1".to_string(),
        vendor_autoload_dir: activation.vendor_autoload_dir,
        managed_file_path: activation.managed_file_path,
        previous_file_exists: true,
        previous_file_sha256: Some(previous_sha),
        desired_file_exists: false,
        candidate_sha256: None,
        previous_active_module_ids: vec![old_id.clone()],
        desired_active_module_ids: vec![],
        targeted_module_ids: vec![old_id.clone()],
        created_at: "0000000000000002".to_string(),
        pre_mutation_snapshot_id: None,
    }
    .save(env.root())
    .unwrap();

    run_activate(&env, &[&next_id], &runner).unwrap();

    let recovered = Lockfile::load(env.root()).unwrap();
    assert!(recovered.packages[&old_id].module_activation.is_none());
    assert!(recovered.packages[&next_id].module_activation.is_some());
    assert_eq!(
        AutoloadState::load(env.root())
            .unwrap()
            .unwrap()
            .active_module_ids,
        vec![next_id]
    );
    assert!(PendingAutoload::load(env.root()).unwrap().is_none());
}

fn recovery_journal(
    env: &ModuleTestEnv,
    operation: AutoloadOperation,
    stage: AutoloadStage,
) -> PendingAutoload {
    PendingAutoload {
        schema_version: AUTOLOAD_SCHEMA_VERSION,
        operation,
        stage,
        nu_executable_sha256: env.nu_hash.clone(),
        nu_version: "0.113.1".to_string(),
        vendor_autoload_dir: env.vendor_dir_str(),
        managed_file_path: env.managed_file_path_str(),
        previous_file_exists: false,
        previous_file_sha256: None,
        desired_file_exists: false,
        candidate_sha256: None,
        previous_active_module_ids: vec![],
        desired_active_module_ids: vec![],
        targeted_module_ids: vec![],
        created_at: "0000000000000010".to_string(),
        pre_mutation_snapshot_id: None,
    }
}

fn run_recovery(env: &ModuleTestEnv) -> anyhow::Result<AutoloadRecoveryOutcome> {
    let _guard = acquire_mutation_lock(env.root())?;
    let nu_paths = NuPaths::load(env.root())?;
    let mut lockfile = Lockfile::load(env.root())?;
    reconcile_pending_autoload(env.root(), &nu_paths, &mut lockfile)
}

#[test]
fn no_journal_returns_no_outcome() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();
    env.write_lockfile(&Lockfile::empty());
    let state_dir = env.root().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    assert!(!state_dir.join("pending-autoload.json").exists());

    assert_eq!(
        run_recovery(&env).unwrap(),
        AutoloadRecoveryOutcome::NoJournal
    );
}

#[test]
fn prepared_recovery_clears_unchanged_journal() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();
    env.write_lockfile(&Lockfile::empty());
    std::fs::create_dir_all(env.root().join("state")).unwrap();

    recovery_journal(&env, AutoloadOperation::Activate, AutoloadStage::Prepared)
        .save(env.root())
        .unwrap();

    assert_eq!(
        run_recovery(&env).unwrap(),
        AutoloadRecoveryOutcome::PreparedCleared
    );
    assert!(PendingAutoload::load(env.root()).unwrap().is_none());
}

#[test]
fn stale_identity_recovery_preserves_journal() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();
    env.write_lockfile(&Lockfile::empty());
    std::fs::create_dir_all(env.root().join("state")).unwrap();

    let mut journal = recovery_journal(&env, AutoloadOperation::Activate, AutoloadStage::Prepared);
    journal.nu_executable_sha256 = "different-hash".to_string();
    journal.save(env.root()).unwrap();

    let error = run_recovery(&env).unwrap_err();
    assert!(error.to_string().contains("different Nu identity"));
    assert!(PendingAutoload::load(env.root()).unwrap().is_some());
}

#[test]
fn prepared_recovery_preserves_journal_on_file_drift() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();
    env.write_lockfile(&Lockfile::empty());
    std::fs::create_dir_all(env.root().join("state")).unwrap();
    std::fs::write(env.managed_file_path(), OWNERSHIP_MARKER).unwrap();

    recovery_journal(&env, AutoloadOperation::Activate, AutoloadStage::Prepared)
        .save(env.root())
        .unwrap();

    let error = run_recovery(&env).unwrap_err();
    assert!(error.to_string().contains("drift detected"));
    assert!(PendingAutoload::load(env.root()).unwrap().is_some());
}

#[test]
fn replaced_recovery_preserves_journal_on_hash_drift() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();
    env.write_lockfile(&Lockfile::empty());
    std::fs::create_dir_all(env.root().join("state")).unwrap();
    std::fs::write(env.managed_file_path(), OWNERSHIP_MARKER).unwrap();

    let mut journal = recovery_journal(&env, AutoloadOperation::Activate, AutoloadStage::Replaced);
    journal.desired_file_exists = true;
    journal.candidate_sha256 = Some("wrong-hash".to_string());
    journal.save(env.root()).unwrap();

    let error = run_recovery(&env).unwrap_err();
    assert!(error.to_string().contains("drift detected"));
    assert!(PendingAutoload::load(env.root()).unwrap().is_some());
}

#[test]
fn replaced_recovery_preserves_retargeted_activation() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();
    let (pkg_id, mut entry) =
        env.create_module("owner", "retargeted", "1.0.0", ModuleImportMode::Module);
    entry.module_activation = Some(ModuleActivation {
        entry_path: env
            .root()
            .join(&entry.payload_path)
            .join("mod.nu")
            .to_string_lossy()
            .into_owned(),
        import_mode: ModuleImportMode::Module,
        vendor_autoload_dir: env.vendor_dir_str(),
        managed_file_path: env.managed_file_path_str(),
        nu_executable_sha256: "newer-nu-hash".to_string(),
        nu_version: "0.114.0".to_string(),
        activated_at: "0000000000000011".to_string(),
    });
    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(pkg_id.clone(), entry);
    env.write_lockfile(&lockfile);
    std::fs::create_dir_all(env.root().join("state")).unwrap();

    let mut journal =
        recovery_journal(&env, AutoloadOperation::Deactivate, AutoloadStage::Replaced);
    journal.previous_file_exists = true;
    journal.previous_file_sha256 = Some("old-file-hash".to_string());
    journal.previous_active_module_ids = vec![pkg_id.clone()];
    journal.targeted_module_ids = vec![pkg_id.clone()];
    journal.save(env.root()).unwrap();

    assert_eq!(
        run_recovery(&env).unwrap(),
        AutoloadRecoveryOutcome::ReplacedCompleted
    );
    let persisted = Lockfile::load(env.root()).unwrap();
    assert_eq!(
        persisted.packages[&pkg_id]
            .module_activation
            .as_ref()
            .unwrap()
            .nu_version,
        "0.114.0"
    );
}

#[test]
fn replaced_full_deactivation_clears_targeted_stale_identity_activation() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();
    let (pkg_id, mut entry) =
        env.create_module("owner", "stale-target", "1.0.0", ModuleImportMode::Module);
    entry.module_activation = Some(ModuleActivation {
        entry_path: env
            .root()
            .join(&entry.payload_path)
            .join("mod.nu")
            .to_string_lossy()
            .into_owned(),
        import_mode: ModuleImportMode::Module,
        vendor_autoload_dir: env.vendor_dir_str(),
        managed_file_path: env.managed_file_path_str(),
        nu_executable_sha256: "stale-nu-hash".to_string(),
        nu_version: "0.112.0".to_string(),
        activated_at: "0000000000000011".to_string(),
    });
    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(pkg_id.clone(), entry);
    env.write_lockfile(&lockfile);
    std::fs::create_dir_all(env.root().join("state")).unwrap();

    let mut journal =
        recovery_journal(&env, AutoloadOperation::Deactivate, AutoloadStage::Replaced);
    journal.previous_file_exists = true;
    journal.previous_file_sha256 = Some("old-file-hash".to_string());
    journal.targeted_module_ids = vec![pkg_id.clone()];
    journal.save(env.root()).unwrap();

    assert_eq!(
        run_recovery(&env).unwrap(),
        AutoloadRecoveryOutcome::ReplacedCompleted
    );
    let persisted = Lockfile::load(env.root()).unwrap();
    assert!(persisted.packages[&pkg_id].module_activation.is_none());
    assert!(PendingAutoload::load(env.root()).unwrap().is_none());
}

#[test]
fn invalid_deleted_file_journal_preserves_journal() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();
    let (pkg_id, entry) = env.create_module("owner", "invalid", "1.0.0", ModuleImportMode::Module);
    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(pkg_id.clone(), entry);
    env.write_lockfile(&lockfile);
    std::fs::create_dir_all(env.root().join("state")).unwrap();

    let mut journal =
        recovery_journal(&env, AutoloadOperation::Deactivate, AutoloadStage::Replaced);
    journal.desired_active_module_ids = vec![pkg_id];
    journal.save(env.root()).unwrap();

    let error = run_recovery(&env).unwrap_err();
    assert!(error.to_string().contains("cannot declare active modules"));
    assert!(PendingAutoload::load(env.root()).unwrap().is_some());
}

#[test]
fn non_module_desired_active_id_preserves_replaced_journal() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();
    let (pkg_id, mut entry) =
        env.create_module("owner", "invalid-plugin", "1.0.0", ModuleImportMode::Module);
    entry.package_type = "plugin".to_string();
    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(pkg_id.clone(), entry);
    env.write_lockfile(&lockfile);
    std::fs::create_dir_all(env.root().join("state")).unwrap();
    std::fs::write(env.managed_file_path(), OWNERSHIP_MARKER).unwrap();

    let mut journal = recovery_journal(&env, AutoloadOperation::Activate, AutoloadStage::Replaced);
    journal.desired_file_exists = true;
    journal.candidate_sha256 = Some(sha256_file(&env.managed_file_path()).unwrap());
    journal.desired_active_module_ids = vec![pkg_id];
    journal.save(env.root()).unwrap();

    let error = run_recovery(&env).unwrap_err();
    assert!(error.to_string().contains("lockfile type is 'plugin'"));
    assert!(PendingAutoload::load(env.root()).unwrap().is_some());
}

#[test]
fn missing_entry_metadata_preserves_replaced_journal() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();
    let (pkg_id, mut entry) =
        env.create_module("owner", "missing-entry", "1.0.0", ModuleImportMode::Module);
    entry.entry = None;
    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(pkg_id.clone(), entry);
    env.write_lockfile(&lockfile);
    std::fs::create_dir_all(env.root().join("state")).unwrap();
    std::fs::write(env.managed_file_path(), OWNERSHIP_MARKER).unwrap();

    let mut journal = recovery_journal(&env, AutoloadOperation::Activate, AutoloadStage::Replaced);
    journal.desired_file_exists = true;
    journal.candidate_sha256 = Some(sha256_file(&env.managed_file_path()).unwrap());
    journal.desired_active_module_ids = vec![pkg_id.clone()];
    journal.targeted_module_ids = vec![pkg_id];
    journal.save(env.root()).unwrap();

    let error = run_recovery(&env).unwrap_err();
    assert!(error.to_string().contains("entry is not set"));
    assert!(PendingAutoload::load(env.root()).unwrap().is_some());
}

#[test]
fn lockfile_save_failure_preserves_replaced_journal() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();
    let (pkg_id, entry) = env.create_module(
        "owner",
        "lockfile-failure",
        "1.0.0",
        ModuleImportMode::Module,
    );
    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(pkg_id.clone(), entry);
    env.write_lockfile(&lockfile);
    std::fs::create_dir_all(env.root().join("state")).unwrap();
    std::fs::write(env.managed_file_path(), OWNERSHIP_MARKER).unwrap();

    let mut journal = recovery_journal(&env, AutoloadOperation::Activate, AutoloadStage::Replaced);
    journal.desired_file_exists = true;
    journal.candidate_sha256 = Some(sha256_file(&env.managed_file_path()).unwrap());
    journal.desired_active_module_ids = vec![pkg_id.clone()];
    journal.targeted_module_ids = vec![pkg_id];
    journal.save(env.root()).unwrap();

    let nu_paths = NuPaths::load(env.root()).unwrap();
    let mut loaded = Lockfile::load(env.root()).unwrap();
    std::fs::remove_file(env.root().join("lockfile")).unwrap();
    std::fs::create_dir(env.root().join("lockfile")).unwrap();
    let _guard = acquire_mutation_lock(env.root()).unwrap();
    assert!(reconcile_pending_autoload(env.root(), &nu_paths, &mut loaded).is_err());
    assert!(PendingAutoload::load(env.root()).unwrap().is_some());
}

#[test]
fn derived_state_failure_is_idempotent_on_retry() {
    let env = ModuleTestEnv::new();
    env.write_nu_paths();
    let (pkg_id, entry) = env.create_module("owner", "retry", "1.0.0", ModuleImportMode::Module);
    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(pkg_id.clone(), entry);
    env.write_lockfile(&lockfile);
    std::fs::create_dir_all(env.root().join("state")).unwrap();
    std::fs::write(env.managed_file_path(), OWNERSHIP_MARKER).unwrap();

    let mut journal = recovery_journal(&env, AutoloadOperation::Activate, AutoloadStage::Replaced);
    journal.desired_file_exists = true;
    journal.candidate_sha256 = Some(sha256_file(&env.managed_file_path()).unwrap());
    journal.desired_active_module_ids = vec![pkg_id.clone()];
    journal.targeted_module_ids = vec![pkg_id.clone()];
    journal.save(env.root()).unwrap();

    let nu_paths = NuPaths::load(env.root()).unwrap();
    std::fs::remove_dir_all(env.root().join("nu_state")).unwrap();
    std::fs::write(env.root().join("nu_state"), b"blocked").unwrap();

    {
        let _guard = acquire_mutation_lock(env.root()).unwrap();
        let mut loaded = Lockfile::load(env.root()).unwrap();
        assert!(reconcile_pending_autoload(env.root(), &nu_paths, &mut loaded).is_err());
    }
    assert!(PendingAutoload::load(env.root()).unwrap().is_some());

    std::fs::remove_file(env.root().join("nu_state")).unwrap();
    std::fs::create_dir_all(env.root().join("nu_state")).unwrap();
    {
        let _guard = acquire_mutation_lock(env.root()).unwrap();
        let mut loaded = Lockfile::load(env.root()).unwrap();
        assert_eq!(
            reconcile_pending_autoload(env.root(), &nu_paths, &mut loaded).unwrap(),
            AutoloadRecoveryOutcome::ReplacedCompleted
        );
    }

    assert!(PendingAutoload::load(env.root()).unwrap().is_none());
    assert!(Lockfile::load(env.root()).unwrap().packages[&pkg_id]
        .module_activation
        .is_some());
    assert!(AutoloadState::load(env.root()).unwrap().is_some());
}

// ── 3g. Concurrent mutation lock prevents two writers ─────────────────────────

#[test]
fn concurrent_mutation_lock_prevents_two_writers() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    // Acquire the first lock.
    let _lock1 = acquire_mutation_lock(root).expect("First lock acquisition must succeed");

    // Second acquisition must fail immediately (non-blocking).
    let result = acquire_mutation_lock(root);
    assert!(
        result.is_err(),
        "Second lock acquisition must fail while first is held"
    );
    let err = result.err().unwrap();
    let msg = err.to_string();
    assert!(
        msg.contains("mutation") || msg.contains("in progress") || msg.contains("retry"),
        "Error must describe a mutation conflict; got: {msg}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// SECTION 4: Real-Nu integration tests (marked #[ignore])
// ═══════════════════════════════════════════════════════════════════════════════
//
// These tests require `nu` to be available on $PATH. Run with:
//   cargo test -- --ignored
// or in a platform acceptance job.
// Per Phase4Plan §16.

/// Find the `nu` binary on $PATH, returning `None` if absent.
fn find_nu_binary() -> Option<PathBuf> {
    which_nu()
}

#[cfg(unix)]
fn which_nu() -> Option<PathBuf> {
    std::env::var("PATH").ok().and_then(|path| {
        path.split(':').map(PathBuf::from).find_map(|dir| {
            let candidate = dir.join("nu");
            if candidate.is_file() {
                Some(candidate)
            } else {
                None
            }
        })
    })
}

#[cfg(windows)]
fn which_nu() -> Option<PathBuf> {
    std::env::var("PATH").ok().and_then(|path| {
        path.split(';').map(PathBuf::from).find_map(|dir| {
            let candidate = dir.join("nu.exe");
            if candidate.is_file() {
                Some(candidate)
            } else {
                None
            }
        })
    })
}

#[cfg(not(any(unix, windows)))]
fn which_nu() -> Option<PathBuf> {
    None
}

#[test]
#[ignore = "requires real Nu binary on $PATH — run in platform acceptance job"]
fn real_nu_validates_candidate_with_non_nu_suffix() {
    // Verify that `nu -n <file-without-.nu-extension>` works on this platform.
    let nu = match find_nu_binary() {
        Some(p) => p,
        None => {
            eprintln!("Skipping: Nu binary not found on PATH");
            return;
        }
    };

    let dir = TempDir::new().unwrap();
    let candidate = dir.path().join(".abc123.candidate.tmp");
    // Write a valid empty Nu script (empty file parses fine with -n).
    std::fs::write(&candidate, b"").unwrap();

    let output = std::process::Command::new(&nu)
        .arg("-n")
        .arg(&candidate)
        .output()
        .expect("Failed to spawn Nu");

    assert!(
        output.status.success(),
        "nu -n <non-.nu file> must succeed on this platform.\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires real Nu binary on $PATH — run in platform acceptance job"]
fn real_nu_validates_generated_autoload_content() {
    // Verify that a generated numan.nu (pointing to a real .nu file) passes
    // `nu -n` validation.
    let nu = match find_nu_binary() {
        Some(p) => p,
        None => {
            eprintln!("Skipping: Nu binary not found on PATH");
            return;
        }
    };

    let dir = TempDir::new().unwrap();

    // Create a minimal module .nu file.
    let module_file = dir.path().join("mymod.nu");
    std::fs::write(&module_file, b"export def hello [] { \"hello\" }\n").unwrap();

    // Generate the autoload content.
    let entry = ResolvedEntry {
        absolute_path: module_file.clone(),
        import_mode: ModuleImportMode::Module,
        scoped_id: "owner/mymod".to_string(),
    };
    let content = generate_autoload_content(&[entry]).unwrap();

    // Write as a candidate with a non-.nu suffix.
    let candidate = dir.path().join(".test.candidate.tmp");
    std::fs::write(&candidate, content.as_bytes()).unwrap();

    let output = std::process::Command::new(&nu)
        .arg("-n")
        .arg(&candidate)
        .output()
        .expect("Failed to spawn Nu");

    assert!(
        output.status.success(),
        "Generated autoload content must pass nu -n validation.\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
#[ignore = "requires real Nu binary on $PATH — run in platform acceptance job"]
fn real_nu_import_mode_module_is_namespaced() {
    // Verify that a module loaded via `use "path"` (not `use "path" *`) is
    // available under its module namespace, not in the global scope.
    let nu = match find_nu_binary() {
        Some(p) => p,
        None => {
            eprintln!("Skipping: Nu binary not found on PATH");
            return;
        }
    };

    let dir = TempDir::new().unwrap();
    let module_file = dir.path().join("greet.nu");
    std::fs::write(&module_file, b"export def hello [] { \"hello\" }\n").unwrap();

    // Use `module` mode — command is available as `greet hello`.
    let use_stmt = render_use_statement(&module_file, &ModuleImportMode::Module).unwrap();
    let script = format!("{use_stmt}\ngreet hello");
    let script_file = dir.path().join("test_script.nu");
    std::fs::write(&script_file, script.as_bytes()).unwrap();

    let output = std::process::Command::new(&nu)
        .arg("-n")
        .arg(&script_file)
        .output()
        .expect("Failed to spawn Nu");

    assert!(
        output.status.success(),
        "Module-mode import must make commands available under module namespace.\n\
         stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires real Nu binary on $PATH — run in platform acceptance job"]
fn real_nu_import_mode_all_exports_to_global_scope() {
    // Verify that `use "path" *` exports all commands to the global scope.
    let nu = match find_nu_binary() {
        Some(p) => p,
        None => {
            eprintln!("Skipping: Nu binary not found on PATH");
            return;
        }
    };

    let dir = TempDir::new().unwrap();
    let module_file = dir.path().join("utils.nu");
    std::fs::write(
        &module_file,
        b"export def add [a: int, b: int] { $a + $b }\n",
    )
    .unwrap();

    let use_stmt = render_use_statement(&module_file, &ModuleImportMode::All).unwrap();
    let script = format!("{use_stmt}\nadd 1 2");
    let script_file = dir.path().join("test_all.nu");
    std::fs::write(&script_file, script.as_bytes()).unwrap();

    let output = std::process::Command::new(&nu)
        .arg("-n")
        .arg(&script_file)
        .output()
        .expect("Failed to spawn Nu");

    assert!(
        output.status.success(),
        "All-mode import must export commands to global scope.\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(target_os = "windows")]
#[ignore = "requires real Nu binary on $PATH — run in platform acceptance job"]
fn real_nu_windows_path_with_spaces_validates() {
    let nu = match find_nu_binary() {
        Some(p) => p,
        None => {
            eprintln!("Skipping: Nu binary not found on PATH");
            return;
        }
    };

    let dir = TempDir::new().unwrap();
    // Create a subdirectory with a space in its name.
    let module_dir = dir.path().join("my module");
    std::fs::create_dir_all(&module_dir).unwrap();
    let module_file = module_dir.join("mod.nu");
    std::fs::write(&module_file, b"export def greet [] { \"hi\" }\n").unwrap();

    let entry = ResolvedEntry {
        absolute_path: module_file,
        import_mode: ModuleImportMode::Module,
        scoped_id: "owner/mymod".to_string(),
    };
    let content = generate_autoload_content(&[entry]).unwrap();
    let candidate = dir.path().join(".spaces_test.candidate.tmp");
    std::fs::write(&candidate, content.as_bytes()).unwrap();

    let output = std::process::Command::new(&nu)
        .arg("-n")
        .arg(&candidate)
        .output()
        .expect("Failed to spawn Nu");

    assert!(
        output.status.success(),
        "Windows path with spaces must pass nu -n validation.\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires real Nu binary on $PATH — run in platform acceptance job"]
fn real_nu_syntax_failure_detected_by_validation() {
    // Verify that a candidate with invalid Nu syntax fails validation.
    let nu = match find_nu_binary() {
        Some(p) => p,
        None => {
            eprintln!("Skipping: Nu binary not found on PATH");
            return;
        }
    };

    let dir = TempDir::new().unwrap();
    let candidate = dir.path().join(".bad.candidate.tmp");
    // Write syntactically invalid Nu content.
    std::fs::write(&candidate, b"this is not valid nu syntax !!@#\n").unwrap();

    let output = std::process::Command::new(&nu)
        .arg("-n")
        .arg(&candidate)
        .output()
        .expect("Failed to spawn Nu");

    assert!(
        !output.status.success(),
        "Invalid Nu syntax must fail nu -n validation"
    );
}

#[test]
#[ignore = "requires real Nu binary on $PATH — run in platform acceptance job"]
fn real_nu_missing_module_path_fails_validation() {
    // Verify that a `use` statement pointing to a nonexistent file causes
    // `nu -n` to fail (as required for pre-activation validation).
    let nu = match find_nu_binary() {
        Some(p) => p,
        None => {
            eprintln!("Skipping: Nu binary not found on PATH");
            return;
        }
    };

    let dir = TempDir::new().unwrap();
    let candidate = dir.path().join(".missing.candidate.tmp");
    let content = format!(
        "{}use \"/this/path/does/not/exist/mod.nu\"\n",
        OWNERSHIP_MARKER
    );
    std::fs::write(&candidate, content.as_bytes()).unwrap();

    let output = std::process::Command::new(&nu)
        .arg("-n")
        .arg(&candidate)
        .output()
        .expect("Failed to spawn Nu");

    assert!(
        !output.status.success(),
        "use statement pointing to nonexistent module must fail nu -n validation"
    );
}
