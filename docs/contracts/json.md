# JSON Contracts

ctx JSON is for local agents and scripts. It can include prompts, command
arguments, typed result identifiers, and local paths. Treat it as private until
a user reviews it.

Command result JSON uses `schema_version: 1` except for
`ctx setup --format json`, `ctx stats --format json`, and
`ctx import --format json`.
The Pro status object embedded by `ctx status --format json` and exposed through MCP
uses its own version 2 contract, described below. Progress-event JSON is stderr
progress output and does not include `schema_version`.

## Setup

```bash
ctx setup --format json
ctx setup --format json --no-daemon
```

Writes local storage and returns schema version 2:

- `schema_version`;
- `data_root`;
- `config_path`;
- `mode`, one of `ready`, `pending`, `stale`, or `unavailable`;
- `history_epoch`;
- `lexical`;
- `catalog`;
- `refresh`;
- `refresh_request`;
- `semantic`;
- `relational`;
- `pro_projection`;
- `daemon`;
- `daemon_autostart`;
- `deprecated_catalog_only_ignored`;
- `source_rebuild_required`;
- `network_required: false`;
- `repo_writes: false`.

When daemon maintenance is enabled, human and machine-readable setup both
health-check and recover the persistent daemon before returning.
`daemon_autostart.status: "verified"` includes the live PID. A one-run
`--no-daemon` opt-out reports `status: "not_requested"` and reason
`explicit_opt_out`; a durable disabled configuration uses reason
`daemon_disabled`. `refresh_request` separately reports whether setup queued
or waited for daemon-owned Core publication. A completed `--wait` request
also includes its request-bound terminal `receipt`; callers should use that
receipt rather than a later periodic daemon job when reporting the setup run.

Setup does not perform a foreground provider import. `--wait` waits for the
daemon-owned Core refresh; without it, setup requests a background Core
refresh. The deprecated `--catalog-only` flag is reported by
`deprecated_catalog_only_ignored` and does not change the persistent lifecycle.
Use `ctx daemon status --format json` for the complete process and applied
configuration state.

## Status

