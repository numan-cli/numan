//! Interactive acceptance: run explicitly with `cargo test --test
//! overlay_session_test -- --ignored --nocapture` and Nu on PATH (or NUMAN_TEST_NU).
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

struct Repl {
    _master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    output: Receiver<Vec<u8>>,
    root: PathBuf,
}

impl Repl {
    fn start(root: &Path, home: &Path) -> Self {
        std::fs::create_dir_all(home).unwrap();
        let config = home.join("config.nu");
        let paths = numan_cli::nu::paths::NuPaths {
            nu_executable: "fixture-nu".into(),
            nu_version: "0.116.1".into(),
            plugin_registry_path: root.join("plugins.msgpackz").to_string_lossy().into_owned(),
            nu_executable_hash: "fixture".into(),
            platform: "test".into(),
            data_dir: None,
            vendor_autoload_dirs: vec![],
            vendor_autoload_dir: None,
        };
        numan_cli::nu::init_file::regenerate_init_file(root, &paths).unwrap();
        let source = format!("source {}", nu_path(&root.join("nu_state/numan_init.nu")));
        std::fs::write(&config, format!(
            "$env.config.show_banner = false\n$env.config.shell_integration.osc133 = true\n$env.config.hooks.pre_prompt = []\n{source}\n"
        )).unwrap();
        let env_config = home.join("env.nu");
        std::fs::write(&env_config, "").unwrap();
        let mut command =
            CommandBuilder::new(std::env::var_os("NUMAN_TEST_NU").unwrap_or_else(|| "nu".into()));
        command.args(["--no-history", "--config"]);
        command.arg(config);
        command.arg("--env-config");
        command.arg(env_config);
        command.cwd(root);
        for key in [
            "HOME",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "NU_VENDOR_AUTOLOAD_DIR",
        ] {
            command.env(key, home);
        }
        command.env("NUMAN_ROOT", root);
        let binary = Path::new(env!("CARGO_BIN_EXE_numan"));
        let paths = std::iter::once(binary.parent().unwrap().to_path_buf())
            .chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ))
            .collect::<Vec<_>>();
        command.env("PATH", std::env::join_paths(paths).unwrap());
        command.env("TERM", "xterm-256color");
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 32,
                cols: 160,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let child = pair
            .slave
            .spawn_command(command)
            .expect("Nu must be installed; set NUMAN_TEST_NU to its executable");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();
        let (send, output) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buffer = [0; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if send.send(buffer[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let mut repl = Self {
            _master: pair.master,
            child,
            writer,
            output,
            root: root.to_path_buf(),
        };
        repl.until_prompt(false);
        repl
    }

    fn until_prompt(&mut self, require_finished: bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(40);
        let mut bytes = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let chunk = self.output.recv_timeout(remaining).unwrap_or_else(|error| {
                panic!(
                    "Timed out waiting for Nu prompt ({error}): {}",
                    String::from_utf8_lossy(&bytes)
                )
            });
            bytes.extend(chunk);
            // Reedline asks the terminal for its cursor position on Windows.
            // A PTY supplies transport only, so the harness must answer DSR.
            if bytes.ends_with(b"\x1b[6n") {
                self.writer.write_all(b"\x1b[1;1R").unwrap();
                self.writer.flush().unwrap();
            }
            let text = String::from_utf8_lossy(&bytes);
            let search = if require_finished {
                text.find("\x1b]133;D").map(|pos| &text[pos..])
            } else {
                Some(text.as_ref())
            };
            if search.is_some_and(|tail| tail.contains("\x1b]133;A")) {
                return text.into_owned();
            }
        }
    }

    fn run(&mut self, command: &str) -> String {
        self.writer.write_all(command.as_bytes()).unwrap();
        self.writer.write_all(b"\r").unwrap();
        self.writer.flush().unwrap();
        self.until_prompt(true)
    }

    fn observe(&mut self, expression: &str) -> Value {
        let path = self
            .root
            .join(format!("observation-{}.json", uuid::Uuid::new_v4()));
        self.run(&format!(
            "({expression}) | to json --raw | save --force {}",
            nu_path(&path)
        ));
        let text =
            std::fs::read_to_string(&path).expect("Nu expression must write its observation");
        serde_json::from_str(&text).unwrap()
    }
}

