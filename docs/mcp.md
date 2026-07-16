# MCP

`ctx mcp serve` starts a read-only MCP server over newline-delimited stdio
JSON-RPC. It is for agents and MCP hosts that prefer tool discovery; the CLI
remains the primary interface.

```bash
ctx mcp serve
ctx integrations install mcp
ctx integrations status mcp
```

`ctx integrations install mcp` adds this local server to supported file-backed
agent configurations. Run `ctx docs show mcp-integrations` for supported hosts,
config paths, and manual snippets.

The server exposes:

- `status`, for local index, daemon, watcher, model-retry, and semantic coverage
  state;
- `sources`, for discovered local history sources;
- `search`, for bounded search over the existing index;
- `sql`, for one read-only SQL statement over the existing index;
- `show_session`, for one indexed transcript by ctx session ID;
- `show_event`, for one indexed event and an optional bounded event window.

## Search Input

MCP search accepts the same canonical `ctx-search-v1` object as CLI
`--query-file` and the SDKs. It does not interpret an opaque Boolean query
string or apply MCP-specific tokenization:

```json
{
  "query": {
    "version": "ctx-search-v1",
    "any": [
      { "all": "disk io pressure" },
      { "phrase": "storage latency" },
      { "literal": "logs_2.db" },
      { "semantic": "the indexing job made the workstation sluggish" }
    ],
    "must": [
      { "all": "codex" }
    ],
    "must_not": [
      { "all": "postgres vacuum" }
    ]
  },
  "backend": "hybrid",
  "limit": 10,
  "include_subagents": true
}
```

`any` clauses are alternatives, every `must` clause is required, and matching
any `must_not` clause excludes the event. `all` requires every analyzed word in
one event; `phrase` requires adjacent words in order; `literal` preserves
punctuation and verifies a contiguous value. At most one `semantic` clause is
allowed, and only in `any`.

Backend selection does not weaken the query. `lexical` rejects semantic
clauses. `semantic` requires semantic to be the sole positive `any` alternative
and still enforces lexical constraints. `hybrid` can union explicit lexical and
semantic alternatives. An explicit semantic request returns a typed readiness
error when the daemon query service, model, or index is unavailable; it does
not silently become a lexical query.

Search defaults to primary-agent sessions. Set `include_subagents: true` when
delegated implementation details, reviews, test output, or failure traces are
relevant. When `CODEX_THREAD_ID` is set, search excludes the active Codex
session tree unless `include_current_session: true` is supplied.

## Read-Only And Bounded Behavior

MCP search and SQL use the existing local index only. They do not discover or
refresh provider history, run plugins, initialize or migrate storage, download
a model, start semantic backfill, or write provider or ctx index data. A
semantic query uses only an already-ready local daemon query service and
existing sidecar coverage.

Search uses the same fixed query, candidate, verification, hydration, payload,
response, result, and elapsed budgets as the CLI. Tool results include text
content plus `structuredContent`. Search `structuredContent` uses search JSON
schema version 2 and always includes the canonical query and
`query_execution`, with resolved/consumed budgets, truncation reasons, semantic
readiness and coverage, completeness, and effective-backend diagnostics.
Incomplete semantic coverage or bounded partial results are explicit; agents
should not infer completeness from result count alone.

The `sql` tool uses the same stable read-only `ctx_*` views and limits as
`ctx sql --json`. Run `ctx docs show sql` for schemas and examples.

Treat all MCP output as private local history. It may include absolute paths,
source metadata, snippets, transcript text, SQL fields, and secret-shaped
strings, and the MCP host may log or forward tool output. Status may also
include private local sidecar, lock, or status paths; those are troubleshooting
hints for this machine, not portable identifiers.
