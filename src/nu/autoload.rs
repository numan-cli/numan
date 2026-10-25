//! Candidate generation and Nu validation for the Numan-managed autoload file.
//!
//! ## Overview
//!
//! Numan owns exactly one external file:
//!
//! ```text
//! <vendor-autoload-dir>/numan.nu
//! ```
//!
//! This module is responsible for:
//!
//! 1. **Entry resolution** — validating that a module entry path is safe
//!    (containment, regular file, `.nu` extension).
//! 2. **Statement rendering** — producing `use "<path>"` or `use "<path>" *`
//!    with correct Nu path-literal escaping for all platforms.
//! 3. **Deterministic file generation** — sorted by scoped package ID with
//!    the ownership header prepended.
//! 4. **Candidate placement** — writing a `.<uuid>.candidate.tmp` file in the
//!    same directory as the live managed file so replacement stays on-filesystem.
//! 5. **Candidate validation** — executing `<nu> -n <candidate>` through an
//!    injectable [`CandidateRunner`] trait so tests can inject a fake runner.
//! 6. **File replacement and deletion** — atomically moving the validated
//!    candidate over the live file, or deleting the managed file when all
//!    modules are deactivated.

use crate::util::output::eprintln;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::core::package::ModuleImportMode;
use crate::util::atomic::write_bytes_atomic;
use crate::util::fs_safety::{assert_managed_file_owned, assert_not_symlink, OWNERSHIP_MARKER};

// ── Entry resolution ──────────────────────────────────────────────────────────

/// A fully-validated module entry descriptor.
///
/// Constructed by [`resolve_entry`]. Callers may pass this directly to
/// [`render_use_statement`].
#[derive(Debug, Clone)]
pub struct ResolvedEntry {
    /// Absolute canonical path to the `.nu` entry file.
    pub absolute_path: PathBuf,
    /// Import mode for this module.
    pub import_mode: ModuleImportMode,
    /// Canonical scoped package ID (e.g. `"owner/name"`), used for ordering.
    pub scoped_id: String,
}

