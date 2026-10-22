use super::{
    app::{App, Command, Focus, Overlay, Shell, View},
    catalog::FieldState,
    operations::{self, Action},
    ui,
};
use crate::core::{
    integrity,
    official_registry::{canonical_json_bytes, RegistrySignature},
    package::*,
    platform::Platform,
    trust::TrustStore,
};
use crate::nu::{autoload::CandidateRunner, paths::NuPaths};
use crate::state::lockfile::Lockfile;
use crate::util::{fs_safety::acquire_mutation_lock, output};
use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ed25519_dalek::{Signer, SigningKey};
use ratatui::{backend::TestBackend, Terminal};
use std::{
    collections::{BTreeMap, HashMap},
    io::Write,
    path::{Path, PathBuf},
};

struct Fixture {
    dir: tempfile::TempDir,
    paths: NuPaths,
    plugin: bool,
}
impl Fixture {
    fn new(plugin: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let binary = root.join("fake-nu.exe");
        std::fs::write(&binary, b"fake Nu for injectable lifecycle tests").unwrap();
        let autoload = root.join("autoload");
        std::fs::create_dir_all(&autoload).unwrap();
        let paths = NuPaths {
            nu_executable: binary.to_string_lossy().into(),
            nu_version: "0.116.1".into(),
            nu_executable_hash: integrity::compute_sha256(&std::fs::read(&binary).unwrap()),
            plugin_registry_path: root.join("plugin.msgpackz").to_string_lossy().into(),
            platform: Platform::detect().triple,
            data_dir: Some(root.to_string_lossy().into()),
            vendor_autoload_dirs: vec![autoload.to_string_lossy().into()],
            vendor_autoload_dir: Some(autoload.to_string_lossy().into()),
        };
        paths.save(root).unwrap();
        let mut config = crate::config::Config::default();
        config.general.default_registry = "fixture".into();
        config.save(root).unwrap();
        let fixture = Self { dir, paths, plugin };
        fixture.write_registry();
        fixture
    }
    fn root(&self) -> &Path {
        self.dir.path()
    }
    fn id(&self) -> &str {
        if self.plugin {
            "test/fixture-plugin"
        } else {
            "test/fixture-module"
        }
    }
    fn write_registry(&self) {
        let root = self.root();
        let entry = if self.plugin {
            if cfg!(windows) {
                "nu_plugin_fixture.exe"
            } else {
                "nu_plugin_fixture"
            }
        } else {
            "mod.nu"
        };
        let mut versions = Vec::new();
        for major in [1, 2] {
            let archive_path = root.join(format!("fixture-{major}.tar.gz"));
            let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);
            let content = format!("export def hello [] {{ 'version {major}' }}\n");
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            archive
                .append_data(&mut header, entry, content.as_bytes())
                .unwrap();
            let bytes = archive.into_inner().unwrap().finish().unwrap();
            std::fs::write(&archive_path, &bytes).unwrap();
            let url = url::Url::from_file_path(&archive_path).unwrap().to_string();
            let sha256 = integrity::compute_sha256(&bytes);
            let artifact = if self.plugin {
                Artifact {
                    kind: "binary".into(),
                    url: None,
                    sha256: None,
                    targets: HashMap::from([(
                        Platform::detect().triple,
                        TargetArtifact {
                            url,
                            sha256,
                            executable_path: entry.into(),
                        },
                    )]),
                    archive_root: None,
                    include: None,
                    entry: None,
                }
            } else {
                Artifact {
                    kind: "archive".into(),
                    url: Some(url),
                    sha256: Some(sha256),
                    targets: HashMap::new(),
                    archive_root: None,
                    include: None,
                    entry: Some(entry.into()),
                }
            };
            versions.push(VersionEntry {
                version: semver::Version::new(major, 0, 0),
                nu_version: ">=0.100.0".into(),
                verified_with: vec![],
                artifact,
                source: None,
                dependencies: BTreeMap::new(),
                activation: (!self.plugin).then_some(RegistryActivationSpec {
                    kind: "nu-module".into(),
                    import: ModuleImportMode::All,
                    entry: None,
                    overlay_name: None,
                    exports: None,
                    bindings: None,
                }),
                provenance: None,
                evidence_tier: None,
                deferral_reason: None,
            });
        }
        let index = RegistryIndex {
            schema_version: 1,
            updated_at: "2026-10-18T00:00:00Z".into(),
            registry_revision: None,
            trust: None,
            packages: vec![Package {
                id: ScopedId::parse(self.id()).unwrap(),
                description: "Offline signed lifecycle fixture".into(),
                repo: "https://example.invalid".into(),
                package_type: if self.plugin {
                    PackageType::Plugin
                } else {
                    PackageType::Module
                },
                tags: vec![],
                versions,
            }],
        };
        let key = SigningKey::from_bytes(&[71; 32]);
        let mut trust = TrustStore::load(root).unwrap();
        trust
            .add_key("fixture", &STANDARD.encode(key.verifying_key().to_bytes()))
            .unwrap();
        trust.save(root).unwrap();
        let value = serde_json::to_value(index).unwrap();
        let sig = RegistrySignature::new(
            "fixture",
            &STANDARD.encode(key.sign(&canonical_json_bytes(&value).unwrap()).to_bytes()),
        );
        let registry = root.join("registry/fixture");
        std::fs::create_dir_all(&registry).unwrap();
        std::fs::write(
            registry.join("index.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        std::fs::write(
            registry.join("index.json.sig"),
            serde_json::to_vec(&sig).unwrap(),
        )
        .unwrap();
    }
    fn install(&self, version: &str) {
        let review = operations::prepare(
            self.root(),
            Action::Install,
            self.id(),
            "fixture",
            Some(version),
        )
        .unwrap();
        quiet(|| operations::execute(self.root(), &review)).unwrap();
    }
}

