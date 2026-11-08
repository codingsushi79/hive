//! Types shared by the daemon and its clients. Everything here crosses the
//! socket as JSON, so it stays plain data.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Has work, waiting for a concurrency slot.
    Queued,
    Running,
    /// Finished its turn; send it a message to continue.
    Idle,
    Failed,
    Stopped,
}

impl AgentState {
    pub fn label(self) -> &'static str {
        match self {
            AgentState::Queued => "queued",
            AgentState::Running => "running",
            AgentState::Idle => "idle",
            AgentState::Failed => "failed",
            AgentState::Stopped => "stopped",
        }
    }

    pub fn busy(self) -> bool {
        matches!(self, AgentState::Running | AgentState::Queued)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Worker,
    Reviewer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub role: Role,
    pub state: AgentState,
    /// The task it was created with.
    pub prompt: String,
    /// Where it runs: its own worktree, or the directory it was given.
    pub dir: String,
    /// Root of the repository it came from, when there is one.
    pub repo: Option<String>,
    /// `hive/<name>`, only for agents isolated in a worktree.
    pub branch: Option<String>,
    /// Commit its changes are measured against.
    pub base_commit: Option<String>,
    pub model: Option<String>,
    pub mode: String,
    pub session_id: Option<String>,
    pub cost_usd: f64,
    pub turns: u32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub created: i64,
    pub updated: i64,
    /// One line describing the most recent thing it did.
    pub activity: String,
    /// `git diff --shortstat` against the base, refreshed after each turn.
    pub diffstat: String,
    /// Messages waiting for the current turn to finish.
    pub pending: usize,
    /// For reviewers: the agent under review.
    pub review_of: Option<String>,
    /// Automatic review rounds still allowed.
    pub rounds_left: u32,
    pub merged: bool,
    pub error: Option<String>,
    pub entries: usize,
    /// The agent that spawned this one, if an agent did.
    #[serde(default)]
    pub parent: Option<String>,
    /// Spend cap in dollars across all turns; 0 means none.
    #[serde(default)]
    pub budget_usd: f64,
    /// Input tokens served from the prompt cache.
    #[serde(default)]
    pub tokens_cached: u64,
    /// Turns started by other agents since a person last spoke to it. Capped
    /// so two agents can't message each other forever.
    #[serde(default)]
    pub auto_turns: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Prompt,
    Text,
    Tool,
    ToolOk,
    ToolErr,
    Result,
    Info,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub ts: i64,
    pub kind: EntryKind,
    pub text: String,
}

impl Entry {
    pub fn new(kind: EntryKind, text: impl Into<String>) -> Self {
        Self {
            ts: now(),
            kind,
            text: text.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub severity: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub line: Option<u32>,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub id: u32,
    pub target: String,
    pub reviewer: String,
    pub state: ReviewState,
    /// `approve`, `changes`, or `unparsed` when the reviewer went off-script.
    pub verdict: Option<String>,
    pub summary: String,
    pub findings: Vec<Finding>,
    pub created: i64,
    /// Findings were handed back to the author.
    pub sent_back: bool,
    /// Hash of the diff that was reviewed, so an unchanged diff isn't
    /// reviewed (and paid for) twice.
    #[serde(default)]
    pub diff_hash: u64,
}

/// Something said between agents, or posted for all of them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: u64,
    pub ts: i64,
    /// An agent name, or `user`.
    pub from: String,
    /// `None` is a post on the shared board.
    pub to: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Totals {
    pub agents: usize,
    pub running: usize,
    pub queued: usize,
    pub idle: usize,
    pub failed: usize,
    pub cost_usd: f64,
    pub tokens: u64,
    /// Share of input tokens that came from the prompt cache, 0–1.
    #[serde(default)]
    pub cache_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub pid: u32,
    pub uptime_secs: u64,
    pub max_concurrent: usize,
    pub claude_bin: String,
    pub default_mode: String,
    pub default_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub agents: Vec<AgentInfo>,
    pub reviews: Vec<Review>,
    #[serde(default)]
    pub notes: Vec<Note>,
    pub totals: Totals,
    pub daemon: DaemonInfo,
}

/// Everything needed to start an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnSpec {
    /// Blank picks one from the prompt.
    pub name: String,
    pub prompt: String,
    pub dir: String,
    pub model: Option<String>,
    pub mode: Option<String>,
    /// Isolate in a fresh git worktree on branch `hive/<name>`.
    pub worktree: bool,
    /// Review automatically after each turn, handing findings back, up to
    /// this many times.
    pub review_rounds: u32,
    /// The agent asking for this one; it hears back when each turn ends.
    #[serde(default)]
    pub parent: Option<String>,
    /// Spend cap in dollars; `None` takes the configured default.
    #[serde(default)]
    pub budget_usd: Option<f64>,
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn fmt_duration(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        s if s < 86400 => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
        s => format!("{}d{:02}h", s / 86400, (s % 86400) / 3600),
    }
}

pub fn fmt_age(ts: i64) -> String {
    fmt_duration((now() - ts).max(0) as u64)
}

pub fn fmt_cost(usd: f64) -> String {
    if usd < 0.01 && usd > 0.0 {
        format!("${usd:.3}")
    } else {
        format!("${usd:.2}")
    }
}

pub fn fmt_tokens(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1e3),
        n => n.to_string(),
    }
}

/// First line, cut to `max` characters with an ellipsis.
pub fn one_line(text: &str, max: usize) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() > max {
        let cut: String = line.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        line.to_string()
    }
}

/// A short, readable agent name from the first words of a task.
pub fn slug(prompt: &str) -> String {
    const SKIP: &[&str] = &["the", "a", "an", "and", "to", "of", "in", "for", "on", "please"];
    let words: Vec<String> = prompt
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .filter(|w| !SKIP.contains(&w.as_str()))
        .take(3)
        .collect();
    let mut name = words.join("-");
    name.truncate(24);
    let name = name.trim_matches('-').to_string();
    if name.is_empty() { "agent".into() } else { name }
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !name.starts_with(['-', '.'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_picks_meaningful_words() {
        assert_eq!(slug("Please add the login page to app"), "add-login-page");
        assert_eq!(slug("!!!"), "agent");
        assert!(valid_name(&slug("Fix the N+1 query in orders.rs")));
    }

    #[test]
    fn names_are_path_and_branch_safe() {
        assert!(valid_name("fix-auth_2"));
        assert!(!valid_name("../x"));
        assert!(!valid_name("a b"));
        assert!(!valid_name("-flag"));
    }
}
