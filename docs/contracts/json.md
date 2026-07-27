# JSON Contracts

ctx JSON is for local agents and scripts. It can include prompts, command
arguments, typed result identifiers, and local paths. Treat it as private until
a user reviews it.

Command result JSON uses `schema_version: 1` except for `ctx import --json`.
The Pro status object embedded by `ctx status --json` and exposed through MCP
uses its own version 2 contract, described below. Progress-event JSON is stderr
progress output and does not include `schema_version`.

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
- `mode`, either `ready`, `background`, or `catalog_only`;
- `indexed_items`;
- `sources`;
- `inventory`;
- `catalog`;
- `catalog_sources`;
- `import`;
- `background_indexing`;
- `network_required: false`;
- `repo_writes: false`.

`import.ran` is true when setup imports in the foreground, including
`ctx setup --wait` and daemon-disabled or `--no-daemon` runs. It is false when
setup queues background indexing or uses `--catalog-only`. When it runs, `import.outcome`,
`import.failure_scope`, `import.failure_type`, `import.totals`, and
`import.sources` use the same semantics and shapes as `ctx import --json`.
Setup still uses top-level schema version 1; these nested fields are additive.
An all-failed foreground setup import prints the complete JSON result and exits
nonzero. Mixed success and source failures remain successful.

`inventory` reports the shared local-history inventory across all native
sources. It includes `sources`, `units`, `source_files`, `source_bytes`,
`source_import_files`, `indexed_source_import_files`,
`pending_source_import_files`, `failed_source_import_files`,
`stale_source_import_files`, and Codex compatibility counters. The legacy
`catalog` and `catalog_sources` blocks are retained for Codex session catalog
consumers.

`background_indexing.enabled` reports whether setup queued indexing work rather
than whether daemon autostart was requested. Machine-readable setup never
starts or nudges the daemon, so
`background_indexing.daemon_autostart.status` is `not_needed` with reason
`machine_readable_output`. The other `not_needed` reasons are
`explicit_opt_out`, `daemon_disabled`, and `catalog_only`.
Use `ctx daemon status --json` for process state. That status distinguishes the
current config request from the last config the running daemon actually
applied. A semantic opt-in is not live merely because
`background_indexing.semantic_enabled` is true.

`ctx setup` may opportunistically start the ctx-owned background daemon
maintenance profile after setup output when `[daemon].enabled` is true,
including for empty and `--wait` human-readable setup runs. Machine-readable
setup, `ctx setup --no-daemon`, and `ctx setup --catalog-only` do not autostart
daemon maintenance. The daemon, when started, reports
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
- `indexed_sessions`;
- `indexed_events`;
- `inventory_units`;
- `inventory_source_bytes`, null when the data root is uninitialized;
- `lexical_index_estimate_seconds`, null when the data root is uninitialized;
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
- `upgrade`;
- `pro`, using the path-safe Local Pro status shape;
- `local_usage`;
- `local_usage_action`, null unless `--usage enable|disable|reset` was used;
- `local_only: true`;
- `read_only: true`.

For status, `read_only: true` means the command does not mutate canonical
history or local Pro graph data. When Pro is installed, entitlement
authorization may advance nonsecret anti-clock-rollback security metadata in
the operating-system key store; that metadata is outside both data stores and
does not change this stable field. Usage `summary` and `detail` are also excluded
from local usage counting: they do not create or update `usage.sqlite`.
Usage control modes return a separate action-focused JSON shape with
`read_only: false` and do not read Core status.

`local_usage` has `schema_version: 1`, `enabled`, `state`,
`definition_version`, and `retention_days: 400`. `state` is `disabled`, `empty`,
`ready`, or `error`. Ready/empty reports include `summary`:

- `first_day_utc` and `last_day_utc`;
- `active_days` and the bounded `ctx_versions` dimension;
- `calls`, `successful_calls`, and `failed_calls`;
- `result_bearing_calls`, `empty_calls`, and `not_applicable_calls`;
- content-free `result_count` and `citation_count`;
- `mcp_response_bytes`, the exact serialized delivered JSON-RPC line bytes,
  including its newline;
- `pro_blame`, with `produced_attribution_requests`,
  `possible_or_reference_only_requests`,
  `no_confident_attribution_requests`, and `error_requests`, plus exact typed
  `file`/`commit`/`pull_request` breakdowns.

The three result classes reconcile to `calls`; failures are currently
`not_applicable`. `mcp_response_bytes` is transport volume, never tokens,
savings, or model context. `ctx status --usage detail --json` also includes
`details.by_operation[]`, grouped by `ctx_version`, `surface`, and closed
`operation`, plus `details.duration_buckets[]`.