```bash
ctx status --format json
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
- compact `local_usage` health (`enabled`, state, and a content-free error when
  unavailable), without aggregates, estimates, or operation details;
- `local_only: true`;
- `read_only: true`.

For status, `read_only: true` means the command does not mutate canonical
history or local Pro graph data. When Pro is installed, entitlement
authorization may advance nonsecret anti-clock-rollback security metadata in
the operating-system key store; that metadata is outside both data stores and
does not change this stable field. Usage control modes return a separate
action-focused JSON shape with `read_only: false` and do not read Core status.

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

`daemon.jobs.history_refresh.rejection_diagnostics` preserves aggregate
`rejected_records` and `sources_completed_with_rejections` from the latest
completed observation of each currently discovered history source. These
diagnostics survive later healthy source cycles and daemon restarts, clear when
that source is observed without rejections, and do not make the daemon fail.
Source-level failures remain terminal and are reported separately.

`ctx daemon status --format json` returns `schema_version`, `daemon`, `pro`, and
`local_only`. `ctx daemon enable --format json` and `ctx daemon disable --format json` return
`schema_version`, `daemon_enabled`, `config_path`, and `local_only`.
`ctx daemon run --format json` returns the daemon object directly. The legacy hidden
`__ctx-daemon` entry point follows the same run output for compatibility.

`ctx doctor --format json` returns `schema_version`, `ok`, `findings`, and the
same top-level `daemon` object used by status so callers can inspect daemon
lifecycle and job state without parsing human findings.

## Stats

```bash
ctx stats --format json
ctx stats --detail --format json
```

Stats is local, offline, read-only, and excluded from its own counts. It does
not create `usage.sqlite` on a pristine root. Its top-level
`schema_version` is 2. The top-level object contains:

- `schema_version`;
- `local_only: true`;
- `read_only: true`;
- `enabled`;
- `state`, one of `disabled`, `empty`, `ready`, or `error`;
- `retention_days`;
- optional `definitions[]`;
- optional `estimates`;
- optional content-free `error`, with `code` and `message`.

`definitions` is absent when measurement is disabled or unavailable, an empty
array when no rows exist, and otherwise contains one object per retained
measurement definition. Each object contains:

- `definition_version`;
- `ctx_versions[]`;
- `first_day_utc`;
- `last_day_utc`;
- `active_days`;
- `summary`;
- optional nonempty `by_operation[]`;
- optional nonempty `duration_buckets[]`.

`summary` contains `calls`, `successful_calls`, `failed_calls`,
`result_bearing_calls`, `empty_calls`, `not_applicable_calls`, `result_count`,
`citation_count`, `delivered_output_bytes`, `delivered_context_bytes`,
`matched_normalized_session_bytes`, `complete_context_eligible_calls`,
`unavailable_context_eligible_calls`, and `pro_blame`. `pro_blame` contains
`requests`, `produced_attribution_requests`, `possible_only_requests`,
`none_requests`, `error_requests`, and `by_target[]`. Each target row contains
`target_type`, `requests`, `produced`, `possible`, `none`, and `error`.

Each `by_operation` row contains `ctx_version`, `surface`, `operation`, `calls`,
`successful_calls`, `failed_calls`, `result_bearing_calls`, `empty_calls`,
`not_applicable_calls`, `result_count`, `citation_count`,
`delivered_output_bytes`, `delivered_context_bytes`,
`matched_normalized_session_bytes`, `complete_context_eligible_calls`, and
`unavailable_context_eligible_calls`. Each `duration_buckets` row contains
`duration_bucket` and `calls`.

CLI `delivered_output_bytes` counts the actual final stdout and stderr bytes
accepted for delivery, including the selected terminal wrapping and ANSI mode.
MCP output bytes count the serialized response transport. These are delivery
measurements, not context measurements.

When complete search-context measurements are available, `estimates` contains
`approximate_context_tokens` with `coefficient_version`,
`delivered_context_bytes`, `low`, `central`, and `high`, plus
`estimated_context_reduction` with `estimate_model_version`,
`coefficient_version`, `covered_calls`, `unavailable_calls`,
`comparison_baseline_bytes`, `observed_delivered_context_bytes`,
`estimated_avoided_context_bytes`, `low`, `central`, and `high`. Estimates are
absent when the required complete measurements are unavailable.

Transport response bytes are never used as context bytes or as the basis for
context-reduction estimates. Migrated rows without byte samples never become
zero-byte measurements. An unavailable store returns the stable content-free
`error` object rather than fabricated aggregates. All report fields remain
aggregate-only and content-free.

Enable, disable, and reset remain explicit `ctx status --usage` controls.
Successful enable/disable controls report `persisted_enabled`,
`effective_enabled`, and `environment_override`; reset reports
`store_state: "cleared"|"missing"`. A failed JSON control exits nonzero with a
parseable, content-free `usage_control_failed` or `usage_reset_failed` error.
Reset is logical deletion, not forensic secure erasure.

## Sources

```bash
ctx sources --format json
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
ctx import --format json
ctx import --format json --no-daemon
```

Requests Core generation publication and returns:

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
`rejected_records` counts. `change` reports whether certified source state
changed; it is independent of insert counters. A deterministic source
replacement can therefore report `change: "changed"`, zero newly imported
events, and skipped existing events while reconciling those rows in place.
Rejection details are exposed as `rejections`
(bounded to five entries); `failed_sources` remains the count of source-level
failures. `sources_completed_with_rejections` counts sources that committed
accepted content while rejecting other records. `resume_mode` is currently `idempotent_rescan` when
`--resume` is passed and `normal_scan` otherwise.

Imports may opportunistically start the ctx-owned daemon maintenance profile
when `[daemon].enabled` is true. Explicit custom JSONL and history-source
imports require its source-refresh endpoint even with JSON output. Set
`ctx import --no-daemon` to prevent autostart; those explicit provider-source
routes then require an already-running endpoint. The daemon, when started, reports
`start_mode: "auto"` and `trigger_command: "import"` through status surfaces.
Import result schema version 2 does not embed daemon process state. Use
`ctx daemon status --format json` to inspect an already-running or explicitly started
daemon.

## Progress

```bash
ctx setup --progress json
ctx import --progress json
ctx import --format json --progress json
```

`--progress json` writes newline-delimited progress objects to stderr for
`setup` and `import`. It does not change command result stdout. This means
`ctx setup --format json --progress json` and `ctx import --format json --progress json`
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
the final command result from stdout when `--format json` is present.

## Show

```bash
ctx show session <ctx-session-id> --format json
ctx show event <ctx-event-id> --format json
```

Writes nothing and returns:

- `schema_version`;
- `target`, either `session` or `event`;
- `payload_type`, either `session_transcript` or `event_window`;
- `mode` for session transcripts;
- `format`;
- `session` for session output;
- `event` for event output;
- `events[]`.

`session` includes the ctx-owned `item_id`, `record_type`, `provider`, and
`provider_session_id` when known. For Codex, `provider_session_id` is the resume
UUID. `event` and `events[]` rows include `ctx_event_id`, `record_type`,
`ctx_session_id`, `provider`, `provider_session_id`, `source_format`,
`sequence`, `event_type`, `role`, `occurred_at`, and complete normalized `text`
or `structured_content` when policy permits. Each rendered event also includes
`content.complete`, `content.policy_status`, and an optional
`content.policy_reason`.

Show reads the active verified Core generation. It does not reopen provider
transcript files at query time and does not return provider source paths,
existence checks, or source cursors.

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
ctx search <query>|--term <term>|--file <path> --format json
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
- `result_window`;
- `truncation`.

`result_window` has exactly `limit`, `returned`, and `more_available`.
`returned` is at most `limit`. `more_available` is `true` only when the same
bounded search pass finds one additional fully shaped result: an event for
event-scoped search, or a distinct session for the default session-scoped
search. Search does not expose a cursor, run a second count scan, or claim an
exact omitted-result total. Text output ends with exactly
`More results available.` only when `more_available` is `true`.

`truncation` independently describes backend candidate-pool limits with
`candidate_pool` and `candidate_pool_truncated`. Candidate-pool truncation does
not by itself make `more_available` true; that flag requires an additional
shaped result.

`query` is the normalized display of the positional query plus repeatable
`--term` alternatives: surrounding whitespace is removed and nonempty
alternatives are joined with ` OR ` in argument order. Suggested scoped-search
commands preserve the positional/`--term` argument shape and shell-quote every
user-provided value. When search uses a non-default data root, each command also
preserves it as a shell-quoted `ctx --data-root <path>` prefix.
`generated_at` is the RFC 3339 UTC time at which the result envelope was
rendered.

Each result can include:

- `ctx_event_id` for event hits;
- `ctx_session_id` when known;
- `provider_session_id`;
- `event_seq`;
- `title`;
- `snippet`;
- `rank`, the one-based position in the final shaped result window;
- `retrieval_score`, the backend-provided diagnostic score; this score is not an
  ordering contract and need not be monotonic after query-coverage and
  session-diversity shaping;
- `result_type`, the concrete hit kind such as `event`, `session`,
  `session_result`, or `indexed_item`;
- `result_scope`, either `session` for a session-level result or `event` for an
  event-level result;
- `session_importance` for default session results, retained as a compatibility
  alias of the diagnostic `retrieval_score` rather than an ordering contract;
- `more_matches_in_session` for default session results;
- `provider`;
- `timestamp`;
- `cwd`;
- `why_matched`;
- `citations[]`;
- `suggested_next_commands[]`;
- `visibility`.

`why_matched[]` can include text, metadata, or touched-file reasons. A touched
file match is backed by normalized touched-file storage and can appear when
search uses `--file <path>` or when file-path metadata contributes to ranking.
`citations[]` can cite sessions, events, or files depending on
which indexed item produced the match.

Search JSON is local/private by default.

`freshness` describes the maintenance request and committed generation observed
by search. It never means that the query process became a foreground writer:

- `mode`, one of `background`, `off`, or `wait`;
- `status`, such as `completed`, `skipped`, `no_sources`, `read_only`,
  `budget_exhausted`, or `failed`. `read_only` means search queried an existing
  committed generation without an accepted maintenance wake;
  `budget_exhausted` means daemon-owned bounded maintenance left backlog while
  search served the latest committed generation;
- `reason`, present for explanatory read-only or skipped states;
- `budget_reasons`, present when `status` is `budget_exhausted`; stable
  machine-readable reasons include `codex_session_limit`,
  `codex_discovery_file_limit`, `manifest_file_limit`, `single_file_bytes`, and
  `total_bytes`;
- `source_count`;
- `daemon_last_run_at_ms`, present when search relies on a recent daemon refresh;
- `totals`, when a daemon receipt supplies the same bounded total fields as
  `ctx import --format json`;
- `error`, present when a background maintenance request failed but committed
  results were still served.

Background mode health-checks and may recover the default-enabled persistent
daemon, then returns the latest committed generation without waiting for
relational, semantic, or Pro catch-up. Wait mode waits for the requested source
frontier and lexical receipt or fails; it never falls back to a foreground
importer. Off mode sends no maintenance wake.

`retrieval` describes the requested and effective search path:

- `requested_mode`, one of `hybrid`, `semantic`, or `lexical`;
- `effective_mode`, one of `lexical`, `semantic`, or `hybrid`;
- `semantic_weight`, the effective semantic contribution used for ranking. It
  is `0.0` when the effective mode is lexical, even if a semantic weight was
  requested. A zero-weight hybrid request performs no model, query-service,
  vector-open, or vector-scan work;
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
reason why a hybrid request used lexical fallback.
`retrieval.semantic_fallback`, when present, is the human-readable explanation.
Semantic-only unavailability is a typed command error, not a successful
`search_results` object with `effective_mode: "lexical"`.

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
`vector_scan_ms`, `chunks_scanned`, `vector_bytes_read`, `events_scored`, and
`semantic_candidates`. These fields
are local performance diagnostics and can reveal corpus size/timing; treat them
as private like the rest of search JSON.

`suggested_next_commands` can include `ctx show event`, `ctx show session`, and
`ctx search "<query>" --session <ctx-session-id>` command strings when the
required ctx IDs are known.

When ctx can identify the active Codex provider session through
`CODEX_THREAD_ID`, search filters include `exclude_provider_session` and omit
that active session tree by default. Passing `--include-current-session` removes
that filter.

## SQL

```bash
ctx sql "SELECT COUNT(*) AS sessions FROM ctx_sessions" --format json
ctx sql --file query.sql --format json
```

Runs one read-only SQL statement against the existing local SQLite metadata
projection and returns:

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
results. `read_only: true` describes database mutation only. The relational
projection and its stable/internal views are metadata-only: they do not
contain event bodies, previews, command/result payloads, or transcript text,
and SQL does not fetch those bodies. Selected metadata can still contain
private paths and identifiers, so clients must not infer that SQL output is
safe to share.

Use stable `ctx_*` views for scripts when possible: `ctx_sessions`,
`ctx_events`, `ctx_files_touched`, and `ctx_sources`. Internal tables remain
queryable for advanced local inspection but are not the preferred compatibility
surface.

## MCP Tool Results

`ctx mcp serve` exposes MCP tools over stdio for status, sources, search, SQL,
showing sessions and events, and Pro status/blame. Startup health-checks and may
recover the default-enabled persistent daemon. Search and blame can send
bounded, content-free maintenance wakes; the MCP process never becomes an
importer or projection writer and never writes provider history or
repositories. Tool results include
`structuredContent` JSON using the same private local fields as CLI JSON. MCP
output may include absolute paths, source metadata, snippets, and transcript
text, and the MCP host may log or forward it.

MCP search follows the same committed-generation and lexical/semantic/hybrid
retrieval contract as CLI search. Hybrid may report lexical fallback when
semantic is disabled or unavailable; semantic-only unavailability is a typed
error, and zero semantic weight performs no vector work. MCP search does not
itself import provider history. It also excludes the active Codex session tree
by default when `CODEX_THREAD_ID` is set; pass
`include_current_session: true` to opt back in.

The MCP `sources` tool includes the same bounded `issues` and
`issues_truncated` fields as `ctx sources --format json`.

The MCP `sql` tool uses the same `sql_result` JSON contract as `ctx sql
--format json`, always read-only. Its `structuredContent` must include the same
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
ctx integrations install mcp --format json
ctx integrations status mcp --format json
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
ctx docs list --format json
ctx docs search <query> --format json
ctx docs show <topic> --format json
```

