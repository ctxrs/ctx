---
name: ctx-agent-history-search
description: Use ctx to search local coding-agent history before acting. Use when prior agent sessions may contain relevant insights, decisions, attempts, or transcript context.
---

# ctx Agent History Search

Use ctx whenever you need to reference previous coding-agent sessions. Those
transcripts can contain user intent, decisions, previous work timelines, past
attempts, and what worked or failed.

Use this skill in two modes:

- retrieval before work, when prior sessions may contain decisions, commands,
  failures, or source citations that affect the current task;
- history research reports, when the user asks an agent or read-only subagent to
  research a historical topic across prior local agent sessions.

## Prerequisites

- Require the `ctx` CLI to be installed and set up. If it is missing and
  installing tools is appropriate for the task, install it with:

  ```bash
  curl -fsSL https://ctx.rs/install | sh
  ```

- First setup can take time while ctx indexes past sessions. If needed, keep it
  running in the background or in tmux, or wait for it to finish.
- If ctx remains unavailable, say local history search is unavailable and do not
  invent results.

## Workflow

1. Confirm ctx is ready when starting from a cold context:

   ```bash
   ctx status
   ctx sources
   ```

   Use `ctx status --json` or `ctx sources --json` only when a script needs
   exact fields.

2. Search with normal language first. Add terms or filters when useful:

   ```bash
   ctx search "<query>"
   ctx search "<query>" --refresh off
   ctx search "<query>" --provider codex
   ctx search "<query>" --workspace <workspace>
   ctx search "<query>" --file <path>
   ctx search "<query>" --since 30d
   ctx search "<query>" --term "<related term>" --term "<error text>"
   ctx search "<query>" --session <ctx-session-id>
   ctx search "<query>" --verbose
   ```

   Use default text output for agent reading. Do not add `--json` for
   search, show, or locate unless you are piping it into `jq` or a script, or
   you need exact machine-readable fields. JSON output is much larger and can
   quickly consume the context window.

   When the prompt asks for a topic history or report across multiple sessions,
   run several `ctx search` queries with different wording and filters to find
   promising sessions. Use scoped
   `ctx search "<query>" --session <ctx-session-id>` when a session looks
   relevant and you need dense event-level matches from that session.

   Default search returns primary-agent sessions so human intent and decisions
   stay prominent. Use `--include-subagents` when implementation details, code
   review notes, test output, or failure traces from subagent sessions are
   likely to matter.

   Use `--verbose` when you need full ctx IDs, provider IDs, citations, and
   copyable follow-up commands without switching to JSON.

   You can write a session transcript to a temporary file, check the file size,
   and then read the relevant parts:

   ```bash
   ctx show session <ctx-session-id> --format markdown --out /tmp/ctx-session.md
   wc -c /tmp/ctx-session.md
   ```

   In Codex, ctx excludes the active session tree by default when
   `CODEX_THREAD_ID` is available, so the current prompt and subagents do not
   dominate historical retrieval. Use `--include-current-session` only when the
   active session tree is the target.

3. Inspect relevant results before relying on them:

   ```bash
   ctx show event <ctx-event-id> --window 5
   ctx show session <ctx-session-id>
   ```

4. Locate original provider material when source identity or resume hints matter:

   ```bash
   ctx locate event <ctx-event-id>
   ctx locate session <ctx-session-id>
   ```

5. Write a transcript of relevant sessions when you, the human, or another
   agent needs a file:

   ```bash
   ctx show session <ctx-session-id> --format markdown --out <output-path>
   ```

## When Search Is Not Enough

Use `ctx sql` only when normal search cannot express the question, such as
counts, joins, audits, or scripts over stable local views. Do not use SQL for
broad transcript text search; `ctx search` is built for that.

Start with the bundled SQL docs:

```bash
ctx docs show sql
ctx docs search "stable views"
```

Common SQL examples:

```bash
ctx sql "SELECT provider, COUNT(*) AS sessions FROM ctx_sessions GROUP BY provider"
ctx sql "SELECT event_type, COUNT(*) AS events FROM ctx_events GROUP BY event_type ORDER BY events DESC"
ctx sql "SELECT path, provider, provider_session_id FROM ctx_files_touched WHERE path LIKE '%AGENTS.md%' LIMIT 20"
```

`ctx sql` is read-only and queries the existing index. It does not refresh,
import, initialize, or migrate ctx storage.

## History Research Reports

When asked to research a historical topic, stay read-only unless the user also
asks for edits. The agent writes the report; ctx only retrieves local source
material.

1. Restate the topic, scope, and desired length if the prompt is ambiguous.
   Prefer concise reports by default; use a longer report when the user asks for
   chronology, alternatives, or detailed evidence.
2. Run several targeted searches. Vary query terms across user wording, file or
   module names, error text, commands, branch names, and decision terms. Start
   with `ctx search "<topic>"`, then broaden with `--term` or narrow with
   `--workspace`, `--provider`, `--file`, `--since`, or
   `--session <ctx-session-id>`.
   Use `--include-subagents` when reviews, implementation attempts, test output,
   or failure traces are likely to live in delegated sessions. Add
   `--refresh off` when the report must not update the local ctx index.
3. Inspect focused sources before drawing conclusions. Prefer `ctx show event`
   for a hit plus nearby turns, and `ctx show session` when the whole session
   arc matters:

   ```bash
   ctx show event <ctx-event-id> --window 5
   ctx show session <ctx-session-id>
   ```

   Use full or log mode only when default output omits necessary evidence.
4. Compare evidence across sessions. Note agreements, conflicts, stale results,
   missing raw sources, and gaps where searches did not find evidence.
5. Produce the report as agent synthesis with citations.

Concise report shape:

- answer or finding;
- strongest supporting ctx IDs;
- important caveats or gaps;
- optional next search or verification step.

Long report shape:

- question and scope;
- search method, including key queries and filters;
- findings or chronology;
- evidence table with provider, ctx session ID, ctx event ID when available, and
  why each source matters;
- conflicts, gaps, and suggested follow-up.

## Citation Rules

- Cite ctx material when it affects your answer or implementation.
- Include the provider, ctx session ID, ctx event ID when available, provider
  session ID when available, and source path or cursor when present.
- If you synthesize across multiple snippets, label the conclusion as your
  synthesis and cite the supporting snippets.
- If a source citation is stale or unavailable, say ctx returned indexed text
  but the raw source could not be opened.

## Safety Rules

- Prefer text output for agent reading. Use JSON only for scripts, `jq`, or
  exact field extraction, and keep JSON outputs small.
- Do not say ctx inferred a decision unless the cited text explicitly states
  that decision.
- Do not state that ctx wrote model analysis.
- Do not paste raw transcripts, large JSON payloads, secrets, tokens, or private
  paths into a user-facing report. Summarize reviewed evidence and quote only
  short excerpts needed to support a claim.
- Treat `~/.ctx`, provider transcript paths, and JSON output as private local
  history unless the user explicitly asks to share reviewed excerpts.
