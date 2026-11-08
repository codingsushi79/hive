//! One function per tab. Each only reads `App`; input lives in `mod.rs`.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState, Wrap};

use super::widgets::{self, clip, focus_panel, kv, panel, wrap};
use super::{App, theme};
use crate::model::*;

fn state_cell(app: &App, a: &AgentInfo) -> Span<'static> {
    let glyph = match a.state {
        AgentState::Running => widgets::SPINNER[app.tick % widgets::SPINNER.len()],
        AgentState::Queued => "◷",
        AgentState::Idle => "●",
        AgentState::Failed => "✗",
        AgentState::Stopped => "■",
    };
    Span::styled(glyph.to_string(), Style::default().fg(theme::state_color(a.state)))
}

fn display_name(a: &AgentInfo, depth: usize) -> String {
    if depth == 0 {
        a.name.clone()
    } else {
        format!("{}└ {}", "  ".repeat(depth - 1), a.name)
    }
}

fn changes(a: &AgentInfo) -> (String, Style) {
    if a.role == Role::Reviewer {
        ("review".into(), theme::muted())
    } else if a.merged {
        ("merged".into(), theme::good())
    } else if a.diffstat.is_empty() {
        ("—".into(), theme::muted())
    } else {
        (a.diffstat.clone(), theme::base())
    }
}

fn no_agents(frame: &mut Frame, area: Rect) {
    widgets::empty(
        frame,
        area,
        "No agents yet",
        &[
            ("n", "start an agent on a task"),
            ("F", "fan one task out to several agents"),
            ("?", "all keys"),
        ],
    );
}

// ------------------------------------------------------------- dashboard ---

pub fn dashboard(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(snap) = app.snapshot.clone() else { return };
    let [stats, table, bottom] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(12),
    ])
    .areas(area);

    let t = &snap.totals;
    let stat = |label: &str, n: String, style: Style| {
        vec![
            Span::styled(format!(" {label} "), theme::label()),
            Span::styled(n, style),
            Span::raw("  "),
        ]
    };
    let mut spans = vec![];
    spans.extend(stat("running", t.running.to_string(), theme::good().add_modifier(Modifier::BOLD)));
    spans.extend(stat("queued", t.queued.to_string(), theme::accent()));
    spans.extend(stat("idle", t.idle.to_string(), theme::warn()));
    spans.extend(stat(
        "failed",
        t.failed.to_string(),
        if t.failed > 0 { theme::bad() } else { theme::muted() },
    ));
    spans.extend(stat("spent", fmt_cost(t.cost_usd), theme::value()));
    spans.extend(stat("tokens", fmt_tokens(t.tokens), theme::value()));
    spans.extend(stat(
        "cached",
        format!("{:.0}%", t.cache_ratio * 100.0),
        if t.cache_ratio >= 0.6 || t.tokens == 0 { theme::good() } else { theme::warn() },
    ));
    spans.extend(stat(
        "slots",
        format!("{}/{}", t.running, snap.daemon.max_concurrent),
        theme::value(),
    ));
    let pending_reviews = snap
        .reviews
        .iter()
        .filter(|r| r.state == ReviewState::Done && r.verdict.as_deref() == Some("changes") && !r.sent_back)
        .count();
    let for_you = snap.notes.iter().filter(|n| n.to.as_deref() == Some("user")).count();
    if for_you > 0 {
        spans.extend(stat("for you", for_you.to_string(), theme::warn()));
    }
    if pending_reviews > 0 {
        spans.extend(stat("to act on", pending_reviews.to_string(), theme::warn()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).block(panel("Fleet")), stats);

    if snap.agents.is_empty() {
        let block = panel("Agents");
        let inner = block.inner(table.union(bottom));
        frame.render_widget(block, table.union(bottom));
        no_agents(frame, inner);
        return;
    }

    agent_table(frame, app, table, true);

    let [detail, reviews] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(bottom);
    agent_detail(frame, app, detail);
    review_summary(frame, app, &snap, reviews);
}

