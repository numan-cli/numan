//! Central TUI theme. This is the Ratatui equivalent of a Lipgloss style
//! sheet: one place for focus, status, and risk-tier styling.
//!
//! `NO_COLOR` disables all foreground color; emphasis falls back to
//! reverse/bold so state is never carried by color alone.

use ratatui::style::{Color, Modifier, Style};

fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some()
}

pub fn accent() -> Style {
    if no_color() {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    }
}

pub fn header() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

pub fn muted() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

pub fn error() -> Style {
    if no_color() {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    }
}

pub fn warning() -> Style {
    if no_color() {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Yellow)
    }
}

pub fn ok() -> Style {
    if no_color() {
        Style::default()
    } else {
        Style::default().fg(Color::Green)
    }
}

/// Risk-tier styling for command badges. Tier is also printed as text, so
/// color is a redundant cue, never the only signal.
pub fn tier(tier: &str) -> Style {
    if no_color() {
        return Style::default();
    }
    if tier.contains("Destructive") {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if tier.contains("External") {
        Style::default().fg(Color::Magenta)
    } else if tier.contains("Network") {
        Style::default().fg(Color::Yellow)
    } else if tier.contains("ReadOnly") {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::Cyan)
    }
}

pub fn cursor() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

/// Style for a status line, based on its apparent severity.
pub fn status(text: &str) -> Style {
    let lower = text.to_lowercase();
    if lower.contains("failed")
        || lower.contains("cannot")
        || lower.contains("error")
        || lower.contains("blocked")
    {
        error()
    } else if lower.contains("stale") || lower.contains("warning") || lower.contains("unavailable")
    {
        warning()
    } else {
        Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_styles_distinguish_destructive_from_read_only() {
        if no_color() {
            return; // color cues are disabled; text labels carry the tier
        }
        assert_ne!(tier("Destructive"), tier("ReadOnly"));
        assert_ne!(tier("Destructive"), tier("RootMutation+Network"));
        assert_eq!(tier("ReadOnly"), tier("ReadOnly"));
    }

    #[test]
    fn status_styles_flag_failures() {
        assert_eq!(status("Operation failed: boom"), error());
        assert_eq!(status("STALE Nu context"), warning());
        assert_eq!(status("Local state loaded."), Style::default());
    }
}
