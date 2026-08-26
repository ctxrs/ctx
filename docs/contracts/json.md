# JSON Contracts

ctx JSON is for local agents and scripts. It can include prompts, command
arguments, typed result identifiers, and local paths. Treat it as private until
a user reviews it.

Command result JSON uses `schema_version: 1` except for
`ctx setup --format json`, `ctx stats --format json`,
`ctx import --format json`, and `ctx search --format json`.
Progress-event JSON is stderr progress output and does not include
`schema_version`.

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
- `daemon`;
- `daemon_autostart`;
- `deprecated_catalog_only_ignored`;
- `source_rebuild_required`;

In automatic indexing mode, human and machine-readable setup both health-check
and recover the persistent daemon before returning. In manual mode, setup does
not start a persistent or finite worker.
`daemon_autostart.status: "verified"` includes the live PID. A one-run
`--no-daemon` opt-out reports `status: "not_requested"` and reason
`explicit_opt_out`; a durable manual configuration uses reason
`daemon_disabled`. `refresh_request` separately reports whether setup queued
or waited for daemon-owned Core publication. A completed `--wait` request
also includes its request-bound terminal `receipt`; callers should use that
receipt rather than a later periodic daemon job when reporting the setup run.

If the platform's native current-user service manager is not operational,
automatic setup starts the same coordinator as a persistent detached process without
forcing an initial Core-refresh wait. `daemon_autostart.status` is then
`"degraded"`, `persistent` is `true`, and `reason` is
`"native_supervisor_unavailable"`; the nested supervisor report uses status
`"manager_unavailable"` and reports that native automatic restart after
failure, login, or reboot is unavailable. Existing native registration
artifacts are preserved when the unavailable manager cannot verify or remove
them. Ownership, identity, integrity, fencing, and security failures remain
errors rather than degraded limitations.

An unmanaged install or custom data root instead uses the persistent
CLI-self-healing fallback process. Its autostart status is `"degraded"` because
automatic restart registration is unavailable, but `persistent` is `true` and
it does not report a process-lifetime limitation. The nested supervisor report
uses status `"fallback"`.

Setup does not perform a foreground provider import. `--wait` waits for the
daemon-owned Core refresh; without it, setup requests a background Core
refresh. When that first request finds zero sources and no prior publication,
setup attaches long enough to certify a verified empty Core generation instead
of returning an uncertified pending state. The deprecated `--catalog-only` flag
is reported by `deprecated_catalog_only_ignored` and does not change the
persistent lifecycle.
Use `ctx status --format json` for complete process, supervisor, and applied
configuration health.

## Status

```bash
ctx status --format json
```

Reads local storage state and returns:

- `schema_version`;
- `initialized`;
- `data_root`;
- `config_path`;
- `indexing`, with effective `mode` (`auto` or `manual`);
- `history_epoch`;
- `lexical`;
- `refresh`;
- `semantic`;
- `daemon`;
- `indexed_items`;
- `indexed_sessions`;
- `indexed_events`;
- `indexed_sources`;
- `upgrade`;
- compact `local_usage` health (`enabled`, state, and a content-free error when
  unavailable), without aggregates, estimates, or operation details;
- `local_only: true`;
- `read_only: true`.

For status, `read_only: true` means the command does not mutate canonical
history or Core search generations. Usage control modes return a separate
action-focused JSON shape with `read_only: false` and do not read Core status.

`history_epoch` and `lexical` identify the verified searchable Core generation.
`refresh` reports the latest observed daemon-owned refresh request and its exact
generation binding. A generation-bound published refresh with deterministic
record rejections but zero source failures has `refresh.status: "ready"`; its
`current.current_rejected_records` remains present for diagnostics. Source
failures, retryable failures, and publication/generation mismatches do not
become ready merely because an older verified generation remains searchable.
`semantic` reports the current source-backed semantic
projection, including exact `flat_f32` document/event/chunk coverage when it is
available. `daemon` reports process and relevant job state. These diagnostic
objects can contain local paths and should not be persisted or forwarded outside
local diagnostics.

## Index Readiness

```bash
ctx index --format json
ctx index mode --format json
ctx index mode auto --format json
ctx index mode manual --format json
ctx index watch --format jsonl
ctx index wait --format json
```

`ctx index --format json` returns the one-shot readiness snapshot. Each watch
line uses the same read-only shape: `schema_version`, `initialized`, `indexing`,
`lexical`, `refresh`, `semantic`, `daemon`, `local_only`, and `read_only`.
`indexing.mode` is `auto` or `manual`. Lexical counts and certified source bytes
describe the currently verified generation. Refresh progress contains only
values reported by the active refresh job; no synthetic work units, failure
counts, rates, or remaining time are added.

