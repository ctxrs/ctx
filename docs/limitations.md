# Limitations

ctx is production-scoped to local history indexing and search retrieval. The
separate `ctx turso` command has an experimental remote-primary persistence path
for its portable event projection.
These limitations are intentional unless another document says a capability has
shipped.

## Provider Coverage

- Codex local import is supported for documented local JSONL sources.
- Pi local import is supported when matching local session JSONL files exist
  under `~/.pi/agent/sessions`, or when an explicit Pi session JSONL file is
  passed with `--path`.
- Additional supported agent harnesses are listed in the provider matrix and are
  imported only when their documented local history paths exist and match the
  supported native formats.
- NanoClaw local import is explicit-path support and is not included in
  `ctx import --all` or pre-search refresh. AstrBot is supported for bounded
  `data_v4.db` locations and imports local LLM context plus available platform
  history rows when present, but upstream AstrBot still treats non-WebChat raw
  IM replies as platform-side history rather than guaranteed `data_v4.db`
  transcript rows.
- Unknown provider formats should not be parsed optimistically.

## Import Semantics

- Imports are explicit unless non-JSON `ctx setup`, native-provider
  `ctx import`, or `ctx daemon run` starts ctx-owned local daemon maintenance.
  Setup/import autostart uses the normal background daemon profile and exits
  after it becomes idle; explicit `ctx daemon run` runs the same coordinator in
  the foreground. Use
  `ctx setup --no-daemon` or `ctx import --no-daemon` for a one-run autostart
  opt-out. Semantic catch-up runs only when the required local model cache
  already exists.
- Current importers use idempotent rescans.
- `--resume` is reported in output but is not a universal provider cursor
  contract.
- Explicit `--path` imports are not remembered as future defaults.

## Search Semantics

- Search quality depends on what providers expose and what importers index.
- Large outputs may be represented as bounded previews.
- Ranking is deterministic for the same local database and options, but it is
  not a claim of semantic understanding.
- Empty or punctuation-only search is invalid. Broad valid queries can still
  return metadata-driven matches.
- Semantic embeddings depend on a compatible local ONNX Runtime backend and
  the opt-in ctx daemon query service. Release/platform combinations without a
  validated local runtime remain lexical-safe: `hybrid` falls back to lexical
  and explicit `semantic` reports a local unavailable/runtime error instead of
  linking an unsupported backend.
- The ctx macOS CLI targets macOS 13, but ONNX Runtime 1.27 follows its upstream
  macOS 14 minimum. On macOS 13, daemon-backed lexical search remains available
  while semantic search is unavailable.
- `ctx turso search` is a remote, ASCII case-insensitive substring search. It does
  not currently use Turso FTS, semantic retrieval, ctx's local ranking, or
  artifact bodies; very large remote projections need a dedicated indexed
  search follow-up before they should replace local lexical-search performance.

## Remote Projection Semantics

- `ctx turso import` uses an in-process-memory ctx store to read all discovered
  native providers and uploads the resulting event projection without creating
  a persistent ctx SQLite index. It is memory-bound for large corpora and scans
  the complete event set on each execution to guarantee that UUID sort order
  cannot lose newly imported history. It does not yet stream normalized records
  directly to libSQL or maintain a remote per-source incremental cursor.
- `ctx turso push` uploads an existing local index. Neither command makes the
  ordinary local `ctx search`, `show`, SQL, MCP, semantic retrieval, or
  artifact-body interfaces remote-capable; use `ctx turso search` for remote
  projection retrieval.
- Event UUID plus provider dedupe keys make repeated pushes and most separately
  captured histories idempotent across Macs. For source-scoped provider keys,
  Turso derives a canonical key from the provider-owned session ID, event index,
  and payload hash so the same session copied to different source paths merges.
  Histories without a stable provider session ID are unioned by their original
  event UUID only.
- Turso/libSQL is the first remote protocol target. Arbitrary hosted SQLite
  files and providers that do not implement the libSQL remote protocol are not
  supported.

## Retrieval Semantics

- Search output is retrieval material, not generated analysis.
- Token counts are estimates.
- If a raw source moves, ctx may still return indexed text from SQLite.
- JSON is local/private and can include sensitive content.

## Operations

- Core setup/import/search are local filesystem operations.
- Official installer-managed binaries can use signed release metadata for
  `ctx upgrade` and managed background auto-upgrade checks.
- Unmanaged installs do not self-upgrade.
- No provider beyond the support matrix should be described as supported.