fn quiet<T>(run: impl FnOnce() -> T) -> T {
    output::with_sink(|_| {}, run)
}

struct FakeRunner;
impl CandidateRunner for FakeRunner {
    fn run(&self, candidate: &Path) -> Result<()> {
        assert!(std::fs::read_to_string(candidate)?.contains("use "));
        Ok(())
    }
}

fn inventory(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    if path.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(inventory(&path));
            } else {
                files.insert(path.clone(), std::fs::read(path).unwrap());
            }
        }
    }
    files
}

#[test]
fn browse_is_inert_with_missing_root_signed_cache_and_pending_journal() {
    let temp = tempfile::tempdir().unwrap();
    let absent = temp.path().join("absent");
    operations::load(&absent).unwrap();
    assert!(!absent.exists());
    let fixture = Fixture::new(false);
    std::fs::create_dir_all(fixture.root().join("state")).unwrap();
    std::fs::write(
        fixture.root().join("state/pending-autoload.json"),
        b"retained evidence",
    )
    .unwrap();
    let before = inventory(fixture.root());
    for _ in 0..3 {
        assert_eq!(operations::load(fixture.root()).unwrap().catalog.len(), 1);
    }
    assert_eq!(inventory(fixture.root()), before);
    assert!(operations::prepare(
        fixture.root(),
        Action::Install,
        fixture.id(),
        "fixture",
        None
    )
    .unwrap_err()
    .to_string()
    .contains("recovery"));
}

#[test]
fn signed_install_is_inert_then_separate_activation_uses_both_lanes() {
    for plugin in [false, true] {
        let fixture = Fixture::new(plugin);
        fixture.install("1.0.0");
        let installed = Lockfile::load(fixture.root()).unwrap();
        assert_eq!(
            operations::activation(&installed.packages[fixture.id()], Some(&fixture.paths)),
            "inactive"
        );
        assert!(!Path::new(&fixture.paths.plugin_registry_path).exists());
        assert!(
            !Path::new(fixture.paths.vendor_autoload_dir.as_ref().unwrap())
                .join("numan.nu")
                .exists()
        );
        let review = operations::prepare(
            fixture.root(),
            Action::Activate,
            fixture.id(),
            "fixture",
            None,
        )
        .unwrap();
        quiet(|| {
            operations::execute_with_activation(fixture.root(), &review, &|args, root, lock| {
                crate::cmd::activate::execute_under_lock_with_hooks(
                    args,
                    root,
                    lock,
                    &|_, _, _| Ok(()),
                    Some(&FakeRunner),
                )
            })
        })
        .unwrap();
        let installed = Lockfile::load(fixture.root()).unwrap();
        assert_eq!(
            operations::activation(&installed.packages[fixture.id()], Some(&fixture.paths)),
            "active"
        );
        let mut other = fixture.paths.clone();
        other.nu_executable_hash = "different".into();
        assert_eq!(
            operations::activation(&installed.packages[fixture.id()], Some(&other)),
            "other Nu"
        );
        assert!(!crate::state::snapshot::list_snapshots(fixture.root())
            .unwrap()
            .is_empty());
    }
}