`ctx index mode --format json` returns `schema_version`, `indexing.mode`,
`config_path`, `local_only`, and `read_only: true`. Supplying `auto` or `manual`
persists the mode and returns `read_only: false` plus
`indexing.requested_mode`, `indexing.overridden`, `daemon.running`, optional
`daemon.pid`, `daemon.persistent`, and `daemon.supervisor`. `overridden` is true
when a process-level control keeps a different effective mode. Mode changes
reconcile supervision to that effective mode. When auto remains effective, ctx
installs or repairs supervision and starts the persistent daemon; manual mode
stops it and removes persistent supervision. Explicit import and search
`--refresh wait` can still use finite workers.

Wait returns one object with `schema_version`, `status` (`ready`, `blocked`, or
`timeout`), `selection`, the final `readiness` snapshot, `local_only`, and
`read_only`. Use `ctx status --format json` for the complete health contract,
including daemon and supervisor diagnostics.

Index snapshots expose the reduced
`semantic.{status,reason,enabled,coverage.{candidate_items,searchable_items,embedded_items,filtered_items,embedded_chunks}}`
shape and `daemon.{status,running,jobs.semantic_index}`. The complete semantic
and daemon fields below describe `ctx status --format json`, not index
snapshots.

`semantic.status` is `disabled`, `pending`, `ready`, or `unavailable`.
`semantic.flat_f32` reports the source-backed projection and can include its
`status`, `reason`, `path`, Core and flat generation identity, semantic document
count, projected and intentionally filtered document counts, active
event/chunk/vector-byte counts, and `last_error`. For a ready generation,
`semantic_documents = projected_documents + filtered_documents` and
`projected_documents = active_events`. These document counters, and the index
snapshot candidate/searchable/embedded/filtered counters derived from them,
are integers in the exact inclusive range `0..9007199254740991`; a larger
internal count makes semantic status unavailable instead of emitting an unsafe
JSON number. Optional `semantic.catch_up` retains the latest semantic-index job
receipt, including its `source_contract_fingerprint` when produced by current
daemon maintenance. Live worker state and coverage are reported under
`daemon.jobs.semantic_index` below.

`daemon` reports the ctx-owned background coordinator state. Fields listed as
nullable may be omitted when unavailable:

- `enabled`, retained as a compatibility-shaped boolean and true when indexing
  mode is `auto`;
- `status`, one of `unknown`, `disabled`, `running`, `stopped`, `completed`,
  `failed`, or `stale_lock`; `completed` remains readable for legacy finite-run
  receipts, while current persistent and finite workers use `stopped` after
  graceful shutdown. A stopped finite worker may render `disabled` because the
  durable indexing mode is manual;
- `running`;
- `pid`, nullable/omitted;
- `started_at_ms`, `heartbeat_at_ms`, and `finished_at_ms`, nullable/omitted;
- `last_error`, nullable/omitted;
- `start_mode`, nullable/omitted, currently `auto` for setup/import/search
  process starts or `manual` for explicit daemon runs;
- `trigger_command`, nullable/omitted, currently `setup`, `import`, or `search`
  for automatic or finite starts;
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
`activation_failed`. `ctx daemon status --format json` remains a hidden
compatibility diagnostic when malformed configuration caused the retained
reload failure; ordinary commands reject malformed configuration.

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
`schema_version` is 3. The top-level object contains:

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

`summary` contains exactly `calls`, `successful_calls`, `failed_calls`,
`result_bearing_calls`, `empty_calls`, `not_applicable_calls`, `result_count`,
`delivered_output_bytes`, `delivered_context_bytes`,
`matched_normalized_session_bytes`, `complete_context_eligible_calls`, and
`unavailable_context_eligible_calls`.

Each `by_operation` row contains exactly `ctx_version`, `surface`, `operation`,
`calls`, `successful_calls`, `failed_calls`, `result_bearing_calls`,
`empty_calls`, `not_applicable_calls`, `result_count`,
`delivered_output_bytes`, `delivered_context_bytes`,
`matched_normalized_session_bytes`, `complete_context_eligible_calls`, and
`unavailable_context_eligible_calls`. Each `duration_buckets` row contains
`duration_bucket` and `calls`.

