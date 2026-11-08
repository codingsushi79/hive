//! The dashboard.
//!
//! A thin client: it holds no agent state of its own, it polls the daemon and
//! renders what comes back. Quitting only ends this process; every agent
//! carries on in the daemon.

mod form;
mod theme;
mod views;
mod widgets;

use anyhow::Result;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::{DefaultTerminal, Frame};

use crate::ipc::{Client, Request};
use crate::model::*;
use form::Form;

const REFRESH: Duration = Duration::from_millis(400);
const POLL: Duration = Duration::from_millis(100);
const TOAST_TTL: Duration = Duration::from_secs(5);
const DIFF_REFRESH: Duration = Duration::from_secs(3);
/// Diffs past this many lines are cut, so a vendored directory can't stall
/// the dashboard.
const DIFF_MAX_LINES: usize = 20_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Agents,
    Reviews,
    Comms,
    Diff,
    Logs,
}

impl Tab {
    const ALL: [Tab; 6] = [Tab::Dashboard, Tab::Agents, Tab::Reviews, Tab::Comms, Tab::Diff, Tab::Logs];

    fn title(self) -> &'static str {
        match self {
            Tab::Dashboard => "Dashboard",
            Tab::Agents => "Agents",
            Tab::Reviews => "Reviews",
            Tab::Comms => "Comms",
            Tab::Diff => "Diff",
            Tab::Logs => "Logs",
        }
    }

    fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
}

struct Confirm {
    prompt: String,
    request: Request,
}

#[derive(Default)]
pub struct Transcript {
    pub name: String,
    pub entries: Vec<Entry>,
    pub total: usize,
}

pub struct DiffCache {
    pub name: String,
    pub text: String,
    fetched: Instant,
}

pub struct App {
    client: Option<Client>,
    pub snapshot: Option<Snapshot>,
    connection_error: Option<String>,
    tab: Tab,
    /// Selection by name, so it survives agents appearing and reordering.
    selected: Option<String>,
    pub review_sel: usize,
    pub transcript: Transcript,
    pub transcript_scroll: usize,
    pub diff: Option<DiffCache>,
    pub diff_scroll: usize,
    pub log: String,
    pub log_scroll: usize,
    pub comms_scroll: usize,
    log_fetched: Option<Instant>,
    form: Option<Form>,
    confirm: Option<Confirm>,
    toast: Option<(String, bool, Instant)>,
    help: bool,
    pub tick: usize,
    last_refresh: Option<Instant>,
    quit: bool,
    open_editor: bool,
}

impl App {
    fn new() -> Self {
        Self {
            client: None,
            snapshot: None,
            connection_error: None,
            tab: Tab::Dashboard,
            selected: None,
            review_sel: 0,
            transcript: Transcript::default(),
            transcript_scroll: 0,
            diff: None,
            diff_scroll: 0,
            log: String::new(),
            log_scroll: 0,
            comms_scroll: 0,
            log_fetched: None,
            form: None,
            confirm: None,
            toast: None,
            help: false,
            tick: 0,
            last_refresh: None,
            quit: false,
            open_editor: false,
        }
    }

    // -------------------------------------------------------------- data ---

    fn client(&mut self) -> Option<&mut Client> {
        if self.client.as_ref().is_none_or(|c| c.is_broken()) {
            match Client::connect() {
                Ok(c) => {
                    self.client = Some(c);
                    self.connection_error = None;
                }
                Err(e) => {
                    self.client = None;
                    self.connection_error = Some(format!("{e:#}"));
                    return None;
                }
            }
        }
        self.client.as_mut()
    }

    fn refresh(&mut self) {
        let Some(client) = self.client() else {
            self.snapshot = None;
            return;
        };
        match client.snapshot() {
            Ok(snap) => {
                self.snapshot = Some(snap);
                self.connection_error = None;
            }
            Err(e) => {
                self.snapshot = None;
                self.connection_error = Some(format!("{e:#}"));
                return;
            }
        }
        self.fix_selection();
        self.refresh_transcript();
        if self.tab == Tab::Diff {
            self.refresh_diff(false);
        }
        if self.tab == Tab::Logs && self.log_fetched.is_none_or(|t| t.elapsed() > Duration::from_secs(1)) {
            self.log_fetched = Some(Instant::now());
            if let Some(c) = self.client()
                && let Ok(text) = c.command(&Request::Log { lines: 500 })
            {
                self.log = text;
            }
        }
    }

