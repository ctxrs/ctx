# Limitations

ctx is production-scoped to local history indexing and search retrieval.
These limitations are intentional unless another document says a capability has
shipped.

## Provider Coverage

- Codex local import is supported for documented local JSONL sources.
- Pi local import is supported only when a matching local `sessions.jsonl` file
  exists.
- Claude, OpenCode, Gemini, Cursor, Copilot CLI, and Factory AI Droid local
  import is supported only when their documented local history paths exist and
  match the supported native formats in the provider matrix.
- Antigravity and Amp may appear in `ctx sources` as detection-only rows, but
  have no native local importer or native transcript parser in the public CLI.
- Developer/test harnesses can import normalized provider JSONL only with
  `CTX_PROVIDER_NORMALIZED_IMPORT_DEV=1`; this is not native provider support.
- Unknown provider formats should not be parsed optimistically.

## Import Semantics

- Imports are explicit; ctx does not collect provider history in the background.
- Current importers use idempotent rescans.
- `--resume` is reported in output but is not a universal provider cursor
  contract.
- Explicit `--path` imports are not remembered as future defaults.

## Search Semantics

- Search quality depends on what providers expose and what importers index.
- Large outputs may be represented as bounded previews.
- Ranking is deterministic for the same local database and options, but it is
  not a claim of semantic understanding.
- Empty or very broad queries can return metadata-driven matches.

## Retrieval Semantics

- Search output is retrieval material, not generated analysis.
- Token counts are estimates.
- If a raw source moves, ctx may still return indexed text from SQLite.
- JSON is local/private and can include sensitive content.

## Operations

- Source builds are documented; public release install commands wait for release
  artifacts and verification instructions.
- Core setup/import/search are local filesystem operations.
- No provider beyond the support matrix should be described as supported.
