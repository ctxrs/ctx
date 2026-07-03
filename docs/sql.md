# SQL

`ctx sql` runs read-only SQL against the existing local ctx SQLite index. Use it
when normal `ctx search` does not express the question: counts, audits, joins,
file/session metadata lookups, or scripts that need structured output.

`ctx sql` does not refresh provider history, import files, initialize storage, or
migrate schemas. Run `ctx status`, `ctx setup`, or `ctx import` first if the
local store needs to be created or migrated.

## Examples

```bash
ctx sql "SELECT provider, COUNT(*) AS sessions FROM ctx_sessions GROUP BY provider"
ctx sql "SELECT event_type, COUNT(*) AS events FROM ctx_events GROUP BY event_type ORDER BY events DESC"
ctx sql "SELECT path, provider, provider_session_id FROM ctx_files_touched WHERE path LIKE '%AGENTS.md%' LIMIT 20"
ctx sql --format json "SELECT ctx_session_id, cwd FROM ctx_sessions ORDER BY started_at_ms DESC LIMIT 5"
ctx sql --format csv --file query.sql
ctx sql - --format raw < query.sql
```

Use normal `ctx search` for transcript text search. Avoid broad scans over
`payload_json`; event payloads can be large and normal search is optimized for
finding text.

## Stable Views

Prefer stable `ctx_*` views. Internal tables remain queryable locally, but they
are implementation details and can change between versions.

`ctx_sessions`:

| Column | Meaning |
| --- | --- |
| `ctx_session_id` | ctx-owned session ID for `ctx show session`. |
| `history_record_id` | ctx history record backing the session, when known. |
| `parent_ctx_session_id` | Parent ctx session ID for subagent/session trees. |
| `root_ctx_session_id` | Root ctx session ID for session trees. |
| `provider` | Provider name such as `codex`, `claude`, or `opencode`. |
| `provider_session_id` | Provider-owned session ID. |
| `external_agent_id` | Provider-owned agent identifier, when present. |
| `agent_type` | `primary`, `subagent`, `reviewer`, `implementer`, or related type. |
| `role_hint` | Provider/importer role hint. |
| `is_primary` | `1` for primary-agent sessions, `0` otherwise. |
| `status` | Imported/session status. |
| `fidelity` | Import fidelity. |
| `started_at_ms`, `ended_at_ms` | Unix epoch milliseconds. |
| `cwd` | Captured working directory, when known. |
| `source_path` | Raw provider source path, when known. |

`ctx_events`:

| Column | Meaning |
| --- | --- |
| `ctx_event_id` | ctx-owned event ID for `ctx show event`. |
| `ctx_session_id` | ctx session ID, when known. |
| `history_record_id` | ctx history record backing the event, when known. |
| `provider`, `provider_session_id` | Provider context from the session. |
| `event_seq` | Provider/session event sequence. |
| `event_type` | `message`, `tool_call`, `tool_output`, `command_started`, `command_output`, `command_finished`, `file_touched`, `vcs_change`, `artifact`, `summary`, or `notice`. |
| `role` | Event role such as `user`, `assistant`, or `tool`, when known. |
| `occurred_at_ms` | Unix epoch milliseconds. |
| `payload_json` | Local private event payload. |
| `redaction_state` | Local payload handling state. `safe_preview` is legacy spelling for a local searchable preview, not share-safe redaction. |
| `fidelity` | Import fidelity. |
| `cwd`, `source_path` | Captured source context, when known. |

`ctx_files_touched`:

| Column | Meaning |
| --- | --- |
| `ctx_file_touch_id` | ctx-owned touched-file row ID. |
| `path`, `old_path` | Touched path and prior path for renames. |
| `change_kind` | `read`, `created`, `modified`, `deleted`, `renamed`, or `unknown`. |
| `line_count_delta` | Imported line delta, when known. |
| `confidence` | `explicit`, `high`, `medium`, `low`, or `unknown`. |
| `ctx_event_id` | Associated event ID, when importer knows it. |
| `ctx_session_id` | Associated session ID, resolved from event, run, or capture source. |
| `history_record_id` | Associated history record, resolved from event, run, row, or capture source. |
| `provider`, `provider_session_id` | Provider context, when resolvable. |
| `created_at_ms`, `updated_at_ms` | Unix epoch milliseconds. |

`ctx_sources`:

| Column | Meaning |
| --- | --- |
| `provider`, `source_format` | Provider and importer/source format. |
| `source_root`, `source_path` | Discovered provider source location. |
| `provider_session_id`, `parent_provider_session_id` | Provider session identifiers. |
| `agent_type`, `role_hint` | Imported session role metadata. |
| `cwd` | Captured working directory, when known. |
| `session_started_at_ms` | Provider session start time in Unix epoch milliseconds. |
| `file_size_bytes`, `file_modified_at_ms`, `cataloged_at_ms` | Catalog metadata. |
| `indexed_status`, `indexed_at_ms`, `indexed_error`, `indexed_event_count` | Import/index status. |

## File Path Queries

Touched-file rows are metadata about files mentioned by imported provider
events. They are not a live filesystem index. A row may be associated directly
with an event, with a command/run, with a history record, or only with a capture
source. The stable view resolves provider and session context when possible.

```sql
SELECT path, provider, provider_session_id, ctx_session_id
FROM ctx_files_touched
WHERE path = 'crates/ctx-cli/src/main.rs'
ORDER BY updated_at_ms DESC
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
- `--json`, alias for `--format json`;
- `--format csv`, script-friendly CSV;
- `--format raw`, one-column raw lines for piping.

`--format raw` requires exactly one selected column.

## Limits

`ctx sql` is intentionally bounded:

- read-only statements only;
- one statement per invocation;
- no query parameters;
- default row, column, SQL byte, and value byte caps;
- timeout for long-running queries;
- JSON output marked `share_safe: false`.

Increase limits only when scripting needs them:

```bash
ctx sql "SELECT * FROM ctx_events LIMIT 500" --max-rows 500 --timeout 30s
```

Keep SQL output local unless you have reviewed it. Payloads, paths, prompts,
tool output, and repository names can contain private data.
