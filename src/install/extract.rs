use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use globset::{Glob, GlobSetBuilder};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use tar::Archive as TarArchive;
use xz2::read::XzDecoder;
use zip::ZipArchive;

/// Maximum number of files we'll extract from a single archive.
const MAX_FILE_COUNT: usize = 10_000;
/// Maximum uncompressed size (100 MB).
const MAX_UNCOMPRESSED_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    TarGz,
    TarXz,
    Tar,
}

impl ArchiveFormat {
    /// Supported archive suffixes, longest-first so compound extensions win.
    ///
    /// Keep this list as the single source of truth for URL/filename detection.
    pub const SUFFIXES: &'static [&'static str] =
        &[".tar.gz", ".tgz", ".tar.xz", ".txz", ".zip", ".tar"];

    /// Strip query/fragment and lowercase a URL or filename for suffix matching.
    fn normalize_url_path(url: &str) -> String {
        let without_query = url.split_once('?').map_or(url, |(path, _)| path);
        without_query
            .split_once('#')
            .map_or(without_query, |(path, _)| path)
            .to_ascii_lowercase()
    }

    /// Return the literal matched suffix (including the leading `.`), or `None`.
    ///
    /// Preserves short forms (`.tgz` / `.txz`) rather than canonicalizing them.
    pub fn matched_suffix(url: &str) -> Option<&'static str> {
        let path = Self::normalize_url_path(url);
        Self::SUFFIXES
            .iter()
            .copied()
            .find(|suffix| path.ends_with(suffix))
    }

    /// Detect format from a URL or filename. Does NOT inspect file contents.
    ///
    /// Query strings and URL fragments are ignored; matching is case-insensitive.
    pub fn from_url(url: &str) -> Option<Self> {
        match Self::matched_suffix(url)? {
            ".zip" => Some(ArchiveFormat::Zip),
            ".tar.gz" | ".tgz" => Some(ArchiveFormat::TarGz),
            ".tar.xz" | ".txz" => Some(ArchiveFormat::TarXz),
            ".tar" => Some(ArchiveFormat::Tar),
            _ => None,
        }
    }

    /// Return the canonical file extension for this format.
    pub fn extension(&self) -> &'static str {
        match self {
            ArchiveFormat::Zip => "zip",
            ArchiveFormat::TarGz => "tar.gz",
            ArchiveFormat::TarXz => "tar.xz",
            ArchiveFormat::Tar => "tar",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExtractConfig {
    pub archive_root: Option<String>,
    pub include: Option<Vec<String>>,
    pub entry: Option<String>,
    /// Override the default 100 MiB uncompressed-size cap (e.g. official Nushell
    /// releases; bootstrap uses 512 MiB and also filters to the `nu` binary).
    pub max_uncompressed_bytes: Option<u64>,
}

fn max_uncompressed_bytes(config: &ExtractConfig) -> u64 {
    config
        .max_uncompressed_bytes
        .unwrap_or(MAX_UNCOMPRESSED_BYTES)
}

#[derive(Debug)]
pub struct ExtractResult {
    pub files: Vec<PathBuf>,
    pub entry_found: bool,
}

/// Extract an archive. `format` must be provided explicitly — never inferred from the filename.
pub fn extract_archive(
    archive_path: &Path,
    dest_dir: &Path,
    config: &ExtractConfig,
    format: ArchiveFormat,
) -> Result<ExtractResult> {
    match format {
        ArchiveFormat::Zip => extract_zip(archive_path, dest_dir, config),
        ArchiveFormat::TarGz => {
            let file = File::open(archive_path)
                .with_context(|| format!("Failed to open archive: {}", archive_path.display()))?;
            let decoder = GzDecoder::new(file);
            let mut archive = TarArchive::new(decoder);
            extract_tar_inner(&mut archive, dest_dir, config)
        }
        ArchiveFormat::TarXz => {
            let file = File::open(archive_path)
                .with_context(|| format!("Failed to open archive: {}", archive_path.display()))?;
            let decoder = XzDecoder::new(file);
            let mut archive = TarArchive::new(decoder);
            extract_tar_inner(&mut archive, dest_dir, config)
        }
        ArchiveFormat::Tar => {
            let file = File::open(archive_path)
                .with_context(|| format!("Failed to open archive: {}", archive_path.display()))?;
            let mut archive = TarArchive::new(file);
            extract_tar_inner(&mut archive, dest_dir, config)
        }
    }
}

fn extract_zip(
    archive_path: &Path,
    dest_dir: &Path,
    config: &ExtractConfig,
) -> Result<ExtractResult> {
    let file = File::open(archive_path)
        .with_context(|| format!("Failed to open archive: {}", archive_path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("Failed to read zip: {}", archive_path.display()))?;

    let mut extracted_files = Vec::new();
    let mut entry_found = false;
    let mut file_count: usize = 0;
    let mut total_bytes: u64 = 0;
    let max_bytes = max_uncompressed_bytes(config);
    let archive_root = config.archive_root.as_deref();
    let include_checker = build_include_checker(config.include.as_deref())?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| anyhow::anyhow!("Zip entry error: {e}"))?;

        // Validate the raw name BEFORE any mangling
        let raw_name = entry.name().to_owned();
        if has_path_traversal_str(&raw_name) {
            bail!("Path traversal detected in archive: {raw_name}");
        }

        // Skip directories
        if entry.is_dir() {
            continue;
        }

        // Archive bomb limits apply to every regular-file entry scanned in the
        // archive, not only entries that pass archive_root / include filters.
        file_count += 1;
        if file_count > MAX_FILE_COUNT {
            bail!(
                "Archive contains more than {MAX_FILE_COUNT} files. \
                 This may be an archive bomb."
            );
        }
        total_bytes += entry.size();
        if total_bytes > max_bytes {
            bail!(
                "Archive uncompressed size exceeds {max_bytes} bytes. \
                 This may be an archive bomb."
            );
        }

        let entry_path = entry.mangled_name().to_owned();

        // Get relative path (strip archive_root if present)
        let relative_path = if let Some(root) = archive_root {
            strip_leading_component(&entry_path, root)
        } else {
            Some(entry_path.clone())
        };

        let relative_path = match relative_path {
            Some(p) => p,
            None => continue,
        };

        // Include filter only decides whether to write the entry to disk.
        if !include_checker.matches(&relative_path) {
            continue;
        }

        // Validate resolved path (no traversal after stripping root)
        if has_path_traversal(&relative_path) {
            bail!("Path traversal detected in archive: {}", raw_name);
        }

        let out_path = dest_dir.join(&relative_path);

        // Create parent directory
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Extract file
        let mut contents = Vec::new();
        entry
            .read_to_end(&mut contents)
            .with_context(|| format!("Failed to read zip entry: {raw_name}"))?;
        std::fs::write(&out_path, contents)?;

        // Check entry point — compare against full relative path
        if let Some(ref entry_pattern) = config.entry {
            if relative_path_matches(&relative_path, entry_pattern) {
                entry_found = true;
            }
        }

        extracted_files.push(out_path);
    }

    Ok(ExtractResult {
        files: extracted_files,
        entry_found,
    })
}

