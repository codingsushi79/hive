//! The hub. Owns every agent process, so agents keep working when the
//! dashboard closes; the TUI and CLI are thin clients over the socket.
//!
//! One turn of an agent is one `claude -p` process. Continuing a conversation
//! starts another with `--resume`, which is also how a stopped agent, or one
//! that survived a daemon restart, picks up where it left off.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::config::{self, Settings};
use crate::ipc::{Client, Request, Response};
use crate::model::*;
use crate::stream::{self, Update};
use crate::{git, paths, prompts, review};

/// Transcript entries kept per agent; older ones are dropped.
const MAX_ENTRIES: usize = 4000;
/// Board posts and messages kept.
const MAX_NOTES: usize = 500;
/// A finished helper's summary is cut to this before it goes to its parent.
const REPORT_MAX: usize = 4000;

/// Who caused a turn. Turns that agents cause in each other are counted and
/// capped; a person speaking resets the count.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    User,
    Agent,
}

#[derive(Serialize, Deserialize)]
struct Agent {
    info: AgentInfo,
    entries: Vec<Entry>,
    inbox: VecDeque<String>,
    /// Final message of the latest turn.
    #[serde(default)]
    last_result: String,
    #[serde(skip)]
    pid: Option<i32>,
    #[serde(skip)]
    stopping: bool,
    #[serde(skip)]
    turn_failed: bool,
    /// Entries dropped off the front, so `since` indices stay stable.
    #[serde(default)]
    dropped: usize,
}

impl Agent {
    fn push(&mut self, entry: Entry) {
        if matches!(entry.kind, EntryKind::Text | EntryKind::Tool | EntryKind::Error) {
            self.info.activity = one_line(&entry.text, 120);
        }
        self.entries.push(entry);
        if self.entries.len() > MAX_ENTRIES {
            let excess = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..excess);
            self.dropped += excess;
        }
        self.info.entries = self.dropped + self.entries.len();
        self.info.updated = now();
    }

    fn is_worktree(&self) -> bool {
        self.info.branch.is_some()
    }
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    agents: Vec<Agent>,
    reviews: Vec<Review>,
    next_review: u32,
    #[serde(default)]
    notes: VecDeque<Note>,
    #[serde(default)]
    next_note: u64,
    #[serde(skip)]
    dirty: bool,
}

impl State {
    fn index(&self, name: &str) -> Result<usize> {
        self.agents
            .iter()
            .position(|a| a.info.name == name)
            .ok_or_else(|| anyhow!("no agent named `{name}`"))
    }

    fn agent(&mut self, name: &str) -> Option<&mut Agent> {
        self.agents.iter_mut().find(|a| a.info.name == name)
    }

    fn running(&self) -> usize {
        self.agents
            .iter()
            .filter(|a| a.info.state == AgentState::Running)
            .count()
    }

    fn note(&mut self, from: &str, to: Option<&str>, text: &str) {
        self.next_note += 1;
        self.notes.push_back(Note {
            id: self.next_note,
            ts: now(),
            from: from.to_string(),
            to: to.map(str::to_string),
            text: text.to_string(),
        });
        while self.notes.len() > MAX_NOTES {
            self.notes.pop_front();
        }
        self.dirty = true;
    }

    /// How many agent-spawns-agent links lie above `name`.
    fn depth(&self, name: &str) -> u32 {
        let mut depth = 0;
        let mut current = name.to_string();
        while let Some(parent) = self
            .agents
            .iter()
            .find(|a| a.info.name == current)
            .and_then(|a| a.info.parent.clone())
        {
            depth += 1;
            current = parent;
            if depth > 64 {
                break;
            }
        }
        depth
    }

    fn unique_name(&self, base: &str) -> String {
        let taken = |n: &str| self.agents.iter().any(|a| a.info.name == n);
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|i| format!("{base}-{i}")).find(|n| !taken(n)).unwrap()
    }
}

pub struct Daemon {
    state: Mutex<State>,
    settings: Mutex<Settings>,
    rt: tokio::runtime::Handle,
    started: Instant,
    shutdown: tokio::sync::Notify,
}

impl Daemon {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn settings(&self) -> Settings {
        self.settings.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    // ---------------------------------------------------------- lifecycle ---

