---
name: hive-lean
description: >-
  Keep multi-agent work cheap: pick models per task, size fan-outs, set
  budgets, write briefs that avoid wasted turns, review efficiently, and read
  hive's cost and cache numbers. Use when planning or running hive agents,
  when the user worries about cost, usage, tokens, or rate limits, or when an
  agent fleet is burning more than expected.
---

# Running a hive lean

Parallel agents multiply cost. Most waste comes from vague briefs, too many
agents, the wrong model, and agents re-reading context. hive already does the
mechanical savings; these are the choices only you can make.

## What hive does for you

- **Shared prompt cache.** Agents get an identical system prompt (machine
  details moved out of it), so the cached prefix is reused across agents.
  `hive ls` and the dashboard show the cache hit rate; 70%+ is healthy.
- **Batched messages.** Messages that arrive while an agent works are
  delivered as one turn, not one turn each.
- **Cheap reviews.** Small diffs are put straight into the reviewer's prompt,
  so it needs no tool calls to read them, and a diff that hasn't changed
  since its last review is never reviewed twice.
- **Lean guidance.** Every agent is told to grep before reading, read ranges
  of large files, batch tool calls, run narrow tests, and stop when done.
- **Caps.** Budgets (`--budget`), `max_concurrent`, `max_agents`,
  `max_depth`, and a cap on agent-triggered turns stop runaway spend.

## Choices that matter most

1. **Fewer, better-briefed agents.** One agent with a precise brief beats
   three with a vague one. Fan out (`-c 2`/`-c 3`) only for genuinely uncertain
   problems where comparing approaches is the point.
2. **Model per task.** `-m haiku` for mechanical edits (renames, boilerplate,
   formatting, simple tests); `-m sonnet` for ordinary features and reviews;
   the strongest model only for hard design or debugging. Set
   `review_model = "sonnet"` in `~/.hive/config.toml` if your default is the
   strongest model.
3. **Complete briefs.** Name files, functions, constraints, and the exact
   check that proves it works. Each clarifying round trip is a full turn.
4. **Budgets on anything exploratory:** `hive spawn --budget 1.50 "..."`.
   The CLI stops the agent at the cap; `budget_usd` in config sets a default.
5. **Review once, at the end.** `-r 1` reviews when the agent finishes; reach
   for `-r 2` or more only on risky changes. Don't review trivial diffs.
6. **Keep conversations going** instead of spawning new agents for follow-ups:
   `hive send <agent> "..."` resumes the same session with its context cached.
   Resume within a few minutes for the cheapest cache reads.
7. **Retire finished agents.** `hive merge` then `hive rm`, so nobody keeps
   messaging an agent whose work is done.

## Reading the numbers

```bash
hive ls          # per-agent cost, cache %, diffstat
hive ls --json   # cost_usd, tokens_in, tokens_cached, tokens_out per agent
```

A low cache ratio on a long-running agent usually means long gaps between
turns (the cache expired); a high token count with a tiny diffstat means it
is reading too much: stop it and send a narrower brief.
