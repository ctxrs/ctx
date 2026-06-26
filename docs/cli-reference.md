# CLI Reference

ctx is a local CLI for indexing and searching agent session history.

## Global Options

```bash
ctx --data-root /tmp/ctx status
CTX_DATA_ROOT=/tmp/ctx ctx status
```

`--data-root` overrides the default ctx root for every command. The environment
variable `CTX_DATA_ROOT` provides the same value. The root is used directly; ctx
does not append another product directory.

## Setup And Health

```bash
ctx setup
ctx setup --catalog-only
ctx setup --json
ctx setup --progress json --json
ctx status
ctx status --json
ctx doctor
ctx doctor --json
ctx validate
ctx validate --json
```

- `setup` creates the data root, opens or creates `work.sqlite`, writes
  `config.toml` when needed, discovers known provider history locations,
  catalogs Codex sessions, imports all discovered importable sources, optimizes
  the local search index, and prints next steps.
- `setup --catalog-only` stops after discovery/cataloging. It is useful for
  fast inventory or troubleshooting, but it does not make history searchable.
- `status` reports the ctx root, database path, config path, indexed item
  count, indexed source count, initialization state, and local-only marker.
- `doctor` opens local storage and reports validation findings.
- `validate` opens local storage and reports database validation findings.

Setup and health checks do not change shell startup files, install repository
integrations, write into source repositories, call model APIs, require API keys,
or start background processes. Core storage checks use the configured data root,
and JSON stdout remains structured.

## Sources

```bash
ctx sources
ctx sources --json
```

`sources` lists provider history locations that ctx knows how to check on this
machine. Current rows include:

- Codex session trees at `~/.codex/sessions`;
- Codex prompt history at `~/.codex/history.jsonl`;
- Pi session JSONL at `~/.pi/sessions.jsonl`;
- native rows for supported Antigravity, Claude, OpenCode, Gemini, Cursor,
  Copilot CLI, and Factory AI Droid local history locations.

Each JSON row includes `provider`, `path`, `exists`, `source_format`, `status`,
`import_support`, `native_import`, `raw_retention`, and any
`unsupported_reason`. `sources` reads home-directory path metadata and writes
nothing to provider files or source repositories.

## Import

```bash
ctx import
ctx import --all
ctx import --provider codex
ctx import --provider pi
ctx import --provider antigravity
ctx import --provider claude
ctx import --provider opencode
ctx import --provider gemini
ctx import --provider cursor
ctx import --provider copilot-cli
ctx import --provider factory-ai-droid
ctx import --path ~/.codex/sessions
ctx import --provider pi --path ~/.pi/sessions.jsonl
ctx import --resume
ctx import --json
ctx import --progress json --json
```

`import` explicitly indexes provider history into the local SQLite store. The
normal first-run path is `ctx setup`, which already imports discovered sources.
Use `import` to repair, re-run, resume, or target a specific provider/path. It
creates the data root and default config if needed, reads provider transcript
files, and writes indexed source metadata, sessions, events, searchable text,
citations, and import totals to SQLite.

Import selection rules:

- with no arguments or with `--all`, import all discovered sources that exist;
- with `--provider`, import discovered sources for that provider;
- with `--path`, import exactly that path;
- with `--path` and no provider, parse the path as Codex format.

The current `--resume` flag is an idempotent-rescan mode marker. JSON reports
`resume: true` and `resume_mode: "idempotent_rescan"`, but provider-native
cursor resume is not a universal contract yet.

## List, Show, Locate, And Export

```bash
ctx list
ctx list --limit 50
ctx list --json
ctx show session <ctx-session-id> --mode full --format text
ctx show session <ctx-session-id> --mode lite --format markdown
ctx show session <ctx-session-id> --mode log --format jsonl
ctx show event <ctx-event-id> --window 3 --format text
ctx show event <ctx-event-id> --before 5 --after 10 --format json
ctx locate session <ctx-session-id>
ctx locate event <ctx-event-id>
ctx export session <ctx-session-id> --mode full --format markdown --out transcript.md
ctx export session <ctx-session-id> --mode log --format jsonl
```

