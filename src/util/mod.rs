pub mod atomic;
pub mod confirm;
pub mod fs_safety;
pub mod hints;
pub mod stdio_redirect;

#[cfg(test)]
pub mod test_paths;

pub fn format_timestamp() -> String {
    format!(
        "{:016}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    )
}
