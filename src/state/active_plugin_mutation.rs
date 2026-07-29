//! Opt-in guard for active-plugin update orchestration (Issue #22 PR3).
//!
//! Default **off**. `numan update` may deactivate → upgrade → reactivate an
//! active plugin only when `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1`. Otherwise,
//! update refuses before lifecycle operations begin.
//!
//! Active-plugin **remove** is always refused regardless of this flag; deactivate
//! first, then remove.

#[cfg(test)]
use std::sync::{Mutex, MutexGuard};

/// Enable orchestration only for the exact value `1`. Missing, empty, and all
/// alternative values fail closed.
pub fn is_enabled() -> bool {
    std::env::var("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION").as_deref() == Ok("1")
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
    fn default_disabled_when_unset() {
        let guard = EnvOverrideGuard::acquire();
        guard.clear();
        assert!(!is_enabled());
    }

    #[test]
    fn enabled_only_for_exact_one() {
        let guard = EnvOverrideGuard::acquire();
        guard.set("1");
        assert!(is_enabled());
        for v in ["0", "01", "true", "TRUE", "yes", "", "1 ", " 1"] {
            guard.set(v);
            assert!(!is_enabled(), "expected disabled for {v:?}");
        }
        guard.clear();
        assert!(!is_enabled());
    }
}
