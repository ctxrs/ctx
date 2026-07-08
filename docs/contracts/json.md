# JSON Contracts

ctx JSON is for local agents and scripts. It can include prompts, command
output previews, and local paths. Treat it as private until a user reviews it.

Command result JSON currently uses `schema_version: 1`. Progress-event JSON is
stderr progress output and does not include `schema_version`.

## Setup

```bash
ctx setup --json
ctx setup --json --no-daemon
```

Writes local storage and returns:

- `schema_version`;
- `data_root`;
- `database_path`;
- `config_path`;
- `mode`, either `ready` or `catalog_only`;
- `indexed_items`;
- `sources`;
- `inventory`;
- `catalog`;
- `catalog_sources`;
- `import`;
- `network_required: false`;
- `repo_writes: false`.

`import.ran` is true for the default setup path and false for
`ctx setup --catalog-only`. When it runs, `import.totals` and `import.sources`
use the same shape as `ctx import --json`.

`inventory` reports the shared local-history inventory across all native
sources. It includes `sources`, `units`, `source_files`, `source_bytes`,
`source_import_files`, `indexed_source_import_files`,
`pending_source_import_files`, `failed_source_import_files`,
`stale_source_import_files`, and Codex compatibility counters. The legacy
`catalog` and `catalog_sources` blocks are retained for Codex session catalog
consumers.

Non-JSON `ctx setup` may opportunistically start the ctx-owned background daemon
maintenance profile after foreground setup work when `[daemon].enabled` is true.
`ctx setup --no-daemon`, `ctx setup --catalog-only`, and any `ctx setup --json`
run do not autostart daemon maintenance. The daemon, when started, reports
`start_mode: "auto"` and `trigger_command: "setup"` through status surfaces.

## Status

```bash
ctx status --json
```

Reads local storage state and returns:

- `schema_version`;
- `initialized`;
- `data_root`;
- `database_path`;
- `config_path`;
- `indexed_items`;
- `indexed_sources`;
- `inventory_units`;
- `pending_inventory_units`;
- `failed_inventory_units`;
- `stale_inventory_units`;
- `cataloged_sessions`;
- `indexed_catalog_sessions`;
- `pending_catalog_sessions`;
- `failed_catalog_sessions`;
- `stale_catalog_sessions`;
- `source_import_files`;
- `indexed_source_import_files`;
- `pending_source_import_files`;
- `failed_source_import_files`;
- `stale_source_import_files`;
- `semantic`;
- `daemon`;
- `local_only: true`;
- `read_only: true`.

`semantic` reports semantic sidecar and background-worker state. Fields listed
as nullable may be omitted when unavailable:

- `status`;
- `running`;
- `pid`, nullable/omitted;
- `started_at_ms`, `heartbeat_at_ms`, and `finished_at_ms`, nullable/omitted;
- `indexed_chunks`, nullable/omitted;
- `model_init_ms`, nullable/omitted;
- `last_error`, nullable/omitted;
- `coverage`;
- `model_cache_available`, true when the local embedding model cache needed by
  the default background semantic worker is already present.

Raw local CLI output may also include diagnostic paths such as `vector_path`,
`lock_path`, and `status_path`. These are absolute paths on the current machine
for troubleshooting the local sidecar/worker. They are not portable identifiers,
may be omitted by adapters, and should not be persisted or forwarded outside
local diagnostics.

`semantic.status` is one of:

- `unknown`, no initialized ctx store is available for live coverage;
- `empty`, the store has no semantic-eligible items;
- `pending`, semantic-eligible items exist but the sidecar is missing, behind,
  or has dirty/stale items queued for re-embedding;
- `ready`, sidecar coverage matches the current searchable item count and the
  dirty queue is empty;
- `running`, the background worker lock belongs to a live process;
- `stale_lock`, a worker lock exists but the recorded process is not live;
- `failed`, the last worker run failed and recorded `last_error`;
- `unavailable`, the sidecar cannot be opened/read by this ctx build;
- `budget_exhausted`, the worker indexed a bounded batch and left queued work.

`semantic.coverage` includes `searchable_items`, `embedded_items`,
`embedded_chunks`, `dirty_items`, `queued_items_estimate`, and
`coverage_ratio`. `dirty_items` counts already-known events whose semantic
vectors may be stale after import or daemon startup freshness checks.

