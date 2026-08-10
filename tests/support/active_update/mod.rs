//! Fixture registry + isolated runner for real-Nu active-plugin update acceptance.
//!
//! Builds a signed dual-version local registry from one real plugin artifact so
//! `numan update` can discover an upgrade without waiting on an official v2.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signer, SigningKey};
use numan_cli::core::integrity;
use numan_cli::core::nu_version::NuVersion;
use numan_cli::core::official_registry::{
    canonical_json_bytes, RegistrySignature, OFFICIAL_REGISTRY,
};
use numan_cli::core::package::{
    Artifact, Package, PackageType, RegistryIndex, ScopedId, TargetArtifact, VersionEntry,
};
use numan_cli::core::platform::Platform;
use numan_cli::core::trust::TrustStore;
use numan_cli::install::extract::ArchiveFormat;
use numan_cli::nu::paths::NuPaths;
use numan_cli::state::lockfile::Lockfile;
use rand_core::OsRng;
use serde_json::Value;
use uuid::Uuid;

use super::acceptance::model::{utc_unix_ms, ChildEnvironment, CommandOutcome, CommandSpec};
use super::acceptance::process::run_command;

pub const REGISTRY_NAME: &str = "fixture";
pub const DEFAULT_PACKAGE_ID: &str = "cptpiepmatz/nu_plugin_highlight";
pub const FROM_VERSION: &str = "1.0.0";
pub const TO_VERSION: &str = "2.0.0";
pub const ENV_FAIL_PLUGIN_RM: &str = "NUMAN_TEST_FAIL_PLUGIN_RM";
pub const ENV_FAIL_PLUGIN_ADD: &str = "NUMAN_TEST_FAIL_PLUGIN_ADD";
pub const ENV_REAL_NU: &str = "NUMAN_TEST_REAL_NU";
pub const ENV_MUTATION: &str = "NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION";

pub struct ActiveUpdateConfig {
    pub output_base: PathBuf,
    pub package_id: String,
    pub artifact_cache: PathBuf,
}

impl ActiveUpdateConfig {
    pub fn from_env() -> Result<Self> {
        let output_base = std::env::var_os("NUMAN_ACCEPTANCE_OUTPUT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/acceptance/active-plugin-update-real-nu"));
        let package_id = std::env::var("NUMAN_ACCEPTANCE_PACKAGE")
            .unwrap_or_else(|_| DEFAULT_PACKAGE_ID.to_string());
        let artifact_cache = std::env::var_os("NUMAN_ACCEPTANCE_ARTIFACT_CACHE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/acceptance/artifact-cache"));
        anyhow::ensure!(
            package_id.split('/').count() == 2,
            "NUMAN_ACCEPTANCE_PACKAGE must be owner/name"
        );
        Ok(Self {
            output_base,
            package_id,
            artifact_cache,
        })
    }
}

pub struct ActiveUpdateRun {
    #[allow(dead_code)]
    pub config: ActiveUpdateConfig,
    pub numan_binary: PathBuf,
    #[allow(dead_code)]
    pub run_id: String,
    #[allow(dead_code)]
    pub run_dir: PathBuf,
    pub root: PathBuf,
    #[allow(dead_code)]
    pub home: PathBuf,
    pub evidence: PathBuf,
    pub fixture_dir: PathBuf,
    pub environment: ChildEnvironment,
    #[allow(dead_code)]
    pub platform: Platform,
    pub public_key_b64: String,
    pub package_id: String,
    #[allow(dead_code)]
    pub executable_path: String,
    #[allow(dead_code)]
    real_nu: PathBuf,
    #[allow(dead_code)]
    shim_dir: PathBuf,
}

