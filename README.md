<img src="docs/assets/ctx-readme-banner.png" alt="ctx is a CLI for searching past agent sessions." width="100%">

ctx is an open-source CLI for fast local search across your past coding agent sessions.

Coding agents usually start from zero. They can inspect the current repo, but they often cannot recover the discussions, decisions, failed attempts, commands, and test results from earlier work.

Those sessions are full of useful context:

- decisions, constraints, intent, and rejected approaches from you
- bug investigations, refactors, file paths, commands, patches, and notes from previous agents

ctx indexes those logs into SQLite on your machine, then gives current and future agents a CLI for finding the prior discussion, command, or failed attempt before they repeat it.

## Install

```bash
curl -fsSL https://ctx.rs/install | sh
```

Installs ctx and indexes discovered local agent history.

## How it works

Your past agent sessions are stored in local provider history files. ctx discovers supported sources, imports the real persisted records, and stores normalized session and event data in a local SQLite database optimized for retrieval.

ctx is written in Rust and stores a local SQLite index, so searches are fast, scriptable, and do not require a background service.

```bash
# Index all of your existing local agent sessions
ctx setup

# Your agent can search prior work with normal language
ctx search "failed migration"

# Results include matching sessions, snippets, and ctx IDs
# evt_01h...  ses_01h...  codex  "migration expected the old cursor name" ...

# Print the matching part of the old transcript
ctx show event <ctx-event-id> --window 3

# Or print a compact transcript of the original session
ctx show session <ctx-session-id> --mode lite
```

Those IDs let your current agent recover arbitrary amount of context from previous sessions as needed.

The CLI does not send your prompts, transcripts, or indexed history to a cloud service, call model APIs, require API keys, or write into your source repositories.

For the full pipeline, see [How ctx works](https://ctx.rs/concepts/how-it-works). For a quick first run, see [Quickstart](https://ctx.rs/first-search).

## Supported agent histories

Support means ctx can discover or read that harness's persisted local history and import it into the local search index. Use `ctx sources --json` on your machine to see which sources are currently `importable`.

| Agent harness | Support |
| --- | --- |
| Claude Code | Supported |
| Codex | Supported |
| Cursor | Supported |
| Pi | Supported |
| OpenCode | Supported |
| Antigravity / Gemini | Supported |
| Factory AI Droid | Supported |
| Copilot | Supported |

## Install the skill

The agent-history search skill teaches an agent to use ctx before it edits:

```text
Search prior local agent sessions with ctx. Inspect the best event or session.
If retrieved history affects your answer, cite the ctx ID you used.
```

See [Agent History Search Skill](https://ctx.rs/agent-history-search-skill) for the installable skill, prompt pattern, and agent-specific setup links.

## How ctx compares

Agent memory tools usually save compact facts, summaries, vectors, or graph nodes. Those can help with stable preferences, but they are weak evidence when the next agent needs to know where a decision came from, what command failed, or what was rejected in the original conversation.

Graphify-style tools answer a different question. They map the current repository: files, symbols, imports, folders, and relationships. ctx searches the prior agent sessions that explain what happened while people and agents changed that repository.

ctx keeps retrieval tied to sessions and events, so another agent can inspect the source before using it. Read more about [agent memory](https://ctx.rs/comparisons/agent-memory), [Graphify-style codebase graphs](https://ctx.rs/comparisons/codebase-graphs), and [grep or log search](https://ctx.rs/comparisons/grep-log-search).

## Explore the docs

| Page | What it covers |
| --- | --- |
| [Install](https://ctx.rs/getting-started/install) | Install ctx, initialize local storage, and index discovered local history. |
| [Quickstart](https://ctx.rs/first-search) | Search local history, inspect an event, open the session, and use JSON output. |
| [Install the skill](https://ctx.rs/agent-history-search-skill) | Teach agents to search prior sessions, inspect cited hits, and report the ctx ID they used. |
| [Cursor](https://ctx.rs/agents/cursor) | Import Cursor agent transcripts and ask Cursor to cite retrieved local history before editing. |
| [How it works](https://ctx.rs/concepts/how-it-works) | Understand discovery, import, SQLite storage, search refresh, and cited retrieval. |
| [Supported agents](https://ctx.rs/concepts/supported-agents) | See which agent histories ctx can discover, import, and search today. |
| [CLI reference](https://ctx.rs/reference/cli) | Review setup, status, sources, import, list, show, locate, export, search, doctor, and validate. |
