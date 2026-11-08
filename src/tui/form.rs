//! Modal forms: start an agent, fan out, message one agent or all of them.
//!
//! Forms only collect text. The daemon validates, and whatever it rejects
//! comes back into the still-open form so nothing has to be retyped.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::theme;
use super::widgets::{centered, panel};
use crate::ipc::Request;
use crate::model::SpawnSpec;

#[derive(Clone, PartialEq, Eq)]
pub enum FormKind {
    NewAgent,
    Fanout,
    Message(String),
    Broadcast,
    Post,
}

pub struct Field {
    pub label: &'static str,
    pub value: String,
    /// Byte offset of the insertion point, always on a char boundary.
    cursor: usize,
    pub hint: &'static str,
    pub required: bool,
    /// Yes/no, flipped with space.
    pub toggle: bool,
}

impl Field {
    fn text(label: &'static str, hint: &'static str, default: &str, required: bool) -> Self {
        Self {
            label,
            value: default.to_string(),
            cursor: default.len(),
            hint,
            required,
            toggle: false,
        }
    }

    fn toggle(label: &'static str, hint: &'static str, on: bool) -> Self {
        Self {
            toggle: true,
            ..Self::text(label, hint, if on { "yes" } else { "no" }, false)
        }
    }

    fn on(&self) -> bool {
        self.value == "yes"
    }

    pub fn set(&mut self, value: String) {
        self.cursor = value.len();
        self.value = value;
    }
}

pub struct Form {
    pub kind: FormKind,
    pub title: String,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub error: Option<String>,
}

fn cwd() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".into())
}

impl Form {
    pub fn new_agent(default_mode: &str) -> Self {
        Self {
            kind: FormKind::NewAgent,
            title: "New agent".into(),
            fields: vec![
                Field::text("task", "what it should do — @file reads a brief, Ctrl-E opens $EDITOR", "", true),
                Field::text("name", "blank: named from the task", "", false),
                Field::text("dir", "project directory", &cwd(), true),
                Field::toggle("worktree", "isolate on branch hive/<name> (space toggles)", true),
                Field::text("model", "blank: CLI default — e.g. opus, sonnet, haiku", "", false),
                Field::text("mode", "acceptEdits · bypassPermissions · plan · auto", default_mode, false),
                Field::text("review", "auto-review rounds after each turn (0 = off)", "0", false),
                Field::text("budget", "spend cap in $ — blank: config default", "", false),
            ],
            focus: 0,
            error: None,
        }
    }

    pub fn fanout(default_mode: &str) -> Self {
        Self {
            kind: FormKind::Fanout,
            title: "Fan out — one task, several agents".into(),
            fields: vec![
                Field::text("task", "every agent gets the same brief, each in its own worktree", "", true),
                Field::text("count", "how many agents", "3", true),
                Field::text("name", "prefix — blank: from the task", "", false),
                Field::text("dir", "project directory (must be a git repo)", &cwd(), true),
                Field::text("model", "blank: CLI default", "", false),
                Field::text("mode", "permission mode", default_mode, false),
                Field::text("review", "auto-review rounds (0 = off)", "0", false),
                Field::text("budget", "spend cap in $ per agent — blank: config default", "", false),
            ],
            focus: 0,
            error: None,
        }
    }

    pub fn message(agent: &str) -> Self {
        Self {
            kind: FormKind::Message(agent.to_string()),
            title: format!("Message {agent}"),
            fields: vec![Field::text(
                "message",
                "continues its session; queued if it's busy",
                "",
                true,
            )],
            focus: 0,
            error: None,
        }
    }

    pub fn broadcast() -> Self {
        Self {
            kind: FormKind::Broadcast,
            title: "Broadcast to every worker".into(),
            fields: vec![Field::text("message", "sent to each unmerged worker", "", true)],
            focus: 0,
            error: None,
        }
    }

    pub fn post() -> Self {
        Self {
            kind: FormKind::Post,
            title: "Post to the board".into(),
            fields: vec![Field::text(
                "note",
                "every agent can read it with `hive board`; nobody is woken up",
                "",
                true,
            )],
            focus: 0,
            error: None,
        }
    }