The report reads the Core-owned `usage.sqlite` sidecar. Its current SQLite
schema version is 4, whose daily aggregate rows contain only `day_utc`,
`definition_version`, `ctx_version`, `surface`, `operation`, `outcome`,
`value_class`, `duration_bucket`, `context_coverage`, `calls`, `result_count`,
`delivered_output_bytes`, `delivered_context_bytes`, and
`matched_normalized_session_bytes`. SQLite schema versions 1 through 3 are
accepted only as migration inputs; new stores and current writes use version 4.
The SQLite schema version and JSON report schema version are independent.

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
- `automatic_discovery`, whether inferred provider history roots are enabled;
- `hidden_missing_sources`;
- `sources[]`;
- `issues[]`;
- `issues_truncated`.

Each built-in provider source includes:

- `provider`;
- `path`;
- `exists`;
- `source_format`;
- `status`;
- `import_support`;
- `native_import`;
- `importable`;
- `unsupported_reason`;
- `selection`, with `kind` (`automatic` or `configured`), configured `root`,
  and configured `group`.

For a configured row, `selection.root` is the exact case-sensitive configured
name and `selection.group` is its configured group or null. For an automatic
row, both values are null. History-source plugin rows retain their plugin
identity fields and do not have configured history root selection metadata.

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
codes are `no_disk_history`, `selector_unreconstructible`,
`insufficient_official_evidence`, `configured_root_conflict`, and
`configured_root_missing`.
A `configured_root_conflict` row additionally contains nullable
`conflict_kind` (`configured_configured` or `automatic_configured`) and
`configured_roots`, a possibly empty array of the recoverable configured
`name` and `path` pairs involved. The existing top-level nullable `path` remains
the path reported by discovery. `issues_truncated` is true when additional
issue rows were omitted. Invalid history-source plugin manifests remain
non-importable rows in `sources[]`; they are not provider discovery issues.
A `configured_root_missing` row represents a durable configured root that
cannot safely produce a concrete source route while absent. Its
`configured_root` member is always present: when the persisted definition is
recoverable it is an object containing `name`, `path`, and nullable `group`;
otherwise it is null. The member is absent, rather than null, on every other
issue code. Recoverable missing-root rows are ordered before automatic issues
so all 64 valid configured roots remain represented at the issue limit; the
root remains configured until restored, replaced, or removed.

Named provider history root mutations have a separate schema-version-1 JSON
result:

```bash
ctx sources add personal --provider claude --root /path/to/claude --source-group work --format json
ctx sources add personal --provider claude --root /path/to/moved-claude --replace --format json
ctx sources add openhands-cli --provider openhands --root /path/to/conversations --kind current-conversations --format json
ctx sources remove personal --format json
```

Both successful shapes contain exactly `schema_version`, `operation`,
`changed`, and `root`. `operation` is `"add"` or `"remove"`; `root` contains
`name`, `provider`, canonical absolute `path`, and nullable `group`. For an
OpenHands root only, `root` additionally contains `kind` with the exact value
`"current-conversations"` or `"legacy-persistence"`; the member is omitted,
not null, for every other provider. Repeating an add with the same name and
identical canonical settings is idempotent and returns `changed: false`.
Reusing the name with different settings fails unless the add includes
`--replace`. A same-provider replacement atomically writes the new canonical
path, kind, and complete group state while retaining `operation: "add"`;
supplying `--source-group` sets the group and omitting it clears the group. It
does not expose an intermediate removed definition. A provider mismatch under
the stable name is rejected. With an absent name, `--replace` performs an
ordinary add. A successful remove returns `changed: true` and the removed root;
removing an absent name is an error, not a successful no-op. Root names and
non-null groups use 1 to 64 ASCII letters, digits, hyphens, or underscores and
remain case-sensitive.

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

In automatic indexing mode, imports may start the persistent ctx-owned daemon.
In manual mode, an explicit import may start a finite Core worker. Both use the
same source-refresh endpoint and publication engine, including for custom JSONL
and history-source imports, and both wait for authoritative Core publication.
Set `ctx import --no-daemon` to prevent any start or restart; explicit routes
then require an already-running endpoint. Output format does not change this
authority. A process started for import reports `start_mode: "auto"` and
`trigger_command: "import"` through live status surfaces.
Import result schema version 2 does not embed daemon process state. Use
`ctx status --format json` to inspect daemon and supervisor health.

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

Daemon-owned source refresh events additionally include:

- `completed_sources`, `total_sources`, and `total_sources_known`;
- `source_completed_records`, the nullable accepted Core-record count for the
  current source, and `source_completed_bytes`, its nullable authoritative
  logical-byte progress;
