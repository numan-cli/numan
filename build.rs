mod install_channel {
    #![allow(dead_code)]
    include!("src/util/install_channel.rs");
}

fn is_cargo_install_build() -> bool {
    if std::env::var("CARGO_INSTALL_ROOT").is_ok() {
        return true;
    }
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        let normalized = out_dir.replace('\\', "/").to_ascii_lowercase();
        if normalized.contains("cargo-install") {
            return true;
        }
    }
    false
}

fn main() {
    if is_cargo_install_build()
        && install_channel::run_cargo_install_guard() != std::process::ExitCode::SUCCESS
    {
        std::process::exit(1);
    }
}