#[test]
fn stale_review_and_lock_contention_refuse_before_install() {
    let fixture = Fixture::new(false);
    let review = operations::prepare(
        fixture.root(),
        Action::Install,
        fixture.id(),
        "fixture",
        None,
    )
    .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(fixture.root().join("config.toml"))
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    assert!(quiet(|| operations::execute(fixture.root(), &review))
        .unwrap_err()
        .to_string()
        .contains("changed since review"));
    assert!(Lockfile::load(fixture.root()).unwrap().is_empty());
    let review = operations::prepare(
        fixture.root(),
        Action::Install,
        fixture.id(),
        "fixture",
        None,
    )
    .unwrap();
    let _lock = acquire_mutation_lock(fixture.root()).unwrap();
    assert!(quiet(|| operations::execute(fixture.root(), &review))
        .unwrap_err()
        .to_string()
        .contains("already in progress"));
}

#[test]
fn update_remove_revalidate_and_read_back_authoritative_state() {
    let fixture = Fixture::new(false);
    fixture.install("1.0.0");
    let update = operations::prepare(
        fixture.root(),
        Action::Update,
        fixture.id(),
        "fixture",
        None,
    )
    .unwrap();
    assert_eq!(update.version, "2.0.0");
    quiet(|| operations::execute(fixture.root(), &update)).unwrap();
    let remove = operations::prepare(
        fixture.root(),
        Action::Remove,
        fixture.id(),
        "fixture",
        None,
    )
    .unwrap();
    quiet(|| operations::execute(fixture.root(), &remove)).unwrap();
    assert!(Lockfile::load(fixture.root()).unwrap().is_empty());
}

#[test]
fn scan_preserves_existing_probe_named_file_and_performs_no_repairs() {
    let fixture = Fixture::new(false);
    let probe = fixture.root().join(".doctor-write-test");
    std::fs::write(&probe, b"user-owned").unwrap();
    let before = inventory(fixture.root());
    let report = quiet(|| operations::scan(fixture.root())).unwrap();
    assert!(report.contains("No repairs or network probes"));
    assert_eq!(before, inventory(fixture.root()));
}

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}
fn app(fixture: &Fixture) -> App {
    let mut app = App::new(fixture.root());
    app.data = Some(operations::load(fixture.root()).unwrap());
    app
}

#[test]
fn focus_and_overlays_cannot_leak_action_keys() {
    let fixture = Fixture::new(false);
    let mut app = app(&fixture);
    app.navigate(View::Discover);
    app.focus = Focus::Navigation;
    for c in "aixurd".chars() {
        assert!(app.key(key(c)).is_none());
    }
    app.focus = Focus::List;
    for field in [false, true] {
        app.overlay = Some(if field {
            Overlay::Filter(String::new())
        } else {
            Overlay::Launcher {
                query: String::new(),
                selected: 0,
            }
        });
        for c in "aixurqd?/".chars() {
            assert!(app.key(key(c)).is_none());
            assert!(!app.exit);
        }
        assert!(app.overlay.is_some());
    }
    app.overlay = None;
    assert!(app
        .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .is_none());
    assert!(matches!(app.overlay, Some(Overlay::Actions { .. })));
    app.overlay = None;
    assert!(matches!(
        app.key(key('i')),
        Some(Command::Prepare(Action::Install, _, _, _))
    ));
    let review = operations::prepare(
        fixture.root(),
        Action::Install,
        fixture.id(),
        "fixture",
        None,
    )
    .unwrap();
    app.overlay = Some(Overlay::Review {
        review: review.clone(),
        confirm: false,
        scroll: 0,
    });
    assert!(app
        .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .is_none());
    assert!(app.overlay.is_none());
    app.overlay = Some(Overlay::Review {
        review,
        confirm: false,
        scroll: 0,
    });
    app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert!(matches!(
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        Some(Command::Execute(_))
    ));
}