fn agent_table(frame: &mut Frame, app: &App, area: Rect, wide: bool) {
    let ordered = app.agent_tree();
    let rows: Vec<Row> = ordered
        .iter()
        .map(|(a, depth)| {
            let (a, depth) = (*a, *depth);
            let (chg, chg_style) = changes(a);
            let mut cells = vec![
                Cell::from(state_cell(app, a)),
                Cell::from(Span::styled(display_name(a, depth), theme::value())),
                Cell::from(Span::styled(
                    a.state.label(),
                    Style::default().fg(theme::state_color(a.state)),
                )),
            ];
            if wide {
                cells.push(Cell::from(Span::styled(chg, chg_style)));
                cells.push(Cell::from(Span::styled(fmt_cost(a.cost_usd), theme::base())));
                cells.push(Cell::from(Span::styled(fmt_age(a.created), theme::muted())));
                let activity = if a.pending > 0 {
                    format!("[{} queued] {}", a.pending, a.activity)
                } else if let Some(e) = &a.error {
                    e.clone()
                } else {
                    a.activity.clone()
                };
                cells.push(Cell::from(Span::styled(
                    activity,
                    if a.error.is_some() { theme::bad() } else { theme::muted() },
                )));
            }
            Row::new(cells)
        })
        .collect();
    let widths: Vec<Constraint> = if wide {
        vec![
            Constraint::Length(1),
            Constraint::Length(24),
            Constraint::Length(8),
            Constraint::Length(16),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Min(10),
        ]
    } else {
        vec![Constraint::Length(1), Constraint::Min(10), Constraint::Length(8)]
    };
    let header = if wide {
        Row::new(["", "AGENT", "STATE", "CHANGES", "COST", "AGE", "DOING"]).style(theme::label())
    } else {
        Row::new(["", "AGENT", "STATE"]).style(theme::label())
    };
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(theme::selected())
        .block(focus_panel(format!("Agents ({})", ordered.len()), true));
    let mut state = TableState::default().with_selected(app.selected_index());
    frame.render_stateful_widget(table, area, &mut state);
}

fn agent_detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(a) = app.selected_agent() else {
        frame.render_widget(panel("Selected"), area);
        return;
    };
    let width = area.width.saturating_sub(12) as usize;
    let mut lines = vec![
        kv(
            "task",
            clip(&a.prompt.replace('\n', " "), width),
            theme::base(),
        ),
        kv("dir", clip(&a.dir, width), theme::muted()),
    ];
    if let Some(branch) = &a.branch {
        lines.push(kv(
            "branch",
            format!("{branch}{}", if a.merged { "  (merged)" } else { "" }),
            theme::accent(),
        ));
    }
    lines.push(kv(
        "model",
        format!(
            "{}   mode {}",
            a.model.clone().unwrap_or_else(|| "default".into()),
            a.mode
        ),
        theme::base(),
    ));
    lines.push(kv(
        "usage",
        format!(
            "{} · {} turns · {} in / {} out",
            fmt_cost(a.cost_usd),
            a.turns,
            fmt_tokens(a.tokens_in),
            fmt_tokens(a.tokens_out)
        ),
        theme::base(),
    ));
    if a.rounds_left > 0 {
        lines.push(kv("auto-rev", format!("{} round(s) left", a.rounds_left), theme::warn()));
    }
    if let Some(e) = &a.error {
        lines.push(kv("error", clip(e, width), theme::bad()));
    }
    lines.push(kv(
        "session",
        a.session_id.clone().unwrap_or_else(|| "—".into()),
        theme::muted(),
    ));
    frame.render_widget(
        Paragraph::new(lines).block(panel(format!("Selected · {}", a.name))),
        area,
    );
}

