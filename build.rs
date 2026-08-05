fn main() {
    if std::env::var("CARGO_INSTALL_ROOT").is_ok()
        && numan_install_guard::run_cargo_install_guard() != std::process::ExitCode::SUCCESS
    {
        std::process::exit(1);
    }
}