An unavailable store omits `summary` and returns only stable content-free
`error.code`/`error.message` values; it never returns zero as a substitute and
never serializes the raw SQLite/config cause or data-root path. Disabled
operation creates no sidecar. Successful enable/disable controls report
`persisted_enabled`, `effective_enabled`, and `environment_override`; reset
reports `store_state: "cleared"|"missing"`. A failed JSON control exits nonzero
with a parseable, content-free `usage_control_failed` or `usage_reset_failed`
error. Reset is logical deletion, not forensic secure erasure.

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
- `semantic_runtime_active`, true only while the running daemon owns its
  semantic query service;
- `config_reload`, with `status`, `out_of_sync`, `requested`, `applied`,
  `last_attempt_at_ms`, `last_applied_at_ms`, and optional `last_error`;
- `lock_path`;
- `status_path`;
- `jobs`.

`config_reload.status` is `applied`, `pending`, `failed`,
`activation_failed`, or `unknown`. `requested` reflects the current effective
daemon/semantic configuration read by the status command. `applied` is the last
configuration acknowledged by the running daemon. A changed config remains
`pending` with `out_of_sync: true` until the daemon reloads it. Parse/read
failures retain the last applied runtime and report `failed`; inability to
establish newly requested semantic runtime ownership reports
`activation_failed`. `ctx daemon status` remains available while the config is
malformed so this retained failure can be diagnosed.

`daemon.jobs.semantic_index` mirrors live semantic coverage and includes
`status`, `enabled`, `runtime_active`, `semantic_enabled`,
`daemon_configured`, `semantic_configured`, `config_reload_status`,
`configuration_pending`, optional current `reason`, optional
`last_run_at_ms`, optional `last_run_status`, optional `last_run_reason`,
optional `last_error`, optional `indexed_chunks`, `model_cache_available`,
`worker_status`, and `coverage` with `searchable_items`, `completed_items`,
`embedded_items`, `embedded_chunks`, `dirty_items`, and
`queued_items_estimate`. Current
`status`/`reason` are derived from live coverage and daemon runtime ownership;
`last_run_*` fields preserve the persisted result from the last daemon
iteration. `enabled` retains its released meaning: daemon and semantic
configuration are enabled and the platform supports semantic service.
`runtime_active` separately reports observed query-service ownership. Before
reload they therefore may differ. A pending opt-in reports `enabled: true`,
`runtime_active: false`, `status: "pending"`, and
`reason: "daemon_config_reload_pending"`; failed query-service activation
reports `status: "failed"` and `reason: "semantic_activation_failed"`. A config
parse/read failure remains in `config_reload` and does not replace the retained
semantic runtime's job `status`, `reason`, or `last_error`. When the daemon is
disabled for ordinary status reporting, the semantic job reports
`enabled: false`, `status: "disabled"`, and `reason: "daemon_disabled"`.

`ctx daemon status --json` returns `schema_version`, `daemon`, `pro`, and
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
- `scope`, either `default` or `all`;
- `hidden_missing_sources`;
- `sources[]`;
- `issues[]`;
- `issues_truncated`.

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
`import_support` is `native`, `explicit`, or `unsupported`. `native_import`
is derived from `import_support == "native"`; explicit sources therefore report
`native_import: false`. `importable` is true when a source is available and
either native or explicitly importable. Explicit sources require a targeted
provider import and are excluded from setup, `--all`, daemon refresh, and
search refresh. `unknown` means the bounded
provider-specific transcript probe hit its scan budget before proving the
source available or empty. `unsupported_reason` is a string for unsupported,
empty, or unknown rows and otherwise null.

`issues[]` reports provider discovery configurations that could not safely
produce a source row. It is additive to `sources[]`, contains at most 64 rows,
and each row includes `provider`, nullable `path`, stable `code`, `message`,
and `message_truncated`. Messages are capped at 512 UTF-8 bytes. Stable issue
codes are `no_disk_history`, `selector_unreconstructible`, and
`insufficient_official_evidence`. `issues_truncated` is true when additional
issue rows were omitted. Invalid history-source plugin manifests remain
non-importable rows in `sources[]`; they are not provider discovery issues.

## Import

```bash
ctx import --json
ctx import --json --no-daemon
```

Writes the local SQLite index and returns:

- `schema_version`;
- `outcome`;
- `failure_scope`;
- `failure_type`;
- `resume`;
- `resume_mode`;
- `totals`;
- `sources[]`.

