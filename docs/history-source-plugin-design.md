# History Source Plugin Design

This document describes history source plugins as implemented on
`codex/history-source-plugins`.

## Problem

ctx has first-party local history adapters for common agent tools, but the agent
ecosystem changes quickly and many tools use custom local storage. Maintaining a
native adapter for every tool would couple ctx to unstable schemas owned by other
projects.

Plugins let tools that ctx does not support natively make their history
searchable without ctx learning their storage shape.

The integration must:

- work fully locally;
- support incremental refresh before `ctx search`;
- avoid a hosted plugin store or in-process extension ABI;
- keep adapter ownership with the third-party tool or user;
- reuse the existing ctx capture, store, and search pipeline;
- provide a batch escape hatch for tools that only write files.

## Non-Goals

This design does not add:

- an in-process plugin ABI;
- a remote marketplace;
- background daemon scheduling;
- plugin installation management;
- native adapters for every third-party agent;
- a guarantee that plugin commands are sandboxed from the local user account.

Plugins are local commands. A user or local tool that installs a plugin is
choosing to run that command with the user's normal local permissions.

## User Model

There are two supported paths.

The preferred path for ongoing integrations is a history source plugin:

1. A manifest declares one or more local history sources.
2. ctx discovers the manifest.
3. ctx runs the source command during explicit import or search refresh.
4. The command writes `ctx-history-jsonl-v1` records to stdout.
5. ctx imports the stream and stores the latest cursor.
6. On the next run, ctx passes that cursor back to the command.

The optional batch path is a file import:

1. A tool writes `ctx-history-jsonl-v1` records to a file.
2. The user or tool runs `ctx import --input-format ctx-history-jsonl-v1 --path ...`.
3. ctx imports the file idempotently.

The file path is useful for simple exporters, debugging, and one-time imports.
It is not the best path for day-to-day refresh because ctx cannot discover,
invoke, or cursor an arbitrary file writer by itself.

## Public Contracts

The feature has two public contracts.

### Manifest Contract

A plugin manifest is JSON at `ctx-history-plugin.json`.

Manifests can be discovered from:

- `$CTX_DATA_ROOT/plugins/<plugin>/ctx-history-plugin.json`;
- entries in `CTX_HISTORY_PLUGIN_PATH`.

The implemented schema is:

```json
{
  "schema_version": 1,
  "name": "example-agent",
  "display_name": "Example Agent",
  "version": "0.1.0",
  "history_sources": [
    {
      "id": "default",
      "display_name": "Example local history",
      "provider_key": "example-agent",
      "source_id": "default",
      "source_format": "example-agent-sqlite-v1",
      "enabled": true,
      "refresh": "auto",
      "command": ["example-agent-to-ctx", "export"],
      "working_dir": ".",
      "env": {
        "EXAMPLE_AGENT_PROFILE": "default"
      },
      "timeout_seconds": 300
    }
  ]
}
```

`schema_version`, `name`, `history_sources[].id`, `source_format`, and
`command` are required.

`provider_key` defaults to the manifest `name`. `source_id` defaults to the
source `id`. `enabled` defaults to `false`. `refresh` defaults to `manual`.
`timeout_seconds` defaults to 300 seconds and is clamped to at least 1 second.

Identifiers must be stable lowercase ASCII values with digits, `.`, `_`, or
`-`. They must start with a lowercase ASCII letter or digit and be no more than
128 bytes.

`command` is an argv array. ctx does not execute it through a shell.

### Stream Contract

Plugin commands and batch files emit `ctx-history-jsonl-v1`.

Each line is a JSON object with one of these `record_type` values:

- `manifest`;
- `source`;
- `session`;
- `event`;
- `file_touch`;
- `edge`.

The stream contract intentionally mirrors the normalized shape ctx already
stores:

- source metadata identifies the exporter, native format, cursor, machine, and
  raw input;
- sessions represent conversations, tasks, runs, branches, or subagents;
- events represent ordered messages and tool events;
- file touches connect history to code search and audit workflows;
- edges preserve parent-child, spawned, forked, resumed, or related sessions.

ctx stores these imports under the bounded internal provider `custom`, while
preserving exporter-owned `provider_key`, `source_id`, `source_format`,
`session_id`, and native metadata.

## Runtime Contract

Before running a plugin command, ctx sets:

- `CTX_DATA_ROOT`;
- `CTX_HISTORY_PLUGIN=1`;
- `CTX_HISTORY_PLUGIN_NAME`;
- `CTX_HISTORY_PLUGIN_MANIFEST`;
- `CTX_HISTORY_SOURCE`, such as `example-agent/default`;
- `CTX_HISTORY_SOURCE_ID`;
- `CTX_HISTORY_PROVIDER_KEY`;
- `CTX_HISTORY_SOURCE_FORMAT`;
- `CTX_HISTORY_CURSOR_STREAM`;
- `CTX_HISTORY_MACHINE_ID`;
- `CTX_HISTORY_FULL_RESCAN`, `1` or `0`;
- `CTX_HISTORY_CURSOR`, when a previous cursor exists and is small enough;
- `CTX_HISTORY_CURSOR_FILE`, a temporary file containing the previous cursor.

Plugins should read `CTX_HISTORY_CURSOR_FILE` first. Inline cursor environment
variables are only a convenience for small cursors.

Plugins must write only `ctx-history-jsonl-v1` to stdout. Progress and warnings
belong on stderr.

ctx clears the inherited environment and re-adds a small allowlist:

- `PATH`;
- `HOME`;
- user and locale variables;
- temporary-directory variables;
- XDG data, config, cache, and state roots.

Manifest `env` entries are then added. This avoids accidental dependence on the
parent shell while still allowing plugin authors to pass explicit configuration.