    /// Start a turn: one agent CLI process fed `message` on stdin.
    fn launch(self: &Arc<Self>, agent: &mut Agent, message: String) -> Result<()> {
        let settings = self.settings();
        let mut cmd = tokio::process::Command::new(&settings.claude_bin);
        cmd.args(["-p", "--output-format", "stream-json", "--verbose"]);
        if let Some(model) = &agent.info.model {
            cmd.args(["--model", model]);
        }
        match agent.info.role {
            Role::Worker => {
                cmd.args(["--permission-mode", &agent.info.mode]);
            }
            Role::Reviewer => {
                cmd.args(["--permission-mode", "default", "--allowedTools", review::REVIEWER_TOOLS]);
            }
        }
        if let Some(session) = &agent.info.session_id {
            cmd.args(["--resume", session]);
        }
        if agent.info.role == Role::Worker && settings.comms {
            // Lets the agent run `hive` itself whatever its permission mode.
            cmd.args(["--allowedTools", "Bash(hive:*)"]);
        }
        if settings.lean {
            // Per-machine details out of the system prompt, so the cached
            // prefix is shared by every agent.
            cmd.arg("--exclude-dynamic-system-prompt-sections");
        }
        if let Some(append) = prompts::system_append(&settings, &agent.info) {
            cmd.args(["--append-system-prompt", &append]);
        }
        if agent.info.budget_usd > 0.0 {
            let left = agent.info.budget_usd - agent.info.cost_usd;
            if left <= 0.0 {
                bail!(
                    "budget of {} spent; raise it to continue",
                    fmt_cost(agent.info.budget_usd)
                );
            }
            cmd.args(["--max-budget-usd", &format!("{left:.4}")]);
        }
        let exe = std::env::current_exe().ok();
        let mut path = std::env::var_os("PATH").unwrap_or_default();
        if let Some(bin) = exe.as_ref().and_then(|e| e.parent()) {
            // Make sure the agent's `hive` is this hive.
            let mut joined = std::ffi::OsString::from(bin);
            joined.push(":");
            joined.push(&path);
            path = joined;
        }
        cmd.env("HIVE_AGENT", &agent.info.name)
            .env("HIVE_HOME", paths::home())
            .env("PATH", path);
        cmd.args(&settings.extra_args)
            .current_dir(&agent.info.dir)
            // A daemon started from inside a Claude Code session inherits
            // this, and the CLI refuses to nest under it.
            .env_remove("CLAUDECODE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Own process group, so stopping an agent also stops whatever its
        // tools started.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }

        let _guard = self.rt.enter();
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting `{}`", settings.claude_bin))?;
        let mut stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");

        agent.pid = child.id().map(|p| p as i32);
        agent.stopping = false;
        agent.turn_failed = false;
        agent.info.state = AgentState::Running;
        agent.info.error = None;
        agent.info.pending = agent.inbox.len();
        agent.push(Entry::new(EntryKind::Prompt, message.clone()));
        let name = agent.info.name.clone();
        crate::log(format!("{name}: turn started (pid {:?})", agent.pid));

        let me = self.clone();
        self.rt.spawn(async move {
            let _ = stdin.write_all(message.as_bytes()).await;
            drop(stdin);

            let (me2, name2) = (me.clone(), name.clone());
            let errors = tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if !line.trim().is_empty() {
                        me2.apply(&name2, vec![Update::Entry(Entry::new(EntryKind::Error, line))]);
                    }
                }
            });
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                me.apply(&name, stream::parse(&line));
            }
            let _ = errors.await;
            let ok = child.wait().await.map(|s| s.success()).unwrap_or(false);
            let me3 = me.clone();
            let _ = tokio::task::spawn_blocking(move || me3.turn_finished(&name, ok)).await;
        });
        Ok(())
    }

    fn apply(&self, name: &str, updates: Vec<Update>) {
        if updates.is_empty() {
            return;
        }
        let mut st = self.lock();
        st.dirty = true;
        let Some(agent) = st.agent(name) else { return };
        for update in updates {
            match update {
                Update::Session { id, model } => {
                    agent.info.session_id = Some(id);
                    if agent.info.model.is_none() {
                        agent.info.model = model;
                    }
                }
                Update::Entry(mut entry) => {
                    // Paths inside the agent's own directory read better relative.
                    let prefix = format!("{}/", agent.info.dir.trim_end_matches('/'));
                    if entry.text.contains(&prefix) {
                        entry.text = entry.text.replace(&prefix, "");
                    }
                    agent.push(entry)
                }
                Update::Result {
                    text,
                    is_error,
                    cost_usd,
                    turns,
                    tokens_in,
                    tokens_cached,
                    tokens_out,
                    session_id,
                } => {
                    agent.info.tokens_cached += tokens_cached;
                    agent.info.cost_usd += cost_usd;
                    agent.info.turns += turns;
                    agent.info.tokens_in += tokens_in;
                    agent.info.tokens_out += tokens_out;
                    if session_id.is_some() {
                        agent.info.session_id = session_id;
                    }
                    agent.turn_failed = is_error;
                    if is_error {
                        agent.info.error = Some(one_line(&text, 200));
                    }
                    agent.push(Entry::new(
                        if is_error { EntryKind::Error } else { EntryKind::Result },
                        text.clone(),
                    ));
                    agent.last_result = text;
                }
            }
        }
    }

    /// A turn's process exited. Decide what happens next: queued messages,
    /// an automatic review, handing findings back, and freeing the slot.
    fn turn_finished(self: &Arc<Self>, name: &str, exit_ok: bool) {
        let mut st = self.lock();
        st.dirty = true;
        let Ok(idx) = st.index(name) else {
            self.schedule(&mut st);
            return;
        };
        {
            let agent = &mut st.agents[idx];
            agent.pid = None;
            agent.info.state = if agent.stopping {
                AgentState::Stopped
            } else if agent.turn_failed || (!exit_ok && agent.last_result.is_empty()) {
                if agent.info.error.is_none() {
                    agent.info.error = Some(
                        agent
                            .entries
                            .iter()
                            .rev()
                            .find(|e| e.kind == EntryKind::Error)
                            .map(|e| one_line(&e.text, 200))
                            .unwrap_or_else(|| "agent exited with an error".into()),
                    );
                }
                AgentState::Failed
            } else {
                AgentState::Idle
            };
            if agent.stopping {
                agent.push(Entry::new(EntryKind::Info, "stopped"));
            }
            agent.stopping = false;
            if let (Role::Worker, Some(base)) = (agent.info.role, &agent.info.base_commit) {
                if agent.is_worktree() {
                    git::track_new_files(Path::new(&agent.info.dir));
                }
                agent.info.diffstat = git::shortstat(Path::new(&agent.info.dir), base);
            }
            crate::log(format!("{name}: turn ended, {}", agent.info.state.label()));
        }

        match st.agents[idx].info.role {
            Role::Reviewer => self.finish_review(&mut st, idx),
            Role::Worker => {
                self.report_to_parent(&mut st, idx);
                let agent = &mut st.agents[idx];
                if agent.info.state == AgentState::Idle {
                    if let Some(next) = take_batch(agent) {
                        self.dispatch(&mut st, idx, next);
                    } else if agent.info.rounds_left > 0
                        && !agent.info.merged
                        && !agent.info.diffstat.is_empty()
                    {
                        agent.info.rounds_left -= 1;
                        if let Err(e) = self.start_review(&mut st, idx) {
                            st.agents[idx].push(Entry::new(
                                EntryKind::Info,
                                format!("no automatic review: {e}"),
                            ));
                        }
                    }
                } else {
                    agent.info.pending = agent.inbox.len();
                }
            }
        }
        self.schedule(&mut st);
    }

    /// Run `message` now if there's a free slot, otherwise queue it.
    fn dispatch(self: &Arc<Self>, st: &mut State, idx: usize, message: String) {
        let max = self.settings().max_concurrent.max(1);
        if st.running() >= max {
            let agent = &mut st.agents[idx];
            agent.inbox.push_front(message);
            agent.info.pending = agent.inbox.len();
            agent.info.state = AgentState::Queued;
            return;
        }
        let agent = &mut st.agents[idx];
        if let Err(e) = self.launch(agent, message) {
            agent.info.state = AgentState::Failed;
            agent.info.error = Some(format!("{e:#}"));
            agent.push(Entry::new(EntryKind::Error, format!("{e:#}")));
        }
    }

    /// Fill free slots with queued agents, oldest first.
    fn schedule(self: &Arc<Self>, st: &mut State) {
        let max = self.settings().max_concurrent.max(1);
        while st.running() < max {
            let Some(idx) = st
                .agents
                .iter()
                .position(|a| a.info.state == AgentState::Queued)
            else {
                break;
            };
            let agent = &mut st.agents[idx];
            match take_batch(agent) {
                Some(message) => {
                    if let Err(e) = self.launch(agent, message) {
                        agent.info.state = AgentState::Failed;
                        agent.info.error = Some(format!("{e:#}"));
                    }
                }
                None => agent.info.state = AgentState::Idle,
            }
            st.agents[idx].info.pending = st.agents[idx].inbox.len();
        }
    }

    /// Tell an agent's parent how its helper's turn went. This is how an
    /// orchestrating agent hears back without polling.
    fn report_to_parent(self: &Arc<Self>, st: &mut State, idx: usize) {
        let child = &st.agents[idx];
        let Some(parent) = child.info.parent.clone() else { return };
        if !matches!(child.info.state, AgentState::Idle | AgentState::Failed)
            || st.index(&parent).is_err()
        {
            return;
        }
        let name = child.info.name.clone();
        let mut summary = child.last_result.trim().to_string();
        if summary.len() > REPORT_MAX {
            let mut cut = REPORT_MAX;
            while !summary.is_char_boundary(cut) {
                cut -= 1;
            }
            summary.truncate(cut);
            summary.push_str(&format!("\n… (cut; `hive result {name}` has the rest)"));
        }
        let outcome = if child.info.state == AgentState::Failed {
            format!("failed: {}", child.info.error.clone().unwrap_or_default())
        } else {
            "finished its turn".to_string()
        };
        let changes = if child.info.diffstat.is_empty() {
            "no changes".to_string()
        } else {
            child.info.diffstat.clone()
        };
        let message = format!(
            "[hive] {name} {outcome}. Changes: {changes}.\n\n{summary}\n\n\
             Next: `hive diff {name}`, then `hive merge {name}`, or `hive msg {name} \"...\"` with corrections."
        );
        st.note(&name, Some(&parent), &format!("{outcome} ({changes})"));
        if let Err(e) = self.send(st, &parent, message, Origin::Agent) {
            crate::log(format!("{name}: couldn't report to {parent}: {e:#}"));
        }
    }

    fn kill(agent: &mut Agent) {
        if let Some(pid) = agent.pid {
            agent.stopping = true;
            unsafe {
                libc::kill(-pid, libc::SIGTERM);
            }
        }
    }

    // ---------------------------------------------------------- commands ---

    fn spawn_agent(self: &Arc<Self>, st: &mut State, spec: SpawnSpec) -> Result<String> {
        let settings = self.settings();
        let prompt = spec.prompt.trim().to_string();
        if prompt.is_empty() {
            bail!("the task is empty");
        }
        let base = if spec.name.trim().is_empty() {
            slug(&prompt)
        } else {
            spec.name.trim().to_string()
        };
        if !valid_name(&base) {
            bail!("`{base}` isn't a usable name: letters, digits, - _ . only");
        }
        let name = if spec.name.trim().is_empty() {
            st.unique_name(&base)
        } else if st.agents.iter().any(|a| a.info.name == base) {
            bail!("an agent named `{base}` already exists");
        } else {
            base
        };

        let live = st.agents.iter().filter(|a| !a.info.merged).count();
        if live >= settings.max_agents {
            bail!(
                "{live} agents already (max_agents = {}); remove or merge some first",
                settings.max_agents
            );
        }
        if let Some(parent) = &spec.parent {
            st.index(parent)
                .map_err(|_| anyhow!("parent agent `{parent}` doesn't exist"))?;
            let depth = st.depth(parent) + 1;
            if depth > settings.max_depth {
                bail!(
                    "agents may only nest {} deep (max_depth); do this part yourself",
                    settings.max_depth
                );
            }
        }

        let dir = PathBuf::from(if spec.dir.trim().is_empty() { "." } else { spec.dir.trim() });
        let dir = dir
            .canonicalize()
            .with_context(|| format!("directory {}", dir.display()))?;
        let repo = git::repo_root(&dir);
        let (work_dir, branch, base_commit) = match (&repo, spec.worktree) {
            (Some(repo), true) => {
                let repo = Path::new(repo);
                let base = git::head(repo)?;
                let branch = format!("hive/{name}");
                if git::branch_exists(repo, &branch) {
                    bail!("branch `{branch}` already exists; pick another name");
                }
                // Keep the agent in the same subdirectory it was pointed at.
                let sub = dir.strip_prefix(repo).unwrap_or(Path::new("")).to_path_buf();
                let path = paths::worktrees_for(repo).join(&name);
                git::add_worktree(repo, &path, &branch, &base)?;
                (path.join(sub), Some(branch), Some(base))
            }
            (Some(repo), false) => (dir.clone(), None, git::head(Path::new(repo)).ok()),
            (None, true) => bail!(
                "{} isn't in a git repository, so it can't get a worktree; turn worktree off to run in place",
                dir.display()
            ),
            (None, false) => (dir.clone(), None, None),
        };

        let model = spec
            .model
            .filter(|m| !m.trim().is_empty())
            .or_else(|| Some(settings.default_model.clone()).filter(|m| !m.is_empty()));
        let mode = spec
            .mode
            .filter(|m| !m.trim().is_empty())
            .unwrap_or(settings.default_mode.clone());
        let agent = Agent {
            info: AgentInfo {
                name: name.clone(),
                role: Role::Worker,
                state: AgentState::Queued,
                prompt: prompt.clone(),
                dir: work_dir.to_string_lossy().to_string(),
                repo,
                branch,
                base_commit,
                model,
                mode,
                session_id: None,
                cost_usd: 0.0,
                turns: 0,
                tokens_in: 0,
                tokens_out: 0,
                created: now(),
                updated: now(),
                activity: "starting".into(),
                diffstat: String::new(),
                pending: 0,
                review_of: None,
                rounds_left: spec.review_rounds,
                merged: false,
                error: None,
                entries: 0,
                parent: spec.parent.clone(),
                budget_usd: spec.budget_usd.unwrap_or(settings.budget_usd).max(0.0),
                tokens_cached: 0,
                auto_turns: 0,
            },
            entries: vec![],
            inbox: VecDeque::new(),
            last_result: String::new(),
            pid: None,
            stopping: false,
            turn_failed: false,
            dropped: 0,
        };
        st.agents.push(agent);
        let idx = st.agents.len() - 1;
        self.dispatch(st, idx, prompt);
        st.dirty = true;
        let state = st.agents[idx].info.state.label();
        crate::log(format!("spawned {name} ({state})"));
        Ok(name)
    }

    fn send(
        self: &Arc<Self>,
        st: &mut State,
        name: &str,
        message: String,
        origin: Origin,
    ) -> Result<String> {
        let message = message.trim().to_string();
        if message.is_empty() {
            bail!("the message is empty");
        }
        let max_auto = self.settings().max_auto_turns;
        let idx = st.index(name)?;
        let agent = &mut st.agents[idx];
        match origin {
            Origin::User => agent.info.auto_turns = 0,
            Origin::Agent => {
                if agent.info.auto_turns >= max_auto {
                    let note = format!(
                        "held a message for {name}: agents have started {max_auto} turns in it in a row; \
                         message it yourself to let them continue"
                    );
                    st.note("hive", Some("user"), &note);
                    bail!("{name} has had {max_auto} agent-triggered turns in a row; the person must step in before more");
                }
                agent.info.auto_turns += 1;
            }
        }
        if agent.info.state.busy() {
            agent.inbox.push_back(message);
            agent.info.pending = agent.inbox.len();
            return Ok(format!("{name} is busy; message queued ({} waiting)", agent.inbox.len()));
        }
        self.dispatch(st, idx, message);
        st.dirty = true;
        Ok(match st.agents[idx].info.state {
            AgentState::Queued => format!("{name} queued, waiting for a free slot"),
            AgentState::Failed => format!(
                "{name} failed to start: {}",
                st.agents[idx].info.error.clone().unwrap_or_default()
            ),
            _ => format!("sent to {name}"),
        })
    }

    fn start_review(self: &Arc<Self>, st: &mut State, idx: usize) -> Result<String> {
        let target = &st.agents[idx];
        if target.info.role == Role::Reviewer {
            bail!("`{}` is a reviewer; review the agent it reviewed instead", target.info.name);
        }
        if target.info.state.busy() {
            bail!("`{}` is still working; review it once it's idle", target.info.name);
        }
        let Some(base) = target.info.base_commit.clone() else {
            bail!("`{}` isn't in a git repository, so there's no diff to review", target.info.name);
        };
        let dir = PathBuf::from(&target.info.dir);
        if target.is_worktree() {
            git::track_new_files(&dir);
        }
        let diff = git::diff(&dir, &base)?;
        if diff.trim().is_empty() {
            bail!("`{}` hasn't changed anything yet", target.info.name);
        }
        let diff_hash = review::diff_hash(&diff);
        if let Some(prev) = st.reviews.iter().rev().find(|r| {
            r.target == target.info.name && r.state == ReviewState::Done && r.diff_hash == diff_hash
        }) {
            bail!(
                "unchanged since review #{} ({}); see the Reviews tab",
                prev.id,
                prev.verdict.clone().unwrap_or_default()
            );
        }
        if st.reviews.iter().any(|r| r.target == target.info.name && r.state == ReviewState::Running) {
            bail!("`{}` already has a review in progress", target.info.name);
        }

        let target_name = target.info.name.clone();
        let settings = self.settings();
        let model = Some(settings.review_model.clone())
            .filter(|m| !m.is_empty())
            .or_else(|| target.info.model.clone());
        let task = target.info.prompt.clone();
        let repo = target.info.repo.clone();
        st.next_review += 1;
        let id = st.next_review;
        let reviewer = st.unique_name(&format!("{target_name}-review"));
        let inline = (diff.len() <= settings.review_inline_kb * 1024).then_some(diff.as_str());
        let prompt = review::prompt(&task, &base, inline);
        st.agents.push(Agent {
            info: AgentInfo {
                name: reviewer.clone(),
                role: Role::Reviewer,
                state: AgentState::Queued,
                prompt: prompt.clone(),
                dir: dir.to_string_lossy().to_string(),
                repo,
                branch: None,
                base_commit: Some(base),
                model,
                mode: "default".into(),
                session_id: None,
                cost_usd: 0.0,
                turns: 0,
                tokens_in: 0,
                tokens_out: 0,
                created: now(),
                updated: now(),
                activity: "starting review".into(),
                diffstat: String::new(),
                pending: 0,
                review_of: Some(target_name.clone()),
                rounds_left: 0,
                merged: false,
                error: None,
                entries: 0,
                parent: None,
                budget_usd: 0.0,
                tokens_cached: 0,
                auto_turns: 0,
            },
            entries: vec![],
            inbox: VecDeque::new(),
            last_result: String::new(),
            pid: None,
            stopping: false,
            turn_failed: false,
            dropped: 0,
        });
        st.reviews.push(Review {
            id,
            target: target_name.clone(),
            reviewer: reviewer.clone(),
            state: ReviewState::Running,
            verdict: None,
            summary: String::new(),
            findings: vec![],
            created: now(),
            sent_back: false,
            diff_hash,
        });
        let ridx = st.agents.len() - 1;
        self.dispatch(st, ridx, prompt);
        st.dirty = true;
        crate::log(format!("review #{id} of {target_name} by {reviewer}"));
        Ok(format!("review #{id} of {target_name} started ({reviewer})"))
    }

    fn finish_review(self: &Arc<Self>, st: &mut State, reviewer_idx: usize) {
        let reviewer = &st.agents[reviewer_idx];
        let (name, state) = (reviewer.info.name.clone(), reviewer.info.state);
        let text = reviewer.last_result.clone();
        let Some(review) = st
            .reviews
            .iter_mut()
            .rev()
            .find(|r| r.reviewer == name)
        else {
            return;
        };
        match state {
            AgentState::Idle => {
                let parsed = review::parse(&text);
                review.state = ReviewState::Done;
                review.verdict = Some(parsed.verdict);
                review.summary = parsed.summary;
                review.findings = parsed.findings;
            }
            AgentState::Stopped | AgentState::Failed => {
                if review.state == ReviewState::Running {
                    review.state = ReviewState::Failed;
                    review.summary = st.agents[reviewer_idx]
                        .info
                        .error
                        .clone()
                        .unwrap_or_else(|| "review did not finish".into());
                }
                return;
            }
            _ => return,
        }
        let id = review.id;
        let target = review.target.clone();
        let wants_changes = review.verdict.as_deref() == Some("changes");
        // In an automatic loop, findings go straight back to the author, but
        // only while another round remains to check the fix.
        let auto = st
            .agents
            .iter()
            .find(|a| a.info.name == target)
            .is_some_and(|a| a.info.rounds_left > 0);
        if wants_changes
            && auto
            && let Err(e) = self.send_findings(st, id, Origin::Agent)
        {
            crate::log(format!("review #{id}: could not hand findings back: {e}"));
        }
    }

    fn send_findings(self: &Arc<Self>, st: &mut State, id: u32, origin: Origin) -> Result<String> {
        let review = st
            .reviews
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow!("no review #{id}"))?;
        if review.state != ReviewState::Done {
            bail!("review #{id} hasn't finished");
        }
        if review.findings.is_empty() {
            bail!("review #{id} has no findings to send");
        }
        review.sent_back = true;
        let message = review::findings_message(&review.reviewer, &review.summary, &review.findings);
        let target = review.target.clone();
        let count = review.findings.len();
        self.send(st, &target, message, origin)?;
        Ok(format!("{count} finding{} sent to {target}", if count == 1 { "" } else { "s" }))
    }

    fn merge(self: &Arc<Self>, st: &mut State, name: &str) -> Result<String> {
        let idx = st.index(name)?;
        let agent = &st.agents[idx];
        let (Some(branch), Some(repo)) = (agent.info.branch.clone(), agent.info.repo.clone()) else {
            bail!("`{name}` works in place, so there's no branch to merge; its changes are already in {}", agent.info.dir);
        };
        if agent.info.state.busy() {
            bail!("`{name}` is still working; stop it or wait until it's idle");
        }
        if agent.info.merged {
            bail!("`{name}` is already merged");
        }
        let dir = PathBuf::from(&agent.info.dir);
        let summary = one_line(&agent.info.prompt, 60);
        git::commit_all(&dir, &format!("{name}: {summary}"))?;
        let repo = PathBuf::from(repo);
        let into = git::current_branch(&repo);
        git::merge(&repo, &branch, &format!("Merge {branch}: {summary}"))?;
        let agent = &mut st.agents[idx];
        agent.info.merged = true;
        agent.push(Entry::new(EntryKind::Info, format!("merged {branch} into {into}")));
        st.dirty = true;
        crate::log(format!("merged {branch} into {into}"));
        Ok(format!("merged {branch} into {into}"))
    }

    fn remove(self: &Arc<Self>, st: &mut State, name: &str) -> Result<String> {
        let idx = st.index(name)?;
        let mut agent = st.agents.remove(idx);
        Self::kill(&mut agent);
        let mut note = String::new();
        if let (Some(branch), Some(repo)) = (&agent.info.branch, &agent.info.repo) {
            let repo = Path::new(repo);
            let root = paths::worktrees_for(repo).join(&agent.info.name);
            if let Err(e) = git::remove_worktree(repo, &root) {
                note = format!(" (worktree: {e})");
            } else if agent.info.merged {
                let _ = git::delete_branch(repo, branch);
            } else {
                note = format!("; branch {branch} kept");
            }
        }
        // Its reviewers go with it.
        let reviewers: Vec<usize> = st
            .agents
            .iter()
            .enumerate()
            .filter(|(_, a)| a.info.review_of.as_deref() == Some(name))
            .map(|(i, _)| i)
            .collect();
        for i in reviewers.into_iter().rev() {
            let mut r = st.agents.remove(i);
            Self::kill(&mut r);
        }
        st.reviews.retain(|r| r.target != name && r.reviewer != name);
        st.dirty = true;
        self.schedule(st);
        Ok(format!("removed {name}{note}"))
    }

    fn snapshot(&self, st: &State) -> Snapshot {
        let settings = self.settings();
        let agents: Vec<AgentInfo> = st.agents.iter().map(|a| a.info.clone()).collect();
        let count = |s: AgentState| agents.iter().filter(|a| a.state == s).count();
        let totals = Totals {
            agents: agents.len(),
            running: count(AgentState::Running),
            queued: count(AgentState::Queued),
            idle: count(AgentState::Idle),
            failed: count(AgentState::Failed),
            cost_usd: agents.iter().map(|a| a.cost_usd).sum(),
            tokens: agents.iter().map(|a| a.tokens_in + a.tokens_out).sum(),
            cache_ratio: {
                let input: u64 = agents.iter().map(|a| a.tokens_in).sum();
                let cached: u64 = agents.iter().map(|a| a.tokens_cached).sum();
                if input == 0 { 0.0 } else { cached as f64 / input as f64 }
            },
        };
        Snapshot {
            agents,
            reviews: st.reviews.clone(),
            notes: st.notes.iter().rev().take(200).rev().cloned().collect(),
            totals,
            daemon: DaemonInfo {
                pid: std::process::id(),
                uptime_secs: self.started.elapsed().as_secs(),
                max_concurrent: settings.max_concurrent,
                claude_bin: settings.claude_bin,
                default_mode: settings.default_mode,
                default_model: settings.default_model,
            },
        }
    }

    fn handle(self: &Arc<Self>, request: Request) -> Response {
        match self.try_handle(request) {
            Ok(response) => response,
            Err(e) => Response::Error {
                message: format!("{e:#}"),
            },
        }
    }

    fn try_handle(self: &Arc<Self>, request: Request) -> Result<Response> {
        let ok = |message: String| Ok(Response::Ok { message });
        match request {
            Request::Ping => ok("pong".into()),
            Request::Snapshot => {
                let st = self.lock();
                Ok(Response::Snapshot {
                    snapshot: Box::new(self.snapshot(&st)),
                })
            }
            Request::Transcript { name, since } => {
                let st = self.lock();
                let agent = &st.agents[st.index(&name)?];
                let from = since.saturating_sub(agent.dropped).min(agent.entries.len());
                Ok(Response::Transcript {
                    entries: agent.entries[from..].to_vec(),
                    total: agent.dropped + agent.entries.len(),
                })
            }
            Request::Diff { name } => {
                let (dir, base, worktree) = {
                    let st = self.lock();
                    let a = &st.agents[st.index(&name)?];
                    let base = a.info.base_commit.clone().ok_or_else(|| {
                        anyhow!("`{name}` isn't in a git repository, so there's no diff")
                    })?;
                    (PathBuf::from(&a.info.dir), base, a.is_worktree())
                };
                if worktree {
                    git::track_new_files(&dir);
                }
                Ok(Response::Text {
                    text: git::diff(&dir, &base)?,
                })
            }
            Request::Log { lines } => {
                let text = std::fs::read_to_string(paths::log_path()).unwrap_or_default();
                let all: Vec<&str> = text.lines().collect();
                let tail = all[all.len().saturating_sub(lines)..].join("\n");
                Ok(Response::Text { text: tail })
            }
            Request::Spawn { spec } => {
                let mut st = self.lock();
                let name = self.spawn_agent(&mut st, spec)?;
                let state = st.agents[st.index(&name)?].info.state;
                ok(match state {
                    AgentState::Queued => format!("{name} queued, waiting for a free slot"),
                    AgentState::Failed => bail!(
                        "{name} failed to start: {}",
                        st.agents[st.index(&name)?].info.error.clone().unwrap_or_default()
                    ),
                    _ => format!("{name} started"),
                })
            }
            Request::Fanout { spec, count } => {
                if !(1..=16).contains(&count) {
                    bail!("fan out to between 1 and 16 agents");
                }
                let mut st = self.lock();
                let base = if spec.name.trim().is_empty() {
                    slug(&spec.prompt)
                } else {
                    spec.name.trim().to_string()
                };
                let mut names = vec![];
                for _ in 0..count {
                    let mut one = spec.clone();
                    one.name = st.unique_name(&base);
                    match self.spawn_agent(&mut st, one) {
                        Ok(name) => names.push(name),
                        Err(e) if names.is_empty() => return Err(e),
                        Err(e) => {
                            return ok(format!("started {} ({}); then: {e:#}", names.len(), names.join(", ")));
                        }
                    }
                }
                ok(format!("started {}: {}", names.len(), names.join(", ")))
            }
            Request::Send { name, message } => {
                let mut st = self.lock();
                ok(self.send(&mut st, &name, message, Origin::User)?)
            }
            Request::Message { from, to, text } => {
                let mut st = self.lock();
                let from = if from.trim().is_empty() { "user".to_string() } else { from };
                let to = match to.as_str() {
                    "parent" => st
                        .agents
                        .iter()
                        .find(|a| a.info.name == from)
                        .and_then(|a| a.info.parent.clone())
                        .ok_or_else(|| anyhow!("`{from}` has no parent agent; message `user` instead"))?,
                    _ => to,
                };
                if to == from {
                    bail!("that's you");
                }
                if text.trim().is_empty() {
                    bail!("the message is empty");
                }
                if to == "user" {
                    st.note(&from, Some("user"), text.trim());
                    return ok("left for the user; it shows in their dashboard".into());
                }
                st.index(&to)?;
                st.note(&from, Some(&to), text.trim());
                if from == "user" {
                    ok(self.send(&mut st, &to, text, Origin::User)?)
                } else {
                    let body = format!("[message from {from}]\n{}", text.trim());
                    ok(self.send(&mut st, &to, body, Origin::Agent)?)
                }
            }
            Request::Post { from, text } => {
                if text.trim().is_empty() {
                    bail!("the post is empty");
                }
                let mut st = self.lock();
                let from = if from.trim().is_empty() { "user".to_string() } else { from };
                st.note(&from, None, text.trim());
                ok(format!("posted #{}", st.next_note))
            }
            Request::Board { limit } => {
                let st = self.lock();
                let posts: Vec<&Note> = st.notes.iter().filter(|n| n.to.is_none()).collect();
                let shown = &posts[posts.len().saturating_sub(limit.max(1))..];
                if shown.is_empty() {
                    return Ok(Response::Text { text: "the board is empty".into() });
                }
                let text = shown
                    .iter()
                    .map(|n| {
                        let at = chrono::DateTime::from_timestamp(n.ts, 0)
                            .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
                            .unwrap_or_default();
                        format!("#{} {at} {}: {}", n.id, n.from, n.text)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(Response::Text { text })
            }
            Request::Result { name } => {
                let st = self.lock();
                let agent = &st.agents[st.index(&name)?];
                Ok(Response::Text {
                    text: agent.last_result.clone(),
                })
            }
            Request::Broadcast { message } => {
                let mut st = self.lock();
                let targets: Vec<String> = st
                    .agents
                    .iter()
                    .filter(|a| a.info.role == Role::Worker && !a.info.merged)
                    .map(|a| a.info.name.clone())
                    .collect();
                if targets.is_empty() {
                    bail!("no workers to broadcast to");
                }
                for name in &targets {
                    self.send(&mut st, name, message.clone(), Origin::User)?;
                }
                ok(format!("sent to {} agents", targets.len()))
            }
            Request::Stop { name } => {
                let mut st = self.lock();
                let agent = st.agent(&name).ok_or_else(|| anyhow!("no agent named `{name}`"))?;
                agent.inbox.clear();
                agent.info.pending = 0;
                match agent.info.state {
                    AgentState::Running => {
                        Self::kill(agent);
                        ok(format!("stopping {name}"))
                    }
                    AgentState::Queued => {
                        agent.info.state = AgentState::Stopped;
                        st.dirty = true;
                        ok(format!("{name} dequeued"))
                    }
                    _ => ok(format!("{name} isn't running")),
                }
            }
            Request::StopAll => {
                let mut st = self.lock();
                let mut n = 0;
                for agent in st.agents.iter_mut() {
                    agent.inbox.clear();
                    agent.info.pending = 0;
                    match agent.info.state {
                        AgentState::Running => {
                            Self::kill(agent);
                            n += 1;
                        }
                        AgentState::Queued => {
                            agent.info.state = AgentState::Stopped;
                            n += 1;
                        }
                        _ => {}
                    }
                }
                st.dirty = true;
                ok(format!("stopping {n} agent{}", if n == 1 { "" } else { "s" }))
            }
            Request::Review { name } => {
                let mut st = self.lock();
                let idx = st.index(&name)?;
                ok(self.start_review(&mut st, idx)?)
            }
            Request::SendFindings { id } => {
                let mut st = self.lock();
                ok(self.send_findings(&mut st, id, Origin::User)?)
            }
            Request::Merge { name } => {
                let mut st = self.lock();
                ok(self.merge(&mut st, &name)?)
            }
            Request::Remove { name } => {
                let mut st = self.lock();
                ok(self.remove(&mut st, &name)?)
            }
            Request::SetMaxConcurrent { n } => {
                let n = n.clamp(1, 32);
                {
                    let mut s = self.settings.lock().unwrap_or_else(|e| e.into_inner());
                    s.max_concurrent = n;
                    config::save(&s)?;
                }
                let mut st = self.lock();
                self.schedule(&mut st);
                ok(format!("up to {n} agents at once"))
            }
            Request::Shutdown => {
                self.shutdown.notify_one();
                ok("shutting down".into())
            }
        }
    }

    fn save(&self) {
        let json = {
            let mut st = self.lock();
            if !st.dirty {
                return;
            }
            st.dirty = false;
            match serde_json::to_string(&*st) {
                Ok(j) => j,
                Err(e) => {
                    crate::log(format!("saving state: {e}"));
                    return;
                }
            }
        };
        let path = paths::state_path();
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, &path)) {
            crate::log(format!("saving state: {e}"));
        }
    }
}