impl ActiveUpdateRun {
    pub fn bootstrap(config: ActiveUpdateConfig, numan_binary: PathBuf) -> Result<Self> {
        require_nu_0_113()?;

        let real_nu = which_nu().context("failed to locate real `nu` on PATH")?;
        let platform = Platform::detect();
        let artifact =
            fetch_official_artifact(&config.package_id, &platform, &config.artifact_cache)?;

        let now = utc_unix_ms();
        let uuid = Uuid::new_v4().simple().to_string();
        let run_id = format!("{now}-{}", &uuid[..8]);
        let base = absolute_path(&config.output_base)?;
        let run_dir = base.join(&run_id);
        let root = run_dir.join("root");
        let home = run_dir.join("home");
        let evidence = run_dir.join("evidence");
        let fixture_dir = run_dir.join("fixture-registry");
        let shim_dir = home.join("shim");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&evidence)?;
        std::fs::create_dir_all(&shim_dir)?;

        let mut environment = ChildEnvironment::isolated(&home)?;
        // Prefer shim so init caches shim identity; shim forwards to real Nu.
        prepend_path(&mut environment, &shim_dir)?;
        environment.variables.insert(
            ENV_REAL_NU.to_string(),
            real_nu.to_string_lossy().into_owned(),
        );
        write_nu_shim(&shim_dir, &real_nu)?;

        let signing_key = SigningKey::generate(&mut OsRng);
        let public_key_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signing_key.verifying_key().to_bytes(),
        );

        let archive_dest = fixture_dir.join(format!("plugin{}", artifact.archive_suffix));
        std::fs::create_dir_all(&fixture_dir)?;
        std::fs::copy(&artifact.archive_path, &archive_dest).with_context(|| {
            format!(
                "failed to copy artifact {} → {}",
                artifact.archive_path.display(),
                archive_dest.display()
            )
        })?;
        let archive_url = absolute_path(&archive_dest)?.to_string_lossy().into_owned();
        let archive_sha = artifact.sha256.clone();

        write_dual_version_registry(
            &fixture_dir,
            &config.package_id,
            &platform.triple,
            &archive_url,
            &archive_sha,
            &artifact.executable_path,
            &signing_key,
        )?;

        let package_id = config.package_id.clone();
        let executable_path = artifact.executable_path;
        Ok(Self {
            config,
            numan_binary,
            run_id,
            run_dir,
            root,
            home,
            evidence,
            fixture_dir,
            environment,
            platform,
            public_key_b64,
            package_id,
            executable_path,
            real_nu,
            shim_dir,
        })
    }

    pub fn set_env(&mut self, key: &str, value: &str) {
        self.environment
            .variables
            .insert(key.to_string(), value.to_string());
    }

    pub fn remove_env(&mut self, key: &str) {
        self.environment.variables.remove(key);
        // Also drop case-insensitive matches on Windows-style maps.
        let doomed: Vec<String> = self
            .environment
            .variables
            .keys()
            .filter(|k| k.eq_ignore_ascii_case(key))
            .cloned()
            .collect();
        for k in doomed {
            self.environment.variables.remove(&k);
        }
    }

    pub fn run_numan(&self, args: &[&str], timeout: Duration) -> Result<CommandOutcome> {
        let mut arguments = vec![
            "--root".to_string(),
            self.root.to_string_lossy().into_owned(),
        ];
        arguments.extend(args.iter().map(|s| (*s).to_string()));
        let spec = CommandSpec::new(
            args.first().copied().unwrap_or("numan"),
            self.numan_binary.clone(),
            arguments,
            timeout,
        );
        let outcome = run_command(&spec, &self.environment)?;
        let step = args.join(" ");
        write_step_evidence(&self.evidence, &step, &outcome)?;
        Ok(outcome)
    }

    pub fn require_ok(&self, args: &[&str], timeout: Duration) -> Result<CommandOutcome> {
        let outcome = self.run_numan(args, timeout)?;
        if outcome.timed_out {
            bail!("timed out: numan {}", args.join(" "));
        }
        if outcome.exit_code != Some(0) {
            bail!(
                "numan {} failed (exit {:?})\nstdout:\n{}\nstderr:\n{}",
                args.join(" "),
                outcome.exit_code,
                String::from_utf8_lossy(&outcome.stdout),
                String::from_utf8_lossy(&outcome.stderr)
            );
        }
        Ok(outcome)
    }

    /// init → plant fixture registry → install @1.0.0 → activate.
    pub fn prepare_active_v1(&mut self) -> Result<()> {
        if self.root.exists() {
            bail!(
                "acceptance root existed before first Numan invocation: {}",
                self.root.display()
            );
        }
        self.require_ok(&["init"], Duration::from_secs(120))?;
        self.plant_fixture_registry()?;
        let install_spec = format!("{}@{FROM_VERSION}", self.package_id);
        self.require_ok(&["install", &install_spec], Duration::from_secs(300))?;
        self.require_ok(&["activate", &self.package_id], Duration::from_secs(120))?;
        let lockfile = Lockfile::load(&self.root)?;
        let entry = lockfile
            .packages
            .get(&self.package_id)
            .with_context(|| format!("missing lockfile entry for {}", self.package_id))?;
        anyhow::ensure!(
            entry.version == FROM_VERSION,
            "expected installed version {FROM_VERSION}, got {}",
            entry.version
        );
        anyhow::ensure!(
            entry.activation.is_some(),
            "expected plugin activation after activate"
        );
        Ok(())
    }

    pub fn plant_fixture_registry(&self) -> Result<()> {
        let mut trust = TrustStore::load(&self.root)?;
        trust.add_key(REGISTRY_NAME, &self.public_key_b64)?;
        trust.save(&self.root)?;

        let config_path = self.root.join("config.toml");
        let config = format!(
            r#"[general]
default_registry = "{REGISTRY_NAME}"

[registries.{REGISTRY_NAME}]
url = "file://{}/index.json"
enabled = true
"#,
            absolute_path(&self.fixture_dir)?
                .to_string_lossy()
                .replace('\\', "/")
        );
        std::fs::write(&config_path, config)?;

        let dest = self.root.join("registry").join(REGISTRY_NAME);
        std::fs::create_dir_all(&dest)?;
        for name in ["index.json", "index.json.sig"] {
            std::fs::copy(self.fixture_dir.join(name), dest.join(name))
                .with_context(|| format!("failed to plant {name}"))?;
        }
        Ok(())
    }

    pub fn lockfile(&self) -> Result<Lockfile> {
        Lockfile::load(&self.root)
    }

    #[allow(dead_code)]
    pub fn nu_paths(&self) -> Result<NuPaths> {
        NuPaths::load(&self.root)
    }

    pub fn mutate_nu_hash(&self, new_hash: &str) -> Result<()> {
        let mut paths = NuPaths::load(&self.root)?;
        paths.nu_executable_hash = new_hash.to_string();
        paths.save(&self.root)?;
        Ok(())
    }

    pub fn delete_nu_paths(&self) -> Result<()> {
        let path = self.root.join("nu_state/paths.json");
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }
}