`ctx docs list --format json` returns:

- `schema_version`;
- `topics[]`.

Each topic includes `id`, `title`, `audience`, `summary`, `tags`, and
`source_path`.

`ctx docs search <query> --format json` returns:

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
ctx upgrade --format json
ctx upgrade --dry-run --format json
ctx upgrade check --format json
ctx upgrade status --format json
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

`ctx upgrade status --format json` returns:

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
- `event_seq`.

## Local Pro

The `pro` object in `ctx status --format json` has `schema_version: 2`,
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
embedded by `ctx doctor --format json` under `pro`. MCP `pro_status` also adds
`conversion_action` and `local_usage`; doctor does not.

`conversion_action` is `pro_monthly_conversion` at `"$20/month"` for `trial`
or an unpriced `pro_restore_access` for `locked`, both pointing to
`ctx pro manage`. The restore action includes `graph_preserved: true` and
`reason: "access_locked"`. It is null for paid `active`,
`canceling_paid`, and `offline_grace` states and does not replace `next_action`.
MCP `pro_status` also embeds the compact `local_usage` report; neither field is
added to blame results or citations.

`ctx pro --format json` and its explicit synonym `ctx pro setup --format json` both run the
idempotent setup path, report operation `setup`, and return the
`schema_version: 1`, `payload_type: "pro_setup"` contract.
`ctx pro --referral <codename> --format json` uses that same setup payload. It accepts
the codename only for the first anonymous-trial challenge and does not echo the
raw codename or opaque claim in JSON. The resulting attribution is immutable.
An accepted referral produces a 30-day trial; setup without one remains the
ordinary 14-day trial.
`ctx pro manage --no-open --format json` and
`ctx pro uninstall (--delete-data|--keep-data) --format json` return the `pro_manage`
and `pro_uninstall` payload types respectively.
Materialization is an internal, idempotent daemon-owned activity requested by
setup and maintenance wakes.
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

