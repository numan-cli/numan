#[test]
fn session_overlay_list_requires_review_and_reconcile_has_neutral_guidance() {
    use super::catalog::{self, ExecutionMode};
    let spec = catalog::spec("overlay list").unwrap();
    let argv = vec![
        "--session".into(),
        uuid::Uuid::new_v4().to_string(),
        "--pid".into(),
        std::process::id().to_string(),
    ];
    let fields = catalog::fields_from_argv(spec, &argv);
    assert_eq!(
        catalog::execution_mode("overlay list", &fields),
        ExecutionMode::Reviewed
    );
    let root = tempfile::tempdir().unwrap();
    let mut command = vec!["overlay".into(), "list".into()];
    command.extend(argv);
    let note = super::operations::execute_cli(root.path(), &command).unwrap();
    assert!(!note.contains("start an interactive"));
    let note = super::operations::execute_cli(root.path(), &["overlay".into(), "reconcile".into()])
        .unwrap();
    assert!(!note.contains("start an interactive"));
}

use super::{
    app::{App, Command, Focus, LauncherAction, Overlay, Shell, TypeFilter, View},
    catalog::FieldState,
    operations::{self, Action},
    theme, ui,
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
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
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
                    entry_points: Vec::new(),
                    targets: Vec::new(),
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
            } else if path.file_name().and_then(|n| n.to_str()) != Some("mutation.lock") {
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
    app.navigate(View::System);
    app.focus = Focus::Details;
    for c in "aixud".chars() {
        assert!(app.key(key(c)).is_none());
    }
    app.navigate(View::Discover);
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
    app.key(key('4'));
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
    assert!(installed.contains("completed"));
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
    assert_eq!(app.focus, Focus::List);
    for expected in [Focus::Details, Focus::Details] {
        assert!(app
            .key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
            .is_none());
        assert_eq!(app.focus, expected);
    }
    for expected in [Focus::List, Focus::List] {
        app.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.focus, expected);
    }
    app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.focus, Focus::Details);
    app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.focus, Focus::List);

    for (k, expected) in [
        ('1', View::Installed),
        ('2', View::Discover),
        ('3', View::Updates),
        ('4', View::Commands),
        ('5', View::System),
    ] {
        app.key(key(k));
        assert_eq!(app.view, expected);
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

#[test]
fn discover_labels_install_only_types_before_install() {
    let fixture = Fixture::new(false);
    let mut app = app(&fixture);
    // Clone the fixture catalog item as an install-only script package (no entry points),
    // plus an activatable script package (with entry_points).
    let mut script_item = app.data.as_ref().unwrap().catalog[0].clone();
    script_item.package.id = ScopedId::parse("test/fixture-script").unwrap();
    script_item.package.package_type = PackageType::Script;
    for v in &mut script_item.package.versions {
        v.activation = None;
    }
    let mut activatable_script = script_item.clone();
    activatable_script.package.id = ScopedId::parse("test/activatable-script").unwrap();
    for v in &mut activatable_script.package.versions {
        v.activation = Some(RegistryActivationSpec {
            kind: "nu-script".into(),
            import: ModuleImportMode::All,
            entry: None,
            overlay_name: None,
            exports: None,
            bindings: None,
            entry_points: vec![ScriptEntryPoint {
                name: "run-fixture".into(),
                path: "mod.nu".into(),
            }],
            targets: Vec::new(),
        });
    }
    let catalog = &mut app.data.as_mut().unwrap().catalog;
    catalog.push(script_item);
    catalog.push(activatable_script);

    app.navigate(View::Discover);
    let rows = app.rows();
    let row = rows
        .iter()
        .find(|row| row.id == "test/fixture-script")
        .expect("script package must be listed in Discover");
    assert!(
        row.details.contains("Install-only"),
        "Discover details must label install-only script packages: {}",
        row.details
    );
    let act_row = rows
        .iter()
        .find(|row| row.id == "test/activatable-script")
        .expect("activatable script must be listed in Discover");
    assert!(
        !act_row.details.contains("Install-only"),
        "Discover must not mark activatable script install-only: {}",
        act_row.details
    );
    assert!(operations::install_only_note(&PackageType::Script).is_some());
    assert!(operations::install_only_note(&PackageType::Completion).is_some());
    assert!(operations::install_only_note(&PackageType::Module).is_none());
    assert!(operations::install_only_note(&PackageType::Plugin).is_none());
}

#[test]
fn update_self_update_is_blocked_but_check_is_read_only() {
    use super::catalog::{self, ExecutionMode};
    let spec = catalog::spec("update").unwrap();
    let self_fields = catalog::fields_from_argv(spec, &["--self".to_string()]);
    assert!(
        matches!(
            catalog::execution_mode("update", &self_fields),
            ExecutionMode::Blocked(reason) if reason.contains("terminal")
        ),
        "non-check update --self must be gated to a terminal handoff"
    );
    let check_fields =
        catalog::fields_from_argv(spec, &["--self".to_string(), "--check".to_string()]);
    assert_eq!(
        catalog::execution_mode("update", &check_fields),
        ExecutionMode::ReadOnly,
        "update --self --check only prints the upgrade command and stays read-only"
    );
    // Executor-level backstop: even a hand-built argv cannot self-update.
    let dir = tempfile::tempdir().unwrap();
    let error = operations::execute_cli(dir.path(), &["update".to_string(), "--self".to_string()])
        .unwrap_err();
    assert!(
        error.to_string().contains("terminal"),
        "execute_cli must refuse self-update with a handoff message: {error}"
    );
}

#[test]
fn every_catalog_command_has_an_executor_path() {
    use super::catalog::{self, ExecutionMode};
    for spec in catalog::catalog() {
        let fields: Vec<FieldState> = spec.fields.iter().map(FieldState::from_spec).collect();
        let mode = catalog::execution_mode(&spec.display, &fields);
        if spec.display == "tui" {
            assert!(
                matches!(mode, ExecutionMode::Blocked(_)),
                "tui must stay blocked inside itself"
            );
            continue;
        }
        assert!(
            !matches!(mode, ExecutionMode::Blocked(_)),
            "{} must classify to an executor path, got {mode:?}",
            spec.display
        );
    }
}

#[test]
fn dry_run_and_pure_status_commands_are_lock_free_and_inert() {
    let fixture = Fixture::new(false);
    fixture.install("1.0.0");
    std::fs::create_dir_all(fixture.root().join("state")).unwrap();
    std::fs::write(
        fixture.root().join("state/pending-autoload.json"),
        b"retained evidence",
    )
    .unwrap();
    let nupm_dir = fixture.root().join("nupm-home");
    std::fs::create_dir_all(&nupm_dir).unwrap();
    let nupm_str = nupm_dir.to_string_lossy().into_owned();
    let _held_lock = acquire_mutation_lock(fixture.root()).unwrap();
    let before = inventory(fixture.root());

    for argv in [
        vec!["gc", "--dry-run"],
        vec!["migrate", "--nupm-home", nupm_str.as_str(), "--dry-run"],
        vec!["setup", "autoupdate", "--status"],
        vec!["update", "--check"],
        vec!["update", "--all", "--check", "--no-sync"],
    ] {
        let argv_str: Vec<String> = argv.into_iter().map(str::to_string).collect();
        let output = quiet(|| operations::execute_read_only(fixture.root(), &argv_str))
            .unwrap_or_else(|e| panic!("{argv_str:?} failed while lock held: {e:#}"));
        assert!(!output.is_empty(), "{argv_str:?} produced empty output");
    }
    assert!(fixture.root().join("state/pending-autoload.json").exists());
    assert!(Lockfile::load(fixture.root())
        .unwrap()
        .packages
        .contains_key(fixture.id()));
    assert_eq!(inventory(fixture.root()), before);

    // Pure `setup loader --status` neither acquires mutation lock nor creates config_dir.
    let missing_dir = fixture.root().join("nonexistent-nu-cfg");
    let config_probe = missing_dir.join("config.nu");
    let args = crate::cmd::setup::LoaderArgs {
        status: true,
        ..Default::default()
    };
    assert!(args.is_pure_status());
    quiet(|| {
        crate::cmd::setup::execute_loader_with_probe_and_root(&args, Some(fixture.root()), || {
            Ok(config_probe.clone())
        })
    })
    .unwrap();
    assert!(
        !missing_dir.exists(),
        "pure `setup loader --status` must not create missing Nu config dir"
    );
}

#[test]
fn doctor_non_scan_is_reviewed_and_skip_stdio_redirect_preserves_stdout() {
    use super::catalog::{self, ExecutionMode};
    let spec = catalog::spec("doctor").unwrap();
    assert_eq!(
        catalog::execution_mode(
            "doctor",
            &catalog::fields_from_argv(spec, &["--json".into()])
        ),
        ExecutionMode::Reviewed
    );
    assert_eq!(
        catalog::execution_mode(
            "doctor",
            &catalog::fields_from_argv(spec, &["--scan".into(), "--json".into()])
        ),
        ExecutionMode::ReadOnly
    );

    let fixture = Fixture::new(false);
    let scan_json = quiet(|| {
        operations::execute_read_only(
            fixture.root(),
            &["doctor".into(), "--scan".into(), "--json".into()],
        )
    })
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&scan_json)
        .unwrap_or_else(|e| panic!("scan JSON invalid: {e}: {scan_json}"));
    assert!(parsed.get("schema_version").is_some());
    assert!(parsed.get("findings").is_some());

    // Direct non-scan --json with skip_stdio_redirect captures valid JSON into writer cleanly.
    let mut out = Vec::new();
    let args = crate::cmd::doctor::DoctorArgs {
        scan: false,
        json: true,
        nupm_home: None,
    };
    let options = crate::cmd::doctor::DoctorOptions {
        skip_network: true,
        skip_stdio_redirect: true,
        ..Default::default()
    };
    let code = quiet(|| {
        crate::cmd::doctor::execute_to_with_options(&args, fixture.root(), options, &mut out)
    })
    .unwrap();
    assert!(matches!(code, 0 | 1), "unexpected doctor exit code {code}");
    let repaired: serde_json::Value = serde_json::from_slice(&out).unwrap_or_else(|e| {
        panic!(
            "repair JSON invalid: {e}: {}",
            String::from_utf8_lossy(&out)
        )
    });
    assert!(repaired.get("repairs").is_some());
}

