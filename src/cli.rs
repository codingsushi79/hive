//! Command line. With no subcommand, opens the dashboard.

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};

use crate::ipc::{Client, Request};
use crate::model::*;
use crate::{daemon, paths, tui};

#[derive(Parser)]
#[command(
    name = "hive",
    version,
    about = "Run, watch and review a fleet of Claude Code agents",
    long_about = "Run, watch and review a fleet of Claude Code agents.\n\n\
Each agent works in its own git worktree, a daemon keeps them going after you \
close the dashboard, and any agent's work can be code-reviewed by another agent \
and merged back with a keystroke. Run `hive` with no arguments for the dashboard."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Args)]
struct SpawnArgs {
    /// The task. `@file` reads it from a file.
    #[arg(required = true, num_args = 1..)]
    prompt: Vec<String>,
    /// Name (default: from the task)
    #[arg(short, long, default_value = "")]
    name: String,
    /// Directory to work in (default: here)
    #[arg(short, long, default_value = ".")]
    dir: String,
    #[arg(short, long)]
    model: Option<String>,
    /// Permission mode: acceptEdits, bypassPermissions, plan, default, auto
    #[arg(long)]
    mode: Option<String>,
    /// Work directly in the directory instead of an isolated worktree
    #[arg(long)]
    in_place: bool,
    /// Review automatically after each turn and hand findings back, up to N times
    #[arg(short, long, default_value_t = 0)]
    review: u32,
    /// Spend cap in dollars (default: budget_usd from config)
    #[arg(long)]
    budget: Option<f64>,
}

impl SpawnArgs {
    fn spec(self) -> Result<SpawnSpec> {
        Ok(SpawnSpec {
            name: self.name,
            prompt: read_prompt(&self.prompt.join(" "))?,
            dir: absolute(&self.dir),
            model: self.model,
            mode: self.mode,
            worktree: !self.in_place,
            review_rounds: self.review,
            // Run by an agent, the new one reports back to it.
            parent: me(),
            budget_usd: self.budget,
        })
    }
}