Import result schema version 2 uses source statuses `success`,
`completed_with_rejections`, and `failure`. Run-level `outcome` uses `success`,
`completed_with_rejections`, `completed_with_source_failures`,
`completed_with_rejections_and_source_failures`, or `failure`.
`failure_scope` in an import result is `none`, `record`, `source`, or
`record_and_source`; `failure_type` is a coarse, non-sensitive classification.
Ctx-owned storage, index, worker, and operational-I/O failures abort before an
import-result JSON object is emitted and exit nonzero. Provider database
corruption, locks, unreadable source files, and malformed source data are
source failures and do not stop independent sources. Failed sources can include
up to five rejection details when every content record was rejected.

`totals` and each source row include `change`, whose value is `changed` or
`no_op`, plus file, byte, session, event, edge, skipped, and
`rejected_records` counts. `change` reports whether canonical source work
changed; it is independent of insert counters. A deterministic source
replacement can therefore report `change: "changed"`, zero newly imported
events, and skipped existing events while reconciling those rows in place.
Rejection details are exposed as `rejections`
(bounded to five entries); `failed_sources` remains the count of source-level
failures. `sources_completed_with_rejections` counts sources that committed
accepted content while rejecting other records. `resume_mode` is currently `idempotent_rescan` when
`--resume` is passed and `normal_scan` otherwise.

Human-readable native imports that target discovered/default provider sources
may opportunistically start the ctx-owned background daemon maintenance profile
after foreground import work when `[daemon].enabled` is true. JSON output never
starts or nudges the daemon. `ctx import --no-daemon`, custom JSONL imports, and explicit
history-source-only imports do not autostart daemon maintenance. The daemon, when started, reports
`start_mode: "auto"` and `trigger_command: "import"` through status surfaces.
Import result schema version 2 does not embed daemon process state. Use
`ctx daemon status --json` to inspect an already-running or explicitly started
daemon.

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
- `payload_type`, either `session_transcript` or `event_window`;
- `mode` for session transcripts;
- `format`;
- `content_policy`, either `indexed` (the default) or `complete`;
- `session` for session output;
- `event` for event output;
- `source`;
- `events[]`.

`session` includes the ctx-owned `item_id`, `record_type`, `provider`, and
`provider_session_id` when known. `event` and `events[]` rows include
`ctx_event_id`, `record_type`, `ctx_session_id`, `sequence`, `event_type`,
`role`, `occurred_at`, `source`, `cursor`, and `text` or `preview`. Each
rendered event also includes `content` with `requested`, `complete`, `origin`,
`stored_truncated`, and `source_verified`. `origin` is `ctx_index` or
`provider_source`.

Complete-content failures are all-or-nothing. JSON mode writes no transcript
and reports a stable error object containing `error`, `error_code`,
`ctx_event_id`, `retryable`, and a `ctx locate event` remediation command.
Current error codes are `source_missing`, `source_unreadable`, `source_changed`,
`hydration_unsupported`, `source_record_missing`, `content_too_large`, and
`content_verification_failed`.

## Locate

```bash
ctx locate session <ctx-session-id> --format json
ctx locate event <ctx-event-id> --format json
```

Writes nothing and returns provenance metadata:

- `schema_version`;
- `payload_type`, either `session_location` or `event_location`;
- `ctx_session_id`;
- `ctx_event_id` for event output;
- `provider`;
- `provider_session_id` when known;
- `source`;
- `resume`.

`source` includes `path`, `cursor`, `exists`, `source_id`, and
`source_format` when known. `resume` includes provider cursor or import resume
metadata when available.
Event locations can additionally include `source_record`,
`complete_content.available`, `complete_content.source_family`, and
`complete_content.locator_kind`. They do not expose locator bytes or complete
body digests.

## Transcript Artifacts

```bash
ctx show session <ctx-session-id> --mode full --format json --out transcript.json
```

With `--out`, writes the requested transcript artifact to that path and prints
nothing on success. Without `--out`, stdout is the requested transcript
artifact. JSON and JSONL artifact rows use the same ctx-owned ID fields as
`show`; JSONL rows include `payload_type: "session_transcript_event"` and wrap
the transcript row in `event`.

## Search

```bash
ctx search <query>|--term <term>|--file <path> --json
```

Returns:

- `schema_version`;
- `payload_type: "search_results"`;
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
- `result_type`, the concrete hit kind such as `event`, `session`,
  `session_result`, or `indexed_item`;
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
- `payload_type: "sql_result"`;
- `read_only: true`;
- `share_safe: false`;
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