`list` reads the local database and returns indexed items up to `--limit`
(default `20`).

`show session` renders one transcript by ctx-owned session ID. `--mode full`
keeps all user/assistant/system message events, `--mode lite` renders a compact
agent-readable transcript with user messages and final assistant messages, and
`--mode log` renders all imported events including tool and command activity.
`--format` accepts `text`, `markdown`, `json`, or `jsonl`.

`show event` renders one ctx-owned event hit. `--before` and `--after` include
neighboring events in the same session; `--window N` is shorthand for
`--before N --after N`. It accepts the same output formats as `show session`.

`locate session` and `locate event` print provenance metadata: ctx IDs,
provider, provider-owned session IDs, source path and cursor, source
availability, import fidelity, and resume/cursor metadata when available.

`export session` renders the same transcript modes and formats as `show
session`. Without `--out`, it writes the artifact to stdout. With `--out`, it
writes the artifact to that path and prints nothing on success.

Provider-owned IDs are metadata, not positional IDs. Positional session and
event arguments are ctx-owned IDs. To look up a provider-owned session, use an
explicit provider lookup such as `--provider codex --provider-session
<provider-session-id>` on commands that support provider lookup.

JSON output may expose local paths, event payloads, and compatibility field
names from the current store schema, so treat it as private local data.

## Search

```bash
ctx search "build failure"
ctx search "sqlite storage" --provider codex
ctx search "retry handling" --repo checkout --since 60d
ctx search "tool output" --event-type tool_output
ctx search --file crates/foo/src/lib.rs
ctx search "token budget" --limit 5 --json
```

`search` quietly refreshes discovered native provider history before querying
indexed sessions and events. The refresh is best-effort and keeps JSON stdout
reserved for the search result object. The query argument is optional so file
or metadata filters can drive a search. Results are local hits over indexed
history. Event hits include `ctx_event_id`; hits with known session context
include `ctx_session_id`; provider metadata including `provider_session_id` is
included when known. Results also include title, snippet, rank, match reasons,
source-path/cursor data, citations, `suggested_next_commands`, and
pagination/truncation fields.

Filters:

- `--provider codex|pi|claude|opencode|antigravity|gemini|cursor|copilot-cli|factory-ai-droid`;
- `--repo <name-or-path>`;
- `--since <rfc3339-or-days>d`, for example `2026-06-01T00:00:00Z` or `30d`;
- `--event-type <event-type>`;
- `--file <path>`;
- `--primary-only`;
- `--include-subagents`;
- `--limit <n>`.

`search` reads provider history and SQLite, and may write newly discovered
history into the local index before querying.

## Progress Output

`setup` and `import` accept `--progress auto|plain|json|none`. `auto` writes
plain progress only to an interactive stderr and stays quiet for `--json` or
non-interactive stderr. `--progress json` writes newline-delimited progress
objects to stderr. It does not change stdout, so command result JSON remains a
single object when `--json` is also present.

Progress JSON is a best-effort operation stream. Each object has
`type: "ctx_progress"` plus `operation`, `phase`, `message`,
`completed_bytes`, `total_bytes`, `percent`, `elapsed_seconds`, `eta_seconds`,
`completed_files`, `total_files`, `imported_events`, and `done`.

## JSON Contract

JSON output is intended for local agents and scripts. It is private unless a
user explicitly reviews and redacts it.

Structured output is available for:

```text
ctx setup --json
ctx status --json
ctx sources --json
ctx import --json
ctx list --json
ctx show session <ctx-session-id> --format json
ctx show event <ctx-event-id> --format json
ctx locate session <ctx-session-id> --format json
ctx locate event <ctx-event-id> --format json
ctx export session <ctx-session-id> --mode full --format json
ctx search [query] --json
ctx doctor --json
ctx validate --json
```

See [contracts/json.md](contracts/json.md) for the current field-level contract
and known compatibility limits.
