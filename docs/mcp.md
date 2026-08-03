# MCP

`ctx mcp serve` starts a local MCP server over newline-delimited stdio JSON-RPC.
It is for agents or MCP hosts that prefer tool discovery over shell commands.
The CLI remains the primary interface. MCP startup performs a bounded,
content-free health-check/wake and recovers the default-enabled persistent
daemon when needed. The MCP process never becomes a provider-history or
derived-state writer.

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
- `search`, search the active Core/Tantivy generation and optional compatible
  semantic generation;
- `show_session`, read a stored Core session transcript by ctx session ID;
- `show_event`, read a stored Core event and optional surrounding window
  by ctx event ID;
- `query_events`, read one bounded deterministic page selected from normalized
  Core events.

`query_events` accepts the same typed identity, relationship, source, role,
event, workspace/file, chronology, order, and content-projection inputs as
`ctx show events`, plus an opaque continuation cursor. It returns one
`event_range_page` in `structuredContent`, including events, the pinned Core
generation, request selection, page usage, freshness/frontier state,
terminal/truncation state, and `next_cursor` when more results remain. It is
read-only after the MCP server's documented startup recovery. Its page is
additionally subject to the aggregate MCP response limit; select
`content=text` or `content=none`, or use CLI JSONL for a large stream.
Before hydration, MCP also rejects any single Core record whose indexed size
cannot fit a conservative projected response envelope. That failure is the
typed `output_limit_exceeded`; CLI JSONL remains the complete local stream.

`show_session` accepts an optional transcript mode plus resumable `limit` and
`cursor` inputs. Mode is applied before the page limit. `limit` defaults to 200
selected events and must be between 1 and 4,096. `cursor` is an opaque, nonempty
ASCII string of at most 4,096 bytes copied from the preceding page's
`next_cursor`; callers must not decode or construct it.

Successful `show_session` `structuredContent` is a `session_transcript` object
whose `events[]` contains one bounded page and whose `pagination` object has:

- `limit`, the requested or default selected-event limit;
- `returned`, the number of events in this page, at most `limit`;
- `has_more`, true only when another selected event remains;
- `next_cursor`, present exactly when `has_more` is true.

Continue with the same `ctx_session_id` and `mode`, the prior `next_cursor`, and
the desired limit. The cursor is exclusive and bound to the exact session and
active Core generation. A generation change returns `cursor_stale`; using a
cursor for another session returns `cursor_mismatch`; malformed cursor content
returns `invalid_cursor`. These are non-retryable typed tool errors. Restart
from the first page after `cursor_stale`; do not retry a mismatched or malformed
cursor unchanged.

`show_event` accepts bounded before, after, or symmetric window sizes. Both show
tools read complete policy-selected records from the active verified
Core/Tantivy generation without reopening provider history. MCP `show_session`
may return fewer than `limit` events with
`has_more: true` to stay within the response budget. After combining exact
`structuredContent` with the text fallback, every show response remains subject
to the 1 MiB MCP aggregate limit; an individually unrepresentable page fails
with `output_limit_exceeded` rather than silently clipping an event. MCP hosts
may log or forward the returned transcript.

This paging contract is MCP-specific. CLI `ctx show session` remains a
complete, unbounded stream unless the user explicitly requests terminal
`--max-events` truncation, and CLI JSONL ends with completion metadata rather
than a cursor. The in-repo Rust SDK follows the complete CLI path when both
`ShowSessionOptions.limit` and `.cursor` are absent, and uses this MCP page
contract when either is supplied.

The `status` tool returns the CLI JSON status read model unchanged in
`structuredContent`: the Core history report plus `upgrade`, `pro`,
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
initialize storage, or write provider data.

The `sources` tool returns the same bounded provider discovery `issues` as
`ctx sources --format json`, including stable issue codes and truncation markers.

MCP search defaults to primary-agent sessions only, matching `ctx search`.
Pass `include_subagents: true` when implementation details, code review notes,
test output, or failure traces from subagent sessions are relevant. When
`CODEX_THREAD_ID` is set, MCP search also excludes the active Codex session tree
by default; pass `include_current_session: true` when the active session tree is
the target.

Malformed tool arguments return `isError: true` with the existing diagnostic
`error` and stable `error_code: "invalid_request"` in `structuredContent`.
Malformed JSON-RPC framing or envelopes continue to use protocol-level parse
and invalid-params errors.

Tool results include MCP text content plus `structuredContent` JSON. Treat all
MCP output as private local history: it may include absolute paths, source
metadata, snippets, and transcript text, and the MCP host may log or forward
tool output.

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