/// Everything waiting in an agent's inbox, as one turn.
fn take_batch(agent: &mut Agent) -> Option<String> {
    if agent.inbox.is_empty() {
        return None;
    }
    let messages: Vec<String> = agent.inbox.drain(..).collect();
    agent.info.pending = 0;
    Some(prompts::batch(messages))
}

fn load_state() -> State {
    let Ok(text) = std::fs::read_to_string(paths::state_path()) else {
        return State::default();
    };
    match serde_json::from_str::<State>(&text) {
        Ok(mut st) => {
            for agent in &mut st.agents {
                if agent.info.state == AgentState::Running {
                    agent.info.state = AgentState::Stopped;
                    agent.push(Entry::new(
                        EntryKind::Info,
                        "interrupted by a daemon restart; send a message to continue",
                    ));
                }
            }
            for review in &mut st.reviews {
                if review.state == ReviewState::Running {
                    review.state = ReviewState::Failed;
                    review.summary = "interrupted by a daemon restart".into();
                }
            }
            st
        }
        Err(e) => {
            crate::log(format!("state.json unreadable ({e}); starting empty"));
            let _ = std::fs::rename(paths::state_path(), paths::state_path().with_extension("json.bad"));
            State::default()
        }
    }
}

async fn serve(daemon: Arc<Daemon>, stream: UnixStream) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => {
                let d = daemon.clone();
                tokio::task::spawn_blocking(move || d.handle(request))
                    .await
                    .unwrap_or_else(|e| Response::Error {
                        message: format!("internal error: {e}"),
                    })
            }
            Err(e) => Response::Error {
                message: format!("bad request: {e}"),
            },
        };
        let mut out = serde_json::to_string(&response).unwrap_or_default();
        out.push('\n');
        if write.write_all(out.as_bytes()).await.is_err() {
            break;
        }
    }
}