fn review_summary(frame: &mut Frame, _app: &App, snap: &Snapshot, area: Rect) {
    let height = area.height.saturating_sub(2) as usize;
    let width = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = snap
        .reviews
        .iter()
        .rev()
        .take(height)
        .map(|r| {
            let (verdict, color) = verdict_label(r);
            let count = if r.findings.is_empty() {
                String::new()
            } else {
                format!(" {}✱", r.findings.len())
            };
            Line::from(vec![
                Span::styled(format!("#{:<3}", r.id), theme::muted()),
                Span::styled(format!("{:<9}", verdict), Style::default().fg(color)),
                Span::styled(clip(&r.target, 18), theme::value()),
                Span::styled(count, theme::warn()),
                Span::styled(
                    format!("  {}", clip(&r.summary, width.saturating_sub(36))),
                    theme::muted(),
                ),
            ])
        })
        .collect();
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " select an agent and press R to review its changes",
            theme::muted(),
        )));
    }
    frame.render_widget(Paragraph::new(lines).block(panel("Reviews")), area);
}

fn verdict_label(r: &Review) -> (String, ratatui::style::Color) {
    match r.state {
        ReviewState::Running => ("reviewing".into(), theme::ACCENT),
        ReviewState::Failed => ("failed".into(), theme::BAD),
        ReviewState::Done => {
            let v = r.verdict.clone().unwrap_or_default();
            let c = theme::verdict_color(r.verdict.as_deref());
            (v, c)
        }
    }
}

// ---------------------------------------------------------------- agents ---

pub fn agents(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.snapshot.as_ref().is_none_or(|s| s.agents.is_empty()) {
        let block = panel("Agents");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        no_agents(frame, inner);
        return;
    }
    let [list, right] =
        Layout::horizontal([Constraint::Length(38), Constraint::Min(30)]).areas(area);
    agent_table(frame, app, list, false);

    let Some(a) = app.selected_agent().cloned() else { return };
    let [head, body] = Layout::vertical([Constraint::Length(4), Constraint::Min(3)]).areas(right);

    let (chg, chg_style) = changes(&a);
    let head_lines = vec![
        Line::from(vec![
            state_cell(app, &a),
            Span::raw(" "),
            Span::styled(a.state.label(), Style::default().fg(theme::state_color(a.state))),
            Span::styled("  ", theme::muted()),
            Span::styled(
                a.branch.clone().unwrap_or_else(|| "in place".into()),
                theme::accent(),
            ),
            Span::styled("  ", theme::muted()),
            Span::styled(chg, chg_style),
            Span::styled(format!("  {}", fmt_cost(a.cost_usd)), theme::base()),
            Span::styled(format!("  {} turns", a.turns), theme::muted()),
            Span::styled(
                if a.pending > 0 { format!("  {} queued msg", a.pending) } else { String::new() },
                theme::warn(),
            ),
        ]),
        Line::from(Span::styled(
            clip(&a.prompt.replace('\n', " "), right.width.saturating_sub(2) as usize),
            theme::muted(),
        )),
    ];
    frame.render_widget(Paragraph::new(head_lines).block(panel(a.name.clone())), head);

    let block = focus_panel(
        if app.transcript_scroll > 0 {
            format!("Transcript  ↑{}", app.transcript_scroll)
        } else {
            "Transcript".to_string()
        },
        false,
    );
    let inner = block.inner(body);
    frame.render_widget(block, body);
    let lines = transcript_lines(app, inner.width as usize);
    if lines.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(" waiting for output…", theme::muted())),
            inner,
        );
        return;
    }
    let visible = widgets::window_from_bottom(&lines, inner.height as usize, &mut app.transcript_scroll);
    frame.render_widget(Paragraph::new(visible), inner);
}

fn transcript_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let mut out = vec![];
    let text_width = width.saturating_sub(3);
    for (i, e) in app.transcript.entries.iter().enumerate() {
        let (glyph, style) = theme::entry_style(e.kind);
        if e.kind == EntryKind::Prompt && i > 0 {
            out.push(Line::from(""));
        }
        // Tool output and long prompts are summaries; keep them short.
        let text = match e.kind {
            EntryKind::ToolOk | EntryKind::ToolErr | EntryKind::Tool => clip(&e.text, text_width),
            EntryKind::Prompt if e.text.lines().count() > 8 => {
                let head: Vec<&str> = e.text.lines().take(6).collect();
                format!("{}\n… ({} more lines)", head.join("\n"), e.text.lines().count() - 6)
            }
            _ => e.text.clone(),
        };
        let indent = if matches!(e.kind, EntryKind::ToolOk | EntryKind::ToolErr) { "  " } else { "" };
        for (j, piece) in wrap(&text, text_width.saturating_sub(indent.len())).into_iter().enumerate() {
            let gutter = if j == 0 { format!("{indent}{glyph} ") } else { format!("{indent}  ") };
            out.push(Line::from(vec![
                Span::styled(gutter, style),
                Span::styled(piece, style),
            ]));
        }
    }
    out
}