#[test]
fn layout_handles_small_wide_and_long_content() {
    let fixture = Fixture::new(false);
    for (width, height) in [(40, 10), (60, 16), (80, 24), (120, 35)] {
        let mut app = app(&fixture);
        app.navigate(View::Discover);
        app.status = "diagnostic ".repeat(100);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("numan"));
        if width >= 60 {
            assert!(text.contains("Focus:"));
        }
        app.key(key('?'));
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    }
}

#[test]
fn cli_handoff_prints_only_on_explicit_exit_and_never_executes() {
    let root = Path::new("a path/$value/quote'\"here");
    let mut app = App::new(root);
    let args = vec!["registry".into(), "sync".into()];
    for shell in [Shell::Nushell, Shell::Pwsh, Shell::Posix] {
        let command = app.handoff(&args, shell);
        assert!(command.starts_with("numan --root "));
        assert!(command.contains("sync"));
    }
    assert_eq!(Shell::Pwsh.quote("a'b"), "'a''b'");
    assert_eq!(Shell::Posix.quote("a'b"), "'a'\"'\"'b'");
    app.overlay = Some(Overlay::Handoff {
        args: args.clone(),
        shell: Shell::Nushell,
    });
    app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.exit_command.is_none());
    assert!(!app.exit);
    app.overlay = Some(Overlay::Handoff {
        args,
        shell: Shell::Nushell,
    });
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.exit_command.is_some());
    assert!(app.exit);
}

#[test]
#[ignore = "requires NUMAN_TEST_REAL_NU; isolated root and local signed artifact only"]
fn real_nu_module_install_activate_spike() {
    let mut fixture = Fixture::new(false);
    let binary =
        PathBuf::from(std::env::var_os("NUMAN_TEST_REAL_NU").expect("set NUMAN_TEST_REAL_NU"));
    fixture.paths.nu_version = crate::core::nu_version::NuVersion::from_binary(&binary)
        .unwrap()
        .version;
    fixture.paths.nu_executable = binary.to_string_lossy().into();
    fixture.paths.nu_executable_hash = integrity::compute_sha256(&std::fs::read(&binary).unwrap());
    fixture.paths.save(fixture.root()).unwrap();
    fixture.install("1.0.0");
    let review = operations::prepare(
        fixture.root(),
        Action::Activate,
        fixture.id(),
        "fixture",
        None,
    )
    .unwrap();
    quiet(|| operations::execute(fixture.root(), &review)).unwrap();
    let projection =
        Path::new(fixture.paths.vendor_autoload_dir.as_ref().unwrap()).join("numan.nu");
    assert!(projection.is_file());
    let result = std::process::Command::new(binary)
        .args([
            "-n",
            "-c",
            &format!(
                "source {}; hello",
                Shell::Nushell.quote(&projection.to_string_lossy())
            ),
        ])
        .env("NUMAN_TEST_PROJECTION", projection)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("version 1"));
}

#[test]
fn commands_view_form_editing_never_leaks_action_keys() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path());
    app.view = View::Commands;
    app.focus = Focus::List;
    assert_eq!(
        app.command_specs().len(),
        crate::tui::catalog::catalog().len()
    );
    app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.command_selected, 1);
    app.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.command_selected, 0);

    app.open_command_form("snapshot rollback");
    assert!(matches!(app.overlay, Some(Overlay::CommandForm(_))));
    // Enter edits the first field; typed action letters are text, never shortcuts.
    assert!(app
        .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .is_none());
    for c in "qixaur".chars() {
        assert!(app.key(key(c)).is_none());
        assert!(!app.exit);
        assert!(matches!(app.overlay, Some(Overlay::CommandForm(_))));
    }
    assert!(app
        .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .is_none());
    if let Some(Overlay::CommandForm(form)) = &app.overlay {
        assert_eq!(form.fields[0].value, "qixaur");
    } else {
        panic!("form closed unexpectedly");
    }
    // Move to the preview row and edit it: appending a space keeps it valid,
    // and committing must re-parse and rebuild the form.
    app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.key(key(' '));
    assert!(app
        .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .is_none());
    if let Some(Overlay::CommandForm(form)) = &app.overlay {
        assert!(form.error.is_none(), "{:?}", form.error);
        assert!(form
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("Preview valid")));
    } else {
        panic!("form closed unexpectedly");
    }
    // Invalid preview edits stay in the editor with an error.
    app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.key(key(';'));
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    if let Some(Overlay::CommandForm(form)) = &app.overlay {
        assert!(form.error.is_some());
        assert!(form.editing.is_some(), "invalid edit must stay editable");
    } else {
        panic!("form closed unexpectedly");
    }
    app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.overlay.is_none());
    assert!(!app.exit);
}

