# Typed MCP invocation and response capture

ctx can retain a provider-native MCP invocation and/or terminal response on a
normalized Core event as optional `mcp_exchange` content. The capture keeps
decoded JSON values structured and links provider records with their native
call ID when that ID is available.

This content contract is separate from exact MCP attribution. The top-level
`mcp_tool_call: {server, tool}` field remains event metadata with its own
qualification matrix. `mcp_exchange` is content-policy governed and may contain
an invocation, a response, or both. Neither field is synthesized from the
other. See
[`mcp-tool-call-attribution.md`](mcp-tool-call-attribution.md) for the
attribution contract.

## CLI and Core wire shape

CLI JSON/JSONL and stored Core records use snake_case. A combined terminal
record can look like:

```json
{
  "mcp_tool_call": {
    "server": "inventory",
    "tool": "lookup"
  },
  "mcp_exchange": {
    "provider_call_id": "call-42",
    "invocation": {
      "server": "inventory",
      "tool": "lookup",
      "arguments": {
        "capture_status": "present",
        "value": {
          "item_id": 7,
          "includeHistory": true
        }
      }
    },
    "response": {
      "status": "succeeded",
      "duration_ns": 42000,
      "text": {
        "capture_status": "normalized_body"
      },
      "payload": {
        "capture_status": "present",
        "value": {
          "item_id": 7,
          "displayName": "widget"
        }
      }
    }
  }
}
```

`provider_call_id` is the decoded provider-native call identifier within the
source session. It, `invocation.server`, and `invocation.tool` are nonempty
decoded UTF-8 values, each bounded to 64 KiB. Present invocation arguments are
JSON objects. A present response payload is the complete decoded provider-native
JSON value, which may be any JSON type.

Response `status` is `succeeded`, `failed`, `cancelled`, `timed_out`, or
`unknown`. Optional `failure_kind` is `tool_reported`, `invocation`, or
`unknown`; `duration_ns` is also optional.

## Typed MCP and SDK wire shape

Typed SDK and MCP event output uses camelCase for contract-owned keys:

```json
{
  "mcpToolCall": {
    "server": "inventory",
    "tool": "lookup"
  },
  "mcpExchange": {
    "providerCallId": "call-42",
    "response": {
      "status": "failed",
      "failureKind": "tool_reported",
      "text": {
        "captureStatus": "normalized_body"
      },
      "payload": {
        "captureStatus": "omitted",
        "reason": "size_limit",
        "observedEncodedBytes": 17000000
      }
    }
  }
}
```

Captured JSON stays JSON. ctx does not camel-case, rewrite, or flatten keys
inside `arguments.value` or `payload.value`; for example, `item_id` remains
`item_id` even in typed camelCase output.

Capture is complete decoded JSON when `capture_status`/`captureStatus` is
`present`. It is not a promise of provider-source lexical byte identity. JSON
whitespace, escaping, member order, and other source-level spellings are not
part of this contract.

## Capture states

Arguments and response payloads use the same four states:

| State | Meaning |
| --- | --- |
| `present` | `value` contains the complete decoded JSON admitted to Core. |
| `absent` | The provider record represented no value for that channel. |
| `unavailable` | The retained provider record did not make a complete decoded value available. |
| `omitted` | ctx did not retain the complete value. The current reason is `size_limit`; `observed_encoded_bytes`/`observedEncodedBytes` is included when known. |

The response `text` field uses `normalized_body`, `absent`, `unavailable`, or
`omitted`. `normalized_body` is a disposition pointer: the text is already in
the event's existing normalized `text`/Core body. Capture never appends a
second copy or adds provider payload JSON to text.

Optional object members are omitted rather than written as `null`. Every
`mcp_exchange` has a nonempty `provider_call_id` and at least one of
`invocation` or `response`.

## Provider granularity

Capture preserves native event boundaries; ctx does not merge every provider
into one synthetic event.

| Provider route | Event shape | Parser revision |
| --- | --- | --- |
| Codex session JSONL, including versioned lanes | The combined terminal record can carry invocation and response together. | `codex-nativepath-core-record-v16-aggregate-content-admission` |
| Warp SQLite | Separate call and result records carry invocation and response respectively, linked by the native call ID. | `warp-source-backed-logical-v5` |
| Copilot CLI JSONL | Separate start and completion records carry invocation and response respectively, linked by the native call ID. | `copilot-cli-direct-native-jsonl-v6-mcp-start-generic-body` |

No other provider route currently publishes this typed capture. This provider
coverage does not change exact top-level attribution qualification or general
provider import support.

Search class follows the event type of the record that owns the capture.
Separate Warp and Copilot CLI invocation records are calls. The combined Codex
terminal record is one `tool_output`, so its normalized response text and any
projected invocation terms are found under `outputs`, not `calls`. ctx does not
duplicate or dual-classify that event.

## Content policy, projection, and limits

`mcp_exchange` exists only on content-policy-selected Core events. Redacted or
omitted Core content has no exchange. Event output includes the exchange only
for the full-content projection:

- `ctx list events --content full` and MCP `query_events` with
  `content: "full"` can return it;
- the reduced body/text projection (`--content text` in the CLI) omits it;
- `content: "none"`/`--content none` omits it;
- full `show_event` output and log-mode `show_session` events can return it.

The separate top-level `mcp_tool_call` metadata survives presentation
`content: "none"` when it was stored. Reducing presentation content therefore
does not erase attribution identity, but it does remove invocation arguments,
provider call IDs, response status and timing, and response payload capture.

The normalized Core body, provider-native `structured_content`, and
`mcp_exchange` share one aggregate 16 MiB encoded-content budget. Oversized
arguments or response payloads become explicit `omitted` states with
`reason: "size_limit"`; ctx does not truncate JSON. If even the compact capture
cannot fit, ctx keeps the ordinary source event and leaves `mcp_exchange`
absent.

## Storage, history, and search

The optional exchange is stored in the normalized Core record and is
retrievable through full show/list/MCP/SDK event output. During an allowlisted
Core contract migration, existing rows keep `mcp_exchange` absent; migration
does not reopen provider sources. A later ordinary provider refresh or
reimport can populate the field when the source is still available.

Lexical search derives a narrow body projection from policy-selected invocation
content. `invocation.server`, `invocation.tool`, and the compact JSON value of
`present` arguments contribute ordinary body-search terms without synthetic
server/tool labels. `absent`, `unavailable`, and `omitted` arguments contribute
no terms.

The provider call ID, response status, failure kind, timing, and structured
response payload remain stored and retrievable but unsearchable. Response text
with the `normalized_body` disposition retains the event's existing body-search
behavior exactly once; capture does not append a duplicate. The projection adds
no semantic text, selector, filter, search result field, SQL column, Local Pro
fact, or hidden network request.

Full show/list/MCP/SDK output remains exact stored-content retrieval. Search
snippets instead come from the Core-backed searchable projection and can
therefore contain projected invocation content. Because the lexical projector
revision participates in generation identity, an older generation must be
rebuilt or pass the documented narrow same-epoch preservation migration before
these terms become searchable. Historical Core rows that have no captured
exchange remain unchanged.

## Privacy and network behavior

MCP arguments and responses can contain credentials, personal data, private
repository details, absolute paths, or proprietary output. Present invocation
arguments become searchable in the local lexical index. Exact machine output
and local search results are private content and are not share-safe without
review. MCP hosts may log or forward returned results.

Import, storage, and local retrieval of the exchange add no hidden network
request. ctx does not send the captured invocation or response anywhere by
default.
