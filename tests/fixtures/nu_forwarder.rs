use std::ffi::OsString;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let rendered = args
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");

    if rendered.contains("plugin rm")
        && std::env::var("NUMAN_TEST_FAIL_PLUGIN_RM").as_deref() == Ok("1")
    {
        eprintln!("NUMAN_TEST_FAIL_PLUGIN_RM: refusing plugin rm");
        return ExitCode::FAILURE;
    }
    if rendered.contains("plugin add")
        && std::env::var("NUMAN_TEST_FAIL_PLUGIN_ADD").as_deref() == Ok("1")
    {
        eprintln!("NUMAN_TEST_FAIL_PLUGIN_ADD: refusing plugin add");
        return ExitCode::FAILURE;
    }

    let real_nu = match std::env::var_os("NUMAN_TEST_REAL_NU") {
        Some(path) => path,
        None => {
            eprintln!("NUMAN_TEST_REAL_NU is required");
            return ExitCode::FAILURE;
        }
    };
    match Command::new(real_nu).args(args).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("failed to invoke real Nu: {error}");
            ExitCode::FAILURE
        }
    }
}
