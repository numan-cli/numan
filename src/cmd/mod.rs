pub mod activate;
pub mod activation_switch;
pub mod completions;
pub mod deactivate;
pub mod doctor;
pub mod gc;
pub mod info;
pub mod init;
pub mod install;
pub mod list;
pub mod nu_pin_offer;
pub mod nupm;
pub mod plugin_lifecycle;
pub mod registry;
pub mod remove;
pub mod search;
pub mod self_update;
pub mod setup;
pub mod setup_tools;
pub mod snapshot;
pub mod try_cmd;
pub mod update;
pub mod use_cmd;

use crate::state::autoload_recovery::AutoloadRecoveryOutcome;

fn print_autoload_recovery(outcome: AutoloadRecoveryOutcome) {
    match outcome {
        AutoloadRecoveryOutcome::NoJournal => {}
        AutoloadRecoveryOutcome::PreparedCleared => {
            eprintln!("   Module journal cleared (no external change occurred).");
        }
        AutoloadRecoveryOutcome::ReplacedCompleted => {
            eprintln!("   Module journal recovery complete.");
        }
    }
}