struct OfficialArtifact {
    archive_path: PathBuf,
    archive_suffix: &'static str,
    sha256: String,
    executable_path: String,
}

fn artifact_suffix(url: &str) -> Result<&'static str> {
    ArchiveFormat::matched_suffix(url)
        .with_context(|| format!("unsupported archive format in artifact URL: {url}"))
}

fn require_nu_0_113() -> Result<NuVersion> {
    let output = Command::new("nu")
        .arg("--version")
        .output()
        .context("failed to spawn `nu --version` (is Nushell on PATH?)")?;
    if !output.status.success() {
        bail!("`nu --version` exited non-zero");
    }
    let detected = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let version = NuVersion::parse(&detected)
        .with_context(|| format!("failed to parse Nu version '{detected}'"))?;
    if version.major != 0 || version.minor != 113 {
        bail!(
            "active-plugin update real-Nu matrix requires Nu 0.113.x, found {}",
            version.version
        );
    }
    Ok(version)
}

fn which_nu() -> Result<PathBuf> {
    let output = Command::new("nu")
        .arg("--version")
        .output()
        .context("nu not found on PATH")?;
    if !output.status.success() {
        bail!("nu --version failed");
    }
    // Resolve via PATH lookup.
    #[cfg(windows)]
    {
        let output = Command::new("where.exe")
            .arg("nu")
            .output()
            .context("where.exe nu failed")?;
        let text = String::from_utf8_lossy(&output.stdout);
        let first = text
            .lines()
            .next()
            .context("where.exe nu returned no paths")?
            .trim();
        Ok(PathBuf::from(first))
    }
    #[cfg(not(windows))]
    {
        let output = Command::new("sh")
            .args(["-c", "command -v nu"])
            .output()
            .context("command -v nu failed")?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        anyhow::ensure!(!text.is_empty(), "command -v nu returned empty");
        Ok(PathBuf::from(text))
    }
}

