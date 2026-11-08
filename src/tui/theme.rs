//! One palette, used everywhere, so state reads the same on every screen.

use ratatui::style::{Color, Modifier, Style};

use crate::model::{AgentState, EntryKind};

pub const ACCENT: Color = Color::Rgb(122, 162, 247);
pub const GOOD: Color = Color::Rgb(158, 206, 106);
pub const WARN: Color = Color::Rgb(224, 175, 104);
pub const BAD: Color = Color::Rgb(247, 118, 142);
pub const MUTED: Color = Color::Rgb(105, 112, 152);
pub const TEXT: Color = Color::Rgb(192, 202, 245);
pub const BORDER: Color = Color::Rgb(59, 66, 97);
pub const SELECTED_BG: Color = Color::Rgb(40, 46, 71);
pub const HEADER: Color = Color::Rgb(187, 154, 247);
pub const CYAN: Color = Color::Rgb(125, 207, 255);

pub fn base() -> Style {
    Style::default().fg(TEXT)
}

pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

pub fn label() -> Style {
    Style::default().fg(MUTED)
}

pub fn value() -> Style {
    Style::default().fg(TEXT).add_modifier(Modifier::BOLD)
}

pub fn accent() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn good() -> Style {
    Style::default().fg(GOOD)
}

pub fn warn() -> Style {
    Style::default().fg(WARN)
}

pub fn bad() -> Style {
    Style::default().fg(BAD)
}

pub fn border() -> Style {
    Style::default().fg(BORDER)
}

pub fn focus_border() -> Style {
    Style::default().fg(ACCENT)
}

pub fn title() -> Style {
    Style::default().fg(HEADER).add_modifier(Modifier::BOLD)
}

pub fn selected() -> Style {
    Style::default()
        .bg(SELECTED_BG)
        .fg(TEXT)
        .add_modifier(Modifier::BOLD)
}

pub fn state_color(state: AgentState) -> Color {
    match state {
        AgentState::Running => GOOD,
        AgentState::Queued => ACCENT,
        AgentState::Idle => WARN,
        AgentState::Failed => BAD,
        AgentState::Stopped => MUTED,
    }
}

pub fn verdict_color(verdict: Option<&str>) -> Color {
    match verdict {
        Some("approve") => GOOD,
        Some("changes") => WARN,
        Some(_) => MUTED,
        None => ACCENT,
    }
}

pub fn severity_color(severity: &str) -> Color {
    match severity.to_ascii_lowercase().as_str() {
        "high" | "critical" => BAD,
        "medium" => WARN,
        _ => MUTED,
    }
}

/// Gutter glyph and colour for a transcript entry.
pub fn entry_style(kind: EntryKind) -> (&'static str, Style) {
    match kind {
        EntryKind::Prompt => ("»", Style::default().fg(HEADER).add_modifier(Modifier::BOLD)),
        EntryKind::Text => ("◆", base()),
        EntryKind::Tool => ("⚙", Style::default().fg(CYAN)),
        EntryKind::ToolOk => ("↳", muted()),
        EntryKind::ToolErr => ("✗", bad()),
        EntryKind::Result => ("✓", good()),
        EntryKind::Info => ("·", muted()),
        EntryKind::Error => ("!", bad()),
    }
}
