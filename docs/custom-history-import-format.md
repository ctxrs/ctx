# Custom History Import Format

`ctx-history-jsonl-v1` is the public JSONL format for importing session history
from tools without a built-in local-history adapter.

## Transport

Version 1 uses an explicit local file path:

```bash
ctx import --format ctx-history-jsonl-v1 --path ./history.jsonl
```

ctx does not discover a fixed storage location for this format. The file is not
read from stdin, and ctx does not execute exporter commands in v1.

Each line is one JSON object. Every object has a `record_type` field with one
of:

- `manifest`
- `source`
- `session`
- `event`
- `file_touch`
- `edge`

Record order is flexible, but exporters should write a manifest first, then
source and session records before their dependent events, file touches, and
edges. Unknown fields are ignored unless they are inside `metadata`, `payload`,
or another explicitly documented open object.

## Manifest

Exactly one manifest record should appear near the top of the file.

Required fields:

- `schema_version`: must be `"ctx-history-jsonl-v1"`.

Example:

```json
{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1","metadata":{"exporter":"example"}}
```

## Source

A source describes the exporting system, input corpus, or incremental cursor.

Required fields:

- `source_id`
- `provider_key`
- `source_format`

Optional fields:

- `raw_uri`
- `raw_source_path`
- `fingerprint`
- `importer_version`
- `observed_at`
- `machine_id`
- `cursor`
- `metadata`

`provider_key` is the exporter-owned namespace, such as `my-agent` or
`internal-build-bot`. Internally ctx stores these rows under the bounded
provider value `custom`, then derives internal session IDs from the structured
`provider_key`, `source_id`, and `session_id` tuple. Public provider, source,
and session IDs are preserved in metadata for display and lookup.

`provider_key` must be 1 to 128 bytes, start with a lowercase ASCII letter or
digit, and contain only lowercase ASCII letters, digits, `.`, `_`, or `-`.

Example:

```json
{"record_type":"source","source_id":"laptop-main","provider_key":"my-agent","source_format":"my-agent-export-v3","raw_source_path":"/home/me/.my-agent/history.jsonl","cursor":{"after":{"stream":"my-agent:laptop-main","cursor":"171","observed_at":"2026-06-23T12:00:00Z"}},"metadata":{"team":"tools"}}
```

## Session

A session describes one conversation, task, or agent run.

Required fields:

- `source_id`
- `session_id`
- `started_at`

Optional fields:

- `parent_session_id`
- `root_session_id`
- `native_session_id`
- `cwd`
- `ended_at`
- `agent_type`
- `role_hint`
- `is_primary`
- `status`
- `metadata`

Use `parent_session_id` and `root_session_id` to model subagents, forks,
handoffs, or resumed tasks when the exporter knows those relationships.

Example:

```json
{"record_type":"session","source_id":"laptop-main","session_id":"run-1","native_session_id":"abc123","cwd":"/workspace/app","started_at":"2026-06-23T12:00:00Z","agent_type":"primary","role_hint":"developer","is_primary":true,"status":"completed"}
```

## Event

An event is a time-ordered item inside a session.

Required fields:

- `source_id`
- `session_id`
- `event_index`: unsigned 64-bit integer.
- `occurred_at`

Optional fields:

- `event_id`
- `native_cursor`
- `event_type`
- `role`
- `payload`
- `preview`
- `metadata`

`event_index` is the stable exporter order within the session. Use
`native_cursor` for provider cursor tokens or byte offsets that should survive
re-imports. `payload` is open JSON; `preview` should be a bounded searchable
summary when payloads are large or sensitive. When `preview` is present, ctx
uses it as the event's searchable payload and preserves any non-empty `payload`
under import metadata.

Example:

```json
{"record_type":"event","source_id":"laptop-main","session_id":"run-1","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T12:00:01Z","payload":{"text":"Find the failing test."},"preview":"Find the failing test.","native_cursor":"line:42"}
```

## File Touch

A file touch records a path that the session read, wrote, created, deleted, or
renamed.

Required fields:

- `source_id`
- `session_id`
- `touch_index`: unsigned 64-bit integer.
- `path`
- `occurred_at`

Optional fields:

- `event_index`
- `change_kind`
- `old_path`
- `line_count_delta`
- `confidence`
- `metadata`

`event_index` links the touch to an event when known. Use `old_path` for
renames, `line_count_delta` for approximate net line changes, and `confidence`
when a touch is inferred from text rather than structured tool output.

Example:

```json
{"record_type":"file_touch","source_id":"laptop-main","session_id":"run-1","touch_index":0,"event_index":1,"path":"crates/app/src/lib.rs","change_kind":"modified","line_count_delta":12,"confidence":"high","occurred_at":"2026-06-23T12:00:03Z"}
```

## Edge

An edge records a relationship between two sessions from the same source.

Required fields:

- `source_id`
- `from_session_id`
- `to_session_id`
- `edge_type`

Optional fields:

- `edge_id`
- `confidence`
- `occurred_at`
- `metadata`

Example:

```json
{"record_type":"edge","source_id":"laptop-main","from_session_id":"run-1","to_session_id":"run-1-worker","edge_type":"spawned","confidence":"explicit","occurred_at":"2026-06-23T12:00:05Z"}
```

## Incremental Semantics

v1 imports are explicit, local, and idempotent. On each
`ctx import --format ctx-history-jsonl-v1 --path <file>`, ctx rescans the file
and upserts equivalent records instead of appending duplicates.

When a source record supplies `cursor`, ctx rewrites its storage stream under a
`provider:custom:<provider_key>:<opaque-id>` namespace and also preserves the
exporter-supplied cursor object in source metadata. Event `native_cursor` values
are also preserved. ctx does not negotiate with external exporters in v1, does
not call exporter commands, and does not request a delta range; exporter
negotiation is a follow-up capability.

If an import is interrupted, run the same command again. The expected behavior
is another idempotent rescan of the same JSONL file.

## Compact Example

```jsonl
{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}
{"record_type":"source","source_id":"demo-source","provider_key":"demo-agent","source_format":"demo-jsonl","raw_source_path":"/tmp/demo-history.jsonl","cursor":{"after":{"stream":"demo-agent:demo-source","cursor":"3","observed_at":"2026-06-23T12:00:00Z"}}}
{"record_type":"session","source_id":"demo-source","session_id":"demo-session","cwd":"/workspace/demo","started_at":"2026-06-23T12:00:00Z","agent_type":"primary","role_hint":"developer","is_primary":true,"status":"completed"}
{"record_type":"event","source_id":"demo-source","session_id":"demo-session","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T12:00:01Z","payload":{"text":"Add a parser test."},"preview":"Add a parser test.","native_cursor":"line:1"}
{"record_type":"file_touch","source_id":"demo-source","session_id":"demo-session","touch_index":0,"event_index":0,"path":"tests/parser.rs","change_kind":"modified","confidence":"high","occurred_at":"2026-06-23T12:00:02Z"}
```
