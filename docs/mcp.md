# MCP

`ctx mcp serve` starts a local MCP server over newline-delimited stdio JSON-RPC.
It is for agents or MCP hosts that prefer tool discovery over shell commands.
The CLI remains the primary interface. Pro blame may perform bounded local
catch-up that updates the canonical Core index, writes the encrypted derived Pro
graph, and writes the projection acknowledgement. It never writes provider
history or repositories.

```bash
ctx mcp serve
ctx integrations install mcp
ctx integrations status mcp
```

`ctx integrations install mcp` can add this local server to supported
file-backed coding-agent MCP configs. Run `ctx docs show mcp-integrations` for
the support matrix, config paths, and manual snippets.

The server exposes 13 tools. Six use the OSS local index:

- `status`, local ctx index status, semantic coverage, and daemon coordinator
  state;
- `sources`, discovered local agent history sources;
- `search`, search the existing index;
- `sql`, run one read-only SQL statement against the existing index;
- `show_session`, return an indexed session transcript by ctx session ID;
- `show_event`, return an indexed event and optional surrounding window by ctx
  event ID.

`show_session` and `show_event` accept `content: "indexed" | "complete"` and
default to `indexed`. Complete mode explicitly reads eligible, verified local
provider records, applies the MCP aggregate response limit, and fails without a
partial result if any selected record cannot be verified. MCP hosts may log or
forward the returned complete transcript, so callers should request it only
when needed. Typed failures are returned in `structuredContent` with the same
stable complete-content error codes as the CLI JSON contract.

Two use the optional local Pro helper:

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
bounded catch-up updates the canonical Core index, writes the encrypted derived
Pro graph, and writes the projection acknowledgement. It never writes provider
history or repositories. The operation is nondestructive and idempotent.

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

MCP search and SQL query the existing index only. They do not refresh provider
history, import files, initialize storage, or write provider data. MCP search
currently uses the lexical search path only.

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

MCP `status` can include semantic and daemon diagnostic path fields such as
`vector_path`, `lock_path`, and `status_path` in `structuredContent`. They are
local troubleshooting hints for this machine, not portable contract IDs.