#[derive(Subcommand)]
enum Command {
    /// Start an agent on a task
    Spawn(SpawnArgs),
    /// Start several agents on the same task, each in its own worktree
    Fanout {
        /// How many agents
        #[arg(short, long, default_value_t = 3)]
        count: usize,
        #[command(flatten)]
        spawn: SpawnArgs,
    },
    /// List agents
    #[command(alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
    /// Continue an agent's conversation
    Send {
        name: String,
        #[arg(required = true, num_args = 1..)]
        message: Vec<String>,
    },
    /// Message an agent (`parent` and `user` work from inside an agent)
    Msg {
        to: String,
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// The shared board: read it, or `hive board post "text"`
    Board {
        #[command(subcommand)]
        action: Option<BoardAction>,
        /// How many recent posts to show
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: usize,
    },
    /// Block until agents finish their turns; print each one's result
    Wait {
        #[arg(required = true)]
        names: Vec<String>,
        /// Give up after this many seconds (exit 2)
        #[arg(long, default_value_t = 540)]
        timeout: u64,
        /// Print only states, not results
        #[arg(short, long)]
        quiet: bool,
    },
    /// Print an agent's final message from its latest turn
    Result { name: String },
    /// Your agent name, when run inside a hive agent
    Whoami,
    /// Install the bundled Claude Code skills (orchestrate, comms, lean)
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
    /// Send a message to every worker
    Broadcast {
        #[arg(required = true, num_args = 1..)]
        message: Vec<String>,
    },
    /// Print an agent's transcript
    Show {
        name: String,
        /// Only the last N entries
        #[arg(short = 'n', long)]
        tail: Option<usize>,
    },
    /// Stop an agent's current turn, or every agent
    Stop {
        name: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Have another agent code-review this agent's changes
    Review { name: String },
    /// List reviews and their findings
    Reviews,
    /// Hand a review's findings back to the agent that was reviewed
    Fix { review: u32 },
    /// Show an agent's changes
    Diff { name: String },
    /// Merge an agent's branch into the repository's current branch
    Merge { name: String },
    /// Remove an agent and its worktree (an unmerged branch is kept)
    #[command(alias = "rm")]
    Remove { name: String },
    /// Manage the background daemon
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
}

#[derive(Subcommand)]
enum BoardAction {
    /// Post a note every agent can read
    Post {
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
}

#[derive(Subcommand)]
enum SkillsAction {
    /// Show bundled skills and whether they're installed
    List {
        /// Check <dir>/.claude/skills instead of ~/.claude/skills
        #[arg(long)]
        project: Option<String>,
    },
    /// Install or update skills (all of them by default)
    Install {
        names: Vec<String>,
        /// Install into <dir>/.claude/skills instead of ~/.claude/skills
        #[arg(long)]
        project: Option<String>,
    },
    /// Remove installed skills (refuses if you've edited them)
    Uninstall {
        names: Vec<String>,
        #[arg(long)]
        project: Option<String>,
    },
    /// Print a skill
    Show { name: String },
}

#[derive(Subcommand)]
enum DaemonAction {
    Start,
    Stop,
    Status,
    /// Run in the foreground
    Run,
    /// Print the daemon log
    Log {
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
    },
}

/// This process's agent name, when it runs inside a hive agent.
fn me() -> Option<String> {
    std::env::var("HIVE_AGENT").ok().filter(|s| !s.is_empty())
}

fn absolute(dir: &str) -> String {
    std::path::Path::new(dir)
        .canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| dir.to_string())
}

/// `@path` reads the task from a file, so long briefs don't need quoting.
pub fn read_prompt(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if let Some(path) = raw.strip_prefix('@') {
        return std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading {path}: {e}"));
    }
    Ok(raw.to_string())
}

fn client() -> Result<Client> {
    daemon::ensure_running()?;
    Client::connect()
}

fn command(request: Request) -> Result<()> {
    println!("{}", client()?.command(&request)?);
    Ok(())
}

/// Poll until every named agent has stopped working.
fn wait(names: Vec<String>, timeout: u64, quiet: bool) -> Result<()> {
    let mut c = client()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    let snap = loop {
        let snap = c.snapshot()?;
        for n in &names {
            if !snap.agents.iter().any(|a| &a.name == n) {
                bail!("no agent named `{n}`");
            }
        }
        let busy = snap
            .agents
            .iter()
            .any(|a| names.contains(&a.name) && a.state.busy());
        if !busy {
            break snap;
        }
        if std::time::Instant::now() >= deadline {
            let still: Vec<&str> = snap
                .agents
                .iter()
                .filter(|a| names.contains(&a.name) && a.state.busy())
                .map(|a| a.name.as_str())
                .collect();
            eprintln!("still working after {timeout}s: {}", still.join(", "));
            std::process::exit(2);
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    };
    let mut failed = false;
    for n in &names {
        let a = snap.agents.iter().find(|a| &a.name == n).expect("checked");
        failed |= a.state == AgentState::Failed;
        println!(
            "== {} · {} · {} · {}",
            a.name,
            a.state.label(),
            if a.diffstat.is_empty() { "no changes" } else { &a.diffstat },
            fmt_cost(a.cost_usd)
        );
        if let Some(e) = &a.error {
            println!("error: {e}");
        }
        if !quiet {
            let text = c.command(&Request::Result { name: n.clone() })?;
            println!("{}\n", text.trim());
        }
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn skills_command(action: SkillsAction) -> Result<()> {
    let dir = |project: &Option<String>| {
        crate::skills::target_dir(project.as_ref().map(std::path::Path::new))
    };
    match action {
        SkillsAction::List { project } => {
            let dir = dir(&project)?;
            println!("bundled skills ({}):", dir.display());
            for (skill, state) in crate::skills::status(&dir) {
                println!("  {:<18} {:<10} {}", skill.name, state, skill.summary);
            }
            println!("\n`hive skills install` adds them all; Claude Code picks them up in new sessions.");
        }
        SkillsAction::Install { names, project } => {
            for line in crate::skills::install(&dir(&project)?, &names)? {
                println!("{line}");
            }
        }
        SkillsAction::Uninstall { names, project } => {
            for line in crate::skills::uninstall(&dir(&project)?, &names)? {
                println!("{line}");
            }
        }
        SkillsAction::Show { name } => print!("{}", crate::skills::find(&name)?.body),
    }
    Ok(())
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let Some(cmd) = cli.command else {
        let started = daemon::ensure_running()?;
        return tui::run(started);
    };
    match cmd {
        Command::Spawn(args) => command(Request::Spawn { spec: args.spec()? }),
        Command::Fanout { count, spawn } => {
            let mut spec = spawn.spec()?;
            spec.worktree = true;
            command(Request::Fanout { spec, count })
        }
        Command::List { json } => {
            let snap = client()?.snapshot()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&snap.agents)?);
                return Ok(());
            }
            if snap.agents.is_empty() {
                println!("no agents. start one with `hive spawn \"<task>\"` or open `hive`");
                return Ok(());
            }
            println!(
                "{:<28} {:<9} {:<9} {:>8} {:>6} {:<16} ACTIVITY",
                "NAME", "ROLE", "STATE", "COST", "CACHE", "CHANGES"
            );
            for a in &snap.agents {
                let name = match &a.parent {
                    Some(p) => format!("{}  ↳{p}", a.name),
                    None => a.name.clone(),
                };
                let cache = if a.tokens_in == 0 {
                    "-".to_string()
                } else {
                    format!("{:.0}%", 100.0 * a.tokens_cached as f64 / a.tokens_in as f64)
                };
                println!(
                    "{:<28} {:<9} {:<9} {:>8} {:>6} {:<16} {}",
                    name,
                    if a.role == Role::Reviewer { "reviewer" } else { "worker" },
                    a.state.label(),
                    fmt_cost(a.cost_usd),
                    cache,
                    if a.role == Role::Reviewer { String::new() } else if a.merged { "merged".to_string() } else { a.diffstat.clone() },
                    one_line(&a.activity, 60)
                );
            }
            let t = &snap.totals;
            println!(
                "\n{} running, {} queued, {} idle · {} · {} tokens · {:.0}% from cache",
                t.running,
                t.queued,
                t.idle,
                fmt_cost(t.cost_usd),
                fmt_tokens(t.tokens),
                t.cache_ratio * 100.0
            );
            if let Some(me) = me() {
                println!("(you are {me})");
            }
            Ok(())
        }
        Command::Send { name, message } => command(Request::Send {
            name,
            message: read_prompt(&message.join(" "))?,
        }),
        Command::Msg { to, text } => command(Request::Message {
            from: me().unwrap_or_default(),
            to,
            text: read_prompt(&text.join(" "))?,
        }),
        Command::Board { action, limit } => match action {
            Some(BoardAction::Post { text }) => command(Request::Post {
                from: me().unwrap_or_default(),
                text: text.join(" "),
            }),
            None => command(Request::Board { limit }),
        },
        Command::Wait { names, timeout, quiet } => wait(names, timeout, quiet),
        Command::Result { name } => {
            let text = client()?.command(&Request::Result { name: name.clone() })?;
            if text.is_empty() {
                println!("{name} hasn't finished a turn yet");
            } else {
                println!("{text}");
            }
            Ok(())
        }
        Command::Whoami => {
            match me() {
                Some(me) => println!("{me}"),
                None => println!("not a hive agent (HIVE_AGENT is unset); messages you send come from `user`"),
            }
            Ok(())
        }
        Command::Skills { action } => skills_command(action),
        Command::Broadcast { message } => command(Request::Broadcast {
            message: read_prompt(&message.join(" "))?,
        }),
        Command::Show { name, tail } => {
            let (entries, _) = client()?.transcript(&name, 0)?;
            let skip = tail.map(|n| entries.len().saturating_sub(n)).unwrap_or(0);
            for e in &entries[skip..] {
                let tag = match e.kind {
                    EntryKind::Prompt => "» you",
                    EntryKind::Text => "  ◆",
                    EntryKind::Tool => "  ⚙",
                    EntryKind::ToolOk => "    ↳",
                    EntryKind::ToolErr => "    ✗",
                    EntryKind::Result => "✓ done",
                    EntryKind::Info => "  ·",
                    EntryKind::Error => "! error",
                };
                println!("{tag} {}", e.text);
            }
            Ok(())
        }
        Command::Stop { name, all } => match (name, all) {
            (Some(name), false) => command(Request::Stop { name }),
            (None, true) => command(Request::StopAll),
            _ => bail!("give an agent name, or --all"),
        },
        Command::Review { name } => command(Request::Review { name }),
        Command::Reviews => {
            let snap = client()?.snapshot()?;
            if snap.reviews.is_empty() {
                println!("no reviews yet. `hive review <agent>` starts one");
            }
            for r in &snap.reviews {
                let verdict = match r.state {
                    ReviewState::Running => "reviewing…".to_string(),
                    ReviewState::Failed => "failed".to_string(),
                    ReviewState::Done => r.verdict.clone().unwrap_or_default(),
                };
                println!("#{:<3} {:<20} {verdict:<10} {}", r.id, r.target, one_line(&r.summary, 80));
                for f in &r.findings {
                    let at = match f.line {
                        Some(l) => format!("{}:{l}", f.file),
                        None => f.file.clone(),
                    };
                    println!("       [{}] {at} — {}", f.severity, f.summary);
                }
            }
            Ok(())
        }
        Command::Fix { review } => command(Request::SendFindings { id: review }),
        Command::Diff { name } => {
            print!("{}", client()?.command(&Request::Diff { name })?);
            Ok(())
        }
        Command::Merge { name } => command(Request::Merge { name }),
        Command::Remove { name } => command(Request::Remove { name }),
        Command::Daemon { action } => match action {
            DaemonAction::Start => {
                if daemon::ensure_running()? {
                    println!("daemon started");
                } else {
                    println!("daemon already running");
                }
                Ok(())
            }
            DaemonAction::Stop => {
                if !Client::probe() {
                    println!("daemon not running");
                    return Ok(());
                }
                println!("{}", Client::connect()?.command(&Request::Shutdown)?);
                Ok(())
            }
            DaemonAction::Status => {
                if !Client::probe() {
                    println!("daemon not running");
                    return Ok(());
                }
                let snap = Client::connect()?.snapshot()?;
                let d = &snap.daemon;
                println!(
                    "running · pid {} · up {} · {} agents ({} running) · max {} at once · {}",
                    d.pid,
                    fmt_duration(d.uptime_secs),
                    snap.totals.agents,
                    snap.totals.running,
                    d.max_concurrent,
                    paths::home().display()
                );
                Ok(())
            }
            DaemonAction::Run => daemon::run(),
            DaemonAction::Log { lines } => {
                let text = std::fs::read_to_string(paths::log_path()).unwrap_or_default();
                let all: Vec<&str> = text.lines().collect();
                for line in &all[all.len().saturating_sub(lines)..] {
                    println!("{line}");
                }
                Ok(())
            }
        },
    }
}