- `current_source`, nullable bounded and control-safe presentation text for the
  active source (not an exact route identifier), and `current_source_progress`,
  nullable typed substep detail;
- `request_id`, `request_state`, `logical_request_id`, `logical_phase`,
  `physical_attempt_id`, `physical_attempt_state`,
  `progress_owner_request_id`, `progress_owner_attempt_state`, and
  `maintenance_wake` when the corresponding status authority supplies them;
- `structured_outcome` when a terminal refresh outcome is available.
- `whole_run_stage`, the current stage on the path to a verified, durable,
  active generation, and `estimated_remaining_millis`, a whole-run numeric
  estimate or `null`. The estimate is intentionally unavailable unless the
  active cold setup attempt has complete exact accounting and has passed its
  credibility gate. `eta_seconds` retains its legacy meaning.

Source record and byte counters reset or clear at source and finalization
boundaries. A scan with no authoritative total does not fabricate a total or
ETA; the common transfer fields use `total_bytes: 0` and `percent: 0.0` as
unknown-denominator sentinels. The `completed_bytes` / `total_bytes` pair is
used when the active transfer has a real denominator; source scans expose their
authoritative completed count through `source_completed_bytes`.

Progress events are operational status events, not durable result records.
Consumers should key on `type` and `operation`, ignore unknown fields, and read
the final command result from stdout when `--format json` is present.

## Show

```bash
ctx show session <ctx-session-id> --format json
ctx show session <ctx-session-id> --format jsonl
ctx show event <ctx-event-id> --format json
```

Show resolves identities and complete policy-selected normalized records from
the active verified Core/Tantivy generation. Session presentation writes each
selected stored event as it is rendered; it does not retain the complete
session. The CLI has no public session cursor or page limit. Without
`--max-events`, `ctx show session` streams every event selected by the requested
mode in deterministic order, so large transcripts are complete rather than
silently capped.

Session JSON is one `session_transcript` object containing:

- `schema_version`;
- `target: "session"`;
- `payload_type: "session_transcript"`;
- `ctx_session_id`, `provider`, and `provider_session_id` when known;
- `provider_key` and `source_id` for custom history-source sessions;
- `mode` and `format`;
- `session` for session output;
- `events[]`.

`--max-events <N>` is an explicit terminal truncation control, not pagination.
When it stops selection, JSON adds
`truncated: {"events": true, "max_events": N}` and does not return a
continuation cursor. Without that option, session JSON has no `pagination`,
`has_more`, or `next_cursor` fields.

Session JSONL emits zero or more event records followed by exactly one terminal
completion record. An event record has
`payload_type: "session_transcript_event"`, the session identity and mode, and
one rendered `event`. The terminal record has
`payload_type: "session_transcript_completion"`, the same session identity and
mode, `events_returned`, `complete`, and, only after explicit `--max-events`
truncation, `truncated: {"events": true, "max_events": N}`. A complete empty
session therefore emits only a completion record with `events_returned: 0` and
`complete: true`. JSONL completion metadata is terminal stream metadata; it is
not an MCP pagination envelope.

Event JSON remains one `event_window` object with `target: "event"`, `format`,
`event`, `events[]`, and `copied_lineage`.

Event-range JSON from `ctx list events --format json` is one
`event_range_page` with `schema_version: 1`, the pinned `generation_id`, the
normalized request domain/filters/direction, `content`, bounded
`events[]`, the requested limit and page usage, `terminal`, `truncated`, `next_cursor`, and
explicit freshness/frontier metadata. The cursor is present exactly when the
page can continue. It is opaque and bound to the complete selection and pinned
generation.

Event-range JSONL emits zero or more `event_range_event` records followed by
exactly one `event_range_completion`. Each event record includes
`schema_version`, `record_type`, `generation_id`, a contiguous zero-based
`ordinal`, and one normalized `event`. The completion echoes the normalized
selection, content projection and requested limit, reports aggregate usage,
and records terminal/truncated/cursor and freshness/frontier state. A consumer
must reject EOF, a second completion, an event after completion, mixed
generations, or noncontiguous ordinals. Diagnostics and typed failures are on
stderr; stdout has only the selected JSON or JSONL.

Event-range event rows expose exact ctx event/source/session and parent/root
session identities, provider and provider-session identity when present,
source format, native identity, sequence and chronology, role/event/agent
fields, content-policy state, selected text/structured content, citations, and
normalized repository evidence already in Core. Event type is an open string;
unknown normalized values are preserved. See
[`event-queries.md`](../event-queries.md) for selection details and jq examples.