impl Drop for Repl {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn nu_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    for count in 1.. {
        let hashes = "#".repeat(count);
        if !path.contains(&format!("'{hashes}")) {
            return format!("r{hashes}'{path}'{hashes}");
        }
    }
    unreachable!()
}

fn fixture(root: &Path) {
    let mut packages = serde_json::Map::new();
    for (package, name, source) in [
        ("tool", "tooling", "export def hello [] { 'hello' }; export-env { $env.NUMAN_OVERLAY_TEST = 'set' }"),
        ("broken", "broken", "export def broken ["),
        ("envfail", "envfail", "export def partial [] { 'partial' }; export-env { $env.NUMAN_PARTIAL_TEST = 'partial'; error make {msg: 'intentional export-env failure'} }"),
    ] {
        let payload = format!("packages/modules/owner/{package}/1");
        std::fs::create_dir_all(root.join(&payload)).unwrap();
        std::fs::write(root.join(&payload).join("overlay.nu"), source).unwrap();
        packages.insert(format!("owner/{package}"), json!({
            "version":"1.0.0", "type":"module", "source":"archive",
            "installed_at":"2026-10-22T00:00:00Z", "payload_path":payload,
            "entry":"overlay.nu", "overlay":{"name":name,"exports": if package == "tool" {vec!["hello"]} else {vec![]}}
        }));
    }
    numan_cli::util::atomic::write_json_atomic(
        &root.join("lockfile"),
        &json!({
            "version":2, "generated_at":"2026-10-22T00:00:00Z", "nu_version":"0.116.1",
            "platform":"test", "packages":packages
        }),
    )
    .unwrap();
}

fn records(root: &Path) -> Vec<numan_cli::state::overlay_state::OverlayRecord> {
    numan_cli::state::overlay_state::OverlayState::load(root)
        .unwrap()
        .overlays
}

#[test]
#[ignore = "requires real Nu and an interactive PTY"]
fn real_nu_overlay_session_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fixture(&root);
    let mut repl = Repl::start(&root, &root.join("home"));

    let lock = numan_cli::util::fs_safety::acquire_mutation_lock(&root).unwrap();
    repl.run("numan overlay use owner/tool");
    drop(lock);
    assert!(records(&root).is_empty());
    assert_eq!(
        repl.observe("$env.__NUMAN_OVERLAY_PENDING? | default {} | is-empty"),
        true
    );

    repl.run("numan overlay use owner/broken");
    assert!(
        records(&root).is_empty(),
        "nu-check failure must abort its reservation"
    );
    repl.run("module foreign { export def foreign-command [] { 'foreign' } }; overlay use foreign as tooling");
    repl.run("numan overlay use owner/tool");
    assert!(records(&root).is_empty());
    assert_eq!(repl.observe("foreign-command"), "foreign");
    repl.run("overlay hide tooling");

    repl.run("numan overlay use owner/tool");
    assert_eq!(repl.observe("{command: (hello), env: $env.NUMAN_OVERLAY_TEST, names: (overlay list | where active | get name)}")["command"], "hello");
    assert_eq!(repl.observe("$env.NUMAN_OVERLAY_TEST"), "set");
    assert_eq!(
        repl.observe("overlay list | where active | get name | any {|name| $name == 'tooling'}"),
        true
    );
    assert_eq!(records(&root).len(), 1);
    assert_eq!(
        records(&root)[0].status,
        numan_cli::state::overlay_state::OverlayStatus::Active
    );
    repl.run("numan overlay use owner/tool");
    assert_eq!(records(&root).len(), 1);
    repl.run("cd home");
    let cwd = repl.observe("$env.PWD");
    repl.run("numan overlay hide owner/tool");
    assert!(records(&root).is_empty());
    assert_eq!(repl.observe("$env.NUMAN_OVERLAY_TEST?"), Value::Null);
    assert_eq!(repl.observe("$env.PWD"), cwd);
    repl.run("numan overlay hide owner/tool");
    repl.run("print ''");
    repl.run("print ''");
    assert!(
        records(&root).is_empty(),
        "cleanup hooks must never replay activation"
    );
    assert_eq!(
        repl.observe("overlay list | where active | get name | any {|name| $name == 'tooling'}"),
        false
    );

    repl.run("numan overlay use owner/tool");
    let lock = numan_cli::util::fs_safety::acquire_mutation_lock(&root).unwrap();
    repl.run("overlay hide tooling --keep-env [PWD]");
    assert_eq!(
        records(&root).len(),
        1,
        "failed reconciliation must keep the reservation"
    );
    drop(lock);
    repl.run("print ''");
    assert!(
        records(&root).is_empty(),
        "native hide must reconcile at the next prompt"
    );
    repl.run("numan overlay use owner/envfail");
    let state = records(&root);
    assert_eq!(state.len(), 1);
    assert_eq!(
        state[0].status,
        numan_cli::state::overlay_state::OverlayStatus::Degraded
    );
    assert!(
        numan_cli::state::overlay_state::OverlayState::ensure_package_mutable(
            &root,
            "owner/envfail"
        )
        .is_err()
    );
}

