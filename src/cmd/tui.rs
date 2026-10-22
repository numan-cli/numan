use anyhow::{bail, Result};
use clap::CommandFactory;
use std::io::IsTerminal;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
enum Launch {
    Open,
    Help(Option<&'static str>),
    Refuse(&'static str),
}

fn select_launch(explicit: bool, enabled: bool, disabled: bool, interactive: bool) -> Launch {
    let reason = if disabled {
        Some("TUI disabled by NUMAN_NO_TUI=1.")
    } else if !enabled {
        Some("This build has no TUI support. Build with --features tui to enable it.")
    } else if !interactive {
        Some("The TUI requires interactive stdin and stdout. Use numan --help for CLI commands.")
    } else {
        None
    };
    match reason {
        None => Launch::Open,
        Some(reason) if explicit => Launch::Refuse(reason),
        Some(reason) => Launch::Help((disabled || !enabled).then_some(reason)),
    }
}

pub fn execute(root: &Path, explicit: bool) -> Result<()> {
    let launch = select_launch(
        explicit,
        cfg!(feature = "tui"),
        std::env::var("NUMAN_NO_TUI").as_deref() == Ok("1"),
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
    );
    match launch {
        Launch::Help(note) => {
            crate::cli::Cli::command().print_help()?;
            println!();
            if let Some(note) = note {
                eprintln!("{note}");
            }
            Ok(())
        }
        Launch::Refuse(reason) => bail!("{reason}"),
        Launch::Open => {
            #[cfg(feature = "tui")]
            {
                if let Some(command) = crate::tui::run(root)? {
                    println!("{command}");
                }
                Ok(())
            }
            #[cfg(not(feature = "tui"))]
            {
                let _ = root;
                unreachable!("feature-disabled dispatch cannot open TUI")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_matrix_and_escape_hatch() {
        for enabled in [false, true] {
            for interactive in [false, true] {
                for disabled in [false, true] {
                    for explicit in [false, true] {
                        let launch = select_launch(explicit, enabled, disabled, interactive);
                        if enabled && interactive && !disabled {
                            assert_eq!(launch, Launch::Open);
                        } else if explicit {
                            assert!(matches!(launch, Launch::Refuse(_)));
                        } else {
                            assert!(matches!(launch, Launch::Help(_)));
                        }
                        if disabled {
                            assert!(format!("{launch:?}").contains("NUMAN_NO_TUI=1"));
                        }
                    }
                }
            }
        }
    }
}
