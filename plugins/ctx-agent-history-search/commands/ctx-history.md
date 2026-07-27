---
description: Search local agent history with ctx
argument-hint: [question or topic]
---

# ctx History

Use the `ctx-agent-history-search` skill for this request.

User request: `$ARGUMENTS`

Search local agent history with `ctx`, prefer default text output for agent
reading, inspect cited events or sessions before making claims, and return a
concise answer with ctx citations. Use `--format json` only when piping to a script,
`jq`, or extracting exact machine fields.