/// Validate and resolve a module entry path.
///
/// # Arguments
///
/// * `numan_root` — The absolute Numan root directory. Used as the containment
///   boundary for the payload.
/// * `payload_path` — Relative path from `numan_root` to the installed payload
///   directory (e.g. `"packages/modules/owner/foo/1.0.0-abc12345"`). Must be a
///   safe relative path with no `..`.
/// * `entry_relative` — Entry path relative to the payload directory
///   (e.g. `"mod.nu"`). Must be safe relative, must end in `.nu`, and must
///   resolve to a regular file.
/// * `import_mode` — How the module's symbols are brought into scope.
/// * `scoped_id` — The package scoped ID string (used only for deterministic
///   ordering in the generated file).
///
/// # Errors
///
/// Returns an error when:
/// - `payload_path` or `entry_relative` are not safe relative paths.
/// - The resolved entry path escapes the payload directory or the Numan root.
/// - The entry file is not a regular file (e.g. it is a symlink or directory).
/// - The entry path does not end in `.nu`.
pub fn resolve_entry(
    numan_root: &Path,
    payload_path: &str,
    entry_relative: &str,
    import_mode: ModuleImportMode,
    scoped_id: &str,
) -> Result<ResolvedEntry> {
    let payload_rel = Path::new(payload_path);
    let entry_rel = Path::new(entry_relative);

    // Validate payload_path is a safe relative path.
    if !crate::util::fs_safety::is_safe_relative_path(payload_rel) {
        bail!(
            "Module '{}': payload_path '{}' is not a safe relative path \
             (must be relative, no '..', no root or platform prefix).",
            scoped_id,
            payload_path
        );
    }

    // Validate entry_relative is a safe relative path.
    if !crate::util::fs_safety::is_safe_relative_path(entry_rel) {
        bail!(
            "Module '{}': entry path '{}' is not a safe relative path \
             (must be relative, no '..', no root or platform prefix).",
            scoped_id,
            entry_relative
        );
    }

    // Validate .nu extension.
    match entry_rel.extension() {
        Some(ext) if ext == "nu" => {}
        _ => {
            bail!(
                "Module '{}': entry path '{}' does not have a '.nu' extension.",
                scoped_id,
                entry_relative
            );
        }
    }

    // Resolve the payload directory within the Numan root.
    let payload_abs = numan_root.join(payload_rel);
    let payload_canonical = payload_abs.canonicalize().with_context(|| {
        format!(
            "Module '{}': failed to canonicalize payload directory '{}'",
            scoped_id,
            payload_abs.display()
        )
    })?;

    // Verify the payload directory itself stays within the Numan root.
    let root_canonical = numan_root.canonicalize().with_context(|| {
        format!(
            "Failed to canonicalize Numan root '{}'",
            numan_root.display()
        )
    })?;
    if !payload_canonical.starts_with(&root_canonical) {
        bail!(
            "Module '{}': payload directory '{}' resolves outside Numan root '{}'.",
            scoped_id,
            payload_canonical.display(),
            root_canonical.display()
        );
    }

    // Resolve the entry path within the payload directory.
    let entry_abs = payload_canonical.join(entry_rel);
    let entry_canonical = entry_abs.canonicalize().with_context(|| {
        format!(
            "Module '{}': failed to canonicalize entry path '{}'",
            scoped_id,
            entry_abs.display()
        )
    })?;

    // Containment: entry must remain under the payload directory.
    if !entry_canonical.starts_with(&payload_canonical) {
        bail!(
            "Module '{}': entry path '{}' resolves outside the payload directory '{}'.",
            scoped_id,
            entry_canonical.display(),
            payload_canonical.display()
        );
    }

    // Must be a regular file — not a symlink, directory, or other special file.
    let meta = std::fs::metadata(&entry_canonical).with_context(|| {
        format!(
            "Module '{}': failed to read metadata for entry '{}'",
            scoped_id,
            entry_canonical.display()
        )
    })?;
    if !meta.is_file() {
        bail!(
            "Module '{}': entry '{}' is not a regular file.",
            scoped_id,
            entry_canonical.display()
        );
    }

    // Symlinks must not have survived canonicalize to a non-regular file, but
    // double-check with symlink_metadata to catch edge cases (e.g. dangling
    // symlinks that point to .nu files on some platforms).
    let sym_meta = std::fs::symlink_metadata(&entry_canonical).with_context(|| {
        format!(
            "Module '{}': failed to read symlink metadata for entry '{}'",
            scoped_id,
            entry_canonical.display()
        )
    })?;
    if sym_meta.file_type().is_symlink() {
        bail!(
            "Module '{}': entry '{}' is a symlink. \
             Numan will not follow symlinks for module entries.",
            scoped_id,
            entry_canonical.display()
        );
    }

    Ok(ResolvedEntry {
        absolute_path: entry_canonical,
        import_mode,
        scoped_id: scoped_id.to_string(),
    })
}

// ── Statement rendering ───────────────────────────────────────────────────────

/// Render a Nu `use` statement for the given absolute module path.
///
/// # Nu path-literal escaping
///
/// Nu uses double-quoted string literals for paths containing special
/// characters. The only character that must be escaped within a double-quoted
/// Nu string is the double-quote character itself (escaped as `\"`). Nu also
/// requires backslashes in Windows paths to be doubled (`\\`).
///
/// This function produces:
///
/// - `Module` → `use "<escaped-path>"`
/// - `All`    → `use "<escaped-path>" *`
///
/// The output is suitable for direct inclusion in a `.nu` source file.
///
/// # Panics
///
/// Never panics. Returns `Err` only if the path is not valid UTF-8.
pub fn render_use_statement(path: &Path, mode: &ModuleImportMode) -> Result<String> {
    let path_str = path
        .to_str()
        .with_context(|| format!("Module entry path '{}' is not valid UTF-8", path.display()))?;

    // Escape for a Nu double-quoted string literal:
    //   1. backslashes → \\   (Windows paths; harmless on Unix)
    //   2. double quotes → \"
    let escaped = path_str.replace('\\', "\\\\").replace('"', "\\\"");

    let stmt = match mode {
        ModuleImportMode::Module => format!("use \"{escaped}\""),
        ModuleImportMode::All => format!("use \"{escaped}\" *"),
    };

    Ok(stmt)
}

// ── Deterministic file generation ─────────────────────────────────────────────

