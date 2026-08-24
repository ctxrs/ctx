# Custom History Import Format

`ctx-history-jsonl-v2` is the public JSONL format for importing session history
from tools without a built-in local-history adapter.

This is a breaking contract. `ctx-history-jsonl-v1` is unsupported: ctx does
not accept it as an input-format alias and does not translate v1 records. An
exporter must write v2 records directly.

## Transports

The same JSONL schema can be imported from an explicit local file path:

```bash
ctx import --input-format ctx-history-jsonl-v2 --path ./history.jsonl
```

or from a history-source plugin manifest that selects a durable provider-owned
file:

```bash
ctx import --history-source my-agent/default
```

ctx does not discover a fixed storage location for this format. File imports
are explicit paths. Plugin imports register a provider-owned path declared by a
local manifest; command-only sources are not importable. See
`docs/history-source-plugins.md`.

Each line is one JSON object. Every object has a `record_type` field with one
of:

- `manifest`
- `source`
- `session`
- `event`
- `file_reference`
- `edge`

Record order is flexible, but exporters should write a manifest first, then
source and session records before their dependent events, file references or
edges. The documented record envelopes are closed: unknown top-level fields
are rejected. `metadata` and `payload` accept any JSON value (not only an
object) and default to `{}` when omitted. Artifact descriptor objects are an
explicit exception to the record-envelope closure; see [Event](#event).

Acceptance by the envelope does not itself promise that a value is retained in
the current Core projection. The sections below identify exporter annotations
that are parsed but currently have no effect on Core identity, deduplication,
lineage, or stored records. All accepted fields still remain part of the exact
source bytes hashed for checkpoint and change detection, so changing a
discarded annotation can trigger a rescan or replacement publication even
though the resulting Core records are unchanged.

## Manifest

Exactly one manifest record must appear in the file, preferably at the top.

Required fields:

- `schema_version`: must be `"ctx-history-jsonl-v2"`.

Optional fields:

- `lineage_contract`: `provider_native_v1` to opt into typed relationship and
  copied-event selectors.
- `producer`: an opaque exporter label.
- `exported_at`: an RFC 3339 timestamp for when the exporter produced this
  file.
- `metadata`: exporter-defined JSON, defaulting to `{}`.

`producer`, `exported_at`, and manifest `metadata` are descriptive annotations
only. They are parsed but are not retained in the current Core projection and
do not affect import identity or lineage.

Example:

```json
{"record_type":"manifest","schema_version":"ctx-history-jsonl-v2","lineage_contract":"provider_native_v1","metadata":{"exporter":"example"}}
```

Omitting `lineage_contract` declares no provider-native lineage proof.
Parent/root identities remain available, but no typed relationship kind is
claimed, and every event origin remains `unknown`. Lineage fields in the file
do not strengthen those claims.

## Source

A source describes an exporting system, input corpus, or incremental cursor.
A file may contain multiple source records, provided each `source_id` is unique;
this lets one export preserve several independently addressable routes.

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
- `trust`: `provider_native`, `provider_export`, `wrapper_observed`,
  `fixture`, `synthetic`, or `unknown` (the default).
- `fidelity`: `full`, `partial`, `imported`, `inferred`, or `summary_only`
  (the default is `imported`).
- `cursor`
- `metadata`: exporter-defined JSON, defaulting to `{}`.

`provider_key` is the exporter-owned namespace, such as `my-agent` or
`internal-build-bot`. The canonical provider route identity is the
`provider_key`/`source_id` pair. A session record's `provider_session_id` is the
provider's one stable session identifier on that route. ctx preserves that
literal provider ID and separately derives its own `ctx_session_id`; exporters
must not place a ctx ID in `provider_session_id`.

`provider_key` must be 1 to 128 bytes, start with a lowercase ASCII letter or
digit, and contain only lowercase ASCII letters, digits, `.`, `_`, or `-`.
`source_id` must not contain `/`. All identity fields reject leading or trailing
whitespace, control characters, blank values, and values over 512 bytes.

Only `source_id` and `provider_key` are retained in the current Core
projection. `source_format` is validated but is not the Core route format;
Core reports the public route as `ctx_history_jsonl_v2`. The remaining source
fields from `raw_uri` through `cursor`, plus `trust`, `fidelity`, and
`metadata`, are parsed but not projected. In particular, ctx does not use
producer-declared trust or fidelity to certify the file, grant provider-native
lineage authority, or alter source identity.

Example:

```json
{"record_type":"source","source_id":"laptop-main","provider_key":"my-agent","source_format":"my-agent-export-v3","raw_source_path":"/home/me/.my-agent/history.jsonl","cursor":{"after":{"stream":"my-agent:laptop-main","cursor":"171","observed_at":"2026-06-23T12:00:00Z"}},"metadata":{"team":"tools"}}
```

## Session

A session describes one conversation, task, or agent run.

Required fields:

- `source_id`
- `provider_session_id`
- `started_at`

Optional fields:

- `parent_provider_session_id`
- `root_provider_session_id`
- `session_relationship`: `root`, `delegated`, `forked`, `resumed_from`,
  or `workflow_child`.
- `external_agent_id`
- `cwd`
- `ended_at`
- `agent_scope`: `primary` or `subagent`.
- `role_hint`
- `status`: `started`, `active`, `idle`, `completed`, `failed`, `interrupted`,
  or `imported` (the default).
- `fidelity`
- `idempotency_key`
- `artifacts`
- `metadata`

With `lineage_contract: provider_native_v1`, use `session_relationship` with
`parent_provider_session_id` and `root_provider_session_id` to state the
provider's typed relationship. `root` has no parent. Every other relationship
requires a parent. Use `agent_scope` to state the exporter-known
primary/subagent scope; when omitted, scope remains unknown.
`session_relationship` is independent lineage authority and does not imply an
agent scope.

Session `fidelity` accepts the same five values as source fidelity and defaults
to `imported`. `idempotency_key` is an opaque exporter string. `artifacts` is
an array of artifact descriptors with the same shape described for event
artifacts below. The current Core projection retains session identity and
lineage fields, `agent_scope`, and `cwd`. It does not retain `started_at`,
`ended_at`, `external_agent_id`, `role_hint`, `status`, `fidelity`,
`idempotency_key`, `artifacts`, or session `metadata`; an `idempotency_key`
does not change the file-level idempotent import behavior.

Example:

```json
{"record_type":"session","source_id":"laptop-main","provider_session_id":"abc123","cwd":"/workspace/app","started_at":"2026-06-23T12:00:00Z","agent_scope":"primary","role_hint":"developer","status":"completed"}
```

## Event

An event is a time-ordered item inside a session.

Required fields:

- `source_id`
- `provider_session_id`
- `event_index`: unsigned 64-bit integer.
- `occurred_at`

Optional fields:

- `event_id`
- `copied_from`
- `native_cursor`
- `event_hash`
- `event_type`
- `role`
- `fidelity`: `full`, `partial`, `imported`, `inferred`, or `summary_only`
  (the default is `imported`).
- `idempotency_key`
- `artifacts`
- `payload`
- `preview`
- `metadata`: exporter-defined JSON, defaulting to `{}`.

`event_index` is the stable exporter order within the session. `native_cursor`
and `preview` are accepted as advisory import hints but are not persisted in
Core. `payload` is arbitrary JSON and is retained as structured event content
subject to Core content limits; ctx derives the normalized event text from it.
Exporters that need stable event identity must use `event_id` and
`event_index`, while durable event content belongs in `payload` rather than the
advisory fields.

`event_hash` and `idempotency_key` are opaque exporter strings. ctx does not
select a hash algorithm, verify `event_hash`, or use either field in event
identity or event deduplication. Neither value is interpreted as a semantic
checkpoint token, although its raw source bytes still participate in the
file-level checkpoint and change digest. Event identity uses `event_id` when
present and otherwise uses `event_index`.

Event `fidelity` is a producer annotation with the same values and default as
source fidelity; it is not currently projected into Core. `artifacts` defaults
to an empty array. Each descriptor requires `provider_artifact_id` and `kind`;
`kind` is one of `transcript`, `stdout`, `stderr`, `screenshot`, `report`,
`diff`, `file_snapshot`, `json`, `markdown`, or `binary`. It may also contain
`media_type`, `source_path`, `preview_text`, `byte_size`, and JSON `metadata`
(default `{}`). Artifact descriptor objects accept unknown keys, but the
current importer does not load artifact content or project descriptors into
Core. Event `metadata` is also parsed but not projected.

`copied_from` is accepted only under `provider_native_v1`. Its fields are:

- `ancestor_provider_session_id`: the provider's stable ID for an
  ancestor session in the same source;
- `ancestor_event_id`: the provider's stable native ID for one event in that
  ancestor;
- `proof`: `native_event_identity`, `native_copied_from_field`, or
  `native_call_result_identity`.

The child event must have a unique stable `event_id`, the child session must
carry a typed non-root relationship, and the selected provider session must be
distinct from the child. The selector is a child-owned ancestor claim and does
not require the ancestor record to be present. `native_event_identity` also
requires the child `event_id` to equal the selected ancestor event ID. An
unstable, self-referential, or proof-inconsistent selector leaves event origin
`unknown`. Similar text, timestamps, roles, tool names, and payload content are
never copy authority. Selectors contain identities only;
do not put transcript content, prompts, paths, or other private payloads in
them. Generic Custom History cannot declare `certified_ordered_prefix`; that
classification requires a built-in adapter that validates its provider's
complete ordered-prefix contract. The `provider_native_v1` contract and
`native_*` proof names are lineage proof terminology inside the v2 schema;
they do not introduce a second session ID.

Example copied event:

```json
{"record_type":"event","source_id":"laptop-main","provider_session_id":"run-2","event_index":0,"event_id":"native-child-event-1","copied_from":{"ancestor_provider_session_id":"run-1","ancestor_event_id":"native-event-1","proof":"native_copied_from_field"},"event_type":"message","role":"user","occurred_at":"2026-06-23T12:10:01Z","payload":{"text":"Find the failing test."}}
```

Example:

```json
{"record_type":"event","source_id":"laptop-main","provider_session_id":"run-1","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T12:00:01Z","payload":{"text":"Find the failing test."},"preview":"Find the failing test.","native_cursor":"line:42"}
```

## File Reference

Use `file_reference` for current exports. It records an exact
provider-declared file value associated with an event.

Required fields:

- `source_id`
- `provider_session_id`
- `reference_index`: unsigned 64-bit integer.
- `event_index`: unsigned 64-bit index of an event in the same session.
- `value`: non-empty exact provider-declared file string.
- `occurred_at`

Optional fields:

- `metadata`: exporter-defined JSON, defaulting to `{}`.

ctx does not normalize `value`. Repeated values are retained when they have
different `reference_index` values. `event_index` is required because every
file fact must attach to a specific event. The current Core projection retains
`value` as that event's file fact. `reference_index` is used for duplicate
validation but is not projected; `occurred_at` and `metadata` are also parsed
but discarded.

Example:

```json
{"record_type":"file_reference","source_id":"laptop-main","provider_session_id":"run-1","reference_index":0,"event_index":1,"value":"crates/app/src/lib.rs","occurred_at":"2026-06-23T12:00:03Z"}
```

## Edge

An edge records a relationship between two sessions from the same source.

Required fields:

- `source_id`
- `from_provider_session_id`
- `to_provider_session_id`

Optional fields:

- `edge_id`
- `relationship`: `delegated`, `forked`, `resumed_from`, or
  `workflow_child`.
- `occurred_at`
- `fidelity`: `full`, `partial`, `imported`, `inferred`, or `summary_only`
  (the default is `imported`).
- `metadata`: exporter-defined JSON, defaulting to `{}`.

`edge_id` is used for duplicate validation but is not projected.
`occurred_at`, `fidelity`, and `metadata` are accepted annotations and are not
currently projected. Under `provider_native_v1`, `source_id`, the two provider
session IDs, and a typed `relationship` can supply the child session's direct
parent and relationship claim; an edge does not create any additional inferred
relationship.

Example:

```json
{"record_type":"edge","source_id":"laptop-main","from_provider_session_id":"run-1","to_provider_session_id":"run-1-worker","relationship":"delegated","occurred_at":"2026-06-23T12:00:05Z"}
```

## Incremental Semantics

v2 imports are explicit, local, and idempotent. On each file import, ctx
rescans the file through its explicit provider-source route. A selected plugin
manifest may point at the same kind of durable provider-owned file; ctx
validates the declared source identity, registers the path in place, and waits
for an authoritative daemon-owned publication receipt.

Import copies policy-selected normalized content into Core. Plugin imports do
not copy command output, write the old Store database, or synthesize a
`NativePath` body. Command-only plugin manifests are typed unsupported in 1.0.
They cannot declare `lineage_contract` or copied-event authority.

If an import is interrupted, run the same command again. The shared custom
JSONL route performs another idempotent scan of the provider-owned source.

## Compact Example

```jsonl
{"record_type":"manifest","schema_version":"ctx-history-jsonl-v2"}
{"record_type":"source","source_id":"demo-source","provider_key":"demo-agent","source_format":"demo-jsonl","raw_source_path":"/tmp/demo-history.jsonl","cursor":{"after":{"stream":"demo-agent:demo-source","cursor":"3","observed_at":"2026-06-23T12:00:00Z"}}}
{"record_type":"session","source_id":"demo-source","provider_session_id":"demo-session","cwd":"/workspace/demo","started_at":"2026-06-23T12:00:00Z","agent_scope":"primary","role_hint":"developer","status":"completed"}
{"record_type":"event","source_id":"demo-source","provider_session_id":"demo-session","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T12:00:01Z","payload":{"text":"Add a parser test."},"preview":"Add a parser test.","native_cursor":"line:1"}
{"record_type":"file_reference","source_id":"demo-source","provider_session_id":"demo-session","reference_index":0,"event_index":0,"value":"tests/parser.rs","occurred_at":"2026-06-23T12:00:02Z"}
```
