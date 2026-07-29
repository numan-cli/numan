//! Canonical CLI fix hints aligned with `docs/numan-doctor.md`.

/// `numan init`
pub const CMD_INIT: &str = "numan init";

/// `numan init --refresh`
pub const CMD_INIT_REFRESH: &str = "numan init --refresh";

/// `numan activate`
pub const CMD_ACTIVATE: &str = "numan activate";

/// `numan activate --check`
pub const CMD_ACTIVATE_CHECK: &str = "numan activate --check";

/// `numan registry sync`
pub const CMD_REGISTRY_SYNC: &str = "numan registry sync";

/// `numan doctor --fix`
pub const CMD_DOCTOR_FIX: &str = "numan doctor --fix";

/// `numan setup nu`
pub const CMD_SETUP_NU: &str = "numan setup nu";

/// `numan setup nu --version <ver>`
pub fn setup_nu_version(version: &str) -> String {
    format!("numan setup nu --version {version}")
}

/// `numan try`
pub const CMD_TRY: &str = "numan try";

/// `numan setup loader`
pub const CMD_SETUP_LOADER: &str = "numan setup loader";

/// `numan setup nu --use-existing <path> --yes`
pub fn setup_nu_use_existing(path: &std::path::Path) -> String {
    format!("numan setup nu --use-existing {} --yes", path.display())
}

/// `numan registry add …`
pub const CMD_REGISTRY_ADD: &str = "numan registry add <name> <url> --key <base64-public-key>";

/// `numan install <owner/name>`
pub const CMD_INSTALL: &str = "numan install <owner/name>";

/// Install command for a concrete package id.
pub fn install_pkg(package_id: &str) -> String {
    format!("numan install {package_id}")
}

/// `numan remove <owner/name>`
pub const CMD_REMOVE: &str = "numan remove <owner/name>";

pub fn remove_pkg(package_id: &str) -> String {
    format!("numan remove {package_id}")
}

/// `numan nupm inspect`
pub const CMD_NUPM_INSPECT: &str = "numan nupm inspect <path>";

pub fn nupm_diff_pkg(package_id: &str) -> String {
    format!("numan nupm diff {package_id}")
}

/// Fix hint when `config.toml` has no registries (`registry.none`).
pub fn registry_none_fix(root: &std::path::Path) -> &'static str {
    use crate::core::official_registry::OFFICIAL_REGISTRY;

    if OFFICIAL_REGISTRY.is_placeholder_key() {
        CMD_REGISTRY_ADD
    } else if root.join("nu_state/paths.json").exists() {
        CMD_DOCTOR_FIX
    } else {
        CMD_INIT
    }
}

/// Format a single-command fix hint: `Run 'numan …'.`
pub fn run(cmd: &str) -> String {
    format!("Run '{cmd}'.")
}

/// Format a two-step fix hint: `Run '…', then '…'.`
pub fn run_then(first: &str, second: &str) -> String {
    format!("Run '{first}', then '{second}'.")
}

/// `numan deactivate`
pub const CMD_DEACTIVATE: &str = "numan deactivate";

/// `numan deactivate <pkg>`
pub fn deactivate_pkg(package_id: &str) -> String {
    format!("numan deactivate {package_id}")
}

/// Hint when an active plugin cannot be removed (Issue #22 gate).
///
/// Remove is always refused while `activation` is set, even when active-plugin
/// update orchestration is enabled. Deactivate first, then remove.
pub fn active_plugin_mutation_gated(package_id: &str) -> String {
    format!(
        "Package '{package_id}' has a plugin activation record. \
Run `numan deactivate {package_id}`, then `numan remove {package_id}`. \
Active-plugin remove stays gated (Issue #22); \
`remove --force` does not bypass plugin activation \
(https://github.com/tonythethompson/numan/issues/22)."
    )
}