`session` includes the ctx-owned `item_id`, `record_type`, `provider`,
`provider_session_id` when known, and `provider_key` plus `source_id` for a
custom history source. For Codex, `provider_session_id` is the resume UUID.
`event` and `events[]` rows include `ctx_event_id`, `record_type`,
`ctx_session_id`, `provider`, custom-source `provider_key` and `source_id`,
`provider_session_id`, `source_format`,
`sequence`, `event_type`, `role`, `occurred_at`, and exact normalized `text`
or `structured_content` when policy permits. Each rendered event also includes
`content.complete`, `content.policy_status`, and an optional
`content.policy_reason`.

A selected full-content event can additionally include optional `activity`.
The revision-1 object can contain exact typed `provider_call_id`, invocation
and/or result channels, and ordered provider-declared facts. Invocation
arguments and structured results use explicit
`present`/`absent`/`unavailable`/`omitted` capture states and remain decoded JSON
when present; result text additionally supports `normalized_body`. CLI/Core
fields use snake_case.

An exact MCP invocation has `protocol: "mcp"` plus nonempty source `server`
and advertised `tool` strings. Machine output preserves admitted activity
exactly. Absence means only that the event has no retained provider activity;
it does not mean that the event was not MCP. `ctx list events --content text`
and `--content none` omit activity; full show-event and log-mode show-session
rows can include it. See
[`mcp-tool-call-attribution.md`](../mcp-tool-call-attribution.md) and
[`mcp-exchange-capture.md`](../mcp-exchange-capture.md).

Human rendering escapes terminal controls and may bound a rendered event.
JSON, JSONL, and MCP `structuredContent` retain the exact admitted activity
value.

Show reads the active Core/Tantivy generation without reopening provider
history. It does not return provider source paths, existence checks, or source
cursors. With `--out`, a
session transcript is staged and atomically installed only after the complete
stream succeeds; a failed stream does not replace an existing destination.

`ShowSessionOptions::default()` has
no `limit` or `cursor` and uses the complete CLI stream. Supplying either option
uses the existing local MCP `show_session` page contract; its returned
`pagination` object is preserved in the SDK session result's additive fields.
Current Core `activity` is preserved as an additive event field; keys inside
captured JSON values are not camelized.

If the active generation changes while `show` or `search` is opening its
verified reader, JSON mode exits nonzero and writes one error object to stderr
with `error: "generation_changed/active_generation_race"`,
`error_code: "generation_changed"`,
`failure_kind: "active_generation_race"`, and `retryable: true`. Clients may
retry the same command; this race is not returned as a successful result.

JSON `search`, `show`, and `locate` also fail before query or rendering when an
existing Core generation lacks valid publication authority. The one stderr
object has equal `error` and `detail` text plus a stable `error_code` and
boolean `retryable`. An uncertified empty generation reports
`source_unavailable` and is retryable; malformed or unknown publication
metadata reports `publication_authority_invalid` and is not retryable. These
states are distinct from a genuinely missing Core generation.

## Locate

```bash
ctx locate session <ctx-session-id> --format json
ctx locate session --provider codex --provider-session <provider-session-id> --format json
ctx locate session --provider-session <provider-session-id> --provider-key <provider-key> --source-id <source-id> --format json
ctx locate event <ctx-event-id> --format json
```

Locate reads only the active verified Core/Tantivy generation. It does not
reopen provider history or return a provider path.

Session JSON is one `session_location` object containing:

- `schema_version: 1`, `target: "session"`, and
  `payload_type: "session_location"`;
- `ctx_session_id`, `provider`, and `provider_session_id` when known;
- `provider_key` and `source_id` for custom history-source sessions;
- nullable `parent_ctx_session_id`, `root_ctx_session_id`, and `started_at`;
- `source` with `ctx_source_id`, `source_format`, `schema_variant`, and
  `provider_identity_version`.

Event JSON is one `event_location` object containing:

- `schema_version: 1`, `target: "event"`, and
  `payload_type: "event_location"`;
- `ctx_event_id`, `ctx_session_id`, `provider`, `provider_session_id`, and
  `provider_event_id` when known;
- `provider_key` and `source_id` for custom history-source events;
- `sequence`, `event_type`, `role`, and `occurred_at`;
- the same bounded `source` identity object as session locate.

## Transcript Artifacts

```bash
ctx show session <ctx-session-id> --mode full --format json --out transcript.json
```

