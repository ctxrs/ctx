# Product Contract

ctx is a local search CLI for existing agent history.

## Promise

Given local provider histories that ctx supports, the CLI treats those native
records as the acquisition authority for import and refresh, publishes an
immutable local Core/Tantivy search generation, and returns deterministic
retrieval results with citations and stable provider/source identities. Core
does not generate an LLM interpretation, upload transcript content, or require
a hosted research agent.

## In Scope

- `ctx setup` initializes local storage, publishes discovered supported local
  transcript formats, and in automatic indexing mode can start persistent
  background indexing. Manual indexing runs no persistent or background daemon.
  `ctx setup --no-daemon` is the one-run daemon-autostart opt-out. Output format
  does not change setup autostart behavior. The deprecated `--catalog-only` flag
  is ignored and does not change setup behavior.
- `ctx sources` reports known local provider history paths, including whether a
  native source is currently importable.
- `ctx sources add [--replace]` and `ctx sources remove` safely edit named
  provider history roots in local configuration. Replacement retains provider
  and stable name while atomically changing path and complete group state.
- `ctx import` publishes supported local transcript formats and selected local
  history-source plugins as complete normalized Core records. In automatic
  mode it may start the persistent daemon. In manual mode an explicit import
  may start the same daemon/Core refresh engine as a finite worker, waits for
  authoritative Core publication, and then lets that worker exit.
  Explicit custom JSONL and history-source imports use its required
  daemon-owned source-refresh endpoint. `ctx import --no-daemon` never starts
  it and therefore requires an already-running endpoint for those explicit
  provider-source routes.
- `ctx search` can request a bounded refresh of discovered native provider
  sources before returning ranked local hits from the active Core generation,
  with event IDs when a hit maps to an imported event. History-source plugins
  enter Core only through explicit single-source import in 1.0.
  In automatic mode, default background refresh may autostart persistent daemon
  maintenance. In manual mode, default and explicit background refresh read
  only the last published generation and never start or wake a worker.
  Explicit `--refresh wait` may start a finite Core worker; `--refresh off`
  never starts or wakes one. Semantic and hybrid search read existing local
  sidecar coverage only; search does not run vector backfill or download
  embedding models. Hybrid uses semantic evidence only after sidecar coverage
  is complete and dirty work is drained; explicit semantic search may query
  partial coverage for diagnostics.
- `ctx show session` and `ctx show event` resolve ctx-owned identities and read complete
  policy-selected normalized records from the active Core/Tantivy generation.
- `ctx list events` provides
  typed indexed selection, deterministic bounded pages, and streaming JSONL;
  it is not a general expression language or another persistent database.
  These commands do not reopen provider history at query time.
  `ctx show session --out` writes transcript artifacts. Search/show expose the
  provider-owned session ID when known; for Codex, it is the resume UUID.
- Official managed distributions may pair Apache-licensed Core with a
  separately signed private companion. Core-only distributions retain the OSS
  commands, and paid routes return a typed companion-unavailable failure when
  that companion is absent.
- `ctx doctor` reports local storage health.
- `ctx docs` exposes embedded public documentation and generated man pages.
- `ctx upgrade` checks and applies signed CLI releases for official
  installer-managed binaries.
- `ctx index` is the focused indexing surface. With no subcommand it shows a
  one-shot status view; `ctx index mode` reads or changes `auto|manual` mode;
  `ctx index watch` follows progress; and `ctx index wait` blocks for selected
  readiness. The canonical config is `[indexing] mode = "auto"|"manual"`, with
  auto as the default. Auto permits persistent background maintenance; manual
  disables it while retaining finite explicit refresh workers. The mode-change
  commands persist the choice and immediately reconcile daemon supervision.
- `ctx status` and `ctx doctor` report ctx-owned daemon and supervisor health.
- `ctx daemon run` is an advanced foreground, blocking maintenance command. It
  does not change the configured indexing mode. The daemon performs bounded
  native provider-history refresh and local semantic indexing/freshness work.
- `ctx semantic enable|status|disable` owns the local semantic-search lifecycle.
  Enablement is the explicit opt-in and starts daemon-owned model acquisition
  and catch-up in auto mode; status is read-only; disablement retains downloaded
  assets. Lexical search remains available while embeddings build; hybrid
  search uses lexical and semantic evidence when semantic coverage is ready.
- `ctx stats` reports bounded local usage/value aggregates from the separate
  owner-private `usage.sqlite` sidecar. This default-on product state is
  independent of remote event reporting, has no network path or identity, keeps
  only daily UTC content-free aggregates for approximately 400 days, and fails
  open at foreground recording boundaries. Completed companion-backed Blame is
  represented only by Core-observed calls, technical outcomes, durations, and
  exact MCP response bytes; no private result semantics enter the store. The
  stats report is read-only, uncounted, and separates measured facts from
  versioned estimates. Detail is an option on `ctx stats`; enable, disable, and
  logical reset remain under `ctx status --usage`.
- Output format does not grant or remove refresh authority. Implicit/background
  operations remain inert in manual mode; explicit import and search
  `--refresh wait` may use a finite worker in either human or JSON output.

## Out Of Scope

- hosted model inference, hidden LLM calls, or API-key-dependent inference by
  ctx; local semantic embedding is allowed only as documented search behavior;
- team and enterprise seats, invitations, SSO, SCIM, or organization
  administration;
- annual plans, device caps, and device-management UI;
- hosted transcript storage or remote repository analysis;
- ask/brief agents, cloud research agents, or universal deterministic detector
  accuracy;
- a ctx history browser UI;
- source repository modification;
- shell startup-file modification;
- API-key requirements for core setup/import/search;
- provider-owned history daemons, hooks, or background collection outside
  documented ctx-owned daemon maintenance;
- self-upgrade for unmanaged source builds, package-manager installs, or copied
  binaries;
- provider-native import claims that are not listed in the support matrix.

## Determinism

For the same verified generation, query, filters, and result limit, search
should return the same ranked material in the same order. Timestamps such as
`generated_at` can differ between runs.

## Citation Contract

Results should preserve enough metadata for an agent to verify important
details:

- provider when known;
- ctx-owned session and event IDs;
- provider-owned session ID when known;
- event sequence when known.

Provider-owned IDs are metadata. Positional command arguments are ctx-owned
IDs unless a command explicitly accepts `--provider ... --provider-session ...`.

Search and show read indexed content and complete policy-selected normalized
records from the active Core/Tantivy generation. Explicit import and background
refresh publish provider-file changes into a new search generation.

## Privacy Contract

Core/Tantivy generations, semantic sidecars, `usage.sqlite`, and JSON output
are private by default. A user must review copied output before sharing it
outside the machine.