Successful `ctx blame file|commit|pr --format json` and MCP `blame` return the protocol
`BlameResult` directly, with no payload wrapper, prose summary, or suggested
claims:

- `target`, a resolved tagged `file`, `commit`, or `pull_request` target;
- `git_snapshot`, required for file results and null for commit/PR results;
- `matches`, typed `file`, `commit`, or `pull_request` matches corresponding to
  the resolved target;
- `evidence`, a complete deduplicated table numbered contiguously from one;
- `next`, null or an opaque cursor plus `more_matches` or
  `more_committed_lines` reason.

Ordinary CLI and MCP blame send a bounded maintenance wake, read the latest
committed Pro generation, and report its frontier or typed stale state while
catch-up proceeds. Only an explicit wait policy waits for a requested frontier;
the query process never performs foreground materialization.

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

Core `show` JSON remains the session/event retrieval contract. There are no Pro
`show`, `timeline`, `facts`, or `related` payloads or compatibility aliases.

Human CLI failures exit nonzero with a stable error token on stderr. When a Pro
or referral command is explicitly invoked with `--format json`, a classified
failure instead writes one compact JSON object to stderr with matching `error`
and `error_code` strings. It contains no untrusted service or helper detail and
is never ANSI-decorated. MCP failures set `isError: true` and return `error`
plus `error_code` in `structuredContent`.
Stable codes include `pro_not_installed`, `commercial_unavailable`,
`entitlement_expired`, `helper_upgrade_required`, `key_store_unavailable`,
`key_store_locked`, `not_materialized`, `protocol_mismatch`,
`source_unavailable`, `repository_unavailable`, `resource_not_found`, `line_out_of_range`,
`stale_snapshot`, `stale_fact`, `ambiguous`, `corrupt_graph`,
`invalid_request`, `invalid_response`, `cancelled`,
`helper_crashed`, and `helper_timeout`.
Native key-store failures use only `key_store_unavailable` and
`key_store_locked`. Unshipped `credential_vault_*` spellings are not aliases.

