//! Central TUI theme. This is the Ratatui equivalent of a Lipgloss style
//! sheet: one place for focus, status, and risk-tier styling.
//!
//! `NO_COLOR` disables all foreground color; emphasis falls back to
//! reverse/bold/bracketed labels so state is never carried by color alone.

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border::{self, Set as BorderSet};
use ratatui::text::Span;

pub const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn no_color() -> bool {
    static NO_COLOR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *NO_COLOR.get_or_init(|| std::env::var_os("NO_COLOR").is_some())
}

pub fn is_ascii() -> bool {
    std::env::var_os("NUMAN_TUI_ASCII").as_deref() == Some(std::ffi::OsStr::new("1"))
}

pub fn border_set(ascii: bool) -> BorderSet<'static> {
    if ascii {
        BorderSet {
            top_left: "+",
            top_right: "+",
            bottom_left: "+",
            bottom_right: "+",
            vertical_left: "|",
            vertical_right: "|",
            horizontal_top: "-",
            horizontal_bottom: "-",
        }
    } else {
        border::ROUNDED
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Warn,
    Err,
    Info,
    Muted,
}

pub fn tone_style_with(tone: Tone, disable_color: bool) -> Style {
    if disable_color {
        return match tone {
            Tone::Err => Style::default().add_modifier(Modifier::BOLD),
            Tone::Warn => Style::default().add_modifier(Modifier::BOLD),
            Tone::Info => Style::default().add_modifier(Modifier::BOLD),
            Tone::Ok => Style::default(),
            Tone::Muted => Style::default().add_modifier(Modifier::DIM),
        };
    }
    match tone {
        Tone::Ok => Style::default().fg(Color::Green),
        Tone::Warn => Style::default().fg(Color::Yellow),
        Tone::Err => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        Tone::Info => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        Tone::Muted => Style::default().add_modifier(Modifier::DIM),
    }
}

pub fn tone_style(tone: Tone) -> Style {
    tone_style_with(tone, no_color())
}

pub fn status_tone(status: &str) -> Tone {
    let lower = status.to_lowercase();
    if matches!(
        lower.as_str(),
        "active" | "compatible" | "proven" | "verified" | "ed25519 ok" | "ready" | "clean"
    ) {
        Tone::Ok
    } else if matches!(
        lower.as_str(),
        "update" | "provisional" | "other nu" | "warn" | "stale"
    ) || lower.contains("warn")
        || lower.contains("other nu")
    {
        Tone::Warn
    } else if matches!(
        lower.as_str(),
        "incompatible" | "error" | "blocked" | "missing" | "corrupt"
    ) || lower.contains("error")
        || lower.contains("fail")
    {
        Tone::Err
    } else if matches!(lower.as_str(), "installed" | "live" | "scanned") {
        Tone::Info
    } else {
        Tone::Muted
    }
}

pub fn tier_tone(tier: &str) -> Tone {
    if tier.contains("Destructive") {
        Tone::Err
    } else if tier.contains("External") {
        Tone::Info
    } else if tier.contains("Network") {
        Tone::Warn
    } else if tier.contains("ReadOnly") {
        Tone::Ok
    } else {
        Tone::Info
    }
}

pub fn tab_style(active: bool) -> Style {
    if !active {
        return Style::default();
    }
    if no_color() {
        Style::default()
            .add_modifier(Modifier::REVERSED)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    }
}

pub fn pill_span_with(text: &str, tone: Tone, disable_color: bool) -> Span<'static> {
    Span::styled(format!("[{text}]"), tone_style_with(tone, disable_color))
}

pub fn pill_span(text: &str, tone: Tone) -> Span<'static> {
    pill_span_with(text, tone, no_color())
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
    fn status_styles_flag_failures() {
        assert_eq!(status("Operation failed: boom"), error());
        assert_eq!(status("STALE Nu context"), warning());
        assert_eq!(status("Local state loaded."), Style::default());
    }

    #[test]
    fn pill_span_always_brackets_and_falls_back_cleanly_under_no_color() {
        let no_color_ok = pill_span_with("active", Tone::Ok, true);
        let no_color_err = pill_span_with("incompatible", Tone::Err, true);
        assert_eq!(no_color_ok.content.as_ref(), "[active]");
        assert_eq!(no_color_err.content.as_ref(), "[incompatible]");
        assert_eq!(no_color_ok.style.fg, None);
        assert_eq!(no_color_err.style.fg, None);
        assert!(no_color_err.style.add_modifier.contains(Modifier::BOLD));

        assert_eq!(status_tone("active"), Tone::Ok);
        assert_eq!(status_tone("provisional"), Tone::Warn);
        assert_eq!(status_tone("incompatible"), Tone::Err);
        assert_eq!(tier_tone("Destructive"), Tone::Err);
        assert_eq!(tier_tone("ReadOnly"), Tone::Ok);
    }

    #[test]
    fn ascii_border_set_uses_only_ascii_chars() {
        let ascii = border_set(true);
        assert_eq!(ascii.top_left, "+");
        assert_eq!(ascii.vertical_left, "|");
        assert_eq!(ascii.horizontal_top, "-");
        let rounded = border_set(false);
        assert_eq!(rounded.top_left, "╭");
    }
}