With `--out`, writes the requested transcript artifact to that path and prints
nothing on success. Without `--out`, stdout is the requested transcript
artifact. JSON and JSONL artifact rows use the same ctx-owned ID fields as
`show`; JSONL uses the event-plus-terminal-completion stream described above.
Artifacts inherit the same complete-by-default behavior and explicit,
non-resumable `--max-events` truncation contract.

## Search

```bash
ctx search <query>|--term <term>|--file <path> --format json
```

Returns:

- `schema_version: 2`;
- `payload_type: "search_results"`;
- `query`;
- `filters`;
- `freshness`;
- `retrieval`;
- `generated_at`;
- `diversification`;
- `results[]`;
- `result_window`;
- `truncation`.

`filters` always includes the resolved `content_scope`, one of `all`,
`transcript`, `calls`, or `outputs`. Omission of the CLI option or MCP input
resolves to `"content_scope": "all"`. The field reports query-time event-class
selection; it does not describe a different retained body, Core schema, or
index generation. `content_scope` and the exact `event_type` filter cannot
appear in the same successful request because those inputs conflict
unconditionally.

When supplied, repeatable CLI root selectors are echoed as `source_root` and
group selectors as `source_groups`, each an array of normalized names. MCP accepts the
equivalent request arrays `source_roots` and `source_groups`. Both selector families
resolve against the pinned Core generation. Every root and group value forms
one OR selection set before that set intersects with independent filters using
AND semantics. Names are case-sensitive, and unknown selectors are typed
request errors; they are never ignored or resolved from live config against an
older generation. Each selector is 1 to 64 ASCII letters, digits, hyphens, or
underscores, and each MCP array contains at most 64 entries. Selector
validation diagnostics remain generic and do not echo rejected contents.

`result_window` has exactly `limit`, `returned`, and `more_available`.
`returned` is at most `limit`. `more_available` is `true` only when the same
bounded search pass finds one additional fully shaped result: an event for
event-scoped search, or an additional session champion for the default
session-scoped search. A false value does not assert exhaustive backend
availability; readers must inspect `truncation`. Search does not expose a
cursor, run a second count scan, or claim an exact omitted-result total. Text
output ends with exactly
`More results available.` only when `more_available` is `true`.

`truncation` independently describes backend candidate-pool limits with
`candidate_pool` and `candidate_pool_truncated`. For lexical participation it
also includes `lexical.work_complete`, `lexical.candidate_set_exhaustive`, and,
only after bounded work exhaustion, `lexical.exhaustion` with the operation
`counter`, `used`, and `limit`. It does not expose segment identifiers,
document identifiers, query text, content, scores, or candidate identities.
`candidate_set_exhaustive` is false when a completed retained heap discarded
lower-relevance matches and whenever bounded work did not complete.
`candidate_pool_truncated` is true for that lexical non-exhaustiveness or when
a backend reaches its fixed candidate cap; it does not claim an exact omitted
count. Backends without an explicit completeness signal remain conservative in
`diversification.status` even when no known cap was reached.
Candidate-pool truncation does not by itself make `more_available` true; that
flag requires an additional shaped result, except that completed dense heap
truncation at the maximum retained horizon proves one additional event.

`diversification` has `status`, `top_n`, and conditionally
`changed_final_top_n`. `status` is `applied`, `not_applicable`, or
`indeterminate`; `top_n` is the requested limit. Dense `--events`, any explicit
`--session`, and limit zero are `not_applicable` and omit
`changed_final_top_n`. Default session search selects one champion per exact
session, orders families by their strongest champion, and emits one remaining
champion per family per relevance-stable round. Ordinary lexical session search
is `applied` only when
bounded lexical work completed and either the candidate set is exhaustive or
at least `top_n` distinct coalesced families were observed. Work exhaustion or
insufficient family coverage is `indeterminate`. Semantic and hybrid candidate
completeness is not yet explicit, so those backends apply the same coalesced
family policy but remain `indeterminate`. `changed_final_top_n` is present only
for `applied` and compares the full event-identity sequence before and after
family shaping.

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
  family shaping;
- `result_type`, the concrete hit kind such as `event`, `session`,
  `session_result`, or `indexed_item`;
- `result_scope`, either `session` for a session-level result or `event` for an
  event-level result;
- `session_importance` for default session results, retained as a compatibility
  alias of the diagnostic `retrieval_score` rather than an ordering contract;
- `more_matches_in_session` for default session results;
- `provider`;
- `provider_key` and `source_id` for custom history-source results;
- `timestamp`;
- `cwd`;
- `citations[]`;
- `suggested_next_commands[]`;
- `visibility`.

