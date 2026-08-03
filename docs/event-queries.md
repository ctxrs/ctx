# Event queries and JSONL

`ctx show events` enumerates complete normalized events from one immutable
Core/Tantivy generation. It is a deterministic machine-oriented complement to
ranked `ctx search` and session-oriented `ctx show session`; it is not a SQL or
general expression language.

The command is local and read-only. It does not refresh history, wake the
daemon, reopen provider files, query provider SQLite databases, initialize a
store, or write a derived generation. Provider history remains authoritative;
the query reads the complete policy-selected Core records already stored in a
verified generation.

## Selection and ordering

With no time arguments, the command enumerates all stored Core events.
Timestamped events are ordered first by timestamp, then event sequence and full
ctx event identity. Events without a timestamp follow in a deterministic tail.
`--direction descending` reverses that complete order, including the untimed
tail.

`--since` and `--until` must be supplied together. They select timestamped
events in the half-open interval `[since, until)`; untimed events are excluded
from a timestamp range.

The typed filters cover:

- provider and exact ctx source identity;
- custom history source, provider key, source ID, and source format;
- provider session and ctx session, parent-session, or root-session identity;
- branch, workspace, event type, role, and agent type;
- primary, subagent, or all agent scopes;
- indexed touched-file metadata.

Filters are applied before page and byte limits. Exact fields and chronology
ranges use the existing Tantivy index. Workspace and file filters use their
existing indexed lowercase projections. An exact public source ID is resolved
against the pinned generation manifest to its full source identity before the
query runs.

## Bounded pages and cursors

JSON returns one bounded page. JSONL reads a sequence of bounded internal pages
and writes each event as it becomes available. `--max-items` and
`--max-bytes` bound internal work; `--limit` bounds the complete command
result. The defaults are 100 events and 1 MiB per internal page, with a 10,000
event global limit; callers can choose lower bounds. A valid event larger than
a soft internal page budget is admitted as a
single item so pagination can always advance. A machine response that cannot
represent that singleton fails intact with a typed resource-limit error; it is
never clipped.

Continuation cursors are opaque, versioned, and exclusive. They bind the exact
normalized selection, direction, order key, and immutable generation. A cursor
from another filter set or range is rejected. A cursor whose generation is no
longer active or retained is rejected rather than silently restarted, because
automatic restart could duplicate or omit events.

JSONL contains zero or more `event_range_event` records followed by exactly one
`event_range_completion` record. Completion reports the pinned generation,
whether the selection is terminal or was truncated by the requested limit, a
continuation cursor when one exists, and exact usage metadata. EOF without
completion is an incomplete stream, even if preceding event lines were valid.
Diagnostics are written to stderr; stdout contains only the requested JSON or
JSONL.

## jq examples

Select complete message events from one provider:

```bash
ctx show events --provider codex --event-type message --content full \
  --limit 1000 --format jsonl |
  jq -c 'select(.record_type == "event_range_event") | .event'
```

Enumerate one root task's subagents while retaining only event identity,
relationship, and chronology fields:

```bash
ctx show events --root-session 01234567-89ab-8def-8123-456789abcdef \
  --scope subagent --content none --limit 1000 --format jsonl |
  jq -c 'select(.record_type == "event_range_event") | .event |
    {ctx_event_id, ctx_session_id, parent_ctx_session_id,
     root_ctx_session_id, sequence, occurred_at_ms, event_type}'
```

Query a time window and print normalized text without materializing the stream:

```bash
ctx show events --since 2026-08-01T00:00:00Z \
  --until 2026-08-02T00:00:00Z --content text --format jsonl |
  jq -r 'select(.record_type == "event_range_event") |
    [.event.ctx_event_id, (.event.text // "")] | @tsv'
```

Inspect completion and truncation metadata:

```bash
ctx show events --source 01234567-89ab-8def-8123-456789abcdef \
  --limit 100 --format jsonl |
  jq -c 'select(.record_type == "event_range_completion")'
```

Use ctx filters for selective work whenever possible. `jq` operates after
bytes leave ctx; its predicates are not indexed and do not make the underlying
query faster.

## Identity, content, and unknown types

Every event carries exact ctx event/source/session identities, provider and
provider-session identity where available, source format, native event
identity, parent/root lineage, chronology, role/type/agent fields, content
policy metadata, and normalized repository evidence already present in Core.
Core deliberately has no provider read-time locator, so the event surface does
not fabricate a provider path or reopen raw history to create one.

Event type is an open string on this surface. A future or provider-specific
event type already admitted to Core is emitted unchanged with its metadata and
structured content. Provider rows that a current normalizer classifies as
ignored never acquired a Core identity and therefore cannot be enumerated;
leaving those authoritative source bytes untouched is not the same as claiming
round-trip visibility.

All JSON and JSONL can contain transcript content, command arguments,
repository evidence, and local workspace paths. Treat it as private local data
and review it before sharing.
