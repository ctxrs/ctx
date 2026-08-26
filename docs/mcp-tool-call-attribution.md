# Exact MCP activity attribution

ctx preserves provider-native tool activity in the optional `activity` field on
current Core event rows. For a qualified MCP invocation, `activity.invocation`
contains `protocol: "mcp"`, the exact source server alias, and the advertised
tool name. The same activity envelope can also retain exact provider call
identity, arguments, terminal result channels, timestamps, and literal
provider-declared facts.

This is content-governed Core activity, not a dedicated top-level attribution
object. It adds no MCP selector, entity, endpoint identity, success inference,
or cross-record causal claim.

## Wire contract

A full event row can contain this snake_case object:

```json
{
  "activity": {
    "revision": 1,
    "provider_call_id": {"Utf8": "call-01"},
    "invocation": {
      "protocol": "mcp",
      "server": "node_repl",
      "tool": "js",
      "arguments": {
        "capture_status": "present",
        "value": {"code": "1 + 1"}
      },
      "started_at_unix_ms": 1786900000000
    },
    "result": {
      "status": "provider::ok",
      "completed_at_unix_ms": 1786900000100,
      "duration_ns": 100000000,
      "text": {"capture_status": "normalized_body"},
      "structured_content": {"capture_status": "absent"}
    }
  }
}
```

`revision` is currently `1`. `provider_call_id` is exact typed provider-native
key material. Providers that emit separate invocation and result events use the
same key on both; a provider that emits one combined terminal event may include
both members in one activity envelope.

An exact MCP invocation has all of the following:

- `protocol` equal to `mcp`;
- a nonempty exact `server` alias declared by the source;
- a nonempty exact advertised `tool` name; and
- a nonempty exact `provider_call_id` when invocation or result content is
  present.

Strings are decoded UTF-8 and bounded to 64 KiB by the current Core contract.
They are not trimmed, normalized, split from a combined name, reconstructed
from current configuration, or truncated into a qualifying value. Partial,
malformed, duplicate, ambiguous, or oversized identity evidence does not become
an exact MCP invocation. The ordinary event and any separately admissible
literal facts or result content can still be retained.

Arguments and structured results use `present`, `absent`, `unavailable`, or
`omitted` capture states. Result text additionally supports
`normalized_body`. These states describe capture completeness; they do not
interpret provider success, failure, or side effects. `status` is an
uninterpreted provider string. For Codex, an empty provider result string is
`absent` in the text channel and remains an exact empty string in the
structured-content channel.

## Provider and format capability

General provider support remains the 41-provider local-history contract in
[`provider-support-matrix.json`](provider-support-matrix.json). Exact MCP
activity attribution has a separate provider + route + source format + format
version contract in
[`mcp-tool-call-attribution-capabilities.json`](mcp-tool-call-attribution-capabilities.json).

Capability revision 4 evaluates all 41 providers across 46 base routes and 49
capability lanes: three `exact`, 45 `not-qualified`, and one `excluded`.
Codex contributes separate session-tree and legacy prompt-history routes; Deep
Agents contributes its local SQLite import plus a separately excluded hosted
trace. Capability revision 4 exact providers are Codex, Warp, and Copilot CLI.
The exact full tuples are:

- Codex `codex_session_jsonl_tree` / `codex-nativepath-jsonl-v0`, for
  unversioned producer generation 1 only. Codex producer versions 0.200.0,
  0.201.0, and 0.202.0 are
  separate explicit `not-qualified` lanes and never inherit exact status.
- Warp `warp_sqlite` / `warp-agent-task-protobuf-v1`, for strict unversioned
  generation 1. The pinned source commits are evidence for that shape, not
  runtime writer-version selectors.
- Copilot CLI `copilot_cli_session_events_jsonl` /
  `copilot-cli-direct-native-jsonl-v1`, for strict unversioned generation 1.
  Observed versions and the pinned source commit are evidence, not runtime
  admission selectors.

The 45 `not-qualified` rows do not mean their providers are unsupported. They
mean only that ctx does not claim exact MCP server/tool activity for those
tuples. New source variants or versions require their own non-overlapping row;
unknown generations fail closed. The public
[evidence runbook](mcp-tool-call-attribution-evidence.md) defines the exactness
bar and executable evidence roles.

## CLI access and filtering

Show operations return complete selected activity:

```bash
ctx show event <ctx-event-id> --format json
ctx show session <ctx-session-id> --mode log --format jsonl
```

Chronology enumeration returns activity only with the full projection:

```bash
ctx list events --provider codex --content full --format jsonl |
  jq -c 'select(.record_type == "event_range_event") |
    .event | select(.activity.invocation.protocol? == "mcp") |
    {ctx_event_id, ctx_session_id, activity}'
```

`--content text` and `--content none` omit `activity` from chronology rows.
Filtering those rows is client-side after ctx emits each row. There is no
dedicated server/tool filter, query selector, SQL column, or separate MCP
attribution command.

For discovery-eligible selected content, retained invocation
protocol/server/tool/present arguments, result status/present text/structured content,
and literal facts enter the shared Core search projection. That projection
supplies lexical terms and snippets as well as semantic source text, so these
values can affect ordinary matching and ranking like other retained Core
content. Provider call IDs, timestamps, durations, and capture-disposition
labels do not enter the projection, and a `normalized_body` result reference
does not duplicate the event body.

## MCP access and pagination

MCP `show_event` and `show_session` return the same optional snake_case
`activity` object in event rows inside `structuredContent`. `query_events`
returns it only when `content` is `full`; `text` and `none` omit it.

For a complete attributed session scan, call `show_session` with the session
ID, `mode: "log"`, and a bounded `limit`. Filter each page on
`activity.invocation.protocol == "mcp"`. When `pagination.has_more` is true,
repeat with the returned `pagination.next_cursor`. Cursors are opaque and
generation-bound; restart after `cursor_stale`.

For cross-session enumeration, page `query_events` with `content: "full"` and
the existing selection cursor, then perform the same field-presence filter on
the client. No MCP tool or argument was added for attribution.

## Storage, privacy, and display

`activity` is part of policy-selected Core content and shares the aggregate
16 MiB selected-content budget with normalized and structured event content.
Oversized complete argument or result channels can become explicit `omitted`
captures. Omitted or non-selected Core content carries no activity. Reimport
recomputes activity from provider history; query paths do not reopen provider
files or consult current MCP configuration.

Provider call IDs, server aliases, tool names, arguments, results, and literal
facts are private local data. They can contain credentials, paths, customer or
repository names, terminal controls, personal data, or proprietary output.
Machine JSON/JSONL and MCP `structuredContent` preserve the admitted activity
value exactly. Human CLI and MCP text rendering escapes terminal controls and
may bound the rendered event; use the machine value when exact bytes matter.
Private local storage is not a search-exclusion guarantee: matching queries,
ranked results, and snippets can surface retained searchable activity values,
so review search output before sharing it.

Release-note credit: Reported by [@j2h4u](https://github.com/j2h4u).
