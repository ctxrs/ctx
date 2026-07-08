# MCP

`ctx mcp serve` starts a read-only MCP server over newline-delimited stdio
JSON-RPC. It is for agents or MCP hosts that prefer tool discovery over shell
commands. The CLI remains the primary interface.

```bash
ctx mcp serve
ctx integrations install mcp
ctx integrations status mcp
```

`ctx integrations install mcp` can add this local server to supported
file-backed coding-agent MCP configs. Run `ctx docs show mcp-integrations` for
the support matrix, config paths, and manual snippets.

The server exposes these tools:

- `status`, local ctx index status;
- `sources`, discovered local agent history sources;
- `search`, search the existing index;
- `sql`, run one read-only SQL statement against the existing index;
- `show_session`, return an indexed session transcript by ctx session ID;
- `show_event`, return an indexed event and optional surrounding window by ctx
  event ID.

MCP search and SQL query the existing index only. They do not refresh provider
history, import files, initialize storage, or write provider data.

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
