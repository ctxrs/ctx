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
ctx setup --json
ctx status
ctx status --json
ctx doctor
ctx doctor --json
ctx validate
ctx validate --json
```

- `setup` creates the data root, opens or creates `work.sqlite`, writes
  `config.toml` when needed, discovers known provider history locations, and
  prints next steps.
- `status` reports the ctx root, database path, config path, indexed item
  count, indexed source count, initialization state, and local-only marker.
- `doctor` opens local storage and reports validation findings.
- `validate` opens local storage and reports database validation findings.

Setup and health checks are local. They do not change shell startup files,
install repository integrations, write into source repositories, call model
APIs, make network calls, require API keys, or start background processes.

## Sources

```bash
ctx sources
ctx sources --json
```

`sources` lists provider history locations that ctx knows how to check on this
machine. Current rows include:

- Codex session trees at `~/.codex/sessions`;
- Codex prompt history at `~/.codex/history.jsonl`;
- Pi session JSONL at `~/.pi/sessions.jsonl`.

Each JSON row includes `provider`, `path`, `exists`, `source_format`, `status`,
and `raw_retention`. `sources` reads home-directory path metadata and writes
nothing.

## Import

```bash
ctx import
ctx import --all
ctx import --provider codex
ctx import --provider pi
ctx import --path ~/.codex/sessions
ctx import --provider pi --path ~/.pi/sessions.jsonl
ctx import --resume
ctx import --json
```

`import` indexes provider history into the local SQLite store. It creates the
data root and default config if needed, reads provider transcript files, and
writes indexed source metadata, sessions, events, searchable text, citations,
and import totals to SQLite.

Import selection rules:

- with no arguments or with `--all`, import all discovered sources that exist;
- with `--provider`, import discovered sources for that provider;
- with `--path`, import exactly that path;
- with `--path` and no provider, parse the path as Codex format;
- with Claude, OpenCode, Antigravity, Gemini, or Cursor, `--path` is required
  and must point to normalized provider JSONL rather than native provider
  history.

The current `--resume` flag is an idempotent-rescan mode marker. JSON reports
`resume: true` and `resume_mode: "idempotent_rescan"`, but provider-native
cursor resume is not a universal contract yet.

## List And Show

```bash
ctx list
ctx list --limit 50
ctx list --json
ctx show <item-uuid>
ctx show <item-uuid> --json
```

`list` reads the local database and returns indexed items up to `--limit`
(default `20`). `show` reads one indexed item UUID and returns the matching
session or compatibility item plus events when available.

These commands write nothing. JSON output may expose local paths, event payloads,
and compatibility field names from the current store schema, so treat it as
private local data.

## Search

```bash
ctx search "build failure"
ctx search "sqlite storage" --provider codex
ctx search "retry handling" --repo checkout --since 60d
ctx search "tool output" --event-type tool_output
ctx search --file crates/foo/src/lib.rs
ctx search "token budget" --limit 5 --json
```

`search` queries indexed sessions and events. The query argument is optional so
file or metadata filters can drive a search. Results include an opaque item ID
usable with `ctx show`, title, snippet, rank, match reasons, provider and event
metadata when known, source-path/cursor data when available, citations, and
pagination/truncation fields.

Filters:

- `--provider codex|pi`;
- `--repo <name-or-path>`;
- `--since <rfc3339-or-days>d`, for example `2026-06-01T00:00:00Z` or `30d`;
- `--event-type <event-type>`;
- `--file <path>`;
- `--primary-only`;
- `--include-subagents`;
- `--limit <n>`.

`search` reads SQLite and writes nothing.

## Context

`ctx context` is deprecated and hidden from default CLI help. Use
`ctx search --json` for machine-readable retrieval. The old command remains
available for compatibility with existing scripts, emits a deprecation warning
on stderr, reads SQLite, and writes nothing.

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
ctx show <item-uuid> --json
ctx search [query] --json
ctx doctor --json
ctx validate --json
```

See [contracts/json.md](contracts/json.md) for the current field-level contract
and known compatibility limits.