## Incremental Semantics

The plugin owns cursor meaning. ctx treats the cursor as an opaque string.

Examples:

- append-only files can use byte offsets;
- SQLite stores can use row ids;
- split stores can use JSON maps keyed by session id or file path;
- API-backed local tools can use an opaque sync token.

On a successful import, ctx stores the cursor emitted by the plugin's `source`
record. Failed runs do not advance the cursor.

`ctx import --history-source ... --reset-cursor` withholds the previous cursor
and sets `CTX_HISTORY_FULL_RESCAN=1`. A reset plugin run must emit a fresh
`source.cursor.after` checkpoint; otherwise ctx rejects the run so an old stored
cursor cannot be reused accidentally.

`ctx search` uses the same freshness model as native provider sources:

- `--refresh background` serves the current index while daemon maintenance
  refreshes enabled auto plugins when possible;
- `--refresh wait` fails if foreground refresh cannot complete;
- `--refresh off` never executes plugin commands.

Provider-filtered search only runs plugin refresh when the provider filter is
`custom` or absent.

## Import And Discovery Behavior

`ctx sources` lists plugin sources without executing plugin commands.

`ctx import --history-source <selector>` runs exactly one matching source.
Selectors can match:

- `plugin/source`;
- `provider_key/source_id`.

The selector must resolve to one source before ctx runs anything.

`ctx import --history-source-manifest <path>` adds a manifest for the current
command without installing it.

`ctx import --all` includes enabled plugin sources, plus discovered native
provider sources.

`ctx setup` does not execute plugin commands.

## Failure Model

Plugin runs fail closed for that run:

- nonzero exit status fails the run;
- invalid stdout fails the run;
- stdout over 64 MiB fails the run;
- stderr over 256 KiB fails the run;
- timeout fails the run;
- source identity mismatches fail before records are imported.

For explicit single-source imports, failures are returned to the user. For
`ctx import --all`, plugin failures can be reported as source failures without
discarding successful imports from other sources. For `ctx search --refresh
background` without daemon ownership, failures are recorded as refresh failures
and search continues against the existing index.

The cursor only advances after a successful source import, so the usual recovery
path is to fix the plugin and run the same command again.

## Security And Trust

This reduces ctx's native schema maintenance burden. It does not make
third-party code harmless. A plugin command is local code. It can read whatever
the current user can read unless the operating system or user wraps it in
additional isolation.

The implemented mitigations are practical guardrails:

- commands are argv arrays, not shell strings;
- ctx clears the environment and re-adds only a small allowlist;
- stdin is closed;
- stdout, stderr, and runtime are bounded;
- cursor files are private temporary files on Unix;
- plugin discovery never executes commands;
- invalid installed manifests are reported by `ctx sources` as non-importable
  rows;
- selectors fail before execution unless they identify exactly one source.

The product should describe plugins as local adapters, not as trusted apps from
ctx.

## Why This Is Smaller Than Native Adapter Expansion

Adding a native adapter requires ctx to own:

- discovery paths;
- native schema parsing;
- incremental logic;
- storage migrations or upstream compatibility breaks;
- tests and fixtures for that provider forever.

The plugin model keeps ctx's owned surface to:

- one manifest schema;
- one stream schema;
- one command runtime;
- one cursor handoff;
- common validation and import behavior.

That is still a public API commitment, but it is a narrower and more durable
commitment than chasing every custom agent database.

## Why Keep Batch File Import

The batch importer uses the same stream parser as plugins. Keeping it provides a
low-friction path for:

- one-off migration;
- local debugging;
- agents that can write a file but cannot easily be invoked by ctx;
- support reproduction cases;
- tests for the stream contract independent of process execution.

The UX distinction should stay clear:

- use a plugin for ongoing search-time refresh;
- use a file for explicit batch import.

## Current Implementation

The current implementation includes:

- `crates/ctx-history-core/src/history_jsonl.rs` for typed
  `ctx-history-jsonl-v1` records;
- custom-history normalization and import in `ctx-history-capture`;
- `crates/ctx-cli/src/history_source_plugins.rs` for manifest discovery,
  command execution, cursor environment, timeout, and output limits;
- CLI support for `--history-source` and `--history-source-manifest`;
- search refresh support for enabled auto plugin sources;
- source listing for valid and invalid installed plugin manifests;
- source-aware search filters for `--history-source`, `--provider-key`,
  `--source-id`, and `--source-format`;
- plugin identity metadata on imported custom sources;
- docs for the stream format and plugin manifest;
- tests for schema round trips, malformed streams, idempotency, cursors,
  discovery, explicit imports, `import --all`, search refresh, failures, and
  timeouts.

## Open Questions Before Shipping

The implementation is mergeable, but these are the product/API questions worth
settling before a stable release:

- Should `ctx-history-jsonl-v1` be documented as stable immediately, or marked
  preview while plugin feedback is collected?
- Should direct file import stay in public CLI help, or be documented as a
  batch/debug path behind the plugin story?
- Should the manifest support a semver range for stream schema versions before
  v2 exists, or is `schema_version: 1` enough for now?
- Should plugin commands receive `CTX_HISTORY_CURSOR_FILE` even when no cursor
  exists, containing a well-known empty value, or is absence simpler?
- Should auto-refresh plugin failures appear more visibly in normal human
  `ctx search` output, or is current best-effort behavior enough?

## Recommendation

Ship this after final API wording review. It solves a real integration problem
with a small local contract, keeps ctx out of third-party storage schemas, and
preserves the native-provider experience of incremental refresh before search.

Keep the batch file importer, but position it as an optional explicit path. The
preferred ongoing integration should remain manifest plus command stdout.