// --------------------------------------------------------------- reviews ---

pub fn reviews(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(snap) = app.snapshot.clone() else { return };
    if snap.reviews.is_empty() {
        let block = panel("Reviews");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        widgets::empty(
            frame,
            inner,
            "No reviews yet",
            &[
                ("R", "review the selected agent's changes (from any tab)"),
                ("n", "new agents can review themselves: set review ≥ 1"),
            ],
        );
        return;
    }
    let [table_area, detail_area] =
        Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);
    let ordered: Vec<&Review> = snap.reviews.iter().rev().collect();
    let rows: Vec<Row> = ordered
        .iter()
        .map(|r| {
            let (verdict, color) = verdict_label(r);
            Row::new(vec![
                Cell::from(Span::styled(format!("#{}", r.id), theme::muted())),
                Cell::from(Span::styled(r.target.clone(), theme::value())),
                Cell::from(Span::styled(verdict, Style::default().fg(color))),
                Cell::from(Span::styled(
                    if r.findings.is_empty() { "—".into() } else { r.findings.len().to_string() },
                    theme::base(),
                )),
                Cell::from(Span::styled(
                    if r.sent_back { "sent" } else { "" },
                    theme::good(),
                )),
                Cell::from(Span::styled(fmt_age(r.created), theme::muted())),
                Cell::from(Span::styled(r.summary.clone(), theme::muted())),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(22),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(5),
            Constraint::Length(7),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(["#", "TARGET", "VERDICT", "FINDINGS", "", "AGE", "SUMMARY"]).style(theme::label()))
    .column_spacing(1)
    .row_highlight_style(theme::selected())
    .block(focus_panel("Reviews", true));
    app.review_sel = app.review_sel.min(ordered.len().saturating_sub(1));
    let mut state = TableState::default().with_selected(Some(app.review_sel));
    frame.render_stateful_widget(table, table_area, &mut state);

    let r = ordered[app.review_sel];
    let block = panel(format!("#{} · {} reviewed by {}", r.id, r.target, r.reviewer));
    let inner = block.inner(detail_area);
    frame.render_widget(block, detail_area);
    let width = inner.width as usize;
    let mut lines = vec![];
    if !r.summary.is_empty() {
        for l in wrap(&r.summary, width.saturating_sub(1)) {
            lines.push(Line::from(Span::styled(format!(" {l}"), theme::base())));
        }
        lines.push(Line::from(""));
    }
    if r.state == ReviewState::Running {
        lines.push(Line::from(Span::styled(
            " reviewing… follow along in the Agents tab",
            theme::accent(),
        )));
    }
    for f in &r.findings {
        let at = match (f.file.is_empty(), f.line) {
            (true, _) => String::new(),
            (false, Some(l)) => format!("{}:{l}", f.file),
            (false, None) => f.file.clone(),
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {:<7}", f.severity.to_uppercase()),
                Style::default()
                    .fg(theme::severity_color(&f.severity))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(at, theme::accent()),
        ]));
        for l in wrap(&f.summary, width.saturating_sub(9)) {
            lines.push(Line::from(Span::styled(format!("        {l}"), theme::base())));
        }
    }
    if r.state == ReviewState::Done && !r.findings.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(if r.sent_back {
            vec![Span::styled(" ✓ sent back to the author", theme::good())]
        } else {
            vec![
                Span::styled(" f", theme::accent()),
                Span::styled(" hand these findings to ", theme::muted()),
                Span::styled(r.target.clone(), theme::value()),
            ]
        }));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

