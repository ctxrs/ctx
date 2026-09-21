---
description: Search agent history or trace code to its original agent session
argument-hint: [question, topic, file, line, commit, or PR]
---

# ctx

Use the `ctx` skill for this request.

User request: `$ARGUMENTS`

Choose local history search or ctx blame based on the request. Inspect cited
events or sessions before making claims, preserve the distinction between
history search and blame, and return a concise answer grounded in ctx
evidence. Prefer default text output for agent reading; use `--format json`
only for scripts or exact machine-readable fields.
