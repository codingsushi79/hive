//! Small shared building blocks: panels, wrapping, scrolling, overlays.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthChar;

use super::theme;

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn panel(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(Span::styled(format!(" {} ", title.into()), theme::title()))
}

/// A panel whose border lights up when it has the keyboard.
pub fn focus_panel(title: impl Into<String>, focused: bool) -> Block<'static> {
    panel(title).border_style(if focused {
        theme::focus_border()
    } else {
        theme::border()
    })
}

/// `label   value` on one line, label muted and value bold.
pub fn kv(label: &str, value: impl Into<String>, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<10}"), theme::label()),
        Span::styled(value.into(), style),
    ])
}

/// Hard-wrap `text` to `width` display columns, keeping its own line breaks
/// and preferring to break at spaces.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(4);
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.replace('\t', "    ");
        if raw.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        let mut cols = 0;
        let mut last_space: Option<(usize, usize)> = None; // (byte idx, cols before)
        for c in raw.chars() {
            let w = c.width().unwrap_or(0);
            if cols + w > width && c == ' ' {
                // Overflowing on a space: break right here and drop it.
                out.push(std::mem::take(&mut line));
                cols = 0;
                last_space = None;
                continue;
            }
            if cols + w > width {
                match last_space {
                    Some((at, _)) if at > 0 => {
                        let rest = line[at + 1..].to_string();
                        line.truncate(at);
                        out.push(std::mem::take(&mut line));
                        cols = rest.chars().map(|c| c.width().unwrap_or(0)).sum();
                        line = rest;
                    }
                    _ => {
                        out.push(std::mem::take(&mut line));
                        cols = 0;
                    }
                }
                last_space = None;
            }
            if c == ' ' {
                last_space = Some((line.len(), cols));
            }
            line.push(c);
            cols += w;
        }
        out.push(line);
    }
    out
}

/// Cut a string to `width` display columns.
pub fn clip(text: &str, width: usize) -> String {
    let mut cols = 0;
    let mut out = String::new();
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if cols + w > width {
            if !out.is_empty() {
                out.pop();
                out.push('…');
            }
            return out;
        }
        cols += w;
        out.push(c);
    }
    out
}

/// The visible window of `lines` for a view scrolled `from_bottom` lines up
/// from the end. Clamps the offset in place so scrolling can't run away.
pub fn window_from_bottom<T: Clone>(lines: &[T], height: usize, from_bottom: &mut usize) -> Vec<T> {
    let max = lines.len().saturating_sub(height);
    *from_bottom = (*from_bottom).min(max);
    let end = lines.len() - *from_bottom;
    lines[end.saturating_sub(height)..end].to_vec()
}

pub fn window_from_top<T: Clone>(lines: &[T], height: usize, from_top: &mut usize) -> Vec<T> {
    let max = lines.len().saturating_sub(height);
    *from_top = (*from_top).min(max);
    lines[*from_top..(*from_top + height).min(lines.len())].to_vec()
}

pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [cell] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(row);
    cell
}

/// A friendly empty state, centred in the area.
pub fn empty(frame: &mut Frame, area: Rect, title: &str, lines: &[(&str, &str)]) {
    let mut text = vec![Line::from(Span::styled(title.to_string(), theme::title())), Line::from("")];
    for (key, what) in lines {
        text.push(Line::from(vec![
            Span::styled(format!("{key:>6}  "), theme::accent()),
            Span::styled(what.to_string(), theme::base()),
        ]));
    }
    let h = text.len() as u16;
    let target = centered(area, 64, h);
    frame.render_widget(Paragraph::new(text), target);
}

pub const HELP: &[(&str, &str)] = &[
    ("1-6 / Tab", "switch view"),
    ("↑ ↓ / j k", "select agent (or review, in Reviews)"),
    ("n", "new agent on a task"),
    ("F", "fan out: one task to several agents at once"),
    ("Enter / i", "message the selected agent (continues its session)"),
    ("b", "broadcast a message to every worker"),
    ("p", "post a note on the shared board (wakes nobody)"),
    ("x / X", "stop the selected agent / stop all"),
    ("R", "code-review the selected agent's changes"),
    ("f", "in Reviews: hand the findings back to the author"),
    ("m", "merge the agent's branch into the repo's current branch"),
    ("d", "remove the agent and its worktree"),
    ("PgUp PgDn / J K", "scroll transcript, diff or log"),
    ("g / G", "jump to top / follow the end"),
    ("+ / -", "allow more / fewer agents at once"),
    ("Ctrl-E", "in a form: edit the field in $EDITOR"),
    ("Q", "shut the daemon down (stops every agent)"),
    ("q / Esc / Ctrl-C", "close the dashboard, agents keep working"),
];

pub fn help_overlay(frame: &mut Frame, area: Rect) {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Closing this dashboard doesn't stop anything. Agents run in a",
            theme::muted(),
        )),
        Line::from(Span::styled(
            "  background daemon; reopen with `hive` any time.",
            theme::muted(),
        )),
        Line::from(""),
    ];
    for (key, description) in HELP {
        lines.push(Line::from(vec![
            Span::styled(format!("  {key:<18}"), theme::accent()),
            Span::styled(*description, theme::base()),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  press any key to close", theme::muted())));
    let height = lines.len() as u16 + 2;
    let target = centered(area, 80, height);
    frame.render_widget(Clear, target);
    frame.render_widget(Paragraph::new(lines).block(panel("Keys")), target);
}

pub fn confirm_overlay(frame: &mut Frame, area: Rect, prompt: &str) {
    let target = centered(area, 64, 8);
    frame.render_widget(Clear, target);
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(format!("  {prompt}"), theme::base())),
        Line::from(""),
        Line::from(vec![
            Span::styled("  y", theme::accent()),
            Span::styled(" confirm    ", theme::muted()),
            Span::styled("any other key", theme::accent()),
            Span::styled(" cancel", theme::muted()),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::WARN))
                .title(Span::styled(" Confirm ", theme::title())),
        ),
        target,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_spaces_and_hard_breaks_long_words() {
        assert_eq!(wrap("hello world foo", 11), vec!["hello world", "foo"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("a\n\nb", 10), vec!["a", "", "b"]);
    }

    #[test]
    fn windows_clamp() {
        let lines: Vec<u32> = (0..10).collect();
        let mut off = 100;
        assert_eq!(window_from_bottom(&lines, 3, &mut off), vec![0, 1, 2]);
        assert_eq!(off, 7);
        let mut off = 0;
        assert_eq!(window_from_bottom(&lines, 3, &mut off), vec![7, 8, 9]);
        let mut top = 50;
        assert_eq!(window_from_top(&lines, 4, &mut top), vec![6, 7, 8, 9]);
    }

    #[test]
    fn clip_marks_truncation() {
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("abc", 4), "abc");
    }
}
