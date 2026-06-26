# Product Contract

ctx is a local search CLI for existing agent history.

## Promise

Given local provider transcripts that ctx supports, the CLI can build a local
SQLite index and return deterministic retrieval results with citations. The
product boundary is retrieval, not interpretation.

## In Scope

- `ctx setup` initializes local storage and indexes discovered supported local
  transcript formats.
- `ctx sources` reports known local provider history paths.
- `ctx import` indexes supported local transcript formats.
- `ctx list` reports indexed session rows.
- `ctx search` refreshes discovered supported local transcript formats before
  returning ranked local hits from the local index, with event IDs when a hit
  maps to an indexed event.
- `ctx show session` and `ctx show event` render transcripts, hits, and context
  windows using ctx-owned IDs.
- `ctx locate session` and `ctx locate event` report provenance and resume
  metadata.
- `ctx export session` writes or prints transcript artifacts.
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
- ctx-owned session and event IDs;
- provider-owned session ID when known;
- event sequence when known;
- source path and cursor when available;
- source availability when checked.

Provider-owned IDs are metadata. Positional command arguments are ctx-owned
IDs unless a command explicitly accepts `--provider ... --provider-session ...`.

If raw source files move, ctx may still return indexed text from SQLite. Output
should make source availability visible when that information is known.

## Privacy Contract

The local index and JSON output are private by default. A user must review and
redact copied output before sharing it outside the machine.
