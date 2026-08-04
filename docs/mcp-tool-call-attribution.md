# Exact MCP tool-call attribution

ctx can preserve the exact MCP dispatch server and advertised tool name when a
qualifying provider history record stores both values at an observed execution
boundary. This is an event-local metadata capability, not a general claim that
every tool call or every supported provider exposes MCP identity.

This top-level identity is separate from the optional, content-governed
`mcp_exchange` invocation/response capture. Presence or absence of one does not
synthesize or qualify the other. See
[`mcp-exchange-capture.md`](mcp-exchange-capture.md) for arguments, response
payloads, call IDs, status, timing, and capture-state semantics.

## Wire contract

Attributed CLI/Core event rows add one optional snake_case object:

```json
{
  "mcp_tool_call": {
    "server": "node_repl",
    "tool": "js"
  }
}
```

Typed SDK and MCP event output exposes the same identity as camelCase
`mcpToolCall: {server, tool}`.

`server` is the exact source-time dispatch key and `tool` is the exact
MCP-advertised tool name. Both are required nonempty decoded UTF-8 strings and
each is bounded to 64 KiB. Whitespace-only native values remain unchanged.
Values are never trimmed, normalized, split from a combined name, reconstructed
from current configuration, or truncated to fit the bound.

Presence means the native terminal/result record supplied the exact pair for an
observed execution. It does not say whether the call succeeded, identify an
endpoint, or define configuration scope. Tool-level and transport failures may
therefore still carry attribution. Arguments and responses, when captured, live
under the separate optional `mcp_exchange` content field and do not broaden this
identity claim.

Absence means only that this event has no qualifying exact durable pair. It does
not mean the event was not MCP. The complete property is omitted rather than
serialized as `null`; partial, ambiguous, malformed, or oversized pairs retain
the ordinary event without attribution.

## Provider and format capability

General provider support remains the 41-provider local-history contract in
[`provider-support-matrix.json`](provider-support-matrix.json). Exact MCP
attribution has a separate provider + route + source format + format version
contract in
[`mcp-tool-call-attribution-capabilities.json`](mcp-tool-call-attribution-capabilities.json).

Capability revision 3 evaluates all 41 providers across 43 base routes and 46
capability lanes: three `exact`, 42 `not-qualified`, and one `excluded`.
Codex contributes separate session-tree and legacy prompt-history routes;
Deep Agents contributes its local SQLite import plus a separately excluded
Deep Agents hosted trace. Capability revision 3 exact providers are
Codex, Warp, and Copilot CLI. The exact full tuples are:

- Codex `codex_session_jsonl_tree` / `codex-nativepath-jsonl-v0`, parser
  `codex-nativepath-core-record-v15`, for unversioned producer generation 1
  only. Codex producer versions 0.200.0, 0.201.0, and 0.202.0 are separate
  explicit `not-qualified` lanes and never inherit that exact status.
- Warp `warp_sqlite` / `warp-agent-task-protobuf-v1`, parser
  `warp-source-backed-logical-v5`, for strict unversioned format generation 1.
  The pinned Warp and protobuf source commits are evidence for that shape, not
  runtime-observable writer-version selectors.
- Copilot CLI `copilot_cli_session_events_jsonl` /
  `copilot-cli-direct-native-jsonl-v1`, parser
  `copilot-cli-direct-native-jsonl-v5-mcp-exchange`, for
  strict unversioned format generation 1. Versions 0.0.393 and 1.0.77 and the
  pinned source commit are observed evidence, not runtime admission selectors.

The 42 `not-qualified` tuple rows include Codex's three observed semver lanes,
its separate legacy prompt-history route, Mistral Vibe, and the local Deep
Agents SQLite route. Mistral persists
the tool and a transport URL or command, but its server alias exists only
inside a combined function name; ctx does not reconstruct it by suffix
splitting. Deep Agents similarly persists a combined MCP wire name rather than
a separate server field. Its hosted trace is the single `excluded` row because
remote observability is outside the local-only boundary; its local SQLite
history import remains generally Supported but not qualified for attribution.

`not-qualified` is deliberately narrower than unsupported. It says only that
ctx does not publish this capability for that tuple. New source variants or
versions require their own tuple and must not silently inherit another row's
status. Unversioned and unknown producer generations fail closed. The public
[evidence runbook](mcp-tool-call-attribution-evidence.md) defines the exactness
bar and typed failure reasons used by the matrix.

Producer bounds use the contract's closed grammar: `unversioned` with a
positive integer generation, sorted 40-hex `source_commits`, structured
`versions` plus inclusive
`minimum`/`maximum` ranges and optional source commits, or the sole
`hosted_boundary`. Free-form labels, opaque kinds, moving branch names, and
catch-all version ranges are invalid. Multiple lanes may share a base route
when their source schemas or producer bounds are explicitly distinct and do
not overlap.

## CLI access and client-side filtering

Ordinary MCP tool results are included only by the log transcript mode:

```bash
ctx show session <ctx-session-id> --mode log --format jsonl
ctx show event <ctx-event-id> --format json
```

`ctx list events` also exposes the field. `--content none` removes payload
content but intentionally retains event metadata such as `mcp_tool_call`:

```bash
ctx list events --provider codex --content none --format jsonl |
  jq -c 'select(.record_type == "event_range_event") |
    .event | select(.mcp_tool_call? != null) |
    {ctx_event_id, ctx_session_id, mcp_tool_call}'
```

This is client-side filtering after ctx emits each JSONL row. There is no
server/tool filter, no search, no query selector, and no SQL access for this
metadata. In particular, there is no `--mcp-server` or `--mcp-tool` selector;
the names are not a search input, search result field, ranking signal, snippet
source, or SQL column.

Full-content JSON/JSONL can additionally expose `mcp_exchange`. The
`--content text` and `--content none` projections omit that content field while
leaving an available top-level `mcp_tool_call` intact.

## MCP access and pagination

MCP `show_event`, `show_session`, and full-content `query_events` return the
same optional identity as camelCase `mcpToolCall` in event rows inside
`structuredContent`. Text fallback safely renders the values but is not the
exact machine authority. Full event rows can also include camelCase
`mcpExchange`; `query_events` with `content: "text"` or `content: "none"`
omits the exchange.

For a complete attributed session scan, call `show_session` with the session
ID, `mode: "log"`, and a bounded `limit`. Filter that page's `events` array on
the client for rows containing `mcpToolCall`. When
`pagination.has_more` is true, repeat the call with the same session ID and
mode plus the returned `pagination.next_cursor`. Stop only when `has_more` is
false. Cursors are opaque and generation-bound; restart from the first page
after `cursor_stale`.

For cross-session enumeration, page `query_events` using its existing
selection and cursor, request `content: "none"` when payload text is not
needed, and perform the same field-presence filter client-side. There are no
new MCP arguments, tools, selectors, or search behavior for attribution.

## Storage and historical rows

The optional pair is stored only on the qualifying normalized Core event. It is
not added to lexical terms, semantic text, usage aggregates, or the Local Pro
graph. Reimport recomputes the field from provider history; query paths never
reparse provider-specific structured content or consult current MCP config.

The separate optional `mcp_exchange` is stored as content on selected Core
events. Its call ID, arguments, status/timing, and response payload are not
lexical or semantic fields, SQL columns, usage aggregates, or Local Pro facts.
Existing normalized-body indexing behavior is unchanged.

During the one allowlisted transition from the immediately preceding
self-contained Core contract, ctx republishes verified records with
`mcp_tool_call` and `mcp_exchange` absent before switching generations. This
preservation step does not reopen provider history. A later ordinary provider
refresh may enrich qualifying source records. Historical records can therefore
remain unattributed and have no exchange capture when their original source is
unavailable, while their existing ctx event and session identities remain
stable.

This transition does not read or migrate the legacy pre-v0.26 Store/SQL epoch.
Unknown, incomplete, or corrupt Core predecessors fail closed and leave the
previous generation authoritative.

## Privacy and display safety

MCP server and tool names are opaque local data. They can contain credentials,
customer or repository names, paths, identifiers, Unicode controls, or other
sensitive text. Exact JSON/JSONL and MCP structured output preserve the native
strings, so this output is private and not share-safe until reviewed. MCP hosts
may also log or forward tool results. Captured arguments and response payloads
are likewise private local content and can contain credentials, personal data,
or proprietary output.

Human views retain the first 256 Unicode scalar values from each component
independently. Terminal and Markdown rendering applies escaping only after that
bound. If a component has a 257th scalar value, its rendered prefix ends with
the exact marker `… [display truncated]`. Text output also emits
`mcp_display_truncated: true` and the exact guidance
`MCP identity display truncated; use --format json or --format jsonl for exact values.`
Markdown emits that same guidance. MCP text fallback instead emits
`MCP identity display truncated; inspect structuredContent for exact JSON values.`

Display escaping is reversible. A literal backslash becomes `\\`; LF, CR, and
tab become `\n`, `\r`, and `\t`; ESC becomes `\x1b`. Other C0 controls, DEL,
and C1 controls use lowercase, at-least-four-digit `\u{xxxx}` notation. The
same notation is used for U+00AD, U+0600–U+0605, U+061C, U+06DD, U+070F,
U+0890–U+0891, U+08E2, U+115F–U+1160, U+180E, U+200B, U+200E–U+200F,
U+2028–U+202E, U+2060–U+206F, U+3164, U+FEFF, U+FFA0, U+FFF0–U+FFFB,
U+110BD, U+110CD, U+13430–U+1343F, U+1BCA0–U+1BCA3,
U+1D173–U+1D17A, U+E0000–U+E00FF, and U+E01F0–U+E0FFF. Other scalar values,
including ZWNJ, ZWJ, combining marks, and variation selectors, remain intact.
Markdown additionally prefixes a backslash before every rendered backslash and
before `` ` ``, `*`, `_`, `{`, `}`, `[`, `]`, `<`, `>`, `(`, `)`, `#`, `+`,
`-`, `.`, `!`, `|`, `=`, and `~`.

Machine JSON, JSONL, and MCP `structuredContent` preserve the full exact values
admitted by the 64 KiB component bound; use those machine formats whenever the
complete values matter. Core `Redacted` or `Omitted` content policy omits
attribution by default; this is distinct from presentation `--content none`,
which retains already-stored attribution metadata. Content policy also governs
`mcp_exchange`, but presentation includes that field only in full-content event
output.

Release-note credit: Reported by [@j2h4u](https://github.com/j2h4u).