/// Generate the full content of `numan.nu` for the given set of resolved entries.
///
/// Entries are sorted by [`ResolvedEntry::scoped_id`] in ascending
/// lexicographic order, which gives a deterministic and human-readable output
/// regardless of the order entries were activated.
///
/// The file begins with the exact ownership header required by
/// [`OWNERSHIP_MARKER`] and is encoded as plain UTF-8 without BOM.
pub fn generate_autoload_content(entries: &[ResolvedEntry]) -> Result<String> {
    generate_managed_content(entries, &[])
}

/// A fully-validated script command ready for managed-file generation
/// (issue #14). Constructed by the activate lane from a package's declared
/// entry points after containment checks via [`resolve_entry`].
#[derive(Debug, Clone)]
pub struct ResolvedScriptCommand {
    /// Canonical scoped package ID (e.g. `"owner/name"`), used for ordering.
    pub scoped_id: String,
    /// Public command name declared by the package.
    pub name: String,
    /// Absolute canonical path to the `.nu` script target.
    pub script_path: PathBuf,
    /// Absolute path to the Nu executable that runs the script.
    pub nu_executable: String,
}

fn escape_nu_string_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Render the wrapper definition for one script command.
///
/// The wrapper is definition-only: sourcing the managed file never runs
/// package code. When a user invokes the command, the wrapper runs the
/// script as a bare external call with the activating Nu executable, so
/// rest arguments, pipeline input, stdout, stderr, and exit status all
/// forward exactly as if the script were an external program (Nu itself
/// propagates a non-zero external exit). Capturing the output instead
/// would break that: Nu aborts the calling script on a failed captured
/// external before user code can forward anything.
pub fn render_script_wrapper(command: &ResolvedScriptCommand) -> Result<String> {
    let script = command.script_path.to_str().with_context(|| {
        format!(
            "Script path '{}' is not valid UTF-8",
            command.script_path.display()
        )
    })?;
    let nu = escape_nu_string_literal(&command.nu_executable);
    let script = escape_nu_string_literal(script);
    Ok(format!(
        "# numan:script {scoped}\n\
         export def --wrapped {name} [...rest: string] {{\n\
         \x20   $in | ^\"{nu}\" \"{script}\" ...$rest\n\
         }}",
        scoped = command.scoped_id,
        name = command.name,
    ))
}

/// Generate the full content of `numan.nu` from module entries and script
/// commands. Modules render first (sorted by scoped ID), then script
/// wrappers (sorted by scoped ID, then command name), under the
/// ownership header.
pub fn generate_managed_content(
    entries: &[ResolvedEntry],
    script_commands: &[ResolvedScriptCommand],
) -> Result<String> {
    let mut sorted: Vec<&ResolvedEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| a.scoped_id.cmp(&b.scoped_id));

    let mut sorted_scripts: Vec<&ResolvedScriptCommand> = script_commands.iter().collect();
    sorted_scripts.sort_by(|a, b| a.scoped_id.cmp(&b.scoped_id).then(a.name.cmp(&b.name)));

    let mut out = String::new();
    out.push_str(OWNERSHIP_MARKER);

    for entry in &sorted {
        out.push('\n');
        let stmt = render_use_statement(&entry.absolute_path, &entry.import_mode)?;
        out.push_str(&stmt);
        out.push('\n');
    }

    for command in &sorted_scripts {
        out.push('\n');
        out.push_str(&render_script_wrapper(command)?);
        out.push('\n');
    }

    Ok(out)
}

// ── CandidateRunner trait ─────────────────────────────────────────────────────

/// An abstraction over executing `<nu> -n <candidate-path>` for validation.
///
/// The real implementation spawns an actual Nu process. Tests inject a
/// [`FakeCandidateRunner`] that either succeeds or fails on demand.
pub trait CandidateRunner {
    /// Execute the Nu binary in no-config mode (`-n`) against `candidate`.
    ///
    /// Returns `Ok(())` when the exit status is zero.
    /// Returns `Err` containing Nu's stderr output when the exit is nonzero.
    fn run(&self, candidate: &Path) -> Result<()>;
}

/// Real [`CandidateRunner`] that spawns the cached Nu executable.
///
/// Constructed from the absolute path to the Nu binary.
pub struct NuCandidateRunner {
    nu_executable: String,
}

impl NuCandidateRunner {
    /// Create a runner that invokes `nu_executable` for validation.
    pub fn new(nu_executable: &str) -> Self {
        Self {
            nu_executable: nu_executable.to_string(),
        }
    }
}

