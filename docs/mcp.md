# MCP

`ctx mcp serve` starts a local MCP server over newline-delimited stdio JSON-RPC.
It is for agents or MCP hosts that prefer tool discovery over shell commands.
The CLI remains the primary interface. MCP startup performs a bounded,
content-free health-check/wake and recovers the default-enabled persistent
daemon when needed. The MCP process never becomes a provider-history or
projection writer.

```bash
ctx mcp serve
ctx integrations install mcp
ctx integrations status mcp
```

`ctx integrations install mcp` can add this local server to supported
file-backed coding-agent MCP configs. Run `ctx docs show mcp-integrations` for
the support matrix, config paths, and manual snippets.

The server advertises its current tool set through MCP discovery rather than a
fixed documented count. Core tools include:

- `status`, the same structured source, upgrade, Pro, and compact local-usage
  status as `ctx status --format json`;
- `sources`, discovered local agent history sources;
- `search`, search the existing index;
- `sql`, run one read-only SQL statement against the existing index;
- `show_session`, return an indexed session transcript by ctx session ID;
- `show_event`, return an indexed event and optional surrounding window by ctx
  event ID.

`show_session` and `show_event` accept `content: "indexed" | "complete"` and
default to `indexed`. Both policies read normalized content from the active
verified Core generation; `indexed` controls item selection. Both apply the MCP
aggregate response limit. MCP hosts may log or forward the returned transcript.
Typed failures are returned in `structuredContent` with the same stable error
codes as the CLI JSON contract.

The `status` tool returns the CLI JSON status read model unchanged in
`structuredContent`: the source-backed history report plus `upgrade`, `pro`,
compact `local_usage`, and `read_only: true`. The added facts remain
machine-only and do not expand the MCP text fallback. The status read does not
import, initialize, refresh, or mutate source, Pro, upgrade, or usage state;
configured post-delivery local-usage accounting remains the independent server
boundary described below.

Optional Local Pro tools include:

- `pro_status`, inspect helper availability, capabilities, nonsecret access
  state, applicable refresh/access/grace deadlines, and compact local usage
  aggregates without returning the helper or usage-store path;
- `blame`, return typed, fully cited provenance for an exact `file`, `commit`,
  or `pull_request` target.

The `blame` target is exactly one of `file`, `commit`, or `pull_request`.
`target.repository` is an optional logical identity such as
`forge:github.com/ctxrs/ctx`, never a path; a numeric PR selector requires it.
File `target.lines` contains positive inclusive `start` and `end` values.

MCP blame defaults to and permits at most 8 complete matches per page. Its
authenticated cursor is bound to the request and current graph state. Every
returned match has all of its referenced entries in the deduplicated evidence
table; the text fallback emits every match and every evidence entry without
clipping. After adding both exact `structuredContent` and the text fallback,
the final serialized JSON-RPC response is capped at 1 MiB. If a complete helper
page exceeds that MCP-specific cap, the tool returns a small
`invalid_response` error asking the caller to lower `limit` or use
`ctx blame ... --format json`; it does not truncate a match or evidence entry and does
not fabricate a continuation cursor.

`pro_status` is read-only. `blame` advertises `readOnlyHint: false` because its
bounded maintenance wake can cause the daemon to advance the encrypted derived
Pro graph. Ordinary blame reads the latest committed Pro generation while that
catch-up proceeds; only an explicit wait policy waits for a requested frontier.
It never writes provider history or repositories. The wake is nondestructive
and idempotent.

PR activity remains separate from code production. PR code membership appears
only when structured captured forge evidence names the canonical PR and exact
Git object ID in the same recognized record. When that proof is absent, the
result explicitly contains no PR-commit relationship. MCP Pro errors use stable
`error_code` values in `structuredContent`.
Helper/graph readiness and subscription access are separate fields. Access is
`trial`, `active`, `canceling_paid`, `offline_grace`, `locked`, or null when it
cannot be determined.

`pro_status` may include a `$20/month` continuation action during a trial and a
neutral, unpriced `pro_restore_access` action with `graph_preserved: true` when
access is locked. It does not show a purchase action for paid active,
`canceling_paid`, or `offline_grace` access and does not replace the existing
`next_action`. Status never opens a browser.

MCP has no referral tool, attribution input, commission status, or referral
promotion. Referral attribution exists only through
`ctx pro --referral <codename>`, and the private aggregate referrer status is
available only through the explicit authenticated `ctx referral status`
command.

Local usage aggregation counts only recognized `tools/call` requests after the
complete JSON-RPC response has serialized, written, and flushed. Initialize,
ping, tool listing, malformed or invalid-ID envelopes, notifications,
pre-initialization protocol errors, unknown tools, and automatic daemon work
are not counted. Recognized tool/argument failures may count; invalid blame
targets use an N/A target class. MCP blame creates one local observation
enriched with its Pro result even though generic MCP and Pro remote event
reporting may independently observe the same boundary. The compact report’s
`mcp_response_bytes` is factual serialized transport bytes, including the
newline—not tokens or savings. Local recording has no network path, is
independent of remote event reporting, and fails silently without changing MCP
output; explicit `pro_status` reports stable content-free usage-store errors
instead of raw paths or causes. The server re-resolves the dedicated local
control for every delivered call; an explicit `false` takes effect before store
I/O, while an unrelated config read/parse failure retains the last known state.

MCP search sends the same bounded maintenance wake as CLI search and then
queries committed generations. It follows the CLI lexical, semantic, and
hybrid contracts, including lexical fallback for unavailable hybrid semantic
state, typed failure for semantic-only unavailability, and no vector work when
the semantic weight is zero. The MCP process does not import provider history,
initialize storage, or write provider data. MCP SQL remains an existing
metadata-projection-only read and does not read transcript bodies.

The `sources` tool returns the same bounded provider discovery `issues` as
`ctx sources --format json`, including stable issue codes and truncation markers.

MCP search defaults to primary-agent sessions only, matching `ctx search`.
Pass `include_subagents: true` when implementation details, code review notes,
test output, or failure traces from subagent sessions are relevant. When
`CODEX_THREAD_ID` is set, MCP search also excludes the active Codex session tree
by default; pass `include_current_session: true` when the active session tree is
the target.

The MCP `sql` tool uses the same read-only stable views and result limits as
`ctx sql --format json`. Prefer stable `ctx_*` views for scripts and agent workflows.
Run `ctx docs show sql` for the view schemas and examples.

Malformed tool arguments return `isError: true` with the existing diagnostic
`error` and stable `error_code: "invalid_request"` in `structuredContent`.
Malformed JSON-RPC framing or envelopes continue to use protocol-level parse
and invalid-params errors.

Tool results include MCP text content plus `structuredContent` JSON. Treat all
MCP output as private local history: it may include absolute paths, source
metadata, snippets, transcript text, and raw SQL result fields, and the MCP host
may log or forward tool output.

Pro query text is selected by the authoritative `payload_type`, not merely by
the presence of a `results` array. Each Pro view keeps its distinct heading and
renders typed targets/resources, fact predicates and objects, confidence,
state, actor/root sessions, canonical citation coordinates, staleness, and
pagination. Unknown Pro query payload types fail closed in text instead of
being presented as search results; callers can still inspect the accompanying
`structuredContent` for the original bounded response.

Like CLI JSON status, MCP `status` can include local source, semantic, daemon,
upgrade, and Pro diagnostic path fields in `structuredContent`. They are local
troubleshooting hints for this machine, not portable contract IDs. Compact
`local_usage` contains only enablement, state, definition/retention versions,
and a stable content-free error when unavailable.
