mod install_channel {
    #![allow(dead_code)]
    include!("src/util/install_channel.rs");
}

fn main() {
    if std::env::var("CARGO_INSTALL_ROOT").is_ok()
        && install_channel::run_cargo_install_guard() != std::process::ExitCode::SUCCESS
    {
        std::process::exit(1);
    }
}