fn fetch_official_artifact(
    package_id: &str,
    platform: &Platform,
    cache_dir: &Path,
) -> Result<OfficialArtifact> {
    std::fs::create_dir_all(cache_dir)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let index_url = OFFICIAL_REGISTRY.production_url;
    let index_text = client
        .get(index_url)
        .send()
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("failed to fetch official index from {index_url}"))?
        .text()?;
    let index: RegistryIndex =
        serde_json::from_str(&index_text).context("failed to parse official registry index")?;
    let package = index
        .packages
        .into_iter()
        .find(|p| p.id.to_string() == package_id)
        .with_context(|| format!("package '{package_id}' not found in official registry"))?;
    anyhow::ensure!(
        package.package_type == PackageType::Plugin,
        "acceptance subject must be a plugin, got {}",
        package.package_type
    );
    let version = package
        .versions
        .first()
        .context("official package has no versions")?;
    let target = version
        .artifact
        .targets
        .get(&platform.triple)
        .with_context(|| {
            format!(
                "package '{package_id}' has no artifact for host triple {}",
                platform.triple
            )
        })?;
    let archive_suffix = artifact_suffix(&target.url)?;

    let cache_name = format!(
        "{}-{}-{}{}",
        package_id.replace('/', "_"),
        version.version,
        platform.triple,
        archive_suffix
    );
    let archive_path = cache_dir.join(&cache_name);
    if !archive_path.exists() {
        let bytes = client
            .get(&target.url)
            .send()
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("failed to download {}", target.url))?
            .bytes()
            .context("failed to read artifact bytes")?;
        let tmp = archive_path.with_extension("part");
        std::fs::write(&tmp, &bytes)?;
        let sha = integrity::compute_sha256(&bytes);
        anyhow::ensure!(
            sha == target.sha256,
            "artifact sha256 mismatch for {}: expected {}, got {}",
            target.url,
            target.sha256,
            sha
        );
        std::fs::rename(&tmp, &archive_path)?;
    }
    let bytes = std::fs::read(&archive_path)?;
    let sha = integrity::compute_sha256(&bytes);
    anyhow::ensure!(
        sha == target.sha256,
        "cached artifact sha256 mismatch: expected {}, got {}",
        target.sha256,
        sha
    );
    Ok(OfficialArtifact {
        archive_path,
        archive_suffix,
        sha256: sha,
        executable_path: target.executable_path.clone(),
    })
}