    fn value(&self, label: &str) -> String {
        self.fields
            .iter()
            .find(|f| f.label == label)
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default()
    }

    fn optional(&self, label: &str) -> Option<String> {
        Some(self.value(label)).filter(|v| !v.is_empty())
    }

    fn number(&self, label: &str) -> Result<u32, String> {
        let v = self.value(label);
        if v.is_empty() {
            return Ok(0);
        }
        v.parse().map_err(|_| format!("{label} must be a number"))
    }

    fn budget(&self) -> Result<Option<f64>, String> {
        let v = self.value("budget");
        let v = v.trim_start_matches('$');
        if v.is_empty() {
            return Ok(None);
        }
        v.parse::<f64>()
            .map(Some)
            .map_err(|_| "budget must be a dollar amount, e.g. 2.50".into())
    }

    // ------------------------------------------------------------- input ---

    pub fn focused(&mut self) -> Option<&mut Field> {
        self.fields.get_mut(self.focus)
    }

    pub fn insert(&mut self, c: char) {
        self.error = None;
        if let Some(f) = self.fields.get_mut(self.focus) {
            if f.toggle {
                match c {
                    ' ' => f.set(if f.on() { "no" } else { "yes" }.into()),
                    'y' | 'Y' => f.set("yes".into()),
                    'n' | 'N' => f.set("no".into()),
                    _ => {}
                }
                return;
            }
            f.value.insert(f.cursor, c);
            f.cursor += c.len_utf8();
        }
    }

    pub fn backspace(&mut self) {
        if let Some(f) = self.fields.get_mut(self.focus)
            && !f.toggle
            && let Some(c) = f.value[..f.cursor].chars().next_back()
        {
            f.cursor -= c.len_utf8();
            f.value.remove(f.cursor);
        }
    }

    pub fn delete(&mut self) {
        if let Some(f) = self.fields.get_mut(self.focus)
            && !f.toggle
            && f.cursor < f.value.len()
        {
            f.value.remove(f.cursor);
        }
    }

    pub fn left(&mut self) {
        if let Some(f) = self.fields.get_mut(self.focus)
            && let Some(c) = f.value[..f.cursor].chars().next_back()
        {
            f.cursor -= c.len_utf8();
        }
    }

    pub fn right(&mut self) {
        if let Some(f) = self.fields.get_mut(self.focus)
            && let Some(c) = f.value[f.cursor..].chars().next()
        {
            f.cursor += c.len_utf8();
        }
    }

    pub fn home(&mut self) {
        if let Some(f) = self.fields.get_mut(self.focus) {
            f.cursor = 0;
        }
    }

    pub fn end(&mut self) {
        if let Some(f) = self.fields.get_mut(self.focus) {
            f.cursor = f.value.len();
        }
    }

    /// Delete the word before the cursor (Ctrl-W).
    pub fn delete_word(&mut self) {
        if let Some(f) = self.fields.get_mut(self.focus) {
            if f.toggle {
                return;
            }
            let before = &f.value[..f.cursor];
            let trimmed = before.trim_end();
            let start = trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
            f.value.replace_range(start..f.cursor, "");
            f.cursor = start;
        }
    }

    pub fn next(&mut self) {
        self.focus = (self.focus + 1) % self.fields.len();
    }

    pub fn prev(&mut self) {
        self.focus = (self.focus + self.fields.len() - 1) % self.fields.len();
    }

    // ------------------------------------------------------------ submit ---

