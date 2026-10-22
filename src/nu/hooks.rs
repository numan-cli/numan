//! Hook event allow-list (issue #16, Phase 5 — data model only).

/// Deliberately small allow-list of Nushell hook events whose composition and
/// acceptance behavior can be proven. Unknown events are rejected at parse time.
pub const ALLOWED_HOOK_EVENTS: &[&str] = &[
    "pre_prompt",
    "pre_execution",
    "env_change",
    "display_output",
];

pub fn is_allowed_event(event: &str) -> bool {
    ALLOWED_HOOK_EVENTS.contains(&event)
}