    fn refresh_transcript(&mut self) {
        if !matches!(self.tab, Tab::Agents) {
            return;
        }
        let Some(name) = self.selected.clone() else { return };
        if self.transcript.name != name {
            self.transcript = Transcript {
                name: name.clone(),
                ..Default::default()
            };
            self.transcript_scroll = 0;
        }
        let since = self.transcript.total;
        let Some(client) = self.client() else { return };
        if let Ok((entries, total)) = client.transcript(&name, since) {
            if total < since {
                // The agent was replaced by a namesake; start over.
                self.transcript = Transcript { name, ..Default::default() };
                return;
            }
            // Keep the reader's place while new lines arrive below.
            if self.transcript_scroll > 0 && !entries.is_empty() {
                self.transcript_scroll += entries.len() * 2;
            }
            self.transcript.entries.extend(entries);
            self.transcript.total = total;
            let excess = self.transcript.entries.len().saturating_sub(4000);
            self.transcript.entries.drain(..excess);
        }
    }

    fn refresh_diff(&mut self, force: bool) {
        let Some(name) = self.selected.clone() else { return };
        let fresh = self
            .diff
            .as_ref()
            .is_some_and(|d| d.name == name && d.fetched.elapsed() < DIFF_REFRESH);
        if fresh && !force {
            return;
        }
        if self.diff.as_ref().is_none_or(|d| d.name != name) {
            self.diff_scroll = 0;
        }
        let Some(client) = self.client() else { return };
        let text = match client.command(&Request::Diff { name: name.clone() }) {
            Ok(text) => {
                let lines = text.lines().count();
                if lines > DIFF_MAX_LINES {
                    let mut cut: String = text.lines().take(DIFF_MAX_LINES).collect::<Vec<_>>().join("\n");
                    cut.push_str(&format!("\n… {} more lines (see `hive diff {name}`)", lines - DIFF_MAX_LINES));
                    cut
                } else {
                    text
                }
            }
            Err(e) => format!("  {e:#}"),
        };
        self.diff = Some(DiffCache {
            name,
            text,
            fetched: Instant::now(),
        });
    }

    /// Agents as a tree in creation order: each followed by the helpers it
    /// spawned and the reviewers reviewing it, with their depth.
    pub fn agent_tree(&self) -> Vec<(&AgentInfo, usize)> {
        let Some(snap) = &self.snapshot else { return vec![] };
        let owner = |a: &AgentInfo| a.review_of.clone().or_else(|| a.parent.clone());
        let exists = |n: &str| snap.agents.iter().any(|a| a.name == n);
        let mut out = vec![];
        fn walk<'a>(
            snap: &'a Snapshot,
            name: &str,
            depth: usize,
            out: &mut Vec<(&'a AgentInfo, usize)>,
        ) {
            for child in snap.agents.iter().filter(|c| {
                c.review_of.as_deref() == Some(name)
                    || (c.review_of.is_none() && c.parent.as_deref() == Some(name))
            }) {
                if out.iter().any(|(o, _)| o.name == child.name) || depth > 16 {
                    continue;
                }
                out.push((child, depth));
                walk(snap, &child.name, depth + 1, out);
            }
        }
        for root in snap
            .agents
            .iter()
            .filter(|a| owner(a).is_none_or(|o| !exists(&o)))
        {
            out.push((root, 0));
            walk(snap, &root.name, 1, &mut out);
        }
        out
    }

    pub fn ordered_agents(&self) -> Vec<&AgentInfo> {
        self.agent_tree().into_iter().map(|(a, _)| a).collect()
    }

    pub fn selected_index(&self) -> Option<usize> {
        let name = self.selected.as_ref()?;
        self.ordered_agents().iter().position(|a| &a.name == name)
    }

    pub fn selected_agent(&self) -> Option<&AgentInfo> {
        let name = self.selected.as_ref()?;
        self.snapshot.as_ref()?.agents.iter().find(|a| &a.name == name)
    }

    fn fix_selection(&mut self) {
        if self.selected_index().is_some() {
            return;
        }
        self.selected = self.ordered_agents().first().map(|a| a.name.clone());
    }

    fn select(&mut self, name: &str) {
        self.selected = Some(name.to_string());
        self.transcript_scroll = 0;
    }