impl CandidateRunner for NuCandidateRunner {
    fn run(&self, candidate: &Path) -> Result<()> {
        let output = std::process::Command::new(&self.nu_executable)
            .arg("-n")
            .arg(candidate)
            .output()
            .with_context(|| {
                format!(
                    "Failed to spawn Nu binary '{}' for candidate validation",
                    self.nu_executable
                )
            })?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "Nu candidate validation failed (exit {:?}).\nstderr: {}\nstdout: {}",
            output.status.code(),
            stderr.trim(),
            stdout.trim()
        );
    }
}

/// A fake [`CandidateRunner`] for use in unit and integration tests.
///
/// When `should_succeed` is `true`, every call returns `Ok(())`.
/// When `false`, every call returns an error with the message in `error_msg`.
pub struct FakeCandidateRunner {
    /// Whether the runner should report success.
    pub should_succeed: bool,
    /// Error message to include when `should_succeed` is `false`.
    pub error_msg: String,
}

impl FakeCandidateRunner {
    /// Construct a runner that always succeeds.
    pub fn success() -> Self {
        Self {
            should_succeed: true,
            error_msg: String::new(),
        }
    }

    /// Construct a runner that always fails with the given message.
    pub fn failure(msg: &str) -> Self {
        Self {
            should_succeed: false,
            error_msg: msg.to_string(),
        }
    }
}

impl CandidateRunner for FakeCandidateRunner {
    fn run(&self, _candidate: &Path) -> Result<()> {
        if self.should_succeed {
            Ok(())
        } else {
            bail!("Nu candidate validation failed: {}", self.error_msg);
        }
    }
}

// ── Candidate placement ───────────────────────────────────────────────────────

/// Write the candidate content to a `.<uuid>.candidate.tmp` file in the same
/// directory as `managed_file`.
///
/// Using a same-directory temporary file ensures that the eventual rename
/// stays on the same filesystem and avoids cross-device move errors. The
/// suffix `.candidate.tmp` (not `.nu`) prevents Nu from loading the file
/// during an incidental shell startup while the candidate exists.
///
/// Returns the path to the candidate file on success.
pub fn write_candidate(managed_file: &Path, content: &str) -> Result<PathBuf> {
    let dir = managed_file.parent().with_context(|| {
        format!(
            "Managed file '{}' has no parent directory",
            managed_file.display()
        )
    })?;

    let uuid = generate_candidate_id();
    let candidate_name = format!(".{uuid}.candidate.tmp");
    let candidate_path = dir.join(&candidate_name);

    write_bytes_atomic(&candidate_path, content.as_bytes()).with_context(|| {
        format!(
            "Failed to write candidate file '{}'",
            candidate_path.display()
        )
    })?;

    Ok(candidate_path)
}

/// Generate a compact random identifier for a candidate file name.
///
/// Uses 8 random hex characters (32 bits of entropy), which is sufficient
/// for temporary file name collision avoidance within a single directory.
fn generate_candidate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Mix timestamp with a thread-id-derived salt for uniqueness.
    // We don't need cryptographic randomness here — just something unique
    // enough to avoid collisions between concurrent Numan invocations.
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let tid = std::thread::current().id();
    // Combine into a short hex string.
    format!("{ts:08x}{:04x}", format!("{tid:?}").len())
}

// ── Candidate validation ──────────────────────────────────────────────────────

/// Validate `candidate` using the provided runner, then clean up on failure.
///
/// On validation failure:
/// - The candidate file is removed.
/// - An error is returned with the package IDs and Nu's error output.
///
/// On success, the candidate file remains in place for the caller to rename
/// over the live managed file.
pub fn validate_candidate(
    candidate: &Path,
    runner: &dyn CandidateRunner,
    scoped_ids: &[&str],
) -> Result<()> {
    let result = runner.run(candidate);
    if let Err(e) = result {
        // Remove the candidate on validation failure.
        if let Err(rm_err) = std::fs::remove_file(candidate) {
            // Non-fatal: report cleanup failure without exposing the generated
            // candidate filename, which contains a random UUID.
            eprintln!("Warning: failed to remove generated candidate file: {rm_err}");
        }
        let ids = scoped_ids.join(", ");
        bail!("Module candidate validation failed for package(s): {ids}\n{e}");
    }
    Ok(())
}

// ── File replacement and deletion ─────────────────────────────────────────────

