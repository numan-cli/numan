use anyhow::{bail, Result};
use sha2::{Digest, Sha256};
use std::io::{BufReader, Read};
use std::path::Path;

fn hash_file(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn verify_file(path: &Path, expected_sha256: &str) -> Result<bool> {
    Ok(hash_file(path)? == expected_sha256)
}

pub fn compute_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    hex::encode(result)
}

pub fn verify_bytes(data: &[u8], expected_sha256: &str) -> Result<bool> {
    let hash = compute_sha256(data);
    Ok(hash == expected_sha256)
}

pub fn verify_and_report(path: &Path, expected_sha256: &str, pkg_name: &str) -> Result<()> {
    let actual = hash_file(path)?;
    if actual != expected_sha256 {
        bail!(
            "Integrity check failed for '{pkg_name}'.
       Expected: sha256:{expected_sha256}
       Got:      sha256:{actual}
       This may indicate a corrupted download or tampered artifact."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn compute_sha256_deterministic() {
        let data = b"hello world";
        let hash1 = compute_sha256(data);
        let hash2 = compute_sha256(data);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn verify_file_matches() {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"test content").unwrap();
        let hash = compute_sha256(b"test content");
        assert!(verify_file(file.path(), &hash).unwrap());
    }

    #[test]
    fn verify_file_mismatch() {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"test content").unwrap();
        assert!(!verify_file(
            file.path(),
            "0000000000000000000000000000000000000000000000000000000000000000"
        )
        .unwrap());
    }

    #[test]
    fn verify_bytes_matches() {
        assert!(verify_bytes(b"test", &compute_sha256(b"test")).unwrap());
    }
}