## Referrals

```bash
ctx referral create <codename> --format json
ctx referral status --format json
ctx referral payout [--country <CC>] [--entity-type <individual|company>] --format json
```

All three commands return schema version 1. JSON referral commands use cached
WorkOS authentication only. They never start device authorization or invoke a
browser opener; a missing cached session fails with
`authentication_required`. The payloads contain no human referral slogan or
promotional message. Any verified person can create a codename; a Pro trial or
subscription is not required.

`ctx referral create <codename> --format json` returns:

- `schema_version`;
- `payload_type: "referral_create"`;
- `codename`;
- `share_command`, exactly `ctx pro --referral <codename>`;
- `disposition`, either `created` or `existing`.

`ctx referral status --format json` returns:

- `schema_version`;
- `payload_type: "referral_status"`;
- `codename`;
- `share_command`, exactly `ctx pro --referral <codename>`;
- `attributed`;
- `subscribed`;
- `earned_cents`;
- `pending_cents`;
- `manual_review_cents`;
- `payable_cents`;
- `processing_cents`;
- `paid_cents`;
- `debt_cents`;
- `currency: "usd"`;
- `payout_state`.

Counts and cent amounts are nonnegative integers. The requested status payload
is the complete machine-readable referrer summary. It is private to the
authenticated referrer and aggregate only: it contains no referred identity,
invoice, or per-referral ledger, and no referral fields are added to ordinary
status or MCP output. `payout_state` is one of
`not_eligible`, `eligible`, `onboarding_pending`, `ready`, or `paused`.