/// Replace the live managed file with the validated candidate.
///
/// The parent directory and the managed file (if it exists) must not be
/// symlinks or reparse points. If the managed file already exists, it must
/// bear the Numan ownership marker before replacement.
///
/// The rename is OS-atomic on both Windows and Unix: the file is fully
/// replaced or left entirely unchanged.
///
/// The candidate file is consumed by this operation. On failure the candidate
/// remains on disk for inspection.
pub fn replace_managed_file(managed_file: &Path, candidate: &Path) -> Result<()> {
    // Safety check: parent directory must not be a symlink.
    if let Some(parent) = managed_file.parent() {
        assert_not_symlink(parent, "vendor-autoload directory")?;
    }
    // Safety check: if the managed file already exists it must be Numan-owned.
    if managed_file.exists() {
        assert_managed_file_owned(managed_file)?;
    }

    // Read the validated candidate content.
    let content = std::fs::read(candidate)
        .with_context(|| format!("Failed to read candidate '{}'", candidate.display()))?;
    // Remove the candidate before writing the managed file so no duplicate exists.
    std::fs::remove_file(candidate)
        .with_context(|| format!("Failed to remove candidate '{}'", candidate.display()))?;
    // Write atomically — on Windows NamedTempFile::persist uses MoveFileExW with
    // MOVEFILE_REPLACE_EXISTING, so this succeeds even when numan.nu already exists.
    crate::util::atomic::write_bytes_atomic(managed_file, &content)?;

    Ok(())
}