/// Hint when the active-plugin update emergency kill switch is set.
pub fn active_plugin_update_disabled(package_id: &str) -> String {
    format!(
        "Package '{package_id}' has a plugin activation record and active-plugin \
update orchestration is disabled by NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION. \
Unset it or set NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1 to re-enable \
deactivate→update→activate, or run `numan deactivate {package_id}` first \
then `numan update {package_id}` while inactive \
(https://github.com/tonythethompson/numan/issues/22)."
    )
}

/// Compact `activate --list` note for active-plugin update availability.
pub fn active_plugin_update_list_note(permitted: bool) -> &'static str {
    if permitted {
        "update: permitted (deactivate→upgrade→activate)"
    } else {
        "update: gated (check Nu identity or the NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION kill switch)"
    }
}

/// Doctor `fix` field for `activation.plugin_mutation_gated`.
///
/// Aligned with [`active_plugin_mutation_gated`], [`active_plugin_update_disabled`],
/// and `docs/active-plugin-gate.md` / `docs/numan-doctor.md`.
pub const ACTIVE_PLUGIN_MUTATION_GATED_FIX: &str =
    "Remove: `numan deactivate <pkg>`, then `numan remove <pkg>`. \
Update: `numan update <pkg>` orchestrates deactivate→upgrade→activate by default; \
NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=0 is the emergency kill switch. \
See docs/active-plugin-gate.md.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_plugin_mutation_gated_mentions_package_and_issue() {
        let hint = active_plugin_mutation_gated("owner/plugin");
        assert!(hint.contains("owner/plugin"));
        assert!(hint.contains("Issue #22"));
        assert!(hint.contains("activation record"));
        assert!(hint.contains("deactivate"));
        assert!(hint.contains("remove"));
        assert!(ACTIVE_PLUGIN_MUTATION_GATED_FIX.contains("docs/active-plugin-gate.md"));
        let deactivate = ACTIVE_PLUGIN_MUTATION_GATED_FIX
            .find("deactivate")
            .expect("fix hint must mention deactivate");
        let remove = ACTIVE_PLUGIN_MUTATION_GATED_FIX
            .find("remove")
            .expect("fix hint must mention remove");
        assert!(
            deactivate < remove,
            "fix hint must list deactivate before remove"
        );
    }

    #[test]
    fn active_plugin_update_disabled_mentions_env_kill_switch() {
        let hint = active_plugin_update_disabled("owner/plugin");
        assert!(hint.contains("owner/plugin"));
        assert!(hint.contains("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION"));
        assert!(hint.contains("Unset it"));
        assert!(hint.contains("numan deactivate owner/plugin"));
        assert!(hint.contains("numan update owner/plugin"));
        assert!(
            !hint.contains("numan remove"),
            "update gate must not suggest remove"
        );
        assert!(ACTIVE_PLUGIN_MUTATION_GATED_FIX.contains("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=0"));
        assert!(ACTIVE_PLUGIN_MUTATION_GATED_FIX.contains("by default"));
        assert!(ACTIVE_PLUGIN_MUTATION_GATED_FIX.contains("numan update"));
    }

    #[test]
    fn run_formats_single_command() {
        assert_eq!(run(CMD_INIT), "Run 'numan init'.");
    }

    #[test]
    fn run_then_formats_two_commands() {
        assert_eq!(
            run_then(CMD_INIT_REFRESH, CMD_ACTIVATE),
            "Run 'numan init --refresh', then 'numan activate'."
        );
    }

    #[test]
    fn registry_none_fix_prefers_init_before_first_init() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(registry_none_fix(dir.path()), CMD_INIT);
    }

    #[test]
    fn registry_none_fix_prefers_doctor_fix_after_init_without_registries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nu_state")).unwrap();
        std::fs::write(dir.path().join("nu_state/paths.json"), b"{}").unwrap();
        assert_eq!(registry_none_fix(dir.path()), CMD_DOCTOR_FIX);
    }
}