fn write_dual_version_registry(
    fixture_dir: &Path,
    package_id: &str,
    triple: &str,
    archive_url: &str,
    archive_sha: &str,
    executable_path: &str,
    signing_key: &SigningKey,
) -> Result<()> {
    let id = ScopedId::parse(package_id)?;
    let mk_version = |major, minor, patch| {
        let mut targets = HashMap::new();
        targets.insert(
            triple.to_string(),
            TargetArtifact {
                url: archive_url.to_string(),
                sha256: archive_sha.to_string(),
                executable_path: executable_path.to_string(),
            },
        );
        VersionEntry {
            version: semver::Version::new(major, minor, patch),
            nu_version: ">=0.113.0 <0.114.0".to_string(),
            verified_with: vec!["0.113.1".to_string()],
            artifact: Artifact {
                kind: "binary".to_string(),
                url: None,
                sha256: None,
                targets,
                archive_root: None,
                include: None,
                entry: None,
            },
            source: None,
            dependencies: BTreeMap::new(),
            activation: None,
            evidence_tier: None,
            deferral_reason: None,
        }
    };

    let package = Package {
        id,
        description: "Fixture dual-version plugin for active-update real-Nu matrix".to_string(),
        repo: "https://github.com/tonythethompson/numan".to_string(),
        package_type: PackageType::Plugin,
        tags: vec!["fixture".to_string(), "acceptance".to_string()],
        versions: vec![mk_version(1, 0, 0), mk_version(2, 0, 0)],
    };

    let index = RegistryIndex {
        schema_version: 1,
        updated_at: "2026-07-22T00:00:00Z".to_string(),
        registry_revision: Some("fixture-active-update-1".to_string()),
        trust: None,
        packages: vec![package],
    };

    let content = serde_json::to_string_pretty(&index)?;
    let value: Value = serde_json::from_str(&content)?;
    let canonical = canonical_json_bytes(&value)?;
    let signature = signing_key.sign(&canonical);
    let sig_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        signature.to_bytes(),
    );
    // Custom registries key trust by registry name (key_id == registry name).
    let envelope = RegistrySignature::new(REGISTRY_NAME, &sig_b64);

    std::fs::write(fixture_dir.join("index.json"), &content)?;
    std::fs::write(
        fixture_dir.join("index.json.sig"),
        serde_json::to_string_pretty(&envelope)?,
    )?;
    Ok(())
}

fn write_nu_shim(shim_dir: &Path, real_nu: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let _ = real_nu;
        // Windows CreateProcess resolves `nu` to `nu.exe` before `nu.cmd`, so the
        // acceptance suite needs a native PE forwarder. Building it requires `rustc`
        // on PATH (normal for `cargo test`; fail loudly if a stripped environment
        // omits the compiler).
        let rustc_probe = Command::new("rustc").arg("--version").output().context(
            "Windows active-update acceptance requires `rustc` on PATH to build \
                 the native Nu forwarder (nu.exe); install the Rust toolchain or ensure \
                 rustup's bin directory is on PATH",
        )?;
        anyhow::ensure!(
            rustc_probe.status.success(),
            "Windows active-update acceptance requires a working `rustc` to build the \
             native Nu forwarder (nu.exe); `rustc --version` failed:\n{}",
            String::from_utf8_lossy(&rustc_probe.stderr)
        );

        let source = shim_dir.join("nu-forwarder.rs");
        let executable = shim_dir.join("nu.exe");
        std::fs::write(&source, include_str!("../../fixtures/nu_forwarder.rs"))?;
        let output = Command::new("rustc")
            .args(["--crate-name", "nu_forwarder"])
            .args(["--edition", "2021"])
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .context("failed to invoke `rustc` while compiling the native Windows Nu forwarder")?;
        let _ = std::fs::remove_file(&source);
        anyhow::ensure!(
            output.status.success(),
            "failed to compile native Windows Nu forwarder with rustc:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let real = real_nu.to_string_lossy();
        let script = shim_dir.join("nu");
        let body = format!(
            r#"#!/bin/sh
set -eu
REAL_NU="${{NUMAN_TEST_REAL_NU:-{real}}}"
ARGS="$*"
case "$ARGS" in
  *plugin\ rm*)
    if [ "${{NUMAN_TEST_FAIL_PLUGIN_RM:-}}" = "1" ]; then
      echo "NUMAN_TEST_FAIL_PLUGIN_RM: refusing plugin rm" >&2
      exit 1
    fi
    ;;
  *plugin\ add*)
    if [ "${{NUMAN_TEST_FAIL_PLUGIN_ADD:-}}" = "1" ]; then
      echo "NUMAN_TEST_FAIL_PLUGIN_ADD: refusing plugin add" >&2
      exit 1
    fi
    ;;
esac
exec "$REAL_NU" "$@"
"#
        );
        std::fs::write(&script, body)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms)?;
        }
        Ok(())
    }
}

