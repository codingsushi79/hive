//! `~/.hive/config.toml`. Every field has a default, so the file is optional
//! and a partial one is fine.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Agents allowed to run a turn at the same time. The rest wait, queued.
    pub max_concurrent: usize,
    /// The agent CLI. Anything that speaks Claude Code's `-p
    /// --output-format stream-json` works.
    pub claude_bin: String,
    /// Empty means whatever the CLI defaults to.
    pub default_model: String,
    /// `acceptEdits`, `bypassPermissions`, `plan`, `default` or `auto`.
    pub default_mode: String,
    /// Model for reviewers. Empty means the reviewed agent's model.
    pub review_model: String,
    /// Appended to every agent invocation.
    pub extra_args: Vec<String>,

    // ---- coordination
    /// Let agents run `hive` themselves: message each other, post to the
    /// board, and spawn helpers.
    pub comms: bool,
    /// Most agents alive at once, however they were created.
    pub max_agents: usize,
    /// How deep agent-spawns-agent may nest. 1 lets your agents spawn
    /// helpers that can't spawn further.
    pub max_depth: u32,
    /// Turns other agents may trigger on one agent before a person has to
    /// step in. Stops message ping-pong.
    pub max_auto_turns: u32,

    // ---- usage
    /// Append concise-work guidance and keep the system prompt identical
    /// across agents so the prompt cache is shared between them.
    pub lean: bool,
    /// Default spend cap per agent in dollars; 0 means none.
    pub budget_usd: f64,
    /// Put diffs up to this size straight into the reviewer's prompt, so it
    /// needs no tool round-trips to read them.
    pub review_inline_kb: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_concurrent: 4,
            claude_bin: "claude".into(),
            default_model: String::new(),
            default_mode: "acceptEdits".into(),
            review_model: String::new(),
            extra_args: Vec::new(),
            comms: true,
            max_agents: 24,
            max_depth: 2,
            max_auto_turns: 20,
            lean: true,
            budget_usd: 0.0,
            review_inline_kb: 48,
        }
    }
}

pub fn load() -> Result<Settings> {
    let path = paths::config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(settings: &Settings) -> Result<()> {
    paths::ensure_dirs()?;
    let text = toml::to_string_pretty(settings)?;
    std::fs::write(paths::config_path(), text)?;
    Ok(())
}