#[test]
fn snapshot_and_overlay_cli_capture_preserves_unredacted_uuids() {
    let fixture = Fixture::new(false);
    fixture.install("1.0.0");
    let mut lf = Lockfile::load(fixture.root()).unwrap();
    if let Some(entry) = lf.packages.get_mut(fixture.id()) {
        entry.overlay = Some(crate::state::lockfile::OverlayPackageSpec {
            name: "fixture_overlay".into(),
            exports: vec![],
        });
    }
    lf.save(fixture.root()).unwrap();
    let snap = crate::state::snapshot::create_snapshot(
        fixture.root(),
        crate::state::snapshot::SnapshotReason::PreMutation,
        crate::state::snapshot::SnapshotTrigger::Activate,
        None,
        None,
    )
    .unwrap();
    let snapshot_id = snap.id;

    let listed = quiet(|| {
        operations::execute_read_only(fixture.root(), &["snapshot".into(), "list".into()])
    })
    .unwrap();
    assert!(
        listed.contains(&snapshot_id),
        "snapshot list must preserve full UUIDv7 identifier"
    );
    assert!(!listed.contains("[redacted UUID]"));

    let inspected = quiet(|| {
        operations::execute_read_only(
            fixture.root(),
            &["snapshot".into(), "inspect".into(), snapshot_id.clone()],
        )
    })
    .unwrap();
    assert!(inspected.contains(&snapshot_id));

    let overlay_use = quiet(|| {
        operations::execute_cli(
            fixture.root(),
            &["overlay".into(), "use".into(), fixture.id().into()],
        )
    })
    .unwrap();
    assert!(overlay_use.contains("overlay use"));
    assert!(overlay_use.contains("interactive Nushell"));
}