/// Run the daemon in the foreground until asked to stop.
pub fn run() -> Result<()> {
    paths::ensure_dirs()?;
    if Client::probe() {
        bail!("a hive daemon is already running");
    }
    let settings = config::load().unwrap_or_else(|e| {
        crate::log(format!("{e:#}; using defaults"));
        Settings::default()
    });
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let socket = paths::socket_path();
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket)
            .with_context(|| format!("binding {}", socket.display()))?;
        std::fs::write(paths::pid_path(), std::process::id().to_string())?;

        let daemon = Arc::new(Daemon {
            state: Mutex::new(load_state()),
            settings: Mutex::new(settings),
            rt: tokio::runtime::Handle::current(),
            started: Instant::now(),
            shutdown: tokio::sync::Notify::new(),
        });
        {
            let d = daemon.clone();
            tokio::task::spawn_blocking(move || {
                let mut st = d.lock();
                d.schedule(&mut st);
            })
            .await?;
        }
        crate::log(format!("daemon up, pid {}", std::process::id()));

        let saver = daemon.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            loop {
                tick.tick().await;
                let s = saver.clone();
                let _ = tokio::task::spawn_blocking(move || s.save()).await;
            }
        });

        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    if let Ok((stream, _)) = accepted {
                        tokio::spawn(serve(daemon.clone(), stream));
                    }
                }
                _ = daemon.shutdown.notified() => break,
                _ = term.recv() => break,
                _ = tokio::signal::ctrl_c() => break,
            }
        }

        crate::log("shutting down: stopping agents".to_string());
        {
            let mut st = daemon.lock();
            for agent in st.agents.iter_mut() {
                if agent.info.state == AgentState::Running {
                    Daemon::kill(agent);
                    agent.info.state = AgentState::Stopped;
                }
            }
            st.dirty = true;
        }
        daemon.save();
        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_file(paths::pid_path());
        anyhow::Ok(())
    })
}

/// Start the daemon in the background, detached from this terminal.
pub fn spawn_detached() -> Result<()> {
    paths::ensure_dirs()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::log_path())?;
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("starting the daemon")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if Client::probe() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!(
        "the daemon didn't come up; see {}",
        paths::log_path().display()
    )
}

/// Make sure a daemon is answering. Returns whether this call started it.
pub fn ensure_running() -> Result<bool> {
    if Client::probe() {
        return Ok(false);
    }
    spawn_detached()?;
    Ok(true)
}