/// Delete the Numan-managed file after verifying ownership.
///
/// Must only be called when the desired module set is empty (full
/// deactivation). Verifies ownership before deleting.
pub fn delete_managed_file(managed_file: &Path) -> Result<()> {
    assert_managed_file_owned(managed_file)?;

    std::fs::remove_file(managed_file)
        .with_context(|| format!("Failed to delete managed file '{}'", managed_file.display()))?;

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ── render_use_statement ─────────────────────────────────────────────────

    #[test]
    fn render_module_mode_simple_unix() {
        let path = Path::new(
            "/home/user/.local/share/nushell/packages/modules/owner/foo/1.0.0-abc/mod.nu",
        );
        let stmt = render_use_statement(path, &ModuleImportMode::Module).unwrap();
        assert_eq!(
            stmt,
            r#"use "/home/user/.local/share/nushell/packages/modules/owner/foo/1.0.0-abc/mod.nu""#
        );
    }

    #[test]
    fn render_all_mode_simple_unix() {
        let path = Path::new("/home/user/packages/bar/1.0.0-aaa/mod.nu");
        let stmt = render_use_statement(path, &ModuleImportMode::All).unwrap();
        assert_eq!(stmt, r#"use "/home/user/packages/bar/1.0.0-aaa/mod.nu" *"#);
    }

    #[test]
    fn render_windows_backslashes_are_doubled() {
        // Build path from raw string so it contains real backslashes.
        let path = PathBuf::from(r"A:\numan\packages\modules\owner\foo\1.0.0-a1b2c3d4\mod.nu");
        let stmt = render_use_statement(&path, &ModuleImportMode::Module).unwrap();
        // Every backslash must be doubled in the Nu string literal.
        assert!(stmt.contains(r"A:\\numan\\packages\\modules\\owner\\foo\\1.0.0-a1b2c3d4\\mod.nu"));
        assert!(stmt.starts_with("use \""));
        assert!(!stmt.ends_with(" *"));
    }

    #[test]
    fn render_windows_backslashes_all_mode() {
        let path = PathBuf::from(
            r"C:\Users\example\nushell\packages\modules\owner\bar\1.2.0-d4c3b2a1\mod.nu",
        );
        let stmt = render_use_statement(&path, &ModuleImportMode::All).unwrap();
        assert!(stmt.ends_with(" *"));
        assert!(stmt.contains(
            r"C:\\Users\\example\\nushell\\packages\\modules\\owner\\bar\\1.2.0-d4c3b2a1\\mod.nu"
        ));
    }

    #[test]
    fn render_path_with_spaces() {
        let path = Path::new("/home/user name/my packages/mod.nu");
        let stmt = render_use_statement(path, &ModuleImportMode::Module).unwrap();
        assert_eq!(stmt, r#"use "/home/user name/my packages/mod.nu""#);
    }

    #[test]
    fn render_path_with_double_quotes() {
        // A path containing a double-quote character (unusual but possible on Unix).
        let path = Path::new("/home/user/path\"with\"quotes/mod.nu");
        let stmt = render_use_statement(path, &ModuleImportMode::Module).unwrap();
        // The quotes must be escaped as \"
        assert!(stmt.contains(r#"path\"with\"quotes"#));
    }

    #[test]
    fn render_path_with_unicode() {
        let path = Path::new("/home/用户/模块/mod.nu");
        let stmt = render_use_statement(path, &ModuleImportMode::Module).unwrap();
        assert_eq!(stmt, "use \"/home/用户/模块/mod.nu\"");
    }

    #[test]
    fn render_path_with_apostrophe() {
        // Apostrophes need no escaping in Nu double-quoted strings.
        let path = Path::new("/home/user's/packages/mod.nu");
        let stmt = render_use_statement(path, &ModuleImportMode::Module).unwrap();
        assert_eq!(stmt, r#"use "/home/user's/packages/mod.nu""#);
    }

    #[test]
    fn render_path_with_brackets_and_parens() {
        let path = Path::new("/opt/nushell/pkgs/owner[1]/mod (v2).nu");
        let stmt = render_use_statement(path, &ModuleImportMode::All).unwrap();
        assert!(stmt.contains("owner[1]"));
        assert!(stmt.contains("mod (v2).nu"));
        assert!(stmt.ends_with(" *"));
    }

    // ── generate_autoload_content ────────────────────────────────────────────

    #[test]
    fn generated_content_starts_with_ownership_marker() {
        let entries = vec![];
        let content = generate_autoload_content(&entries).unwrap();
        assert!(content.starts_with(OWNERSHIP_MARKER));
    }

    #[test]
    fn generated_content_sorted_by_scoped_id() {
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
        let alpha_pos = content.find("owner/alpha").unwrap_or(usize::MAX);
        let beta_pos = content.find("owner/beta").unwrap_or(usize::MAX);
        let zeta_pos = content.find("owner/zeta").unwrap_or(usize::MAX);

        // We find the position by looking for the path strings in order.
        let alpha_path_pos = content.find("alpha").unwrap();
        let beta_path_pos = content.find("beta").unwrap();
        let zeta_path_pos = content.find("zeta").unwrap();
        assert!(
            alpha_path_pos < beta_path_pos,
            "alpha should come before beta"
        );
        assert!(
            beta_path_pos < zeta_path_pos,
            "beta should come before zeta"
        );
        let _ = (alpha_pos, beta_pos, zeta_pos); // suppress unused warnings
    }

    #[test]
    fn generated_content_uses_correct_import_modes() {
        let entries = vec![
            ResolvedEntry {
                absolute_path: PathBuf::from("/root/packages/modules/owner/foo/1.0.0-aaa/mod.nu"),
                import_mode: ModuleImportMode::Module,
                scoped_id: "owner/foo".to_string(),
            },
            ResolvedEntry {
                absolute_path: PathBuf::from("/root/packages/modules/owner/bar/1.0.0-bbb/mod.nu"),
                import_mode: ModuleImportMode::All,
                scoped_id: "owner/bar".to_string(),
            },
        ];

        let content = generate_autoload_content(&entries).unwrap();
        // bar (All mode) comes before foo (Module mode) lexicographically
        assert!(content.contains("use \"/root/packages/modules/owner/bar/1.0.0-bbb/mod.nu\" *"));
        assert!(content.contains("use \"/root/packages/modules/owner/foo/1.0.0-aaa/mod.nu\""));
        // foo line must NOT have the trailing " *"
        let foo_line = content
            .lines()
            .find(|l| l.contains("owner/foo"))
            .unwrap_or("");
        assert!(
            !foo_line.ends_with(" *"),
            "foo is Module mode, should not have ' *'"
        );
    }

    #[test]
    fn generated_content_empty_entries_is_marker_only() {
        let content = generate_autoload_content(&[]).unwrap();
        // Must start with the marker and have nothing else meaningful.
        assert!(content.starts_with(OWNERSHIP_MARKER));
        // No "use" statements.
        assert!(!content.contains("use "));
    }

    // ── FakeCandidateRunner ──────────────────────────────────────────────────

    #[test]
    fn fake_runner_success() {
        let runner = FakeCandidateRunner::success();
        runner.run(Path::new("/nonexistent/candidate.tmp")).unwrap();
    }

    #[test]
    fn fake_runner_failure() {
        let runner = FakeCandidateRunner::failure("syntax error");
        let err = runner
            .run(Path::new("/nonexistent/candidate.tmp"))
            .unwrap_err();
        assert!(err.to_string().contains("syntax error"));
    }

    // ── write_candidate ──────────────────────────────────────────────────────

    #[test]
    fn write_candidate_uses_tmp_suffix_not_nu() {
        let dir = tempfile::tempdir().unwrap();
        let managed = dir.path().join("numan.nu");
        let content =
            "# Generated and managed by Numan. Do not edit.\n# Numan autoload schema: 1\n";
        let candidate = write_candidate(&managed, content).unwrap();

        // Must be in the same directory.
        assert_eq!(candidate.parent().unwrap(), dir.path());
        // Must not end in .nu — Nu must not load it on shell startup.
        assert_ne!(candidate.extension().and_then(|e| e.to_str()), Some("nu"));
        // Must end in .tmp.
        assert_eq!(candidate.extension().and_then(|e| e.to_str()), Some("tmp"));
        // File name must contain "candidate".
        assert!(candidate
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("candidate"));
        // Content must be written correctly.
        let read_back = std::fs::read_to_string(&candidate).unwrap();
        assert_eq!(read_back, content);
    }

    // ── validate_candidate ───────────────────────────────────────────────────

    #[test]
    fn validate_candidate_success_leaves_file() {
        let dir = tempfile::tempdir().unwrap();
        let candidate = dir.path().join(".abc.candidate.tmp");
        std::fs::write(&candidate, OWNERSHIP_MARKER).unwrap();

        let runner = FakeCandidateRunner::success();
        validate_candidate(&candidate, &runner, &["owner/foo"]).unwrap();

        // File must still exist after success.
        assert!(candidate.exists());
    }

    #[test]
    fn validate_candidate_failure_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let candidate = dir.path().join(".abc.candidate.tmp");
        std::fs::write(&candidate, OWNERSHIP_MARKER).unwrap();

        let runner = FakeCandidateRunner::failure("bad import");
        let err = validate_candidate(&candidate, &runner, &["owner/foo"]).unwrap_err();

        assert!(err.to_string().contains("owner/foo"));
        assert!(err.to_string().contains("bad import"));
        // Candidate file must be removed on failure.
        assert!(!candidate.exists());
    }

    // ── replace_managed_file ─────────────────────────────────────────────────

    #[test]
    fn replace_managed_file_no_prior_file() {
        let dir = tempfile::tempdir().unwrap();
        let managed = dir.path().join("numan.nu");
        let candidate = dir.path().join(".abc.candidate.tmp");
        let content = format!("{}\nuse \"/foo/mod.nu\"\n", OWNERSHIP_MARKER);
        std::fs::write(&candidate, &content).unwrap();

        replace_managed_file(&managed, &candidate).unwrap();

        assert!(managed.exists());
        assert!(!candidate.exists()); // consumed by rename
        let read_back = std::fs::read_to_string(&managed).unwrap();
        assert_eq!(read_back, content);
    }

    #[test]
    fn replace_managed_file_over_existing_owned_file() {
        let dir = tempfile::tempdir().unwrap();
        let managed = dir.path().join("numan.nu");
        // Write an existing owned managed file.
        let old_content = format!("{}\nuse \"/old/mod.nu\"\n", OWNERSHIP_MARKER);
        std::fs::write(&managed, &old_content).unwrap();

        // Write candidate.
        let candidate = dir.path().join(".xyz.candidate.tmp");
        let new_content = format!("{}\nuse \"/new/mod.nu\"\n", OWNERSHIP_MARKER);
        std::fs::write(&candidate, &new_content).unwrap();

        replace_managed_file(&managed, &candidate).unwrap();

        let read_back = std::fs::read_to_string(&managed).unwrap();
        assert_eq!(read_back, new_content);
        assert!(!candidate.exists());
    }

    #[test]
    fn replace_managed_file_rejects_unowned_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let managed = dir.path().join("numan.nu");
        // Write a file that does NOT have the ownership marker.
        std::fs::write(&managed, "# Some other autoload file\n").unwrap();

        let candidate = dir.path().join(".abc.candidate.tmp");
        std::fs::write(&candidate, OWNERSHIP_MARKER).unwrap();

        let err = replace_managed_file(&managed, &candidate).unwrap_err();
        assert!(
            err.to_string().contains("managed-file drift") || err.to_string().contains("ownership"),
            "Expected ownership error, got: {err}"
        );
    }

    // ── delete_managed_file ──────────────────────────────────────────────────

    #[test]
    fn delete_managed_file_removes_owned_file() {
        let dir = tempfile::tempdir().unwrap();
        let managed = dir.path().join("numan.nu");
        let mut f = std::fs::File::create(&managed).unwrap();
        f.write_all(OWNERSHIP_MARKER.as_bytes()).unwrap();
        drop(f);

        delete_managed_file(&managed).unwrap();
        assert!(!managed.exists());
    }

    #[test]
    fn delete_managed_file_rejects_unowned_file() {
        let dir = tempfile::tempdir().unwrap();
        let managed = dir.path().join("numan.nu");
        std::fs::write(&managed, "# Not Numan owned\n").unwrap();

        let err = delete_managed_file(&managed).unwrap_err();
        assert!(
            err.to_string().contains("managed-file drift") || err.to_string().contains("ownership"),
            "Expected ownership error, got: {err}"
        );
    }

    // ── resolve_entry ────────────────────────────────────────────────────────

    #[test]
    fn resolve_entry_rejects_dotdot_in_payload() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let err = resolve_entry(
            root,
            "../escape",
            "mod.nu",
            ModuleImportMode::Module,
            "owner/foo",
        )
        .unwrap_err();
        assert!(err.to_string().contains("safe relative path"));
    }

    #[test]
    fn resolve_entry_rejects_dotdot_in_entry() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Create a valid payload directory first.
        let payload = "packages/modules/owner/foo/1.0.0-abc";
        std::fs::create_dir_all(root.join(payload)).unwrap();
        let err = resolve_entry(
            root,
            payload,
            "../escape.nu",
            ModuleImportMode::Module,
            "owner/foo",
        )
        .unwrap_err();
        assert!(err.to_string().contains("safe relative path"));
    }

    #[test]
    fn resolve_entry_rejects_non_nu_extension() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let payload = "packages/modules/owner/foo/1.0.0-abc";
        std::fs::create_dir_all(root.join(payload)).unwrap();
        let entry_path = root.join(payload).join("mod.sh");
        std::fs::write(&entry_path, b"").unwrap();

        let err = resolve_entry(
            root,
            payload,
            "mod.sh",
            ModuleImportMode::Module,
            "owner/foo",
        )
        .unwrap_err();
        assert!(err.to_string().contains(".nu"));
    }

    #[test]
    fn resolve_entry_rejects_directory_as_entry() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let payload = "packages/modules/owner/foo/1.0.0-abc";
        // Create the entry path as a directory (not a file).
        let entry_dir = root.join(payload).join("mod.nu");
        std::fs::create_dir_all(&entry_dir).unwrap();

        let err = resolve_entry(
            root,
            payload,
            "mod.nu",
            ModuleImportMode::Module,
            "owner/foo",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("not a regular file")
                || err.to_string().contains("failed to canonicalize"),
            "Expected regular-file or canonicalize error, got: {err}"
        );
    }

    #[test]
    fn resolve_entry_succeeds_for_valid_nu_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let payload = "packages/modules/owner/foo/1.0.0-abc";
        let payload_dir = root.join(payload);
        std::fs::create_dir_all(&payload_dir).unwrap();
        let entry_file = payload_dir.join("mod.nu");
        std::fs::write(&entry_file, b"# nushell module\n").unwrap();

        let resolved =
            resolve_entry(root, payload, "mod.nu", ModuleImportMode::All, "owner/foo").unwrap();

        assert_eq!(resolved.scoped_id, "owner/foo");
        assert_eq!(resolved.import_mode, ModuleImportMode::All);
        assert!(resolved.absolute_path.ends_with("mod.nu"));
        assert!(resolved.absolute_path.is_absolute());
    }

    #[test]
    fn resolve_entry_rejects_absolute_payload_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let err = resolve_entry(
            root,
            "/absolute/payload",
            "mod.nu",
            ModuleImportMode::Module,
            "owner/foo",
        )
        .unwrap_err();
        assert!(err.to_string().contains("safe relative path"));
    }
}