struct NoopRunner;
impl CandidateRunner for NoopRunner {
    fn run(&self, candidate: &Path) -> Result<()> {
        let _ = std::fs::read_to_string(candidate)?;
        Ok(())
    }
}

#[test]
fn deactivate_prepare_execute_and_post_deactivate_remove() {
    for plugin in [false, true] {
        let fixture = Fixture::new(plugin);
        fixture.install("1.0.0");
        let activate_review = operations::prepare(
            fixture.root(),
            Action::Activate,
            fixture.id(),
            "fixture",
            None,
        )
        .unwrap();
        quiet(|| {
            operations::execute_with_lifecycle(
                fixture.root(),
                &activate_review,
                &|args, root, lock| {
                    crate::cmd::activate::execute_under_lock_with_hooks(
                        args,
                        root,
                        lock,
                        &|_, _, _| Ok(()),
                        Some(&FakeRunner),
                    )
                },
                &|_, _, _| unreachable!(),
            )
        })
        .unwrap();

        let deactivate_review = operations::prepare(
            fixture.root(),
            Action::Deactivate,
            fixture.id(),
            "fixture",
            None,
        )
        .unwrap();
        assert_eq!(deactivate_review.action, Action::Deactivate);

        // Stale review check refuses Deactivate if lockfile/config changes before execution.
        std::fs::OpenOptions::new()
            .append(true)
            .open(fixture.root().join("config.toml"))
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        assert!(quiet(|| {
            operations::execute_with_lifecycle(
                fixture.root(),
                &deactivate_review,
                &|_, _, _| unreachable!(),
                &|args, root, validate| {
                    crate::cmd::deactivate::execute_reviewed_with_candidate_runner_and_unregistrar(
                        args,
                        root,
                        Some(&NoopRunner),
                        &|_, _, _| Ok(()),
                        validate,
                    )
                },
            )
        })
        .unwrap_err()
        .to_string()
        .contains("changed since review"));

        // Re-prepare & execute Deactivate cleanly.
        let deactivate_review = operations::prepare(
            fixture.root(),
            Action::Deactivate,
            fixture.id(),
            "fixture",
            None,
        )
        .unwrap();
        quiet(|| {
            operations::execute_with_lifecycle(
                fixture.root(),
                &deactivate_review,
                &|_, _, _| unreachable!(),
                &|args, root, validate| {
                    crate::cmd::deactivate::execute_reviewed_with_candidate_runner_and_unregistrar(
                        args,
                        root,
                        Some(&NoopRunner),
                        &|_, _, _| Ok(()),
                        validate,
                    )
                },
            )
        })
        .unwrap();
        let lockfile = Lockfile::load(fixture.root()).unwrap();
        assert_eq!(
            operations::activation(&lockfile.packages[fixture.id()], Some(&fixture.paths)),
            "inactive"
        );

        // Now Remove succeeds cleanly once deactivated.
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
}

#[test]
fn batch_update_prepares_and_updates_eligible_packages_and_notes_gated() {
    let fixture = Fixture::new(false);
    let root = fixture.root();
    // Write a 3-package signed index (two inactive modules + one active module) with v1.0.0 & v2.0.0.
    let mut pkgs = Vec::new();
    for name in ["test/batch-a", "test/batch-b", "test/batch-active"] {
        let mut versions = Vec::new();
        for major in [1u64, 2] {
            let short = name.split('/').next_back().unwrap();
            let archive_path = root.join(format!("{short}-{major}.tar.gz"));
            let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);
            let content = format!("export def hi [] {{ '{short} v{major}' }}\n");
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            archive
                .append_data(&mut header, "mod.nu", content.as_bytes())
                .unwrap();
            let bytes = archive.into_inner().unwrap().finish().unwrap();
            std::fs::write(&archive_path, &bytes).unwrap();
            let url = url::Url::from_file_path(&archive_path).unwrap().to_string();
            let sha256 = integrity::compute_sha256(&bytes);
            versions.push(VersionEntry {
                version: semver::Version::new(major, 0, 0),
                nu_version: ">=0.100.0".into(),
                verified_with: vec![],
                artifact: Artifact {
                    kind: "archive".into(),
                    url: Some(url),
                    sha256: Some(sha256),
                    targets: HashMap::new(),
                    archive_root: None,
                    include: None,
                    entry: Some("mod.nu".into()),
                },
                source: None,
                dependencies: BTreeMap::new(),
                activation: Some(RegistryActivationSpec {
                    kind: "nu-module".into(),
                    import: ModuleImportMode::All,
                    entry: None,
                    overlay_name: None,
                    exports: None,
                    bindings: None,
                    entry_points: Vec::new(),
                    targets: Vec::new(),
                }),
                provenance: None,
                evidence_tier: None,
                deferral_reason: None,
            });
        }
        pkgs.push(Package {
            id: ScopedId::parse(name).unwrap(),
            description: "Batch test pkg".into(),
            repo: "https://example.invalid".into(),
            package_type: PackageType::Module,
            tags: vec![],
            versions,
        });
    }
    let index = RegistryIndex {
        schema_version: 1,
        updated_at: "2026-10-18T00:00:00Z".into(),
        registry_revision: None,
        trust: None,
        packages: pkgs,
    };
    let key = SigningKey::from_bytes(&[71; 32]);
    let value = serde_json::to_value(index).unwrap();
    let sig = RegistrySignature::new(
        "fixture",
        &STANDARD.encode(key.sign(&canonical_json_bytes(&value).unwrap()).to_bytes()),
    );
    let registry = root.join("registry/fixture");
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

    for id in ["test/batch-a", "test/batch-b", "test/batch-active"] {
        let rev = operations::prepare(root, Action::Install, id, "fixture", Some("1.0.0")).unwrap();
        quiet(|| operations::execute(root, &rev)).unwrap();
    }
    let act_rev =
        operations::prepare(root, Action::Activate, "test/batch-active", "fixture", None).unwrap();
    quiet(|| {
        operations::execute_with_activation(root, &act_rev, &|args, root, lock| {
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

    let batch = operations::prepare(root, Action::BatchUpdate, "", "", None).unwrap();
    assert_eq!(batch.action, Action::BatchUpdate);
    assert_eq!(batch.targets.len(), 2);
    assert!(batch
        .effects
        .iter()
        .any(|e| e.contains("Skipped:") && e.contains("test/batch-active")));

    let summary = quiet(|| operations::execute(root, &batch)).unwrap();
    assert!(summary.contains("BatchUpdate completed"));
    let lf = Lockfile::load(root).unwrap();
    assert_eq!(lf.packages["test/batch-a"].version, "2.0.0");
    assert_eq!(lf.packages["test/batch-b"].version, "2.0.0");
    assert_eq!(lf.packages["test/batch-active"].version, "1.0.0");
}

#[test]
fn active_package_remove_and_update_offer_deactivate_followup() {
    let fixture = Fixture::new(false);
    fixture.install("1.0.0");
    let act_rev = operations::prepare(
        fixture.root(),
        Action::Activate,
        fixture.id(),
        "fixture",
        None,
    )
    .unwrap();
    quiet(|| {
        operations::execute_with_activation(fixture.root(), &act_rev, &|args, root, lock| {
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

    let mut app = app(&fixture);
    app.navigate(View::Installed);
    assert!(app.key(key('x')).is_none());
    assert!(matches!(
        app.overlay,
        Some(Overlay::Text {
            ref title,
            follow_up: Some(_),
            ..
        }) if title == "Deactivate required"
    ));
    assert!(matches!(
        app.key(key('d')),
        Some(Command::Prepare(Action::Deactivate, id, _, None)) if id == fixture.id()
    ));

    app.overlay = None;
    app.navigate(View::Updates);
    assert_eq!(app.rows().len(), 1);
    assert!(app.key(key('u')).is_none());
    assert!(matches!(
        app.overlay,
        Some(Overlay::Text {
            ref title,
            follow_up: Some(_),
            ..
        }) if title == "Active package update gated"
    ));
    assert!(matches!(
        app.key(key('d')),
        Some(Command::Prepare(Action::Deactivate, id, _, None)) if id == fixture.id()
    ));
}

#[test]
fn post_install_handle_finished_offers_activate_only_when_activatable() {
    let fixture = Fixture::new(false);
    let review = operations::prepare(
        fixture.root(),
        Action::Install,
        fixture.id(),
        "fixture",
        Some("1.0.0"),
    )
    .unwrap();
    quiet(|| operations::execute(fixture.root(), &review)).unwrap();

    let mut review_only = review.clone();
    review_only.id = "test/install-only-script".into();
    let mut app = app(&fixture);
    super::handle_finished(
        &mut app,
        fixture.root(),
        Some(Command::Execute(review)),
        Ok("Install completed; authoritative state reloaded.".into()),
    );
    assert!(matches!(
        app.overlay,
        Some(Overlay::Text {
            follow_up: Some(ref fu),
            ..
        }) if fu.key == 'a' && fu.label.contains("Activate")
    ));
    assert!(matches!(
        app.key(key('a')),
        Some(Command::Prepare(Action::Activate, id, _, None)) if id == fixture.id()
    ));

    // Install-only script (no entry_points) must NOT offer an Activate follow-up.
    let mut app2 = App::new(fixture.root());
    let mut lf = Lockfile::load(fixture.root()).unwrap();
    let mut entry = lf.packages.values().next().unwrap().clone();
    entry.package_type = "script".into();
    entry.script_entry_points = None;
    lf.packages.clear();
    lf.packages.insert("test/install-only-script".into(), entry);
    lf.save(fixture.root()).unwrap();
    super::handle_finished(
        &mut app2,
        fixture.root(),
        Some(Command::Execute(review_only)),
        Ok("Install completed.".into()),
    );
    assert!(matches!(
        app2.overlay,
        Some(Overlay::Text {
            follow_up: None,
            ..
        })
    ));
}

#[test]
fn ctrl_p_fuzzy_palette_searches_all_sources_and_opens_forms_in_place() {
    let fixture = Fixture::new(false);
    // Pre-install: on Discover, empty Ctrl+P query offers Install smart action for selected compatible package.
    let mut pre_app = app(&fixture);
    pre_app.navigate(View::Discover);
    assert!(pre_app
        .launcher_items("")
        .iter()
        .any(|i| matches!(i.action, LauncherAction::Native(Action::Install, ..))));

    fixture.install("1.0.0");
    let _snap = crate::state::snapshot::create_snapshot(
        fixture.root(),
        crate::state::snapshot::SnapshotReason::PreMutation,
        crate::state::snapshot::SnapshotTrigger::Install,
        None,
        None,
    )
    .unwrap();
    let mut app = app(&fixture);
    app.record_recent(
        vec!["gc".into(), "--dry-run".into()],
        "Preview reclaimable".into(),
        true,
    );
    app.navigate(View::Discover);

    // Ctrl+P opens palette
    assert!(app
        .key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .is_none());
    assert!(matches!(app.overlay, Some(Overlay::Launcher { .. })));

    // Empty query includes Recent, installed smart Activate action, and Presets
    let empty_items = app.launcher_items("");
    assert!(empty_items.iter().any(|i| i.tag == "Recent"));
    assert!(empty_items
        .iter()
        .any(|i| matches!(i.action, LauncherAction::Native(Action::Activate, ..))));
    assert!(empty_items.iter().any(|i| i.tag == "Preset"));

    // Typing "rollback" finds `numan snapshot rollback` form and pressing Enter opens it in-place
    for c in "rollb".chars() {
        app.key(key(c));
    }
    let query_items = app.launcher_items("rollb");
    let form_idx = query_items
        .iter()
        .position(|i| {
            matches!(&i.action, LauncherAction::OpenForm { display, .. } if display == "snapshot rollback")
        })
        .expect("snapshot rollback must appear in Ctrl+P fuzzy results");
    for _ in 0..form_idx {
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert!(app
        .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .is_none());
    assert!(matches!(
        app.overlay,
        Some(Overlay::CommandForm(ref f)) if f.display == "snapshot rollback"
    ));
    // Underlying view and cursor remain preserved in-place
    assert_eq!(app.view, View::Discover);
    assert_eq!(app.selected, 0);

    // Esc returns cleanly to underlying tab
    app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.overlay.is_none());
    assert_eq!(app.view, View::Discover);

    // Searching fixture package finds both Installed and Discover/Snapshot matches
    let pkg_items = app.launcher_items("fixture");
    assert!(pkg_items.iter().any(|i| i.tag == "Installed"));
    assert!(pkg_items.iter().any(|i| i.tag == "Activate"));
}

#[test]
fn system_view_renders_five_sections_and_triggers_doctor_scan() {
    let fixture = Fixture::new(false);
    fixture.install("1.0.0");
    let mut app = app(&fixture);
    app.key(key('5'));
    assert_eq!(app.view, View::System);
    let ids: Vec<String> = app.rows().into_iter().map(|r| r.id).collect();
    assert_eq!(
        ids,
        vec![
            "Identity",
            "Doctor",
            "Snapshots",
            "Registries",
            "Transcript"
        ]
    );

    // Doctor is at index 1; pressing [s] in View::System requests Command::Scan
    app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(matches!(app.key(key('s')), Some(Command::Scan)));

    super::handle_finished(
        &mut app,
        fixture.root(),
        Some(Command::Scan),
        Ok("Local doctor scan: 0 errors, 0 warnings. No repairs or network probes.".into()),
    );
    assert!(app.doctor_report.is_some());
    assert_eq!(app.rows()[1].state, "clean");
    assert!(app
        .recent_runs
        .iter()
        .any(|r| r.display == "doctor --scan" && r.read_only));

    app.overlay = None;
    let mut terminal = Terminal::new(TestBackend::new(120, 35)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    for section in [
        "Identity",
        "Doctor",
        "Snapshots",
        "Registries",
        "Transcript",
    ] {
        assert!(text.contains(section), "missing section {section}: {text}");
    }
}

#[test]
fn command_form_pickers_cycle_options_and_support_cursor_editing() {
    let fixture = Fixture::new(false);
    let snap = crate::state::snapshot::create_snapshot(
        fixture.root(),
        crate::state::snapshot::SnapshotReason::PreMutation,
        crate::state::snapshot::SnapshotTrigger::Install,
        None,
        None,
    )
    .unwrap();
    let mut app = app(&fixture);
    app.open_command_form("snapshot rollback");
    // Right arrow cycles SnapshotId picker without entering free-text mode
    app.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    match &app.overlay {
        Some(Overlay::CommandForm(form)) => assert_eq!(form.fields[0].value, snap.id),
        _ => panic!("expected CommandForm"),
    }

    // Cursor editing mid-string: Home -> Right x2 -> insert 'X' -> Backspace -> Delete
    app.open_command_form("info");
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    for c in "abde".chars() {
        app.key(key(c));
    }
    app.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    app.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    app.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    app.key(key('X'));
    app.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    app.key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match &app.overlay {
        Some(Overlay::CommandForm(form)) => assert_eq!(form.fields[0].value, "abe"),
        _ => panic!("expected CommandForm"),
    }

    // Context-prefilled package id must be stripped for `activate --check`
    app.navigate(View::Installed);
    app.open_command_form_with_pkg("activate", Some("test/fixture-module"));
    // Activate fields: packages(0), verbose(1), list(2), check(3), preview(4), execute(5)
    for _ in 0..3 {
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // toggle --check on
    app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let cmd = app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        cmd,
        Some(Command::ExecuteCli(ref argv)) if argv == &vec!["activate".to_string(), "--check".to_string()]
    ));
}

#[test]
fn responsive_layout_mouse_hits_spinner_gauge_and_ascii_borders() {
    let fixture = Fixture::new(false);
    let mut app = app(&fixture);

    // Medium width (65x22): all 5 compact tabs clickable + pane toggle chip switches Focus
    let mut med = Terminal::new(TestBackend::new(65, 22)).unwrap();
    med.draw(|f| ui::draw(f, &mut app)).unwrap();
    assert!(!app.small);
    assert!(!app.wide);
    for (idx, r) in app.regions.tabs.iter().enumerate() {
        assert!(r.width > 0, "tab {idx} was clipped at width 65");
    }
    let toggle = app
        .regions
        .pane_toggle_chip
        .expect("medium layout must expose pane toggle chip");
    assert_eq!(app.focus, Focus::List);
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: toggle.x,
        row: toggle.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.focus, Focus::Details);

    // Wide layout (100x24): click Discover tab [2], click Type chip, click first list row, click [i] Install
    let mut wide = Terminal::new(TestBackend::new(100, 24)).unwrap();
    wide.draw(|f| ui::draw(f, &mut app)).unwrap();
    for (idx, r) in app.regions.tabs.iter().enumerate() {
        assert!(r.width > 0, "tab {idx} was clipped at width 100");
    }
    let disc_tab = app.regions.tabs[1];
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: disc_tab.x + 1,
        row: disc_tab.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.view, View::Discover);

    wide.draw(|f| ui::draw(f, &mut app)).unwrap();
    let type_chip = app.regions.type_chip.expect("Discover type chip missing");
    assert_eq!(app.type_filter, TypeFilter::All);
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: type_chip.x + 1,
        row: type_chip.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.type_filter, TypeFilter::Plugin);
    // Cycle back to All so fixture module appears again
    for _ in 0..4 {
        wide.draw(|f| ui::draw(f, &mut app)).unwrap();
        let chip = app.regions.type_chip.unwrap();
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: chip.x + 1,
            row: chip.y,
            modifiers: KeyModifiers::NONE,
        });
    }
    assert_eq!(app.type_filter, TypeFilter::All);

    wide.draw(|f| ui::draw(f, &mut app)).unwrap();
    let list_rows = app.regions.list_rows_area.expect("list rows rect missing");
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: list_rows.x,
        row: list_rows.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.selected, 0);

    let install_btn = app
        .regions
        .detail_buttons
        .iter()
        .find(|(_, kind)| matches!(kind, super::app::DetailClick::Prepare(Action::Install)))
        .map(|(r, _)| *r)
        .expect("Discover must render clickable Install button");
    let cmd = app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: install_btn.x,
        row: install_btn.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(matches!(cmd, Some(Command::Prepare(Action::Install, ..))));

    // Width 80 check: all 5 tabs including [5]System remain visible & clickable
    let mut edge = Terminal::new(TestBackend::new(80, 24)).unwrap();
    edge.draw(|f| ui::draw(f, &mut app)).unwrap();
    for (idx, r) in app.regions.tabs.iter().enumerate() {
        assert!(r.width > 0, "tab {idx} was clipped at width 80");
    }

    // Spinner & download progress bar render cleanly
    app.busy = true;
    app.spinner_tick = 3;
    app.download_progress = Some((50, Some(100)));
    wide.draw(|f| ui::draw(f, &mut app)).unwrap();
    let text: String = wide
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains(theme::SPINNER_FRAMES[3]));
    assert!(text.contains("50%"));

    // ASCII fallback border set works without panicking
    let ascii = theme::border_set(true);
    assert_eq!(ascii.vertical_left, "|");
    assert_eq!(ascii.horizontal_top, "-");
    assert_eq!(ascii.top_left, "+");
}

#[test]
fn form_mouse_validates_preview_before_dispatching_rebuilt_command() {
    use super::app::{EditTarget, ModalClick};
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path());
    let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
    app.open_command_form("list");
    let Some(Overlay::CommandForm(form)) = &app.overlay else {
        unreachable!()
    };
    let valid_preview = app.command_preview(form);
    for (preview, valid) in [
        (format!("{valid_preview}; bad"), false),
        (valid_preview, true),
    ] {
        app.open_command_form("info");
        if let Some(Overlay::CommandForm(form)) = &mut app.overlay {
            form.selected = form.fields.len();
            form.editing = Some(EditTarget::Preview);
            form.edit_buffer = preview.clone();
            form.edit_cursor = preview.chars().count();
        }
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let execute = match &app.overlay {
            Some(Overlay::CommandForm(form)) => form.fields.len() + 1,
            _ => unreachable!(),
        };
        let rect = app
            .regions
            .modal_buttons
            .iter()
            .find_map(|(rect, click)| {
                matches!(click, ModalClick::FieldRow(index) if *index == execute).then_some(*rect)
            })
            .unwrap();
        let command = app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        if valid {
            assert!(matches!(command, Some(Command::ExecuteCli(argv)) if argv == vec!["list"]));
            assert!(app.overlay.is_none());
        } else {
            assert!(command.is_none());
            let Some(Overlay::CommandForm(form)) = &app.overlay else {
                panic!("editor lost");
            };
            assert_eq!(form.editing, Some(EditTarget::Preview));
            assert_eq!(form.edit_buffer, preview);
            assert!(form.error.is_some());
        }
    }
}

#[test]
fn form_field_cursor_scrolls_without_moving_hit_targets() {
    use super::app::{EditTarget, ModalClick};
    use ratatui::style::Modifier;
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path());
    app.open_command_form("info");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let targets = app.regions.modal_buttons.clone();
    for cursor in [160, 0, 80, 159] {
        if let Some(Overlay::CommandForm(form)) = &mut app.overlay {
            form.editing = Some(EditTarget::Field(0));
            form.edit_buffer = "界".repeat(160);
            form.edit_cursor = cursor;
        }
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let rect = app
            .regions
            .modal_buttons
            .iter()
            .find_map(|(rect, click)| matches!(click, ModalClick::FieldRow(0)).then_some(*rect))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert!(
            (rect.x..rect.x + rect.width).any(|x| {
                let cell = &buffer[(x, rect.y)];
                cell.modifier.contains(Modifier::REVERSED)
                    && cell.symbol() == if cursor == 160 { " " } else { "界" }
            }),
            "cursor {cursor} hidden"
        );
        assert_eq!(
            app.regions
                .modal_buttons
                .iter()
                .map(|(r, _)| *r)
                .collect::<Vec<_>>(),
            targets.iter().map(|(r, _)| *r).collect::<Vec<_>>()
        );
    }
}
