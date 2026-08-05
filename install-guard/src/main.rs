use std::env;
use std::process::ExitCode;

use numan_install_guard::{
    run_cargo_install_guard, run_homebrew_install_guard, run_winget_install_guard,
};

fn main() -> ExitCode {
    match env::args().nth(1).map(|s| s.to_ascii_lowercase()) {
        Some(arg) if arg == "cargo" => run_cargo_install_guard(),
        Some(arg) if arg == "winget" => run_winget_install_guard(),
        Some(arg) if arg == "brew" || arg == "homebrew" => run_homebrew_install_guard(),
        _ => {
            eprintln!("usage: numan-install-guard <cargo|winget|brew>");
            ExitCode::from(2)
        }
    }
}