fn prepend_path(environment: &mut ChildEnvironment, first: &Path) -> Result<()> {
    let first = absolute_path(first)?;
    let key = environment
        .variables
        .keys()
        .find(|k| k.eq_ignore_ascii_case("PATH"))
        .cloned()
        .unwrap_or_else(|| "PATH".to_string());
    let current = environment.variables.get(&key).cloned().unwrap_or_default();
    let mut entries = vec![first];
    entries.extend(std::env::split_paths(&current));
    let joined = std::env::join_paths(entries).context("failed to join PATH")?;
    environment
        .variables
        .insert(key, joined.to_string_lossy().into_owned());
    Ok(())
}

fn write_step_evidence(evidence: &Path, step: &str, outcome: &CommandOutcome) -> Result<()> {
    let safe: String = step
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let dir = evidence.join(format!("{:03}_{safe}", utc_unix_ms() % 1000));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("stdout.txt"), &outcome.stdout)?;
    std::fs::write(dir.join("stderr.txt"), &outcome.stderr)?;
    let mut meta = std::fs::File::create(dir.join("meta.txt"))?;
    writeln!(meta, "step={step}")?;
    writeln!(meta, "exit={:?}", outcome.exit_code)?;
    writeln!(meta, "timed_out={}", outcome.timed_out)?;
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir()?;
    Ok(cwd.join(path))
}

pub fn resolve_numan_binary() -> Result<PathBuf> {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if cfg!(debug_assertions) {
        path.push("target/debug/numan");
    } else {
        path.push("target/release/numan");
    }
    #[cfg(windows)]
    {
        path.set_extension("exe");
    }
    if path.exists() {
        return Ok(path);
    }
    // Fallback: cargo test may place the bin next to the test exe.
    let exe = std::env::current_exe()?;
    let candidate = exe
        .parent()
        .unwrap()
        .join("numan")
        .with_extension(if cfg!(windows) { "exe" } else { "" });
    if candidate.exists() {
        return Ok(candidate);
    }
    bail!(
        "numan binary not found at {} (run `cargo build` first)",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::{artifact_suffix, write_nu_shim};

    #[test]
    fn artifact_suffix_preserves_supported_release_formats() {
        assert_eq!(
            artifact_suffix("https://example.invalid/PLUGIN.ZIP").unwrap(),
            ".zip"
        );
        assert_eq!(
            artifact_suffix("https://example.invalid/plugin.tar.gz").unwrap(),
            ".tar.gz"
        );
        assert_eq!(
            artifact_suffix("https://example.invalid/plugin.tar.xz?download=1").unwrap(),
            ".tar.xz"
        );
        assert_eq!(
            artifact_suffix("https://example.invalid/plugin.tgz").unwrap(),
            ".tgz"
        );
        assert_eq!(
            artifact_suffix("https://example.invalid/plugin.txz#release-asset").unwrap(),
            ".txz"
        );
        assert_eq!(
            artifact_suffix("https://example.invalid/plugin.tar").unwrap(),
            ".tar"
        );
    }

    #[test]
    fn artifact_suffix_rejects_unknown_formats() {
        let error = artifact_suffix("https://example.invalid/plugin.bin").unwrap_err();
        assert!(error.to_string().contains("unsupported archive format"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_nu_shim_is_a_native_executable() {
        if std::process::Command::new("rustc")
            .arg("--version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!(
                "skipping windows_nu_shim_is_a_native_executable: `rustc` is not available on PATH"
            );
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        write_nu_shim(temp.path(), std::path::Path::new("ignored.exe")).unwrap();
        let bytes = std::fs::read(temp.path().join("nu.exe")).unwrap();
        assert_eq!(&bytes[..2], b"MZ");
        assert!(!temp.path().join("nu.cmd").exists());
    }
}