`daemon` reports the ctx-owned background coordinator state. Fields listed as
nullable may be omitted when unavailable:

- `enabled`;
- `status`, one of `unknown`, `disabled`, `running`, `completed`, `failed`, or
  `stale_lock`;
- `running`;
- `pid`, nullable/omitted;
- `started_at_ms`, `heartbeat_at_ms`, and `finished_at_ms`, nullable/omitted;
- `last_error`, nullable/omitted;
- `start_mode`, nullable/omitted, currently `auto` for setup/import autostarts
  or `manual` for explicit daemon runs;
- `trigger_command`, nullable/omitted, currently `setup` or `import` for
  automatic starts;
- `lock_path`;
- `status_path`;
- `jobs`.

`daemon.jobs.semantic_index` mirrors live semantic coverage and includes
`status`, `enabled`, optional current `reason`, optional
`last_run_at_ms`, optional `last_run_status`, optional `last_run_reason`,
optional `last_error`, optional `indexed_chunks`, `model_cache_available`,
`worker_status`, and `coverage` with `searchable_items`, `completed_items`,
`embedded_items`, `embedded_chunks`, `dirty_items`, and
`queued_items_estimate`. Current
`status`/`reason` are derived from live coverage; `last_run_*` fields preserve
the persisted result from the last daemon iteration. When the daemon is disabled
for ordinary status reporting, the semantic job reports `enabled: false`,
`status: "disabled"`, and `reason: "daemon_disabled"`.

`daemon.jobs.cloud_sync` currently reports `status: "disabled"`,
`enabled: false`, `reason: "not_configured"`, `network_allowed: false`,
nullable/omitted `last_upload_at_ms`, and `queued_items_estimate: 0`.

`ctx daemon status --json` returns `schema_version`, `daemon`, and
`local_only`. `ctx daemon enable --json` and `ctx daemon disable --json` return
`schema_version`, `daemon_enabled`, `config_path`, and `local_only`.
`ctx daemon run --json` returns the daemon object directly. The legacy hidden
`__ctx-daemon` entry point follows the same run output for compatibility.

`ctx doctor --json` returns `schema_version`, `ok`, `progress`, `findings`, and
the same top-level `daemon` object used by status so callers can inspect daemon
lifecycle and job state without parsing human findings.

## Sources

```bash
ctx sources --json
```

Returns:

- `schema_version`;
- `sources[]`.

Each source includes:

- `provider`;
- `path`;
- `exists`;
- `source_format`;
- `status`;
- `import_support`;
- `native_import`;
- `importable`;
- `unsupported_reason`.

`status` is `available`, `empty`, `unknown`, `missing`, or `unsupported`.
`import_support` is `native` or `unsupported`. `native_import` is a boolean
derived from `import_support == "native"`. `importable` is true only when the
source is both available and natively importable. `unknown` means the bounded
provider-specific transcript probe hit its scan budget before proving the
source available or empty. `unsupported_reason` is a string for unsupported,
empty, or unknown rows and otherwise null.

## Import

```bash
ctx import --json
ctx import --json --no-daemon
```

Writes the local SQLite index and returns:

- `schema_version`;
- `resume`;
- `resume_mode`;
- `totals`;
- `sources[]`.

`totals` and each source row include file, byte, session, event, edge, skipped,
and failed counts. `resume_mode` is currently `idempotent_rescan` when
`--resume` is passed and `normal_scan` otherwise.

Non-JSON native imports that target discovered/default provider sources may
opportunistically start the ctx-owned background daemon maintenance profile
after foreground import work when `[daemon].enabled` is true. `ctx import --no-daemon`, custom JSONL
imports, explicit history-source-only imports, and any `ctx import --json` run
do not autostart daemon maintenance. The daemon, when started, reports
`start_mode: "auto"` and `trigger_command: "import"` through status surfaces.

## Progress

```bash
ctx setup --progress json
ctx import --progress json
ctx import --json --progress json
```

`--progress json` writes newline-delimited progress objects to stderr for
`setup` and `import`. It does not change command result stdout. This means
`ctx setup --json --progress json` and `ctx import --json --progress json`
write the command result object to stdout and zero or more progress objects to
stderr.

Each progress object includes:

- `type: "ctx_progress"`;
- `operation`, currently `setup` or `import`;
- `phase`;
- `message`;
- `completed_bytes`;
- `total_bytes`;
- `percent`;
- `elapsed_seconds`;
- `eta_seconds`, nullable when no estimate is available or the operation is
  complete;
- `completed_files`, nullable;
- `total_files`, nullable;
- `imported_events`, nullable;
- `done`.

Progress events are operational status events, not durable result records.
Consumers should key on `type` and `operation`, ignore unknown fields, and read
the final command result from stdout when `--json` is present.

## Show

```bash
ctx show session <ctx-session-id> --format json
ctx show event <ctx-event-id> --format json
```

Writes nothing and returns:

- `schema_version`;
- `item_type`, either `session_transcript` or `event_window`;
- `mode` for session transcripts;
- `format`;
- `session` for session output;
- `event` for event output;
- `source`;
- `events[]`.

`session` includes the ctx-owned `item_id`, `provider`, and
`provider_session_id` when known. `event` and `events[]` rows include
`ctx_event_id`, `ctx_session_id`, `sequence`, `event_type`, `role`,
`occurred_at`, `source`, `cursor`, and `text` or `preview`.

## Locate

```bash
ctx locate session <ctx-session-id> --format json
ctx locate event <ctx-event-id> --format json
```

Writes nothing and returns provenance metadata:

- `schema_version`;
- `item_type`, either `session_location` or `event_location`;
- `ctx_session_id`;
- `ctx_event_id` for event output;
- `provider`;
- `provider_session_id` when known;
- `source`;
- `resume`.

`source` includes `path`, `cursor`, `exists`, `source_id`, and
`source_format` when known. `resume` includes provider cursor or import resume
metadata when available.

## Transcript Artifacts

```bash
ctx show session <ctx-session-id> --mode full --format json --out transcript.json
```

With `--out`, writes the requested transcript artifact to that path and prints
nothing on success. Without `--out`, stdout is the requested transcript
artifact. JSON and JSONL artifact rows use the same ctx-owned ID fields as
`show`.

## Search

```bash
ctx search <query>|--term <term>|--file <path> --json
```

Returns:

- `schema_version`;
- `query`;
- `filters`;
- `freshness`;
- `retrieval`;
- `generated_at`;
- `results[]`;
- `pagination`;
- `truncation`.

Each result can include:

- `ctx_event_id` for event hits;
- `ctx_session_id` when known;
- `provider_session_id`;
- `event_seq`;
- `title`;
- `snippet`;
- `rank`;
- `result_scope`, either `session` for a session-level result or `event` for an
  event-level result;
- `session_importance` for default session results;
- `more_matches_in_session` for default session results;
- `provider`;
- `timestamp`;
- `cwd`;
- `source_path`;
- `source_exists`;
- `cursor`;
- `why_matched`;
- `citations[]`;
- `suggested_next_commands[]`;
- `visibility`.

`why_matched[]` can include text, metadata, or touched-file reasons. A touched
file match is backed by normalized touched-file storage and can appear when
search uses `--file <path>` or when file-path metadata contributes to ranking.
`citations[]` can cite sessions, events, files, or source metadata depending on
which indexed item produced the match.

Search JSON is local/private by default.

`freshness` describes the pre-search refresh attempt:

- `mode`, one of `background`, `off`, or `wait`;
- `status`, such as `completed`, `skipped`, `no_sources`, `read_only`,
  `budget_exhausted`, or `failed`. `read_only` means foreground refresh skipped
  writes because the existing index is readable but not writable by this binary,
  or because daemon background refresh owns freshness for this command;
  `budget_exhausted` means foreground refresh imported a bounded batch and served
  results while leaving more backlog for a later search or `--refresh wait`;
- `reason`, present for explanatory read-only or skipped states;
- `budget_reasons`, present when `status` is `budget_exhausted`; stable
  machine-readable reasons include `codex_session_limit`,
  `codex_discovery_file_limit`, `manifest_file_limit`, `single_file_bytes`, and
  `total_bytes`;
- `source_count`;
- `daemon_last_run_at_ms`, present when search relies on a recent daemon refresh;
- `totals`, using the same import total fields as `ctx import --json`;
- `error`, present when refresh failed but results were still served.

`retrieval` describes the requested and effective search path:

- `requested_mode`, one of `hybrid`, `semantic`, or `lexical`;
- `effective_mode`, one of `lexical`, `semantic`, or `hybrid`;
- `semantic_weight`, the effective semantic contribution used for ranking. It
  is `0.0` when the effective mode is lexical, even if a semantic weight was
  requested;
- `semantic_status`;
- `semantic_fallback_code`, nullable/omitted stable reason code for clients;
- `semantic_fallback`, nullable/omitted;
- `embedding_model`, nullable/omitted;
- `coverage`;
- `worker`, using the same shape as `status.semantic`, nullable/omitted;
- `diagnostics`, nullable/omitted and present when semantic vector retrieval
  runs.

`retrieval.semantic_status` is one of:

- `skipped`, lexical retrieval was used and no semantic lookup ran;
- `unavailable`, the semantic sidecar is missing, empty, unreadable, or otherwise
  not usable for the request;
- `partial`, some but not all searchable items have embeddings;
- `ready`, sidecar coverage is complete for the current searchable item count
  and dirty work is drained.

`retrieval.semantic_fallback_code`, when present, is the stable machine-readable
reason why the requested semantic/hybrid path degraded to lexical.
`retrieval.semantic_fallback`, when present, is the human-readable explanation.

`retrieval.coverage` includes `embedded_items`, `embedded_chunks`,
`searchable_items`, `indexed_now`, and `dirty_items` when known. Coverage counts
are numbers when present; null count fields are pruned from public SDK fixtures
and typed SDK shapes.

The SDK `agent-history-v1` contract camel-cases the same retrieval fields
(`requestedMode`, `effectiveMode`, `semanticWeight`, and so on). SDK contract
search results expose retrieval at the top level of `search`; TypeScript and
Python type the core retrieval/coverage fields, while Go, .NET, JVM, and Swift
preserve retrieval as camel-cased JSON values. Per-hit retrieval details are not
part of v1 unless a future CLI JSON shape emits them. Local diagnostic path
fields such as `vector_path`/`vectorPath` can still appear as additive JSON from
the local CLI adapter, but they are intentionally not stable SDK fields.

`retrieval.diagnostics` can include `query_embed_ms`, `vector_backend`,
`vector_scan_ms`, `chunks_scanned`, `vector_bytes_read`, `events_scored`,
`hydration_ms`, `stale_events_dropped`, and `semantic_candidates`. These fields
are local performance diagnostics and can reveal corpus size/timing; treat them
as private like the rest of search JSON.

`suggested_next_commands` can include `ctx show event`, `ctx show session`,
`ctx search "<query>" --session <ctx-session-id>`, `ctx locate event`, and
`ctx locate session` command strings when the required ctx IDs are known.

When ctx can identify the active Codex provider session through
`CODEX_THREAD_ID`, search filters include `exclude_provider_session` and omit
that active session tree by default. Passing `--include-current-session` removes
that filter.

## SQL

```bash
ctx sql "SELECT COUNT(*) AS sessions FROM ctx_sessions" --json
ctx sql --file query.sql --format json
```

Runs one read-only SQL statement against the existing local SQLite index and
returns:

- `schema_version`;
- `item_type: "sql_result"`;
- `read_only: true`;
- `columns[]`, ordered selected column names;
- `rows[]`, ordered arrays matching `columns[]`;
- `returned_rows`;
- `truncated.rows`;
- `truncated.values`;
- `limits.max_rows`;
- `limits.max_columns`;
- `limits.max_value_bytes`;
- `limits.max_sql_bytes`;
- `limits.timeout_ms`;
- `elapsed_ms`.

Scalar SQL values are encoded as JSON nulls, numbers, or strings when they fit
the configured value cap. Truncated text values are encoded as objects with
`type: "text"`, `value`, `bytes`, and `truncated: true`. Blob values are
encoded as objects with `type: "blob"`, `bytes`, `preview_hex`, and
`truncated`.

Use stable `ctx_*` views for scripts when possible: `ctx_sessions`,
`ctx_events`, `ctx_files_touched`, and `ctx_sources`. Internal tables remain
queryable for advanced local inspection but are not the preferred compatibility
surface.

## MCP Tool Results

`ctx mcp serve` exposes read-only MCP tools over stdio for status, sources,
search, SQL, showing sessions, and showing events. Tool results include
`structuredContent` JSON using the same private local fields as CLI JSON. MCP
output may include absolute paths, source metadata, snippets, and transcript
text, and the MCP host may log or forward it.