Ordinary search results do not carry `copied_lineage` and do not run one
reverse lookup per hit. Their event-local `event_copy`, when present, is the
selected occurrence's positive direct provider evidence only.
Search schema v2 removes the schema-v1 per-result `copied_lineage`; explicit
show-event/event-window reads remain the public direct-lineage surface.

`copied_lineage` is a schema-v2 object on the explicit show-event/event-window
envelope. It is a bounded, direct one-hop query-time view; its resolution does
not affect publication, ranking, grouping, recall, or semantic eligibility.
Its current required fields are listed below;
schema-v2 readers must ignore additional fields:

- `schema_version: 2`;
- `resolution`, with `state` equal to `resolved` or `unresolved`, the direct
  target `ctx_event_id`, and a nullable `ctx_session_id` when the target session
  is not known;
- `selected_depth`, zero when the selected event is the anchor and one when its
  one direct copied-from target is the anchor;
- `observed_count`, exact when `truncated` is false and otherwise a lower bound;
- numeric `returned`, the number of retained occurrence rows;
- `occurrences[]`, each with full `ctx_event_id`, `ctx_session_id`, direct
  `copied_from_ctx_event_id` and `copied_from_ctx_session_id`, parent and
  child-claimed-root session IDs, `session_relationship`, and direct-edge
  `depth` (currently always one);
- `relationship_counts`, keyed by relationship kind and subject to the same
  exact-versus-lower-bound rule;
- `truncated`.

Missing copy targets are unresolved references. Reverse lookup can return
direct child claims even when the selected target event is absent. A direct
self-copy claim is rejected at Core admission, and this surface neither detects
nor reports transitive cycles. Missing targets do not invalidate the immutable
generation or hide admitted occurrences.

Show event retains at most 20 direct occurrences after at most 4,096 reverse
posting visits. Selected-event and its one direct copied-from target resolution
share at most 2,048 exact event-and-session identity posting visits, counting
live and deleted rows. There is no parent, root, copy-component, or transitive
lineage traversal. The query fails with a typed bound error if more work would
be required. The complete object is limited to 64 KiB. Preview retention alone
does not make an otherwise complete count truncated. The current writer does
not emit `more_available`,
compact-ID fields, or an exhaustive cursor. CLI JSON and MCP structured content
always use full UUIDs; compact aliases exist only in the separately rendered
human/MCP text projection.

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
semantic catch-up. Wait mode waits for the requested source
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
- `semantic_diagnostics`, nullable/omitted and present when semantic vector
  retrieval runs.

`retrieval.semantic_status` is one of:

- `skipped`, lexical retrieval was used and no semantic lookup ran;
- `unsupported`, a hybrid request used lexical retrieval because its content
  scope or exact event type has no semantic projection;
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

Index readiness coverage uses `candidate_items` for the pre-content-filter Core
population, `filtered_items` for intentional semantic content filtering, and
`searchable_items`/`embedded_items` for acknowledged active flat-F32 events.
Ready coverage therefore satisfies
`candidate_items = searchable_items + filtered_items` and
`searchable_items = embedded_items`.

The SDK `agent-history-v1` contract keeps schema version 1 and normalizes the
resolved filter as `search.filters.contentScope`, with the same exact four
values. The filters object remains extensible, so SDK consumers must continue
to tolerate additive filter fields. The contract camel-cases the same
retrieval fields (`requestedMode`, `effectiveMode`, `semanticWeight`, and so
on). SDK contract
search results expose retrieval at the top level of `search`; TypeScript and
Python type the core retrieval/coverage fields, while Go, .NET, JVM, and Swift
preserve retrieval as camel-cased JSON values. Per-hit retrieval details are not
part of v1 unless a future CLI JSON shape emits them. Local diagnostic path
fields such as `vector_path`/`vectorPath` can still appear as additive JSON from
the local CLI adapter, but they are intentionally not stable SDK fields.

`retrieval.semantic_diagnostics` can include `query_embed_ms`, `vector_backend`,
`vector_scan_ms`, `chunks_scanned`, `vector_bytes_read`, `events_scored`, and
`semantic_candidates`. These fields
are local performance diagnostics and can reveal corpus size/timing; treat them
as private like the rest of search JSON.

`suggested_next_commands` can include `ctx show event`, `ctx show session`, and
`ctx search "<query>" --session <ctx-session-id>` command strings when the
required ctx IDs are known.

