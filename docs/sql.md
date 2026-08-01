# SQL

`ctx sql` runs read-only SQL against the local SQLite metadata projection. Use
it for counts, audits, joins, and metadata lookups that normal `ctx search` does
not express.

On a completely fresh data root, `ctx sql "SELECT 1"` initializes only an empty
`relational.sqlite` schema so the query can run. It does not create a Core
generation, refresh provider history, import files, return Core event bodies,
or populate the projection. Run `ctx setup` or `ctx import` when you need
history rows or daemon-owned projection maintenance.

When an active Core generation exists, SQL uses the latest coherent relational
projection. Core may already be newer, or a later catch-up may have failed:
that published projection remains queryable, and JSON results report the
relational generation, the observed Core generation, `stale: true`, and a
projection status of `ready` or `behind`. A `behind` projection contains the
complete prior generation, never partially updated rows. Each invocation pins
one internally consistent SQLite read transaction; a later invocation may
observe a newer coherent projection. When Core is absent, only the canonical
empty projection is accepted; a populated or generation-bound projection fails
closed.

The projection contains metadata only. It does not store event bodies,
previews, command or result payloads, structured content, or transcript text.
Use `ctx search` for transcript search and `ctx show` for complete Core-backed
event content.

## Examples

```bash
ctx sql "SELECT provider, COUNT(*) AS sessions FROM ctx_sessions GROUP BY provider"
ctx sql "SELECT event_type, COUNT(*) AS events FROM ctx_events GROUP BY event_type ORDER BY events DESC"
ctx sql --format json "SELECT ctx_session_id, cwd FROM ctx_sessions ORDER BY started_at_ms DESC LIMIT 5"
ctx sql --format csv --file query.sql
ctx sql - --format raw < query.sql
```

These complete queries use only stable views and can be copied into `ctx sql`:

```sql
SELECT provider, COUNT(*) AS sessions
FROM ctx_sessions
GROUP BY provider
ORDER BY provider;
```

```sql
SELECT ctx_session_id, event_seq, event_type, role
FROM ctx_events
ORDER BY ctx_session_id, event_seq
LIMIT 20;
```

```sql
SELECT f.path, f.provider, s.provider_session_id, f.ctx_session_id
FROM ctx_files_touched AS f
JOIN ctx_sessions AS s USING (ctx_session_id)
WHERE f.path LIKE '%AGENTS.md%'
ORDER BY f.observed_at_ms DESC
LIMIT 20;
```

```sql
SELECT logical_repository_id, observation_kind, object_id, reference_name
FROM ctx_vcs_observations
WHERE object_id IS NOT NULL
ORDER BY observed_at_ms DESC
LIMIT 20;
```

## Stable Views

Prefer the eight stable `ctx_*` views below. Internal tables are implementation
details and can change between relational schema versions.

### `ctx_sessions`

One row per Core session.

| Column | Meaning |
| --- | --- |
| `ctx_session_id` | ctx-owned session ID for `ctx show session`. |
| `parent_ctx_session_id` | Parent ctx session ID, when the parent is present in the projection. |
| `root_ctx_session_id` | Root ctx session ID, when the root is present in the projection. |
| `source_id` | Stable Core source ID. |
| `provider` | Provider name such as `codex`, `claude`, or `opencode`. |
| `source_format` | Provider source format. |
| `provider_session_id` | Provider-owned session ID, when available. |
| `agent_type` | Provider-normalized agent type. |
| `is_primary` | `1` for a primary agent and `0` otherwise. |
| `branch` | Captured branch, when available. |
| `workspace` | Captured workspace, when available. |
| `cwd` | Captured working directory, when available. |
| `started_at_ms` | Earliest event time in Unix epoch milliseconds, when available. |
| `ended_at_ms` | Latest event time in Unix epoch milliseconds, when available. |
| `health` | Relational session health. |

### `ctx_events`

One row per Core event. `event_seq` is a 20-character, zero-padded unsigned
decimal string. This preserves the complete Core `u64` range and means ordinary
`ORDER BY event_seq` uses the same order as unsigned Core sequences.

| Column | Meaning |
| --- | --- |
| `ctx_event_id` | ctx-owned event ID for `ctx show event`. |
| `ctx_session_id` | Owning ctx session ID. |
| `source_id` | Stable Core source ID. |
| `provider` | Provider name. |
| `source_format` | Provider source format. |
| `provider_session_id` | Provider-owned session ID, when available. |
| `native_event_id_json` | Provider-native event ID encoded as JSON, when available. |
| `event_seq` | Exact sortable Core `u64` sequence as 20-character decimal text. |
| `event_type` | Provider-normalized event type. |
| `role` | Event role such as `user`, `assistant`, or `tool`, when available. |
| `occurred_at_ms` | Event time in Unix epoch milliseconds, when available. |
| `parser_revision` | Parser revision that produced the Core record. |
| `normalization_revision` | Core normalization revision. |
| `content_policy_revision` | Core content-policy revision. |
| `content_policy_status` | `selected`, `redacted`, or `omitted`. |
| `branch` | Session branch, when available. |
| `workspace` | Session workspace, when available. |
| `cwd` | Session working directory, when available. |

### `ctx_files_touched`

Repository-scoped file observations attached to Core events.

| Column | Meaning |
| --- | --- |
| `ctx_file_touch_id` | Stable projection row ID composed from event ID and ordinal. |
| `ctx_event_id` | Associated ctx event ID. |
| `ctx_session_id` | Associated ctx session ID. |
| `source_id` | Stable Core source ID. |
| `provider` | Provider name. |
| `source_format` | Provider source format. |
| `repository_binding_id` | Event-local repository binding ID. |
| `logical_repository_id` | Stable logical repository ID. |
| `path` | Repository-relative observed path. |
| `old_path` | Prior repository-relative path for a rename, when available. |
| `observation_kind` | Normalized file observation kind. |
| `observed_at_ms` | Observation time in Unix epoch milliseconds, when available. |