`manual_review_cents` is accrued cash awaiting an explicit review outcome.
`processing_cents` is cash sent for payout but not yet settled. `paid_cents`
is historical cash actually settled and never decreases after a reversal;
post-paid reversals increase `debt_cents`. Every status payload satisfies:

```text
earned_cents + debt_cents
  = pending_cents + manual_review_cents + payable_cents
    + processing_cents + paid_cents
```

The amounts summarize a $10 cash commission for each distinct qualifying $20
monthly Pro invoice, invoices 1 through 12, capped at $120 per direct referral.
Invoice 1 and invoice 2 commissions remain pending until invoice 2 settles, the
required 14-day hold elapses, authoritative reconciliation completes, and
manual review makes them payable. Each invoice 3 through 12 commission has its
own 14-day hold, reconciliation, and manual-review gate. A refund or dispute
voids an unpaid commission. Reversal of a paid commission becomes debt, a
negative adjustment against future earnings subject to manual review, never an
external clawback.

`ctx referral payout --format json` returns:

- `schema_version`;
- `payload_type: "referral_payout"`;
- `payout_state`;
- `onboarding_url`, a one-use Stripe-hosted URL;
- `expires_at_unix`;
- `browser_opened: false`.

`--no-open` is optional and redundant in JSON mode. `--country` accepts a
two-letter uppercase ISO country code, and `--entity-type` accepts
`individual` or `company` when the hosted onboarding request requires them.
No payout command accepts bank or card details.

Stable hosted referral failures include `authentication_required`,
`referral_codename_conflict`, `referral_not_eligible`, `referral_not_found`,
`referral_payout_unavailable`, and `referral_self_referral`. Malformed or
out-of-bounds hosted results fail with `invalid_response`.

## Doctor

```bash
ctx doctor --format json
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
