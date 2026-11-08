//! What every agent is told about the hive it's in, and how to work lean.
//!
//! This text goes in the system prompt, so it is written once per agent and
//! then cached. The shared parts are byte-identical across agents and come
//! first, so agents share the cached prefix as far as possible; the per-agent
//! line comes last.

use crate::config::Settings;
use crate::model::{AgentInfo, Role};

const LEAN: &str = "\
Work efficiently; every token you read or write costs money.
- Be concise. Don't narrate plans at length, restate the task, or echo file contents back.
- Find code with Grep/Glob before reading, and read only the parts you need (use offset/limit on large files).
- Don't re-read a file you've already read unless it changed. Don't re-run a command whose output you have.
- Batch independent tool calls into one step.
- Run the narrowest test or check that proves your change, not the whole suite, until the end.
- When the task is done, stop. Finish with a short summary of what changed and anything left undone.";

const COMMS: &str = "\
You are one agent in a hive: several Claude Code agents working in parallel, managed by the `hive` CLI. Coordination commands (run them with Bash):
- `hive ls` — the other agents, what they're doing, and what they've changed.
- `hive msg <agent> \"text\"` — message another agent. Use `parent` for the agent that started you and `user` for the person running the hive. The recipient gets it after its current turn, and its reply comes back to you as a new message. Each message costs the recipient a whole turn: send one only when it changes what they'll do, and put everything in one message.
- `hive board post \"text\"` / `hive board` — a shared notice board. Posting is free for everyone else; use it for facts others may need (interfaces you changed, files you own, decisions). Read it when you start and before touching shared code.
- `hive spawn -n <name> \"task\"` — hand a self-contained subtask to a new agent in its own worktree. Do this only for substantial work that can proceed in parallel with yours; small things are cheaper to do yourself. Give it a complete brief: it knows nothing you don't write down. You'll get a message with its result each time it finishes a turn, so end your turn instead of waiting or polling.
- `hive diff <agent>` / `hive merge <agent>` — inspect a helper's changes, then merge its branch into your working tree's branch.
Never message an agent just to acknowledge or say thanks.";

/// The text appended to an agent's system prompt, or `None` for nothing.
pub fn system_append(settings: &Settings, agent: &AgentInfo) -> Option<String> {
    let comms = settings.comms && agent.role == Role::Worker;
    let mut parts: Vec<String> = vec![];
    if settings.lean {
        parts.push(LEAN.to_string());
    }
    if comms {
        parts.push(COMMS.to_string());
        let mut me = format!("You are the agent named `{}`.", agent.name);
        if let Some(parent) = &agent.parent {
            me.push_str(&format!(
                " You were started by `{parent}`; report back to it with `hive msg parent` only if your final summary won't be enough (it receives that automatically)."
            ));
        }
        parts.push(me);
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// Several queued messages delivered as one turn: one context reload, one
/// cache read, instead of one per message.
pub fn batch(messages: Vec<String>) -> String {
    if messages.len() == 1 {
        return messages.into_iter().next().unwrap_or_default();
    }
    let n = messages.len();
    let mut out = format!("{n} messages arrived while you were working:\n");
    for (i, m) in messages.into_iter().enumerate() {
        out.push_str(&format!("\n--- message {} of {n} ---\n{m}\n", i + 1));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batching_keeps_singles_untouched() {
        assert_eq!(batch(vec!["hi".into()]), "hi");
        let b = batch(vec!["a".into(), "b".into()]);
        assert!(b.starts_with("2 messages"));
        assert!(b.contains("--- message 2 of 2 ---\nb"));
    }
}