### `ctx_sources`

One row per source in the active Core generation.

| Column | Meaning |
| --- | --- |
| `source_id` | Stable Core source ID. |
| `provider` | Provider name. |
| `source_format` | Provider source format. |
| `schema_variant` | Provider schema variant. |
| `provider_identity_version` | Provider identity-contract version. |
| `parser_revision` | Parser revision recorded by Core. |
| `indexed_event_count` | Number of projected Core events for the source. |
| `health` | Source health in the active projection. |

### `ctx_repositories`

Repository bindings selected for Core events.

| Column | Meaning |
| --- | --- |
| `ctx_event_id` | Associated ctx event ID. |
| `ctx_session_id` | Associated ctx session ID. |
| `repository_binding_id` | Event-local repository binding ID. |
| `logical_repository_id` | Stable logical repository ID. |
| `checkout_id` | Stable checkout ID, when available. |
| `worktree_id` | Stable worktree ID, when available. |
| `git_object_format` | Git object format, when known. |
| `association_policy_revision` | Repository-association policy revision. |

### `ctx_vcs_observations`

Repository-scoped version-control observations attached to Core events.

| Column | Meaning |
| --- | --- |
| `ctx_event_id` | Associated ctx event ID. |
| `ctx_session_id` | Associated ctx session ID. |
| `repository_binding_id` | Event-local repository binding ID. |
| `logical_repository_id` | Stable logical repository ID. |
| `observation_kind` | Normalized VCS observation kind. |
| `object_format` | Object ID format, when available. |
| `object_id` | Commit or object ID, when available. |
| `reference_name` | Branch or reference name, when available. |
| `relative_path` | Repository-relative path, when available. |
| `outcome_json` | Complete typed repository outcome payload for `outcome` observations; otherwise null. |
| `observed_at_ms` | Observation time in Unix epoch milliseconds, when available. |

### `ctx_repository_abstentions`

Cases where Core intentionally did not select a repository binding.

| Column | Meaning |
| --- | --- |
| `ctx_event_id` | Associated ctx event ID. |
| `ctx_session_id` | Associated ctx session ID. |
| `evidence_kind` | Repository evidence kind considered by Core. |
| `reason` | Stable abstention reason. |
| `association_policy_revision` | Repository-association policy revision. |

### `ctx_projection_metadata`

The active relational receipt and its binding to one committed Core generation.

| Column | Meaning |
| --- | --- |
| `schema_version` | Relational SQLite schema version. |
| `contract_version` | Stable-view contract version. |
| `materializer_revision` | Relational materializer revision. |
| `build_generation` | Monotonic relational rebuild/catch-up counter. |
| `core_generation_id` | Active committed Core generation ID. |
| `target_core_generation_id` | Requested Core generation while the projection is behind. |
| `status` | `empty`, `ready`, or `behind`. |
| `source_count` | Active source count. |
| `session_count` | Active session count. |
| `event_count` | Active event count. |
| `repository_binding_count` | Active repository-binding count. |
| `file_touch_count` | Active file-observation count. |
| `vcs_observation_count` | Active VCS-observation count. |
| `last_error` | Bounded last catch-up error, when present. |
| `core_manifest_version` | Active Core manifest version. |
| `core_record_version` | Active Core record version. |
| `core_record_contract_fingerprint` | Active Core record-contract fingerprint. |
| `core_lexical_schema_version` | Active Core lexical schema version. |
| `core_policy_schema_hash` | Active Core policy schema hash. |

## File Path Queries

Touched-file rows are metadata about repository-relative paths observed in
provider events. They are not a live filesystem index. Join `ctx_sessions` when
you also need provider-owned session identity:

```sql
SELECT f.path, f.provider, s.provider_session_id, f.ctx_session_id
FROM ctx_files_touched AS f
JOIN ctx_sessions AS s USING (ctx_session_id)
WHERE f.path = 'crates/ctx-cli/src/main.rs'
ORDER BY f.observed_at_ms DESC
LIMIT 20;
```

Combine file metadata with normal search when you need transcript relevance:

```bash
ctx search "release blocker" --file crates/ctx-cli/src/main.rs
```

## Input And Output

Pass SQL as an argument, from stdin with `-`, or with `--file`:

```bash
ctx sql "SELECT COUNT(*) FROM ctx_events"
ctx sql - < query.sql
ctx sql --file query.sql
```

Formats:

- `--format table`, the default human-readable table;
- `--format json`, structured output with columns, rows, limits, timing, and truncation;
- `--format csv`, script-friendly CSV;
- `--format raw`, one-column raw lines for piping.

`--format raw` requires exactly one selected column.

## Limits

`ctx sql` is intentionally bounded:

- read-only statements only;
- one statement per invocation;
- no query parameters;
- default row, column, SQL byte, and value byte caps;
- a timeout for long-running queries.

`--max-columns` limits the final selected result columns. SQLite view expansion
uses the separate fixed product cap, so a two-column query over wider stable
views does not fail merely because those views expose more than two columns.

Increase limits only when a local script needs them:

```bash
ctx sql "SELECT * FROM ctx_events LIMIT 500" --max-rows 500 --timeout 30s
```

Keep SQL output local unless you have reviewed it. Paths, provider identifiers,
workspace metadata, and repository names can contain private data.
