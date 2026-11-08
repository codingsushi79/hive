---
name: hive-comms
description: >-
  How hive agents talk to each other: direct messages, the shared board,
  reporting to a parent agent or the person, and avoiding message loops. Use
  when you are running as a hive agent (the HIVE_AGENT environment variable is
  set, or your instructions say you're in a hive) and need to coordinate,
  share an interface or decision, ask a question, or hand off; or when the
  user wants to message a running hive agent.
---

# Communicating inside a hive

`echo $HIVE_AGENT` prints your name when you're a hive agent. Everything below
goes through the `hive` CLI.

## Pick the cheapest channel

| Need | Use | Cost |
|---|---|---|
| Tell everyone a fact (an interface, a file you own, a decision) | `hive board post "..."` | nothing until someone reads it |
| Check what others have announced | `hive board` (`-n 20` for recent only) | one command |
| See who's doing what | `hive ls` | one command |
| Need another agent to **act** or **answer** | `hive msg <agent> "..."` | a full turn for them |
| Question only the person can answer | `hive msg user "..."` | shows in their dashboard |

Direct messages wake the recipient and cost a whole turn of its context.
Prefer the board for anything informational.

## Writing a message

- One message with everything: the question, the context needed to answer
  it, and what you'll do with the answer.
- Name files and symbols exactly (`src/api/user.rs: fn load_user`).
- Never send acknowledgements, thanks, or "done" notes; a parent already gets
  your final summary automatically each time you finish a turn.

```bash
hive msg schema-owner "I need users.last_login (timestamptz, nullable) for the \
session-expiry job. Are you adding it in your migration, or should I add 0042?"
```

## Receiving

Messages arrive as your next turn, starting with `[message from <name>]`.
Several that arrived while you worked are delivered together. Reply with
`hive msg <name>` only if they asked for something; otherwise just act on it.

Messages from your helpers' finished turns start with `[hive] <name> finished`.
Check their work with `hive diff <name>`, then `hive merge <name>` or send
corrections with `hive msg <name>`.

## Board etiquette

Post when you:
- start owning a file or module others might touch: `owning src/billing/*`
- change a shared interface: `renamed Config::timeout -> Config::request_timeout`
- make a decision others should follow: `errors: use thiserror, not anyhow, in lib crates`

Read the board when you start, and before editing anything shared.

## Loops and limits

Agents can only trigger a limited number of turns in each other before the
person has to step in. If two agents disagree, don't argue by message: post
both positions on the board and `hive msg user` for a decision.