// ------------------------------------------------------------------ diff ---

pub fn diff(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(a) = app.selected_agent().cloned() else {
        let block = panel("Diff");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        no_agents(frame, inner);
        return;
    };
    let title = format!(
        "Diff · {}{}{}",
        a.name,
        a.branch.as_ref().map(|b| format!(" · {b}")).unwrap_or_default(),
        if a.diffstat.is_empty() { String::new() } else { format!(" · {}", a.diffstat) }
    );
    let block = focus_panel(title, true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let text = match &app.diff {
        Some(d) if d.name == a.name => d.text.clone(),
        _ => {
            frame.render_widget(Paragraph::new(Span::styled(" loading…", theme::muted())), inner);
            return;
        }
    };
    if text.trim().is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(" no changes yet", theme::muted())),
            inner,
        );
        return;
    }
    let width = inner.width as usize;
    let lines: Vec<Line> = text
        .lines()
        .map(|l| {
            let style = if l.starts_with("diff --git") {
                theme::title()
            } else if l.starts_with("+++") || l.starts_with("---") || l.starts_with("index ") {
                theme::muted()
            } else if l.starts_with("@@") {
                Style::default().fg(theme::CYAN)
            } else if l.starts_with('+') {
                theme::good()
            } else if l.starts_with('-') {
                theme::bad()
            } else {
                theme::base()
            };
            Line::from(Span::styled(clip(&l.replace('\t', "    "), width), style))
        })
        .collect();
    let visible = widgets::window_from_top(&lines, inner.height as usize, &mut app.diff_scroll);
    frame.render_widget(Paragraph::new(visible), inner);
}

// ------------------------------------------------------------------ logs ---

pub fn logs(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = focus_panel("Daemon log", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines: Vec<Line> = app
        .log
        .lines()
        .flat_map(|l| wrap(l, inner.width as usize))
        .map(|l| {
            let style = if l.contains("error") || l.contains("failed") {
                theme::bad()
            } else {
                theme::base()
            };
            Line::from(Span::styled(l, style))
        })
        .collect();
    let visible = widgets::window_from_bottom(&lines, inner.height as usize, &mut app.log_scroll);
    frame.render_widget(Paragraph::new(visible), inner);
}

// ----------------------------------------------------------------- comms ---

pub fn comms(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(snap) = app.snapshot.clone() else { return };
    let block = focus_panel("Board and messages", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if snap.notes.is_empty() {
        widgets::empty(
            frame,
            inner,
            "Nothing said yet",
            &[
                ("p", "post a note every agent can read"),
                ("Enter", "message the selected agent"),
                ("", "agents use `hive msg` and `hive board post` themselves"),
            ],
        );
        return;
    }
    let width = inner.width as usize;
    let mut lines: Vec<Line> = vec![];
    for n in &snap.notes {
        let at = chrono::DateTime::from_timestamp(n.ts, 0)
            .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_default();
        let (arrow, to, to_style) = match n.to.as_deref() {
            None => ("▸", "board".to_string(), theme::muted()),
            Some("user") => ("→", "you".to_string(), theme::warn().add_modifier(Modifier::BOLD)),
            Some(to) => ("→", to.to_string(), theme::accent()),
        };
        let head = format!("{at} {} {arrow} {to}  ", n.from);
        let indent = head.chars().count().min(width / 2);
        let body = wrap(&n.text, width.saturating_sub(indent).max(10));
        for (i, piece) in body.into_iter().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![
                    Span::styled(format!("{at} "), theme::muted()),
                    Span::styled(n.from.clone(), theme::value()),
                    Span::styled(format!(" {arrow} "), theme::muted()),
                    Span::styled(to.clone(), to_style),
                    Span::raw("  "),
                    Span::styled(piece, theme::base()),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(indent)),
                    Span::styled(piece, theme::base()),
                ]));
            }
        }
    }
    let visible = widgets::window_from_bottom(&lines, inner.height as usize, &mut app.comms_scroll);
    frame.render_widget(Paragraph::new(visible), inner);
}