    fn move_selection(&mut self, delta: isize) {
        if self.tab == Tab::Reviews {
            let n = self.snapshot.as_ref().map(|s| s.reviews.len()).unwrap_or(0);
            if n > 0 {
                self.review_sel = (self.review_sel as isize + delta).clamp(0, n as isize - 1) as usize;
            }
            return;
        }
        let names: Vec<String> = self.ordered_agents().iter().map(|a| a.name.clone()).collect();
        if names.is_empty() {
            return;
        }
        let current = self.selected_index().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, names.len() as isize - 1) as usize;
        self.select(&names[next].clone());
        if self.tab == Tab::Agents {
            self.refresh_transcript();
        }
        if self.tab == Tab::Diff {
            self.refresh_diff(false);
        }
    }

    fn selected_review(&self) -> Option<&Review> {
        let snap = self.snapshot.as_ref()?;
        snap.reviews.iter().rev().nth(self.review_sel)
    }

    // ----------------------------------------------------------- actions ---

    fn request(&mut self, request: Request) -> Result<String, String> {
        let Some(client) = self.client() else {
            return Err("daemon unreachable".into());
        };
        client.command(&request).map_err(|e| format!("{e:#}"))
    }

    fn send(&mut self, request: Request) {
        let result = self.request(request);
        match result {
            Ok(m) => self.toast(m, false),
            Err(e) => self.toast(e, true),
        }
        self.last_refresh = None;
    }

    fn toast(&mut self, message: String, is_error: bool) {
        self.toast = Some((message, is_error, Instant::now()));
    }

    fn with_selected(&mut self, f: impl FnOnce(&mut Self, AgentInfo)) {
        match self.selected_agent().cloned() {
            Some(a) => f(self, a),
            None => self.toast("no agent selected — press n to start one".into(), true),
        }
    }

    fn scroll(&mut self, lines: isize) {
        // Positive scrolls toward older content / further down a diff.
        let bump = |v: &mut usize| *v = (*v as isize + lines).max(0) as usize;
        match self.tab {
            Tab::Agents => bump(&mut self.transcript_scroll),
            Tab::Logs => bump(&mut self.log_scroll),
            Tab::Comms => bump(&mut self.comms_scroll),
            Tab::Diff => {
                self.diff_scroll = (self.diff_scroll as isize - lines).max(0) as usize;
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.form.is_some() {
            self.form_key(key);
            return;
        }
        if self.help {
            self.help = false;
            return;
        }
        if let Some(confirm) = self.confirm.take() {
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                let shutdown = matches!(confirm.request, Request::Shutdown);
                self.send(confirm.request);
                if shutdown {
                    self.quit = true;
                }
            }
            return;
        }
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char(c @ '1'..='6') => {
                self.set_tab(Tab::ALL[c as usize - '1' as usize]);
            }
            KeyCode::Tab => self.set_tab(Tab::ALL[(self.tab.index() + 1) % Tab::ALL.len()]),
            KeyCode::BackTab => {
                self.set_tab(Tab::ALL[(self.tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len()])
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageUp | KeyCode::Char('K') => self.scroll(10),
            KeyCode::PageDown | KeyCode::Char('J') => self.scroll(-10),
            KeyCode::Char('u') if ctrl => self.scroll(10),
            KeyCode::Char('d') if ctrl => self.scroll(-10),
            KeyCode::Char('g') | KeyCode::Home => match self.tab {
                Tab::Agents => self.transcript_scroll = usize::MAX,
                Tab::Logs => self.log_scroll = usize::MAX,
                Tab::Diff => self.diff_scroll = 0,
                _ => {}
            },
            KeyCode::Char('G') | KeyCode::End => match self.tab {
                Tab::Agents => self.transcript_scroll = 0,
                Tab::Logs => self.log_scroll = 0,
                Tab::Comms => self.comms_scroll = 0,
                Tab::Diff => self.diff_scroll = usize::MAX,
                _ => {}
            },
            KeyCode::Char('n') => {
                let mode = self.default_mode();
                self.form = Some(Form::new_agent(&mode));
            }
            KeyCode::Char('F') => {
                let mode = self.default_mode();
                self.form = Some(Form::fanout(&mode));
            }
            KeyCode::Char('b') => self.form = Some(Form::broadcast()),
            KeyCode::Char('p') => self.form = Some(Form::post()),
            KeyCode::Enter | KeyCode::Char('i') if self.tab == Tab::Reviews => {
                // Jump to the reviewed agent.
                if let Some(target) = self.selected_review().map(|r| r.target.clone()) {
                    self.select(&target);
                    self.set_tab(Tab::Agents);
                }
            }
            KeyCode::Enter | KeyCode::Char('i') => {
                self.with_selected(|app, a| app.form = Some(Form::message(&a.name)))
            }
            KeyCode::Char('x') => self.with_selected(|app, a| app.send(Request::Stop { name: a.name })),
            KeyCode::Char('X') => {
                self.confirm = Some(Confirm {
                    prompt: "Stop every running and queued agent?".into(),
                    request: Request::StopAll,
                })
            }
            KeyCode::Char('R') => self.with_selected(|app, a| {
                let target = a.review_of.clone().unwrap_or(a.name);
                app.send(Request::Review { name: target });
            }),
            KeyCode::Char('f') => match self.selected_review().map(|r| r.id) {
                Some(id) if self.tab == Tab::Reviews => self.send(Request::SendFindings { id }),
                _ => self.toast("f works in the Reviews tab (3)".into(), true),
            },
            KeyCode::Char('m') => self.with_selected(|app, a| {
                let Some(branch) = a.branch.clone() else {
                    app.toast(format!("{} works in place; nothing to merge", a.name), true);
                    return;
                };
                let summary = if a.diffstat.is_empty() { "no changes".into() } else { a.diffstat.clone() };
                app.confirm = Some(Confirm {
                    prompt: format!("Merge {branch} ({summary}) into the repo's current branch?"),
                    request: Request::Merge { name: a.name },
                });
            }),
            KeyCode::Char('d') | KeyCode::Delete => self.with_selected(|app, a| {
                let what = match (&a.branch, a.merged) {
                    (Some(_), true) => "its worktree and merged branch",
                    (Some(_), false) => "its worktree (the unmerged branch is kept)",
                    (None, _) => "it (files in place are untouched)",
                };
                app.confirm = Some(Confirm {
                    prompt: format!("Remove {} and {what}?", a.name),
                    request: Request::Remove { name: a.name },
                });
            }),
            KeyCode::Char('D') => self.set_tab(Tab::Diff),
            KeyCode::Char('r') => {
                self.refresh_diff(true);
                self.last_refresh = None;
            }
            KeyCode::Char('+') | KeyCode::Char('=') => self.nudge_slots(1),
            KeyCode::Char('-') => self.nudge_slots(-1),
            KeyCode::Char('Q') => {
                self.confirm = Some(Confirm {
                    prompt: "Shut the daemon down? Every running agent stops.".into(),
                    request: Request::Shutdown,
                })
            }
            _ => {}
        }
    }

    fn default_mode(&self) -> String {
        self.snapshot
            .as_ref()
            .map(|s| s.daemon.default_mode.clone())
            .unwrap_or_else(|| "acceptEdits".into())
    }

    fn nudge_slots(&mut self, delta: isize) {
        let current = self.snapshot.as_ref().map(|s| s.daemon.max_concurrent).unwrap_or(4) as isize;
        self.send(Request::SetMaxConcurrent {
            n: (current + delta).max(1) as usize,
        });
    }

    fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        match tab {
            Tab::Agents => self.refresh_transcript(),
            Tab::Diff => self.refresh_diff(true),
            Tab::Logs => self.log_fetched = None,
            _ => {}
        }
        self.last_refresh = None;
    }

    fn form_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(form) = self.form.as_mut() else { return };
        match key.code {
            KeyCode::Esc => self.form = None,
            KeyCode::Char('c') if ctrl => self.form = None,
            KeyCode::Char('e') if ctrl => self.open_editor = true,
            KeyCode::Char('a') if ctrl => form.home(),
            KeyCode::Char('w') if ctrl => form.delete_word(),
            KeyCode::Char('u') if ctrl => {
                if let Some(f) = form.focused() {
                    f.set(String::new());
                }
            }
            KeyCode::Enter => {
                let request = match form.request() {
                    Ok(r) => r,
                    Err(e) => {
                        form.error = Some(e);
                        return;
                    }
                };
                let spawned = matches!(request, Request::Spawn { .. } | Request::Fanout { .. });
                match self.request(request) {
                    Ok(message) => {
                        self.form = None;
                        // Follow a new agent straight into its transcript.
                        if spawned
                            && let Some(name) = message.split_whitespace().next()
                        {
                            let name = name.trim_end_matches(':').to_string();
                            self.last_refresh = None;
                            self.refresh();
                            if self.snapshot.as_ref().is_some_and(|s| s.agents.iter().any(|a| a.name == name)) {
                                self.select(&name);
                                self.set_tab(Tab::Agents);
                            }
                        }
                        self.toast(message, false);
                    }
                    Err(e) => {
                        if let Some(form) = self.form.as_mut() {
                            form.error = Some(e);
                        }
                    }
                }
            }
            KeyCode::Tab | KeyCode::Down => form.next(),
            KeyCode::BackTab | KeyCode::Up => form.prev(),
            KeyCode::Left => form.left(),
            KeyCode::Right => form.right(),
            KeyCode::Home => form.home(),
            KeyCode::End => form.end(),
            KeyCode::Backspace => form.backspace(),
            KeyCode::Delete => form.delete(),
            KeyCode::Char(c) if !ctrl => form.insert(c),
            _ => {}
        }
    }

    fn tidy_up(&mut self) {
        if self
            .toast
            .as_ref()
            .is_some_and(|(_, _, at)| at.elapsed() > TOAST_TTL)
        {
            self.toast = None;
        }
    }
}