    /// The request this form describes, or what's missing.
    pub fn request(&self) -> Result<Request, String> {
        if let Some(f) = self
            .fields
            .iter()
            .find(|f| f.required && f.value.trim().is_empty())
        {
            return Err(format!("{} is required", f.label));
        }
        let task = || crate::cli::read_prompt(&self.value("task")).map_err(|e| e.to_string());
        let message = || crate::cli::read_prompt(&self.value("message")).map_err(|e| e.to_string());
        Ok(match &self.kind {
            FormKind::NewAgent => Request::Spawn {
                spec: SpawnSpec {
                    name: self.value("name"),
                    prompt: task()?,
                    dir: self.value("dir"),
                    model: self.optional("model"),
                    mode: self.optional("mode"),
                    worktree: self.fields.iter().any(|f| f.label == "worktree" && f.on()),
                    review_rounds: self.number("review")?,
                    parent: None,
                    budget_usd: self.budget()?,
                },
            },
            FormKind::Fanout => {
                let count = self.number("count")? as usize;
                Request::Fanout {
                    spec: SpawnSpec {
                        name: self.value("name"),
                        prompt: task()?,
                        dir: self.value("dir"),
                        model: self.optional("model"),
                        mode: self.optional("mode"),
                        worktree: true,
                        review_rounds: self.number("review")?,
                        parent: None,
                        budget_usd: self.budget()?,
                    },
                    count,
                }
            }
            FormKind::Message(name) => Request::Send {
                name: name.clone(),
                message: message()?,
            },
            FormKind::Broadcast => Request::Broadcast { message: message()? },
            FormKind::Post => Request::Post {
                from: String::new(),
                text: self.value("note"),
            },
        })
    }

    // ------------------------------------------------------------ render ---

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let width = area.width.saturating_sub(8).clamp(40, 96);
        let height = (self.fields.len() * 2 + 6) as u16;
        let target = centered(area, width, height);
        frame.render_widget(Clear, target);

        let value_width = (width as usize).saturating_sub(4 + 11 + 1);
        let mut lines = vec![Line::from("")];
        for (i, field) in self.fields.iter().enumerate() {
            let focused = i == self.focus;
            let label_style = if focused { theme::accent() } else { theme::label() };
            let mark = if field.required { "*" } else { " " };
            let mut spans = vec![
                Span::styled(if focused { "▸" } else { " " }, theme::accent()),
                Span::styled(format!("{:<9}{mark} ", field.label), label_style),
            ];
            spans.extend(value_spans(field, focused, value_width));
            lines.push(Line::from(spans));
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(12)),
                Span::styled(
                    if focused { field.hint } else { "" },
                    theme::muted().add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(match &self.error {
            Some(e) => Line::from(Span::styled(format!(" ✗ {e}"), theme::bad())),
            None => Line::from(vec![
                Span::styled(" Enter", theme::accent()),
                Span::styled(" submit  ", theme::muted()),
                Span::styled("Tab/↑↓", theme::accent()),
                Span::styled(" field  ", theme::muted()),
                Span::styled("Ctrl-E", theme::accent()),
                Span::styled(" editor  ", theme::muted()),
                Span::styled("Esc", theme::accent()),
                Span::styled(" cancel", theme::muted()),
            ]),
        });
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(panel(&self.title).border_style(theme::focus_border())),
            target,
        );
    }
}

/// The field's value, scrolled so the cursor stays visible. Newlines show
/// as ⏎ so a multi-line brief still fits on one row.
fn value_spans(field: &Field, focused: bool, width: usize) -> Vec<Span<'static>> {
    let shown = |s: &str| s.replace('\n', "⏎");
    if field.toggle {
        let style = if field.on() { theme::good() } else { theme::muted() };
        return vec![Span::styled(
            format!("[{}] {}", if field.on() { "x" } else { " " }, field.value),
            if focused { style.add_modifier(Modifier::BOLD) } else { style },
        )];
    }
    let before = shown(&field.value[..field.cursor]);
    let after = shown(&field.value[field.cursor..]);
    if !focused {
        let all = shown(&field.value);
        return vec![Span::styled(super::widgets::clip(&all, width), theme::base())];
    }
    // Keep the tail of `before` that fits, leaving room for the cursor.
    let room = width.saturating_sub(2);
    let mut before_shown = before.clone();
    while before_shown.width() > room {
        before_shown.remove(0);
    }
    let cursor_char: String = after.chars().next().map(String::from).unwrap_or(" ".into());
    let rest: String = after.chars().skip(1).collect();
    let left = width.saturating_sub(before_shown.width() + 1);
    vec![
        Span::styled(before_shown, theme::value()),
        Span::styled(cursor_char, Style::default().bg(theme::ACCENT).fg(theme::SELECTED_BG)),
        Span::styled(super::widgets::clip(&rest, left), theme::value()),
    ]
}