#[test]
fn commands_browsing_and_form_editing_are_inert() {
    let fixture = Fixture::new(false);
    let before = inventory(fixture.root());
    let mut app = app(&fixture);
    app.focus = Focus::Navigation;
    for _ in 0..3 {
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(app.view, View::Commands);
    app.focus = Focus::List;
    app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.open_command_form("registry add");
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    for c in "custom".chars() {
        app.key(key(c));
    }
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.overlay.is_none());
    assert_eq!(inventory(fixture.root()), before);
}

#[test]
fn commands_layout_lists_catalog_and_details() {
    let fixture = Fixture::new(false);
    let mut app = app(&fixture);
    app.view = View::Commands;
    let mut terminal = Terminal::new(TestBackend::new(120, 35)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("Commands"), "{text}");
    assert!(text.contains("Command details"), "{text}");
    assert!(text.contains("Phase"), "{text}");
}

#[test]
fn read_only_execution_captures_output_and_refuses_mutations() {
    let fixture = Fixture::new(false);
    let listed = operations::execute_read_only(fixture.root(), &["list".into()]).unwrap();
    assert!(listed.contains("No packages installed"), "{listed}");
    let registries =
        operations::execute_read_only(fixture.root(), &["registry".into(), "list".into()]).unwrap();
    assert!(!registries.is_empty());
    let snapshots =
        operations::execute_read_only(fixture.root(), &["snapshot".into(), "list".into()]).unwrap();
    assert!(snapshots.contains("No snapshots"), "{snapshots}");
    for argv in [
        vec!["remove", "owner/pkg"],
        vec!["snapshot", "rollback", "test-snapshot-id"],
        vec!["registry", "sync"],
        vec!["update", "--self"],
    ] {
        let argv: Vec<String> = argv.into_iter().map(str::to_string).collect();
        assert!(
            operations::execute_read_only(fixture.root(), &argv).is_err(),
            "{argv:?} must refuse in Phase 1"
        );
    }
    // Read-only execution must not write.
    let before = inventory(fixture.root());
    operations::execute_read_only(fixture.root(), &["list".into()]).unwrap();
    assert_eq!(inventory(fixture.root()), before);
}

#[test]
fn form_execute_row_returns_read_only_command() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path());
    app.open_command_form("list");
    // No fields: row 0 is preview, row 1 is execute.
    app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(matches!(
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        Some(Command::ExecuteCli(argv)) if argv == vec!["list".to_string()]
    ));
    assert!(app.overlay.is_none());
    // A mutating command opens the generic review instead of executing.
    app.open_command_form("remove");
    // Fields: package, yes, force. Preview is row 3, execute is row 4.
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    for c in "owner/pkg".chars() {
        app.key(key(c));
    }
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    for _ in 0..4 {
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert!(app
        .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .is_none());
    assert!(
        matches!(app.overlay, Some(Overlay::GenericReview { .. })),
        "remove must stop at the review, not execute"
    );
}

