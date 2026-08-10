use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

pub fn download_file(url: &str, dest: &Path) -> Result<()> {
    // Handle local file paths (for testing and local installs)
    if url.starts_with("file://") || (!url.contains("://") && std::path::Path::new(url).exists()) {
        let src = if url.starts_with("file://") {
            // Strip file:// prefix
            #[cfg(windows)]
            let path = url.strip_prefix("file://").unwrap_or(url);
            #[cfg(not(windows))]
            let path = url.strip_prefix("file://").unwrap_or(url);
            std::path::PathBuf::from(path)
        } else {
            std::path::PathBuf::from(url)
        };

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&src, dest)
            .with_context(|| format!("Failed to copy {} to {}", src.display(), dest.display()))?;
        return Ok(());
    }

    // HTTP/HTTPS download
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    let response = client
        .get(url)
        .send()
        .with_context(|| format!("Failed to download: {url}"))?;

    // Preserve HTTPS for callers using a secure URL; never follow a downgrade.
    if url.starts_with("https://") && response.url().scheme() != "https" {
        anyhow::bail!(
            "Refusing HTTPS download redirected to non-HTTPS URL: {}",
            response.url()
        );
    }

    if !response.status().is_success() {
        anyhow::bail!("Download failed: HTTP {}", response.status());
    }

    let total_size = response.content_length().unwrap_or(0);

    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .unwrap(),
    );

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = File::create(dest)?;
    let mut reader = response;
    let mut buffer = vec![0u8; 8192];
    let mut downloaded = 0u64;

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        file.write_all(&buffer[..bytes_read])?;
        downloaded += bytes_read as u64;
        pb.set_position(downloaded);
    }

    pb.finish_with_message("downloaded");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_file_copies_local_plain_path() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.txt");
        std::fs::write(&src, b"hello").unwrap();
        let dest = dir.path().join("nested/dest.txt");

        download_file(src.to_str().unwrap(), &dest).unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
    }

    #[test]
    fn download_file_copies_file_url() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.txt");
        std::fs::write(&src, b"world").unwrap();
        let dest = dir.path().join("dest.txt");
        let url = format!("file://{}", src.display());

        download_file(&url, &dest).unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"world");
    }
}
