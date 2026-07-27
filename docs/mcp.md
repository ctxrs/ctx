# MCP

`ctx mcp serve` starts a local MCP server over newline-delimited stdio JSON-RPC.
It is for agents or MCP hosts that prefer tool discovery over shell commands.
The CLI remains the primary interface. Pro graph queries may idempotently catch
up only the separate derived graph.

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

Seven use the optional local Pro helper:

- `pro_status`, inspect helper availability, capabilities, nonsecret access
  state, and applicable refresh/access/grace deadlines without returning the
  helper path;
- `show_resource` and `locate_resource`, resolve a typed resource and its exact
  evidence;
- `blame`, join a file or line with Git and producing-session provenance;
- `timeline`, `related`, and `facts`, return bounded cited graph views.

Pro query targets use the resource kinds `repository`, `worktree`, `branch`,
`commit`, `file`, `pull_request`, `issue`, `command`, `check`, `session`,
`agent`, and `run`. Query `target.repository` is an optional logical identity
such as `forge:github.com/ctxrs/ctx` and is not a path.
Unscoped results expose an opaque `resource.id` that can be reused as the same
kind's `target.value` after the caller selects the intended match.
`target.line` is a positive 1-based source line and is accepted only when
`target.kind` is `file`; other kinds return the typed `invalid_request` error.

Only `facts`, `timeline`, and `related` are page-capable. Their authenticated
cursors are bound to the query and current graph state. Tampered cursors are
invalid and graph changes make previous cursors stale. `show_resource`,
`locate_resource`, and `blame` are bounded and unpaged. MCP Pro errors use
stable `error_code` values in `structuredContent`.
Helper/graph readiness and subscription access are separate fields. Access is
`trial`, `active`, `canceling_paid`, `offline_grace`, `locked`, or null when it
cannot be determined.

MCP search and SQL query the existing index only. They do not refresh provider
history, import files, initialize storage, or write provider data. MCP search
currently uses the lexical search path only.

MCP search defaults to primary-agent sessions only, matching `ctx search`.
Pass `include_subagents: true` when implementation details, code review notes,
test output, or failure traces from subagent sessions are relevant. When
`CODEX_THREAD_ID` is set, MCP search also excludes the active Codex session tree
by default; pass `include_current_session: true` when the active session tree is
the target.

The MCP `sql` tool uses the same read-only stable views and result limits as
`ctx sql --json`. Prefer stable `ctx_*` views for scripts and agent workflows.
Run `ctx docs show sql` for the view schemas and examples.

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