/// Edit the focused form field in `$EDITOR`, for briefs longer than a line.
fn edit_in_editor(terminal: &mut DefaultTerminal, app: &mut App) {
    app.open_editor = false;
    let Some(form) = app.form.as_mut() else { return };
    let Some(field) = form.focused() else { return };
    let path = std::env::temp_dir().join(format!("hive-{}.md", std::process::id()));
    if std::fs::write(&path, &field.value).is_err() {
        return;
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    ratatui::restore();
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&path)
        .status();
    *terminal = ratatui::init();
    let _ = terminal.clear();
    match status {
        Ok(s) if s.success() => {
            if let Ok(text) = std::fs::read_to_string(&path) {
                field.set(text.trim_end().to_string());
            }
        }
        _ => form.error = Some(format!("{editor} didn't exit cleanly; field unchanged")),
    }
    let _ = std::fs::remove_file(&path);
}

pub fn run(we_started_daemon: bool) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, we_started_daemon);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, we_started_daemon: bool) -> Result<()> {
    let mut app = App::new();
    loop {
        if app.last_refresh.is_none_or(|t| t.elapsed() >= REFRESH) {
            app.refresh();
            app.last_refresh = Some(Instant::now());
        }
        app.tidy_up();
        terminal.draw(|frame| draw(frame, &mut app))?;

        if event::poll(POLL)? {
            match event::read()? {
                Event::Key(key) => app.handle_key(key),
                Event::Resize(..) => {}
                _ => {}
            }
        }
        if app.open_editor {
            edit_in_editor(terminal, &mut app);
        }
        app.tick = app.tick.wrapping_add(1);
        if app.quit {
            break;
        }
    }
    // Don't leave an idle daemon behind that this session started for nothing.
    if we_started_daemon
        && let Some(snap) = &app.snapshot
        && snap.agents.is_empty()
        && let Some(c) = app.client()
    {
        let _ = c.command(&Request::Shutdown);
    }
    Ok(())
}

fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, app, header);
    match app.snapshot.is_some() {
        true => match app.tab {
            Tab::Dashboard => views::dashboard(frame, app, body),
            Tab::Agents => views::agents(frame, app, body),
            Tab::Reviews => views::reviews(frame, app, body),
            Tab::Comms => views::comms(frame, app, body),
            Tab::Diff => views::diff(frame, app, body),
            Tab::Logs => views::logs(frame, app, body),
        },
        false => draw_disconnected(frame, app, body),
    }
    draw_footer(frame, app, footer);

    if app.help {
        widgets::help_overlay(frame, frame.area());
    }
    if let Some(confirm) = &app.confirm {
        widgets::confirm_overlay(frame, frame.area(), &confirm.prompt);
    }
    if let Some(form) = &app.form {
        form.render(frame, frame.area());
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(Span::styled(" ⬡ hive ", theme::title()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [tabs_area, status_area] =
        Layout::horizontal([Constraint::Min(30), Constraint::Length(56)]).areas(inner);

    let titles: Vec<Line> = Tab::ALL
        .iter()
        .enumerate()
        .map(|(i, t)| {
            Line::from(vec![
                Span::styled(format!("{} ", i + 1), theme::muted()),
                Span::raw(t.title()),
            ])
        })
        .collect();
    frame.render_widget(
        Tabs::new(titles)
            .select(app.tab.index())
            .style(theme::base())
            .highlight_style(theme::accent())
            .divider(Span::styled("│", theme::border())),
        tabs_area,
    );

    let status = match (&app.snapshot, &app.connection_error) {
        (Some(snap), _) => {
            let t = &snap.totals;
            let live = t.running > 0;
            let spinner = if live { widgets::SPINNER[app.tick % widgets::SPINNER.len()] } else { "•" };
            Line::from(vec![
                Span::styled(
                    format!("{spinner} "),
                    Style::default().fg(if live { theme::GOOD } else { theme::MUTED }),
                ),
                Span::styled(format!("{}", t.running), theme::good()),
                Span::styled(" running ", theme::label()),
                Span::styled(format!("{}", t.queued), theme::accent()),
                Span::styled(" queued ", theme::label()),
                Span::styled(format!("{}", t.idle), theme::warn()),
                Span::styled(" idle  ", theme::label()),
                Span::styled(fmt_cost(t.cost_usd), theme::value()),
                Span::styled("  up ", theme::label()),
                Span::styled(fmt_duration(snap.daemon.uptime_secs), theme::value()),
            ])
        }
        (None, Some(_)) => Line::from(Span::styled("daemon unreachable", theme::bad())),
        (None, None) => Line::from(Span::styled("connecting…", theme::muted())),
    };
    frame.render_widget(Paragraph::new(status).right_aligned(), status_area);
}

fn draw_disconnected(frame: &mut Frame, app: &App, area: Rect) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled("  Can't reach the hive daemon.", theme::bad())),
        Line::from(""),
        Line::from(Span::styled(
            format!("  {}", app.connection_error.clone().unwrap_or_default()),
            theme::muted(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Retrying. Start it with `hive daemon start`, or see `hive daemon log`.",
            theme::base(),
        )),
    ];
    frame.render_widget(Paragraph::new(lines).block(widgets::panel("Disconnected")), area);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    if let Some((message, is_error, _)) = &app.toast {
        let style = if *is_error { theme::bad() } else { theme::good() };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(if *is_error { " ✗ " } else { " ✓ " }, style),
                Span::styled(message.clone(), style),
            ])),
            area,
        );
        return;
    }
    let keys: &[(&str, &str)] = match app.tab {
        Tab::Agents => &[
            ("↑↓", "select"),
            ("Enter", "message"),
            ("n", "new"),
            ("x", "stop"),
            ("R", "review"),
            ("m", "merge"),
            ("PgUp/Dn", "scroll"),
            ("?", "help"),
        ],
        Tab::Reviews => &[
            ("↑↓", "select"),
            ("f", "send findings to author"),
            ("Enter", "open author"),
            ("R", "review again"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Tab::Diff => &[
            ("↑↓", "agent"),
            ("PgUp/Dn", "scroll"),
            ("r", "refresh"),
            ("R", "review"),
            ("m", "merge"),
            ("?", "help"),
        ],
        Tab::Logs => &[("PgUp/Dn", "scroll"), ("G", "latest"), ("?", "help"), ("q", "quit")],
        Tab::Comms => &[
            ("p", "post to board"),
            ("Enter", "message selected agent"),
            ("b", "broadcast"),
            ("PgUp/Dn", "scroll"),
            ("?", "help"),
        ],
        Tab::Dashboard => &[
            ("↑↓", "select"),
            ("n", "new agent"),
            ("F", "fan out"),
            ("Enter", "message"),
            ("R", "review"),
            ("m", "merge"),
            ("±", "slots"),
            ("?", "help"),
            ("q", "quit (agents keep going)"),
        ],
    };
    let mut spans = vec![Span::raw(" ")];
    for (key, description) in keys {
        spans.push(Span::styled(*key, theme::accent()));
        spans.push(Span::styled(format!(" {description}   "), theme::muted()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn agent(name: &str, state: AgentState, role: Role) -> AgentInfo {
        AgentInfo {
            name: name.into(),
            role,
            state,
            prompt: "Add rate limiting to the login endpoint and cover it with tests".into(),
            dir: "/home/me/.hive/worktrees/app-1234/".to_string() + name,
            repo: Some("/home/me/app".into()),
            branch: (role == Role::Worker).then(|| format!("hive/{name}")),
            base_commit: Some("abc".into()),
            model: Some("opus".into()),
            mode: "acceptEdits".into(),
            session_id: Some("5f0c-…".into()),
            cost_usd: 0.42,
            turns: 7,
            tokens_in: 120_000,
            tokens_out: 3_400,
            created: now() - 300,
            updated: now(),
            activity: "Bash  cargo test --workspace".into(),
            diffstat: "3 files +48 -5".into(),
            pending: 0,
            review_of: (role == Role::Reviewer).then(|| "rate-limit".into()),
            rounds_left: 1,
            merged: false,
            error: None,
            entries: 5,
            parent: None,
            budget_usd: 0.0,
            tokens_cached: 90_000,
            auto_turns: 0,
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.snapshot = Some(Snapshot {
            agents: vec![
                agent("rate-limit", AgentState::Running, Role::Worker),
                agent("fix-flaky-test", AgentState::Idle, Role::Worker),
                agent("rate-limit-review", AgentState::Running, Role::Reviewer),
                agent("docs-pass", AgentState::Queued, Role::Worker),
                AgentInfo { parent: Some("rate-limit".into()), ..agent("limiter-tests", AgentState::Running, Role::Worker) },
            ],
            reviews: vec![Review {
                id: 1,
                target: "fix-flaky-test".into(),
                reviewer: "fix-flaky-test-review".into(),
                state: ReviewState::Done,
                verdict: Some("changes".into()),
                summary: "The retry masks a real race in the fixture teardown.".into(),
                findings: vec![Finding {
                    severity: "high".into(),
                    file: "tests/db.rs".into(),
                    line: Some(88),
                    summary: "Retrying hides the race instead of fixing the teardown order.".into(),
                }],
                created: now() - 60,
                sent_back: false,
                diff_hash: 0,
            }],
            notes: vec![
                Note { id: 1, ts: now() - 90, from: "rate-limit".into(), to: None, text: "owning src/routes/auth.rs".into() },
                Note { id: 2, ts: now() - 30, from: "docs-pass".into(), to: Some("user".into()), text: "Should the API docs cover v1 too?".into() },
            ],
            totals: Totals { agents: 4, running: 2, queued: 1, idle: 1, failed: 0, cost_usd: 1.68, tokens: 494_000, cache_ratio: 0.82 },
            daemon: DaemonInfo {
                pid: 1,
                uptime_secs: 3700,
                max_concurrent: 2,
                claude_bin: "claude".into(),
                default_mode: "acceptEdits".into(),
                default_model: String::new(),
            },
        });
        app.selected = Some("rate-limit".into());
        app.transcript = Transcript {
            name: "rate-limit".into(),
            total: 5,
            entries: vec![
                Entry::new(EntryKind::Prompt, "Add rate limiting to the login endpoint"),
                Entry::new(EntryKind::Text, "I'll look at how the login route is wired first."),
                Entry::new(EntryKind::Tool, "Grep  fn login"),
                Entry::new(EntryKind::ToolOk, "src/routes/auth.rs:41: pub async fn login(  (+2 lines)"),
                Entry::new(EntryKind::Tool, "Bash  cargo test --workspace"),
            ],
        };
        app.diff = Some(DiffCache {
            name: "rate-limit".into(),
            text: "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,3 @@\n fn a() {}\n-old\n+new\n+more".into(),
            fetched: Instant::now(),
        });
        app
    }

    fn render(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(130, 34)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn every_tab_renders() {
        let mut app = app();
        for tab in Tab::ALL {
            app.tab = tab;
            let screen = render(&mut app);
            if std::env::var("HIVE_PRINT").is_ok() {
                println!("{screen}");
            }
            assert!(screen.contains("hive"));
        }
        app.form = Some(Form::new_agent("acceptEdits"));
        let screen = render(&mut app);
        if std::env::var("HIVE_PRINT").is_ok() {
            println!("{screen}");
        }
        assert!(screen.contains("New agent"));
    }

    #[test]
    fn reviewers_follow_their_target() {
        let app = app();
        let order: Vec<&str> = app.ordered_agents().iter().map(|a| a.name.as_str()).collect();
        assert_eq!(order, ["rate-limit", "rate-limit-review", "limiter-tests", "fix-flaky-test", "docs-pass"]);
    }
}