fn extract_tar_inner<R: Read>(
    archive: &mut TarArchive<R>,
    dest_dir: &Path,
    config: &ExtractConfig,
) -> Result<ExtractResult> {
    let mut extracted_files = Vec::new();
    let mut entry_found = false;
    let mut file_count: usize = 0;
    let mut total_bytes: u64 = 0;
    let max_bytes = max_uncompressed_bytes(config);
    let archive_root = config.archive_root.as_deref();
    let include_checker = build_include_checker(config.include.as_deref())?;

    for entry in archive
        .entries()
        .map_err(|e| anyhow::anyhow!("Failed to read tar entries: {e}"))?
    {
        let mut entry = entry.map_err(|e| anyhow::anyhow!("Tar entry error: {e}"))?;
        let entry_path = entry
            .path()
            .map_err(|e| anyhow::anyhow!("Failed to get tar entry path: {e}"))?
            .into_owned();

        let entry_type = entry.header().entry_type();

        // Skip directories and special entries
        match entry_type {
            tar::EntryType::Directory => continue,
            tar::EntryType::Regular | tar::EntryType::Continuous => {}
            tar::EntryType::Symlink
            | tar::EntryType::Link
            | tar::EntryType::Char
            | tar::EntryType::Block
            | tar::EntryType::Fifo => {
                bail!(
                    "Refusing to extract non-regular tar entry type {:?}: {}",
                    entry_type,
                    entry_path.display()
                );
            }
            _ => {
                // GNULongLink, GNULongName, GNUSparse, XHeader, XGlobalHeader
                // These are metadata entries, skip them
                continue;
            }
        }

        // Archive bomb limits apply to every regular-file entry scanned in the
        // archive, not only entries that pass archive_root / include filters.
        file_count += 1;
        if file_count > MAX_FILE_COUNT {
            bail!(
                "Archive contains more than {MAX_FILE_COUNT} files. \
                 This may be an archive bomb."
            );
        }
        total_bytes += entry.header().size().unwrap_or(0);
        if total_bytes > max_bytes {
            bail!(
                "Archive uncompressed size exceeds {max_bytes} bytes. \
                 This may be an archive bomb."
            );
        }

        // Get relative path (strip archive_root if present)
        let relative_path = if let Some(root) = archive_root {
            strip_leading_component(&entry_path, root)
        } else {
            Some(entry_path.clone())
        };

        let relative_path = match relative_path {
            Some(p) => p,
            None => continue,
        };

        // Validate path (no traversal)
        if has_path_traversal(&relative_path) {
            bail!(
                "Path traversal detected in archive: {}",
                entry_path.display()
            );
        }

        // Include filter only decides whether to write the entry to disk.
        if !include_checker.matches(&relative_path) {
            continue;
        }

        let out_path = dest_dir.join(&relative_path);

        // Create parent directory
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Extract file
        entry
            .unpack(&out_path)
            .with_context(|| format!("Failed to extract: {}", entry_path.display()))?;

        // Check entry point — compare against full relative path
        if let Some(ref entry_pattern) = config.entry {
            if relative_path_matches(&relative_path, entry_pattern) {
                entry_found = true;
            }
        }

        extracted_files.push(out_path);
    }

    Ok(ExtractResult {
        files: extracted_files,
        entry_found,
    })
}