#[test]
fn generic_review_gates_mutating_execution_and_mode_classifies() {
    use crate::tui::catalog::{self, ExecutionMode};
    assert_eq!(
        catalog::execution_mode("tui", &[]),
        ExecutionMode::Blocked("The TUI cannot open inside itself.")
    );
    assert_eq!(
        catalog::execution_mode("list", &[]),
        ExecutionMode::ReadOnly
    );

    let root = Path::new("/tmp/numan-commands-test");
    let mut app = App::new(root);
    let remove = catalog::spec("remove").unwrap();
    assert_eq!(
        catalog::execution_mode(
            "remove",
            &remove
                .fields
                .iter()
                .map(FieldState::from_spec)
                .collect::<Vec<_>>()
        ),
        ExecutionMode::Reviewed
    );
    app.open_command_form("remove");
    // Fill the required package field.
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    for c in "owner/pkg".chars() {
        app.key(key(c));
    }
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    // Fields: package, yes, force. Preview is row 3, execute is row 4.
    for _ in 0..4 {
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert!(app
        .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .is_none());
    match &app.overlay {
        Some(Overlay::GenericReview {
            effects, confirm, ..
        }) => {
            assert!(!confirm, "review must default to Cancel");
            assert!(effects.iter().any(|line| line.contains("Destructive")));
            assert!(effects.iter().any(|line| line.contains("--yes")));
        }
        other => panic!("expected generic review, got {}", other.is_some()),
    }
    // Enter on the default Cancel choice closes the review without executing.
    assert!(app
        .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .is_none());
    assert!(app.overlay.is_none(), "Cancel must close the review");
    // Re-open the review, arm Confirm with Tab, and execute.
    app.open_command_form("remove");
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    for c in "owner/pkg".chars() {
        app.key(key(c));
    }
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    for _ in 0..4 {
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert!(app
        .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .is_none());
    assert!(matches!(app.overlay, Some(Overlay::GenericReview { .. })));
    app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert!(matches!(
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        Some(Command::ExecuteCli(argv)) if argv == vec!["remove".to_string(), "owner/pkg".to_string()]
    ));
}

#[test]
fn generic_executor_installs_and_removes_and_refuses_nested_tui() {
    let fixture = Fixture::new(false);
    let installed = quiet(|| {
        operations::execute_cli(
            fixture.root(),
            &["install".to_string(), fixture.id().to_string()],
        )
    })
    .unwrap();
    assert!(installed.contains("completed"), "{installed}");
    assert!(Lockfile::load(fixture.root())
        .unwrap()
        .packages
        .contains_key(fixture.id()));
    // gc --dry-run runs through the reviewed executor without deleting.
    quiet(|| operations::execute_cli(fixture.root(), &["gc".into(), "--dry-run".into()])).unwrap();
    assert!(Lockfile::load(fixture.root())
        .unwrap()
        .packages
        .contains_key(fixture.id()));
    // Remove injects --yes only at execution; the lockfile entry must be gone.
    quiet(|| {
        operations::execute_cli(
            fixture.root(),
            &["remove".to_string(), fixture.id().to_string()],
        )
    })
    .unwrap();
    assert!(Lockfile::load(fixture.root()).unwrap().is_empty());
    // Nested TUI and unknown execution refuse without state changes.
    assert!(operations::execute_cli(fixture.root(), &["tui".into()]).is_err());
    assert!(operations::execute_cli(
        fixture.root(),
        &[
            "snapshot".into(),
            "rollback".into(),
            "test-snapshot-id".into()
        ]
    )
    .is_err());
}

#[test]
fn commands_list_shows_scrollbar_and_tier_badges() {
    let fixture = Fixture::new(false);
    let mut app = app(&fixture);
    app.view = View::Commands;
    // Short height: 40 catalog rows cannot fit, so a scrollbar thumb appears.
    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("\u{2588}"), "scrollbar thumb missing: {text}");
    assert!(text.contains("Destructive"), "{text}");
    assert!(text.contains("ReadOnly"), "{text}");
}

#[test]
fn arrow_focus_is_bounded_and_refresh_requires_explicit_review() {
    let fixture = Fixture::new(false);
    let mut app = app(&fixture);
    app.focus = Focus::Navigation;
    for expected in [Focus::List, Focus::Details, Focus::Details] {
        assert!(app
            .key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
            .is_none());
        assert_eq!(app.focus, expected);
    }
    for expected in [Focus::List, Focus::Navigation, Focus::Navigation] {
        app.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.focus, expected);
    }
    app.key(key('R'));
    assert!(matches!(app.overlay, Some(Overlay::Sync)));
    app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.overlay.is_none());
    app.key(key('R'));
    assert!(matches!(
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        Some(Command::Sync)
    ));
    app.overlay = Some(Overlay::Filter(String::new()));
    assert!(app.key(key('R')).is_none());
    assert!(matches!(app.overlay, Some(Overlay::Filter(_))));
}
