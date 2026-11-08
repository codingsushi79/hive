<div align="center">

# hive

**Run, watch and review a fleet of Claude Code agents from one TUI, with a daemon that keeps them working after you close it.**

![rust](https://img.shields.io/badge/rust-stable-orange.svg)
![license](https://img.shields.io/badge/license-MIT-blue.svg)

</div>

## Install

```bash
cargo install --git https://github.com/codingsushi79/hive
```

Or from a checkout: `./install.sh` (or `cargo install --path .`).

Needs Rust and the [Claude Code](https://claude.com/claude-code) CLI (`claude`) on your `PATH`.

## Quick start

```bash
cd ~/code/my-app
hive
```

Press `n`, type a task, press `Enter`. The agent starts in its own git
worktree on branch `hive/<name>`, and you land in its live transcript. Start a
few more. Press `R` on one to have another agent review its changes, `f` in
the Reviews tab to hand the findings back, and `m` to merge the branch when
you're happy.

Press `q` to close the dashboard. **The agents keep working.** Run `hive` again
to come back.

From the shell, without the TUI:

```bash
hive spawn "Add rate limiting to the login endpoint, with tests"
hive spawn -r 2 -m opus "Fix the flaky test in tests/db.rs"   # auto-review, up to 2 rounds
hive fanout -c 3 "Make the search page load faster"          # 3 independent attempts
hive ls
hive send add-rate-limiting "Also cover the password-reset route"
hive review add-rate-limiting
hive reviews
hive fix 1                   # send review #1's findings to its author
hive diff add-rate-limiting
hive merge add-rate-limiting
hive wait add-rate-limiting fix-flaky --timeout 600   # block until done, print results
hive result fix-flaky                                 # its last final message
```

## Skills for your AI

hive ships three [Agent Skills](https://docs.claude.com/en/docs/claude-code/skills)
inside the binary, so Claude itself knows how to drive a hive:

```bash
hive skills install                 # into ~/.claude/skills (all sessions)
hive skills install --project .     # or just this repo's .claude/skills
hive skills list                    # installed / outdated / missing
```

| Skill | Teaches Claude to |
|---|---|
| `hive-orchestrate` | decide when delegating pays off, write good briefs, spawn/fan out, `wait`, review, merge |
| `hive-comms` | pick the cheapest channel (board vs message), write useful messages, avoid loops |
| `hive-lean` | choose models per task, size fan-outs, set budgets, read cost and cache numbers |

Re-run `install` after upgrading hive to update them; `uninstall` refuses to
delete a skill you've edited.

## Agents that coordinate

Every agent can run `hive` itself (it's allowed `Bash(hive:*)` whatever its
permission mode) and is told who it is and how:

```bash
hive msg <agent|parent|user> "..."   # wakes the recipient; replies come back as messages
hive board post "owning src/billing" # shared notes; wakes nobody
hive board                           # read them
hive spawn -n helper "..."           # a helper in its own worktree, reporting back
hive merge helper                    # merges into the *parent's* branch
```

When a helper finishes a turn, its parent automatically receives its summary
and diffstat, so orchestrating agents end their turn instead of polling. The
Comms tab (`4`) shows the board and every message; `p` posts, and messages to
`user` are counted on the dashboard.

Guard rails: `max_agents` (24) alive at once, `max_depth` (2) levels of
agents spawning agents, and `max_auto_turns` (20): after that many turns
triggered by other agents, an agent stops accepting agent messages until you
talk to it.

## Spending less

On by default (`lean = true`):

- **Shared prompt cache.** Machine-specific details are moved out of the
  system prompt (`--exclude-dynamic-system-prompt-sections`) and the shared
  briefing comes before the per-agent line, so every agent reuses the same
  cached prefix. Cache hit rate is shown per agent and fleet-wide.
- **Lean briefing.** Agents are told to grep before reading, read ranges of
  big files, batch tool calls, run narrow tests, and stop when done.
- **Batched inboxes.** Messages that arrive mid-turn are delivered together
  as one turn, not one turn each.
- **Cheaper reviews.** Diffs up to `review_inline_kb` (48 KB) go straight into
  the reviewer's prompt, so it needs no tool round-trips; an unchanged diff
  is never reviewed twice.
- **Budgets.** `--budget 2.50` (or `budget_usd` in config) caps an agent's
  total spend; the CLI enforces the remainder each turn with `--max-budget-usd`.
- **Event-driven orchestration.** Parents are notified when helpers finish
  rather than polling, and `hive wait` blocks without spending tokens.

## What it does

- **Many agents, no collisions.** Every agent gets its own worktree and branch,
  so five agents can edit the same repo at once without touching your checkout
  or each other. `--in-place` (or `worktree: no` in the form) runs in the
  directory directly when you want that instead.
- **Concurrency slots.** At most `max_concurrent` agents run a turn at once
  (`+`/`-` in the TUI); the rest wait **queued** and start as slots free up.
  Fan out ten attempts and they won't all hammer your rate limit at once.
- **Conversations, not one-shots.** Messaging an agent continues its Claude
  Code session (`--resume`). Message a busy agent and it's queued for when the
  current turn finishes. `b` broadcasts one message to every worker.
- **Code review built in.** `R` starts a reviewer: a separate agent in the
  author's worktree with **read-only tools** (Read, Grep, Glob, `git diff/log/show/status`),
  told the original task and asked for a structured verdict. Findings show up
  in the Reviews tab by severity, file and line, and one key hands them back to
  the author.
- **Review loops.** Give an agent `review ≥ 1` and after each turn it's
  reviewed automatically; if the reviewer asks for changes, the findings go
  straight back and the fix is reviewed again, until it's approved or the rounds
  run out.
- **See everything.** Live transcripts (text, every tool call and its result),
  per-agent diffs against the commit it started from, diffstat, cost, tokens
  and turns, fleet totals.
- **Merge with a key.** `m` commits whatever the agent left uncommitted and
  merges `hive/<name>` into the repo's current branch with `--no-ff`. A
  conflicted merge is aborted, leaving your checkout exactly as it was.
- **Survives the UI.** Agents run under a detached daemon. Closing the
  dashboard, or the terminal, stops nothing. If the daemon itself restarts,
  interrupted agents are marked stopped and continue from their session when
  you message them.

## Keys

| Key | Action |
|---|---|
| `1`–`6`, `Tab` | Dashboard · Agents · Reviews · Comms · Diff · Logs |
| `↑ ↓` / `j k` | select agent (review, in Reviews) |
| `n` | new agent |
| `F` | fan out: one task, several agents |
| `Enter` / `i` | message the selected agent |
| `b` | broadcast to every worker |
| `p` | post on the shared board |
| `x` / `X` | stop the selected agent / stop all |
| `R` | review the selected agent's changes |
| `f` | in Reviews: send the findings to the author |
| `m` | merge the agent's branch |
| `d` | remove the agent and its worktree (an unmerged branch is kept) |
| `PgUp PgDn` / `J K` | scroll transcript, diff or log |
| `g` / `G` | top / follow the end |
| `+` / `-` | more / fewer concurrent agents |
| `Q` | shut the daemon down, stopping every agent |
| `q` / `Esc` / `Ctrl-C` | close the dashboard, agents keep working |
| `?` | help |

In forms: `Tab`/`↑↓` move between fields, `Enter` submits, `Ctrl-E` opens the
field in `$EDITOR` for a multi-line brief, `Ctrl-W`/`Ctrl-U` delete a word/the
line, and `@path/to/brief.md` as a task reads it from a file. A request the
daemon rejects keeps the form open with everything you typed.

## Configuration

`~/.hive/config.toml`, all optional. `HIVE_HOME` relocates everything.

```toml
max_concurrent = 4              # turns running at once; the rest queue
claude_bin = "claude"           # any CLI speaking Claude Code's stream-json
default_model = ""              # e.g. "opus"; empty = the CLI's default
default_mode = "acceptEdits"    # acceptEdits, bypassPermissions, plan, auto, default
review_model = ""               # empty = same model as the agent under review
extra_args = []                 # appended to every invocation

comms = true                    # agents may run `hive` (message, board, spawn)
max_agents = 24                 # alive at once
max_depth = 2                   # agent → helper → helper, no deeper
max_auto_turns = 20             # agent-triggered turns before you must step in

lean = true                     # shared cache prefix + concise-work briefing
budget_usd = 0                  # default per-agent cap; 0 = none
review_inline_kb = 48           # inline diffs this small into review prompts
```

Agents run headless (`claude -p`), so a tool the permission mode doesn't allow
is denied rather than prompted for. `acceptEdits` lets agents edit files but
denies most shell commands; use `auto` or `bypassPermissions` (ideally in a
sandbox) for agents that need to run builds and tests, or add
`extra_args = ["--allowedTools", "Bash(cargo:*)"]`.

State lives in `~/.hive/`: `state.json` (agents, transcripts, reviews),
`daemon.log`, and `worktrees/`.

## Design

```
hive (TUI / CLI)  ──unix socket, JSON lines──▶  hive daemon
  thin client                                    ├── agent "auth"      claude -p --resume … ─ worktree hive/auth
  renders snapshots                              ├── agent "auth-review" (read-only tools) ── same worktree
  holds no state                                 ├── agent "search"    claude -p … ────────── worktree hive/search
                                                 ├── scheduler: slots, queue, inboxes
                                                 └── review loop: verdict → findings → author
```

One turn of an agent is one `claude -p --output-format stream-json` process in
its own process group, fed the message on stdin. The daemon parses the stream
into transcript entries, cost and token counts, and captures the session id so
the next message resumes the same conversation. Stopping an agent signals the
whole process group, so tools it launched stop too.

## Tests

```bash
cargo test
```

Covers stream-json parsing, review-verdict extraction (fenced, bare, malformed),
the worktree → diff → commit → merge round trip on a real repository, text
wrapping and scrolling, and renders every TUI tab and form headlessly.

## License

MIT.