`share_safe` is required and is always `false` for schema-version-1 SQL
results. `read_only: true` describes database mutation only; selected rows can
still contain prompts, transcript content, command arguments, and local paths.
Clients must not infer that SQL output is safe to share from its read-only
status.

Use stable `ctx_*` views for scripts when possible: `ctx_sessions`,
`ctx_events`, `ctx_files_touched`, and `ctx_sources`. Internal tables remain
queryable for advanced local inspection but are not the preferred compatibility
surface.

## MCP Tool Results

`ctx mcp serve` exposes read-only MCP tools over stdio for status, sources,
search, SQL, showing sessions and events, and Pro status. Pro blame can perform
bounded local catch-up that updates the canonical Core index, writes the
encrypted derived Pro graph, and writes the projection acknowledgement. It
never writes provider history or repositories. Tool results include
`structuredContent` JSON using the same private local fields as CLI JSON. MCP
output may include absolute paths, source metadata, snippets, and transcript
text, and the MCP host may log or forward it.

MCP search does not refresh or import provider history and currently uses the
lexical search path only. It also excludes the active Codex session tree by
default when `CODEX_THREAD_ID` is set; pass `include_current_session: true` to
opt back in.

The MCP `sources` tool includes the same bounded `issues` and
`issues_truncated` fields as `ctx sources --json`.

The MCP `sql` tool uses the same `sql_result` JSON contract as `ctx sql
--json`, always read-only. Its `structuredContent` must include the same
required `read_only: true` and `share_safe: false` fields and preserve the CLI
column, row, truncation, and limit semantics. CLI and MCP consumers must treat
a missing or non-false `share_safe` value as incompatible SQL-result output,
not as permission to share it.

Tool-level argument validation failures set `isError: true`, preserve the
diagnostic `error`, and add stable `error_code: "invalid_request"` in
`structuredContent`. JSON-RPC framing and envelope failures retain the
protocol-level parse-error or invalid-params responses.

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

Daemon-owned automatic upgrade does not write JSON to foreground stdout. Its
single scheduler state and replacement journal live beside the managed
executable. Windows self-upgrade can report `scheduled` with `applied: false`
while a helper waits for the running `ctx.exe` to exit and then replaces the
binary and sidecar.

## Citation Fields

Citations can include:

- `item_id`;
- `target_type`;
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

## Local Pro

The `pro` object in `ctx status --json` has `schema_version: 2`,
`payload_type: "pro_status"`,
`state`, `installed`, `ready`, `materialized`, `helper_version`,
`protocol_version`, `capabilities`, `error_code`, `access_state`,
`refresh_after_unix`, `access_deadline_unix`, `grace_deadline_unix`, and a typed
`next_action`. `ctx status` adds nullable `conversion_action`. Access fields are
null when access cannot be determined. The
generic `state` remains helper/graph readiness; `access_state` is independently
`trial`, `active`, `canceling_paid`, `offline_grace`, or `locked`.
After an uninstall that deliberately preserves local Pro data, `state` is
`uninstalled_data_preserved` and `next_action.reason` is
`restore_preserved_pro_data`; a first-use installation remains `not_setup` with
`helper_missing`.
The same base path-safe shape is returned by the MCP `pro_status` tool and
embedded by `ctx doctor --json` under `pro`. MCP `pro_status` also adds
`conversion_action` and `local_usage`; doctor does not.

`conversion_action` is `pro_monthly_conversion` at `"$15/month"` for `trial`
or an unpriced `pro_restore_access` for `locked`, both pointing to
`ctx pro manage`. The restore action includes `graph_preserved: true` and
`reason: "access_locked"`. It is null for paid `active`,
`canceling_paid`, and `offline_grace` states and does not replace `next_action`.
MCP `pro_status` also embeds the compact `local_usage` report; neither field is
added to blame results or citations.

