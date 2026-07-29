//! Kill switch for active-plugin update orchestration (Issue #22 PR3).
//!
//! Default **off**. `numan update` may deactivate → upgrade → reactivate an
//! active plugin only when explicitly enabled with the environment variable;
//! otherwise, update refuses while a matching `activation` is set.
//!
//! Active-plugin **remove** is always refused regardless of this flag; deactivate
//! first, then remove.

#[cfg(test)]
use std::sync::{Mutex, MutexGuard};

/// Orchestration is enabled only by an explicit value of `1`; missing, empty,
/// and alternative values disable it as a fail-safe.
pub fn is_enabled() -> bool {
    matches!(
        std::env::var("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION").as_deref(),
        Ok("1")
    )
}

/// Shared mutex for tests that mutate `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION`.
#[cfg(test)]
pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

/// RAII helper: holds [`ENV_LOCK`], saves prior env, restores on drop.
#[cfg(test)]
pub(crate) struct EnvOverrideGuard {
    _lock: MutexGuard<'static, ()>,
    previous: Option<String>,
}

#[cfg(test)]
impl EnvOverrideGuard {
    pub(crate) fn acquire() -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION").ok();
        Self {
            _lock: lock,
            previous,
        }
    }

    pub(crate) fn set(&self, value: &str) {
        std::env::set_var("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION", value);
    }

    pub(crate) fn clear(&self) {
        std::env::remove_var("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION");
    }
}

#[cfg(test)]
impl Drop for EnvOverrideGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION", value),
            None => std::env::remove_var("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_when_unset() {
        let guard = EnvOverrideGuard::acquire();
        guard.clear();
        assert!(!is_enabled());
    }

    #[test]
    fn explicit_false_and_invalid_values_disable_orchestration() {
        let guard = EnvOverrideGuard::acquire();
        guard.set("1");
        assert!(is_enabled(), "expected enabled for explicit opt-in");
        for v in ["0", "true", "TRUE", "yes", "false", "FALSE", "no", "", "on", "TRUE "] {
            guard.set(v);
            assert!(!is_enabled(), "expected disabled for {v:?}");
        }
        guard.clear();
        assert!(!is_enabled());
    }
}