MCP search does not refresh or import provider history and currently uses the
lexical search path only. It also excludes the active Codex session tree by
default when `CODEX_THREAD_ID` is set; pass `include_current_session: true` to
opt back in.

The MCP `sql` tool uses the same `sql_result` JSON contract as `ctx sql
--json`, always read-only.

## Integrations

```bash
ctx integrations install mcp --json
ctx integrations status mcp --json
```

MCP integration JSON returns:

- `integration`, currently `mcp`;
- `server.name`, `server.command`, and `server.args`;
- `scope`, either `global` or `project`;
- `results[]`.

Each install result includes:

- `agent`;
- `agent_display_name`;
- `scope`;
- `path`, or null for unsupported targets;
- `detected`;
- `supported`;
- `success`;
- `previous_status`;
- `status`;
- `already_installed`;
- `modified`;
- `error`.

Each status result uses the same target fields and includes `status` and
`error`. Status values are `current`, `missing`, `conflict`, `invalid_config`,
and `unsupported`.

## Docs

```bash
ctx docs list --json
ctx docs search <query> --json
ctx docs show <topic> --format json
```

`ctx docs list --json` returns:

- `schema_version`;
- `topics[]`.

Each topic includes `id`, `title`, `audience`, `summary`, `tags`, and
`source_path`.

`ctx docs search <query> --json` returns:

- `schema_version`;
- `query`;
- `results[]`.

Each result uses the topic fields above and adds `score`.

`ctx docs show <topic> --format json` returns one topic object plus:

- `schema_version`;
- `body`, containing the embedded markdown source.

Docs JSON is generated from embedded static docs and does not read provider
history or SQLite.

## Upgrade

```bash
ctx upgrade --json
ctx upgrade --dry-run --json
ctx upgrade check --json
ctx upgrade status --json
```

`ctx upgrade` and `ctx upgrade check` return:

- `schema_version`;
- `command`, either `upgrade` or `upgrade_check`;
- `ok`;
- `status`, such as `available`, `up_to_date`, `dry_run`, `applied`, or
  `scheduled`;
- `message`;
- `current_version`;
- `latest_version`;
- `update_available`;
- `channel`;
- `platform`;
- `metadata_url`;
- `artifact_url`;
- `install_path`;
- `managed`;
- `applied`;
- `dry_run`;
- `warnings[]`.

`ctx upgrade status --json` returns:

- `schema_version`;
- `command: "upgrade_status"`;
- `state`;
- `install`.

`state` is the last local upgrade-state object when present, or
`status: "never_checked"`. `install.managed` is true only when the running
binary has a matching official installer sidecar. Unmanaged installs report
`managed: false` and a `reason`.

Background upgrade checks do not write JSON to stdout. They write
`upgrade-state.json` and `logs/upgrade.log` under the ctx data root. Windows
self-upgrade can report `scheduled` with `applied: false` while a helper waits
for the running `ctx.exe` to exit and then replaces the binary and sidecar.

## Citation Fields

Citations can include:

- `item_id`;
- `item_type`;
- `ctx_event_id`;
- `ctx_session_id`;
- `label`;
- `time`;
- `provider`;
- `session_id`;
- `event_seq`;
- `source_path`;
- `source_exists`;
- `cursor`.

`source_exists: false` means indexed text is available but the raw source
was not present at the stored path when checked.

## Doctor

```bash
ctx doctor --json
```

Reads local storage and returns findings:

- `schema_version`;
- `ok`;
- `progress`;
- `findings`.

Doctor checks the main SQLite store plus read-only semantic sidecar health. It
does not initialize embedding models or write sidecar data. Search may
initialize an already-cached local embedding model only for explicit
semantic/hybrid query embedding; it does not download models or write sidecar
data from the search path.

## Provider Smoke

Provider smoke tests call normal `ctx` commands with temporary local storage and
static fixtures. Their output is ordinary command JSON covered by the command
schemas above; there is no separate provider artifact schema in the public CLI.

## Compatibility Limits

Compatibility `item_id`, `id`, `session_id`, and `event_id` fields can remain
in some outputs. New integrations should prefer ctx-owned `ctx_session_id` and
`ctx_event_id` where present, and should treat provider-owned IDs as metadata
unless an explicit provider lookup flag is present.