#[test]
#[ignore = "requires real Nu and an interactive PTY"]
fn real_nu_native_hide_preserving_marker_remains_degraded() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fixture(&root);
    let mut repl = Repl::start(&root, &root.join("home"));
    repl.run("numan overlay use owner/tool");
    let output = repl.run("let marker = ($env | columns | where {|key| $key | str starts-with '__NUMAN_OVERLAY__'} | first); overlay hide tooling --keep-env [PWD $marker]");
    assert_eq!(records(&root).len(), 1, "{output}");
    assert_eq!(
        records(&root)[0].status,
        numan_cli::state::overlay_state::OverlayStatus::Degraded,
        "{output}"
    );
    assert_eq!(
        repl.observe("overlay list | where active | get name | any {|name| $name == 'tooling'}"),
        false
    );
    assert!(
        numan_cli::state::overlay_state::OverlayState::ensure_package_mutable(&root, "owner/tool")
            .is_err()
    );
    repl.run("print ''");
    assert_eq!(
        records(&root)[0].status,
        numan_cli::state::overlay_state::OverlayStatus::Degraded
    );
}

#[test]
#[ignore = "requires real Nu and an interactive PTY"]
fn real_nu_overlay_parse_race_does_not_replay() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fixture(&root);
    let mut repl = Repl::start(&root, &root.join("home"));
    let entry = root.join("packages/modules/owner/tool/1/overlay.nu");
    repl.run(&format!(
        "numan overlay use owner/tool; 'export def broken [' | save --force {}",
        nu_path(&entry)
    ));
    // The statement passed nu-check before the payload changed. Its later
    // string-hook parse fails, so cleanup must already have removed that hook.
    repl.run("print ''");
    assert_eq!(records(&root).len(), 1);
    assert_eq!(
        records(&root)[0].status,
        numan_cli::state::overlay_state::OverlayStatus::Degraded
    );
    std::fs::write(&entry, "export def hello [] { 'unexpected replay' }").unwrap();
    repl.run("print ''");
    assert_eq!(
        repl.observe("overlay list | where active | get name | any {|name| $name == 'tooling'}"),
        false
    );
    assert_eq!(
        repl.observe("$env.__NUMAN_OVERLAY_PENDING? | default {} | is-empty"),
        true
    );
    assert!(
        numan_cli::state::overlay_state::OverlayState::ensure_package_mutable(&root, "owner/tool")
            .is_err()
    );
}

#[test]
#[ignore = "requires real Nu and two interactive PTYs"]
fn real_nu_overlay_sessions_are_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fixture(&root);
    let mut first = Repl::start(&root, &root.join("home-first"));
    first.run("numan overlay use owner/tool");
    let mut second = Repl::start(&root, &root.join("home-second"));
    assert_eq!(second.observe("numan overlay list | length"), 0);
    assert_eq!(
        second.observe("overlay list | where active | get name | any {|name| $name == 'tooling'}"),
        false
    );
    second.run("numan overlay use owner/tool");
    let state = records(&root);
    assert_eq!(state.len(), 2);
    assert_ne!(state[0].session_id, state[1].session_id);
    second.run("numan overlay hide owner/tool");
    assert_eq!(records(&root).len(), 1);
    assert_eq!(first.observe("hello"), "hello");
    drop(first);
    second.run("print ''");
    second.run("numan overlay list");
    assert!(
        records(&root).is_empty(),
        "dead shell must not pin payloads forever"
    );
}