For direct CLI searches, when ctx can identify the current session
unambiguously for Codex, DeepSeek Harness, Grok Build, Pi, Claude Code, Goose,
Hermes, Shelley, Qwen Code, or Mux, it applies the automatic
current-session-tree exclusion. Unsupported or ambiguous detection fails open
and applies no automatic exclusion. Passing
`--include-current-session` removes the automatic exclusion. Repeatable
`--exclude-session` accepts a ctx UUID or unambiguous prefix and excludes the
exact named session; it conflicts with `--session`. JSON search filters echo
those explicit selectors in the repeatable `exclude_session` field.

## MCP Tool Results

`ctx mcp serve` exposes MCP tools over stdio for status, sources, search,
and showing sessions and events. Startup health-checks may recover the
default-enabled persistent daemon. Search can send a bounded, content-free
maintenance wake; the MCP process never becomes an importer or derived-state
writer and never writes provider history. Tool results include
`structuredContent` JSON carrying the same typed data as CLI JSON, with
contract-owned event keys in camelCase. MCP output may include absolute paths,
source metadata, snippets, transcript text, MCP arguments, and response
payloads, and the MCP host may log or forward it.

MCP search follows the same committed-generation and lexical/semantic/hybrid
retrieval contract as CLI search. Hybrid may report lexical fallback when
semantic is disabled or unavailable; semantic-only unavailability is a typed
error, and zero semantic weight performs no vector work. MCP search does not
itself import provider history and does not automatically exclude the caller's
session. Direct CLI current-session detection and
`--include-current-session` do not apply to MCP calls.
Its optional `source_roots` and `source_groups` arrays use the same generation-pinned
source-key filter as CLI search. If both arrays are absent, all indexed roots
are searched.

MCP `show_event`, `show_session`, and full-content `query_events` structured
event rows reuse the same snake_case `activity` value. Text fallback is
display-safe rather than the exact machine authority. `query_events` with
`content: "text"` or `content: "none"` omits activity. Discovery-eligible
retained protocol/server/tool/present arguments, result status/present text/structured
content, and facts enter the shared Core projection used by CLI and MCP search
matching, ranking, snippets, and semantic source text. Activity adds no
dedicated selector or SQL field. Paginated MCP callers filter each returned page
client-side and continue with the existing opaque cursor; `show_session`
requires `mode: "log"` for ordinary tool events.

The MCP `sources` tool returns `schema_version`, `automatic_discovery`,
`sources`, `issues`, `issues_truncated`, and `read_only: true`. Its built-in
provider rows use the same `selection` objects as CLI JSON, so configured
`root` values and non-null configured `group` values enumerate candidates for
MCP search `source_roots` and `source_groups`. Automatic rows have null `root` and
`group`; plugin rows do not participate in configured history root selection.
The bounded `issues` and `issues_truncated` fields retain the CLI contract.

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
history or ctx data-root state.

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
- `auto_upgrade`, including `mode` and `enabled`;
- `state`;
- `install`.

`state` is the last local upgrade-state object when present, or
`status: "never_checked"`. `install.managed` is true only when the running
binary has a matching official installer sidecar. Unmanaged installs report
`managed: false` and a `reason`.

Automatic upgrade does not write JSON to foreground stdout. Auto indexing with
the full daemon profile uses the enabled persistent daemon as the sole automatic
check and apply driver. Manual indexing, source-refresh-only mode, ordinary
foreground commands, MCP, and finite workers perform no automatic upgrade work.
Daemon and explicit upgrades share one scheduler state and replacement journal
beside the managed executable. Windows self-upgrade can
report `scheduled` with `applied: false` while a helper waits for the running
`ctx.exe` to exit and then replaces the binary and sidecar.

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

## Doctor

```bash
ctx doctor --format json
```

Reads local storage and returns findings:

- `schema_version`;
- `ok`;
- `progress`;
- `findings`.

Doctor checks Core/Tantivy generation health, read-only semantic sidecar health,
source/daemon state, and compact local-usage health. Its JSON includes daemon
status. It does not initialize embedding models or write sidecar data.
Semantic or hybrid search may ask the daemon query service to embed the query
from an already-cached local model; search does not download models or write
sidecar data from the search path.

## Provider Smoke

Provider smoke tests call normal `ctx` commands with temporary local storage and
static fixtures. Their output is ordinary command JSON covered by the command
schemas above; there is no separate provider artifact schema in the public CLI.

## Compatibility Limits

Compatibility `item_id`, `id`, `session_id`, and `event_id` fields can remain
in some outputs. New integrations should prefer ctx-owned `ctx_session_id` and
`ctx_event_id` where present, and should treat provider-owned IDs as metadata
unless an explicit provider lookup flag is present.