/// Build a glob-based include checker from patterns.
/// Patterns use standard glob syntax: `*.nu`, `git/**`, `bin/*`, etc.
fn build_include_checker(patterns: Option<&[String]>) -> Result<IncludeChecker> {
    let patterns = match patterns {
        Some(p) if !p.is_empty() => p.to_vec(),
        _ => return Ok(IncludeChecker::PassAll),
    };

    let mut builder = GlobSetBuilder::new();
    for pattern in &patterns {
        // Normalize: interpret `*` as a glob on a single path component
        // and `**` as crossing directory boundaries
        let glob_str = if pattern.contains('/') || pattern.contains('\\') {
            // Path-aware pattern — normalize separators
            pattern.replace('\\', "/")
        } else {
            // Simple filename pattern
            pattern.clone()
        };

        let glob =
            Glob::new(&glob_str).with_context(|| format!("Invalid glob pattern: '{pattern}'"))?;
        builder.add(glob);
    }

    let glob_set = builder.build().context("Failed to compile glob patterns")?;
    Ok(IncludeChecker::Glob(glob_set))
}

enum IncludeChecker {
    PassAll,
    Glob(globset::GlobSet),
}

impl IncludeChecker {
    fn matches(&self, path: &Path) -> bool {
        match self {
            IncludeChecker::PassAll => true,
            IncludeChecker::Glob(glob) => {
                // Normalize the path to use forward slashes for matching
                let path_str = path.to_string_lossy().replace('\\', "/");
                glob.is_match(&path_str)
            }
        }
    }
}

/// Check if a resolved entry path matches an entry pattern.
/// Compares against the full normalized relative path, not just the basename.
fn relative_path_matches(path: &Path, pattern: &str) -> bool {
    let path_str = path.to_string_lossy().replace('\\', "/");
    let pattern_norm = pattern.replace('\\', "/");

    // Exact match
    if path_str == pattern_norm {
        return true;
    }

    // Glob match
    if let Ok(glob) = Glob::new(&pattern_norm) {
        let compiled = glob.compile_matcher();
        return compiled.is_match(&path_str);
    }

    false
}

