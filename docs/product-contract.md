# Product Contract

ctx is a local search CLI for existing agent history.

## Promise

Given local provider transcripts that ctx supports, the CLI can build a local
SQLite index and return deterministic retrieval results with citations. The
product boundary is retrieval, not interpretation.

## In Scope

- `ctx setup` initializes local storage.
- `ctx sources` reports known local provider history paths.
- `ctx import` indexes supported local transcript formats.
- `ctx list` and `ctx show` inspect indexed items.
- `ctx search` refreshes discovered supported local transcript formats before
  returning ranked matches from the local index.
- `ctx doctor` and `ctx validate` report local storage health.
- JSON output supports local agents and scripts.

## Out Of Scope

- model inference by ctx;
- remote accounts or sync;
- browser UI;
- source repository modification;
- shell startup-file modification;
- API-key requirements for core setup/import/search;
- background collection;
- provider-native import claims that are not listed in the support matrix.

## Determinism

For the same database, query, filters, and result limit, search should return
the same ranked material in the same order. Timestamps such as `generated_at`
can differ between runs.

## Citation Contract

Results should preserve enough metadata for an agent to verify important
details:

- provider when known;
- session ID when known;
- event ID or event sequence when known;
- source path and cursor when available;
- source availability when checked.

If raw source files move, ctx may still return indexed text from SQLite. Output
should make source availability visible when that information is known.

## Privacy Contract

The local index and JSON output are private by default. A user must review and
redact copied output before sharing it outside the machine.
