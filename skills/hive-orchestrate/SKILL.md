---
name: hive-orchestrate
description: >-
  Delegate work to a fleet of parallel Claude Code agents with the `hive` CLI:
  spawn agents in isolated git worktrees, fan one task out to several, wait
  for results, have agents code-review each other, and merge their branches.
  Use when a task splits into independent parts that can run in parallel,
  when the user asks for agents, sub-agents, a swarm, parallel attempts,
  "have another agent review it", or mentions hive; and when you are a hive
  agent deciding whether to hand off a subtask.
---

# Orchestrating agents with hive

`hive` runs Claude Code agents in the background. Each agent gets its own git
worktree on branch `hive/<name>`, so agents never edit the same checkout. A
daemon keeps them running; the person can watch everything with `hive` (TUI).

## Decide first: is delegating worth it?

Every agent pays to load context from scratch. Delegate only when:

- the work splits into parts that are **independent** (different files or
  modules, no shared interface still being designed), and
- each part is **substantial** (roughly 10+ minutes of focused work), or
- you want **independent attempts** at a hard problem to compare.

Do small, sequential, or tightly coupled work yourself. Two agents editing the
same function will conflict at merge time.

## Spawn

```bash
hive spawn -n auth-limits "Add rate limiting to POST /login in src/routes/auth.rs. \
Use the existing Redis client in src/cache.rs. Limit 5/min per IP; return 429 with \
Retry-After. Add tests in tests/auth.rs. Run only those tests."
```

Write the brief as if for a contractor who has never seen the repo: goal,
where the code is, constraints, how to verify, what *not* to touch. A vague
brief costs more in wasted agent turns than a precise one costs to write.

Useful flags: `-m sonnet|opus|haiku` (cheaper models for mechanical work),
`-r 1` (auto-review once when it finishes), `--budget 2.50` (dollar cap),
`-d <dir>` (default: current directory), `--in-place` (no worktree; avoid
when other agents are active).

Several agents on one task, to compare approaches:

```bash
hive fanout -c 3 -n search-perf "Make /search p95 under 200ms. Measure before/after."
```

## Wait and read results

If you are a hive agent, **don't wait**: end your turn. Each helper you spawn
messages you its final summary every time it finishes a turn.

From a normal session, block until agents are idle (exit code 1 on failure):

```bash
hive wait auth-limits search-perf search-perf-2 --timeout 540
hive result auth-limits        # its last final message
hive diff auth-limits          # what it changed
```

Keep `--timeout` under your shell tool's limit and loop if needed.

## Steer

```bash
hive send auth-limits "Also cover /password-reset with the same limiter."
hive stop auth-limits
hive ls                        # state, cost, diffstat, current activity
```

Messages to a busy agent queue and are delivered together after its turn.

## Review, fix, merge

```bash
hive review auth-limits        # a read-only reviewer agent checks the diff against the task
hive reviews                   # verdicts and findings (severity, file:line)
hive fix 3                     # hand review #3's findings back to the author
hive merge auth-limits         # commit + merge hive/auth-limits into the current branch
hive rm auth-limits            # remove the worktree (unmerged branches are kept)
```

A merge that conflicts is aborted and nothing changes; resolve by messaging
the agent to rebase onto the current branch, or merge by hand.

When you are a hive agent, `hive merge <helper>` merges into *your* branch.

## Limits that will stop you

The daemon enforces `max_agents`, `max_depth` (how deep agents may spawn
agents), per-agent budgets, and a cap on turns agents can trigger in each
other. If a spawn or message is refused, the error says which; don't retry in
a loop, tell the person.