`ctx pro --json` and its explicit synonym `ctx pro setup --json` both run the
idempotent setup path, report operation `setup`, and return the
`schema_version: 1`, `payload_type: "pro_setup"` contract.
`ctx pro manage --no-open --json` and
`ctx pro uninstall (--delete-data|--keep-data) --json` return the `pro_manage`
and `pro_uninstall` payload types respectively.
Materialization is an internal,
idempotent part of setup, daemon freshness, and blame catch-up.
The `pro_manage` payload includes `portal_url`, `browser_opened`, the compact
`local_usage` report, `conversion_action`, and the same nonsecret access
state/deadline fields. A locked account preserves canonical history, encrypted
derived data, and keys; successful resubscription followed by `ctx pro`
restores access. JSON mode never invokes a browser opener and always reports
`browser_opened: false`.
`pro_uninstall` reports `helper_removed`, `local_pro_data` (`preserved`,
`deleted`, or `absent`), `canonical_history_preserved`, and `next_action`.
Explicit `--delete-data` reports `local_pro_data: "deleted"` only after the
authoritative local Pro inventory has been verified absent. JSON callers must
provide one of the two data-choice flags and are never prompted. Missing,
never-Pro, and already-empty roots report `absent` with `next_action: null` and
do not create a Pro root or preservation marker. This is a Pro-state-only no-op
contract: the eligible foreground `pro_uninstall` operation may still create or
increment default-on Core `usage.sqlite`. `absent` means no graph-family file
existed at deletion time; an initialized or helper-present root can still
delete and verify root-scoped credentials and graph-key records before
returning that classification. Corrupt credential inventory fails before any
deletion and emits no success payload. An interrupted deletion retains
root-local retry metadata; setup and `--keep-data` fail until a later
`--delete-data` verifies and completes the same installation-scoped cleanup.

Successful `ctx blame file|commit|pr --json` and MCP `blame` return the protocol
`BlameResult` directly, with no payload wrapper, prose summary, or suggested
claims:

- `target`, a resolved tagged `file`, `commit`, or `pull_request` target;
- `git_snapshot`, required for file results and null for commit/PR results;
- `matches`, typed `file`, `commit`, or `pull_request` matches corresponding to
  the resolved target;
- `evidence`, a complete deduplicated table numbered contiguously from one;
- `next`, null or an opaque cursor plus `more_matches` or
  `more_committed_lines` reason.

File matches contain an inclusive committed line range, commit reference,
line-level evidence numbers, and zero or more typed production attributions.
Commit matches use a closed fact type and predicate vocabulary and preserve
confidence and state. Human rendering groups them as `Produced by`,
`Possible producers`, and `Also recorded`, so inspection and reference facts
cannot be mistaken for production.

PR matches contain exactly one activity or commit-membership relationship.
Activity actions are typed and remain separate from production. A PR commit
relationship is present only when recognized structured captured forge evidence
binds the canonical PR identity and exact Git object ID in the same record.
Co-occurring standalone URL/OID identifiers or prose are insufficient; when no
structured proof exists, associated commits are explicitly unproven.

Each evidence-number list is nonempty, sorted, unique, and resolves into the
page's evidence table. Every table entry is referenced by at least one returned
match. MCP returns at most 8 complete matches per page; CLI defaults to 20 and
permits at most 100. MCP additionally caps the final serialized blame JSON-RPC
response at 1 MiB after adding exact `structuredContent` and its text fallback.
An over-cap helper page fails with `invalid_response` and guidance to lower
`limit` or use CLI JSON; MCP does not truncate matches or evidence and does not
invent a continuation cursor. Under the cap, typed structured content is exact.
Neither surface clips evidence for a returned match.
Continuation cursors are authenticated and bound to the request and graph
state. Tampering returns `invalid_request`; a changed snapshot returns
`stale_snapshot`.

OSS `show` and `locate` JSON for session/event retrieval is unchanged. There
are no Pro `show`, `locate`, `timeline`, `facts`, or `related` payloads or
compatibility aliases.

CLI failures exit nonzero with a stable error token on stderr. MCP failures set
`isError: true` and return `error` plus `error_code` in `structuredContent`.
Stable codes include `pro_not_installed`, `commercial_unavailable`,
`entitlement_expired`, `helper_upgrade_required`, `key_store_unavailable`,
`key_store_locked`, `not_materialized`, `protocol_mismatch`,
`source_unavailable`, `repository_unavailable`, `line_out_of_range`,
`stale_snapshot`, `stale_fact`, `ambiguous`, `corrupt_graph`,
`invalid_request`, `invalid_response`, `cancelled`,
`helper_crashed`, and `helper_timeout`.
Native key-store failures use only `key_store_unavailable` and
`key_store_locked`. Unshipped `credential_vault_*` spellings are not aliases.

## Doctor

```bash
ctx doctor --json
```

Reads local storage and returns findings:

- `schema_version`;
- `ok`;
- `progress`;
- `findings`.

Doctor checks the main SQLite store, read-only semantic sidecar health, and an
installed Pro helper. Its JSON includes `daemon` and `pro` status. It
does not initialize embedding models or write sidecar data. Semantic or hybrid
search may ask the daemon query service to embed the query from an
already-cached local model; search does not download models or write sidecar
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