/// Strip the archive_root prefix from a path
fn strip_leading_component(path: &Path, root: &str) -> Option<PathBuf> {
    let components: Vec<&std::ffi::OsStr> = path.components().map(|c| c.as_os_str()).collect();
    if components.is_empty() {
        return None;
    }

    // Check if first component matches root
    if components[0] != std::ffi::OsStr::new(root) {
        return None;
    }

    // Return remaining path
    if components.len() == 1 {
        return None; // It's just the root directory
    }

    Some(components[1..].iter().collect())
}

/// Check if a path string contains dangerous traversal.
/// Only rejects leading `..` or absolute paths — inner `..` is fine.
fn has_path_traversal_str(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    // Reject leading ..
    if normalized.starts_with("../") || normalized == ".." {
        return true;
    }
    // Reject absolute paths
    if normalized.starts_with('/') {
        return true;
    }
    false
}

/// Check if a path contains traversal components
fn has_path_traversal(path: &Path) -> bool {
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => return true,
            std::path::Component::RootDir => return true,
            _ => {}
        }
    }
    false
}

/// Compute SHA256 of a file
pub fn compute_file_sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .with_context(|| format!("Failed to open file for hashing: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_zip(dir: &Path, files: &[(&str, &[u8])]) -> PathBuf {
        let zip_path = dir.join("test.zip");
        let file = File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        for (name, content) in files {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(content).unwrap();
        }

        zip.finish().unwrap();
        zip_path
    }

    fn create_test_tar_xz(dir: &Path, files: &[(&str, &[u8])]) -> PathBuf {
        let xz_path = dir.join("test.tar.xz");
        let file = File::create(&xz_path).unwrap();
        let encoder = xz2::write::XzEncoder::new(file, 6);
        let mut builder = tar::Builder::new(encoder);

        for (name, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *content).unwrap();
        }

        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();
        xz_path
    }

    #[test]
    fn extract_zip_basic() {
        let tmp = TempDir::new().unwrap();
        let zip_path = create_test_zip(
            tmp.path(),
            &[("test.txt", b"hello"), ("subdir/nested.txt", b"nested")],
        );

        let dest = tmp.path().join("extracted");
        std::fs::create_dir(&dest).unwrap();

        let result = extract_archive(
            &zip_path,
            &dest,
            &ExtractConfig::default(),
            ArchiveFormat::Zip,
        )
        .unwrap();
        assert_eq!(result.files.len(), 2);
        assert!(dest.join("test.txt").exists());
        assert!(dest.join("subdir/nested.txt").exists());
    }

    #[test]
    fn extract_zip_with_archive_root() {
        let tmp = TempDir::new().unwrap();
        let zip_path = create_test_zip(
            tmp.path(),
            &[
                ("pkg-1.0.0/file.txt", b"content"),
                ("pkg-1.0.0/inner/other.txt", b"other"),
            ],
        );

        let dest = tmp.path().join("extracted");
        std::fs::create_dir(&dest).unwrap();

        let config = ExtractConfig {
            archive_root: Some("pkg-1.0.0".to_string()),
            include: None,
            entry: None,
            ..ExtractConfig::default()
        };

        let result = extract_archive(&zip_path, &dest, &config, ArchiveFormat::Zip).unwrap();
        assert_eq!(result.files.len(), 2);
        assert!(dest.join("file.txt").exists());
        assert!(dest.join("inner/other.txt").exists());
    }

    #[test]
    fn extract_zip_with_include_filter() {
        let tmp = TempDir::new().unwrap();
        let zip_path = create_test_zip(
            tmp.path(),
            &[
                ("main.nu", b"main script"),
                ("README.md", b"readme"),
                ("lib.nu", b"library"),
            ],
        );

        let dest = tmp.path().join("extracted");
        std::fs::create_dir(&dest).unwrap();

        let config = ExtractConfig {
            archive_root: None,
            include: Some(vec!["*.nu".to_string()]),
            entry: None,
            ..ExtractConfig::default()
        };

        let result = extract_archive(&zip_path, &dest, &config, ArchiveFormat::Zip).unwrap();
        assert_eq!(result.files.len(), 2);
        assert!(dest.join("main.nu").exists());
        assert!(dest.join("lib.nu").exists());
        assert!(!dest.join("README.md").exists());
    }

    #[test]
    fn extract_zip_include_filter_still_enforces_uncompressed_size_cap() {
        let tmp = TempDir::new().unwrap();
        // Excluded sibling is listed first so the size cap trips before any
        // included entry is written.
        let zip_path = create_test_zip(
            tmp.path(),
            &[("nu_plugin_polars", &[0u8; 2048]), ("nu", b"shell")],
        );

        let dest = tmp.path().join("extracted");
        std::fs::create_dir(&dest).unwrap();

        let err = extract_archive(
            &zip_path,
            &dest,
            &ExtractConfig {
                include: Some(vec!["nu".to_string()]),
                max_uncompressed_bytes: Some(1024),
                ..ExtractConfig::default()
            },
            ArchiveFormat::Zip,
        )
        .expect_err("excluded siblings must still count toward the archive bomb cap");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Archive uncompressed size exceeds"),
            "unexpected error: {msg}"
        );
        assert!(
            !dest.join("nu").exists(),
            "included entry must not be written after the size cap fails"
        );
        assert!(
            !dest.join("nu_plugin_polars").exists(),
            "excluded plugin must never be written"
        );
    }

    #[test]
    fn extract_zip_with_path_include_filter() {
        let tmp = TempDir::new().unwrap();
        let zip_path = create_test_zip(
            tmp.path(),
            &[
                ("git/mod.nu", b"module"),
                ("git/README.md", b"readme"),
                ("other.nu", b"other"),
            ],
        );

        let dest = tmp.path().join("extracted");
        std::fs::create_dir(&dest).unwrap();

        let config = ExtractConfig {
            archive_root: None,
            include: Some(vec!["git/**".to_string()]),
            entry: None,
            ..ExtractConfig::default()
        };

        let result = extract_archive(&zip_path, &dest, &config, ArchiveFormat::Zip).unwrap();
        assert_eq!(result.files.len(), 2);
        assert!(dest.join("git/mod.nu").exists());
        assert!(dest.join("git/README.md").exists());
        assert!(!dest.join("other.nu").exists());
    }

    #[test]
    fn extract_zip_with_entry_full_path() {
        let tmp = TempDir::new().unwrap();
        let zip_path = create_test_zip(
            tmp.path(),
            &[("git/mod.nu", b"module"), ("git/other.nu", b"other")],
        );

        let dest = tmp.path().join("extracted");
        std::fs::create_dir(&dest).unwrap();

        let config = ExtractConfig {
            archive_root: None,
            include: None,
            entry: Some("git/mod.nu".to_string()),
            ..ExtractConfig::default()
        };

        let result = extract_archive(&zip_path, &dest, &config, ArchiveFormat::Zip).unwrap();
        assert!(result.entry_found);
    }

    #[test]
    fn extract_zip_rejects_traversal_raw_name() {
        let tmp = TempDir::new().unwrap();

        // Build a zip with a traversal entry manually
        let zip_path = tmp.path().join("traversal.zip");
        {
            let file = File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file("../../etc/passwd", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"bad").unwrap();
            zip.finish().unwrap();
        }

        let dest = tmp.path().join("extracted");
        std::fs::create_dir(&dest).unwrap();

        let result = extract_archive(
            &zip_path,
            &dest,
            &ExtractConfig::default(),
            ArchiveFormat::Zip,
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Path traversal"),
            "Expected path traversal error, got: {err}"
        );
    }

    #[test]
    fn strip_leading_component_works() {
        let path = PathBuf::from("pkg-1.0.0/file.txt");
        let result = strip_leading_component(&path, "pkg-1.0.0").unwrap();
        assert_eq!(result, PathBuf::from("file.txt"));
    }

    #[test]
    fn strip_leading_component_no_match() {
        let path = PathBuf::from("other-1.0.0/file.txt");
        let result = strip_leading_component(&path, "pkg-1.0.0");
        assert!(result.is_none());
    }

    #[test]
    fn include_matches_extension() {
        let checker = build_include_checker(Some(&["*.nu".to_string()])).unwrap();
        assert!(checker.matches(&PathBuf::from("script.nu")));
        assert!(!checker.matches(&PathBuf::from("script.rs")));
    }

    #[test]
    fn include_matches_path_glob() {
        let checker = build_include_checker(Some(&["git/**".to_string()])).unwrap();
        assert!(checker.matches(&PathBuf::from("git/mod.nu")));
        assert!(checker.matches(&PathBuf::from("git/README.md")));
        assert!(!checker.matches(&PathBuf::from("other.nu")));
    }

    #[test]
    fn include_matches_no_filter() {
        let checker = build_include_checker(None).unwrap();
        assert!(checker.matches(&PathBuf::from("anything.txt")));
    }

    #[test]
    fn has_path_traversal_detects_parent() {
        let path = PathBuf::from("../../etc/passwd");
        assert!(has_path_traversal(&path));

        let safe = PathBuf::from("subdir/file.txt");
        assert!(!has_path_traversal(&safe));
    }

    #[test]
    fn has_path_traversal_str_works() {
        assert!(has_path_traversal_str("../../etc/passwd"));
        assert!(has_path_traversal_str("../foo"));
        assert!(has_path_traversal_str(".."));
        assert!(has_path_traversal_str("/etc/passwd"));
        assert!(!has_path_traversal_str("foo/bar"));
        // Inner .. is fine — only leading/absolute traversal is dangerous
        assert!(!has_path_traversal_str("foo/../bar/baz"));
    }

    #[test]
    fn relative_path_matches_full_path() {
        assert!(relative_path_matches(
            &PathBuf::from("git/mod.nu"),
            "git/mod.nu"
        ));
        assert!(relative_path_matches(
            &PathBuf::from("git/mod.nu"),
            "git/*.nu"
        ));
        assert!(!relative_path_matches(
            &PathBuf::from("other/mod.nu"),
            "git/mod.nu"
        ));
    }

    #[test]
    fn extract_tar_xz_basic() {
        let tmp = TempDir::new().unwrap();
        let xz_path = create_test_tar_xz(
            tmp.path(),
            &[
                ("test.txt", b"hello-xz"),
                ("subdir/nested.txt", b"nested-xz"),
            ],
        );

        let dest = tmp.path().join("extracted");
        std::fs::create_dir(&dest).unwrap();

        let result = extract_archive(
            &xz_path,
            &dest,
            &ExtractConfig::default(),
            ArchiveFormat::TarXz,
        )
        .unwrap();
        assert_eq!(result.files.len(), 2);
        assert_eq!(
            std::fs::read_to_string(dest.join("test.txt")).unwrap(),
            "hello-xz"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("subdir/nested.txt")).unwrap(),
            "nested-xz"
        );
    }

    #[test]
    fn archive_format_from_url() {
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/pkg.zip"),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/pkg.tar.gz"),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/pkg.tgz"),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/pkg.tar.xz"),
            Some(ArchiveFormat::TarXz)
        );
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/pkg.txz"),
            Some(ArchiveFormat::TarXz)
        );
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/pkg.tar"),
            Some(ArchiveFormat::Tar)
        );
        assert_eq!(ArchiveFormat::from_url("https://example.com/pkg.bin"), None);
        assert_eq!(ArchiveFormat::extension(&ArchiveFormat::TarXz), "tar.xz");
    }

    #[test]
    fn archive_format_from_url_strips_query_fragment_and_case() {
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/PKG.ZIP?download=1"),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/pkg.tar.xz?dl=1#asset"),
            Some(ArchiveFormat::TarXz)
        );
        assert_eq!(
            ArchiveFormat::matched_suffix("https://example.com/plugin.TGZ#release"),
            Some(".tgz")
        );
        assert_eq!(
            ArchiveFormat::matched_suffix("https://example.com/plugin.txz?x=1"),
            Some(".txz")
        );
    }

    #[test]
    fn compute_file_sha256_deterministic() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.bin");
        std::fs::write(&file_path, b"test content").unwrap();

        let hash1 = compute_file_sha256(&file_path).unwrap();
        let hash2 = compute_file_sha256(&file_path).unwrap();
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64);
    }
}
