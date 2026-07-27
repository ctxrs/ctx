# Product Contract

ctx is a local search CLI for existing agent history.

## Promise

Given local provider transcripts that ctx supports, the CLI builds a local
SQLite index and returns deterministic retrieval results with citations. The
paid Local Pro helper adds an encrypted local work graph with deterministic,
evidence-backed fact materialization. Neither path generates an LLM
interpretation, uploads transcript or repository content, or requires a hosted
research agent.

## In Scope

- `ctx setup` initializes local storage, indexes discovered supported local
  transcript formats, and can opportunistically start the default-on ctx-owned
  background daemon maintenance profile. An explicit `[daemon] enabled = false`
  remains a durable opt-out.
  `ctx setup --no-daemon`,
  `ctx setup --catalog-only`, and `ctx setup --json` do not autostart
  maintenance.
- `ctx sources` reports known local provider history paths, including whether a
  native source is currently importable.
- `ctx import` indexes supported local transcript formats and selected local
  history-source plugins, and can opportunistically start the same short
  one-pass ctx-owned maintenance profile when `[daemon].enabled` is true for
  native provider imports. `ctx import --no-daemon`, custom JSONL imports, explicit
  history-source-only imports, and `ctx import --json` do not autostart
  maintenance.
- `ctx search` can refresh a bounded batch from discovered native provider
  sources and enabled auto history-source plugins before returning ranked local
  hits from the local index, with event IDs when a hit maps to an indexed event.
  Default background refresh may autostart daemon maintenance even while
  semantic search is disabled. Semantic and hybrid search read existing local
  sidecar coverage only; search does not run vector backfill or download
  embedding models. Hybrid uses semantic evidence only after sidecar coverage
  is complete and dirty work is drained; explicit semantic search may query
  partial coverage for diagnostics.
- `ctx show session` and `ctx show event` render transcripts, hits, and context
  windows using ctx-owned IDs, and `ctx show session --out` writes transcript
  artifacts.
- `ctx locate session` and `ctx locate event` report provenance and resume
  metadata.
- `ctx pro` uses hosted WorkOS sign-in, activates a Stripe-backed trial or
  subscription, installs or repairs the signed target-specific helper, and
  catches the encrypted graph up. When Stripe returns a new Checkout session,
  the command waits for access in the same invocation. `ctx pro setup` is a
  supported explicit synonym.
  `ctx status` reports Pro state and the next useful action without mutating
  canonical history or graph data. Entitlement authorization may advance
  nonsecret anti-clock-rollback metadata.
  `ctx pro manage` opens hosted account and billing
  management. Interactive `ctx pro uninstall` asks whether to delete local Pro
  data; noninteractive callers must explicitly pass `--delete-data` or
  `--keep-data`. The keep path is local-only and preserves local Pro data;
  verified `--delete-data` remains idempotently available after the helper is
  gone.
- Pro materialization is an internal idempotent capability invoked by setup,
  daemon freshness, and graph queries. Repository roots are inferred from
  canonical activity rather than accepted as setup flags.
- Pro resource forms of `ctx show` and `ctx locate`, plus `ctx blame`,
  `ctx timeline`, and `ctx facts`, return bounded deterministic records with
  exact canonical citations. A graph query may catch stale derived state up;
  that changes only the encrypted graph. Pure canonical tail appends
  resume from the durable frontier; incompatible mutation epochs, legacy
  derived state, or repository semantics trigger a token-checked derived reset
  and replay without changing canonical history. Materialization reports
  `NotMaterialized`, `Partial`, `NeedsResume`, `NeedsRebuild`, or `Ready`; only
  facts and timeline expose continuation cursors. Typed relationship traversal
  remains available to agents through MCP.
- `ctx sql` runs one read-only SQL statement against the existing local index
  for advanced inspection when normal search is not expressive enough.
- `ctx doctor` reports local storage health.
- `ctx docs` exposes embedded public documentation and generated man pages.
- `ctx upgrade` checks and applies signed CLI releases for official
  installer-managed binaries.
- `ctx daemon` is the first-class local coordinator surface for status,
  enable/disable config, opportunistic maintenance started by setup/import when
  enabled, and foreground maintenance runs. The coordinator performs bounded
  native provider-history refresh and local semantic indexing/freshness work.
  Setup/import
  autostart reports semantic status read-only; explicit `ctx daemon run` is the
  path that may perform semantic catch-up.
- `ctx status` and `ctx doctor` report ctx-owned daemon coordinator state.
- JSON output supports local agents and scripts. Setup and import JSON do not
  autostart daemon maintenance; search JSON follows its explicit
  `--refresh background|off|wait` lifecycle.

## Local Pro Access States

| State | Meaning | Local Pro graph access |
| --- | --- | --- |
| trial | 14-day trial with a payment method | allowed |
| active | paid monthly subscription | allowed |
| canceling-paid | canceled at period end, but the current period is paid | allowed through the paid deadline |
| grace | the bounded signed offline grant remains valid | allowed through the final grace deadline |
| locked | no active access or valid grace remains | denied; encrypted graph is preserved for recovery |

`ctx pro manage` handles cancellation, payment recovery, and resubscription.
Core OSS setup, search, indexed show/locate, and SQL remain available in every
state. Pro uninstall and explicit Pro data deletion also remain available.
Hosted traffic is limited to identity,
billing, entitlements, signed release metadata, and authenticated artifact
delivery; it excludes transcript text, source code, repository paths or URLs,
Git objects, graph facts, citations, and queries.

## Out Of Scope

- hosted model inference, hidden LLM calls, or API-key-dependent inference by
  ctx; local semantic embedding is allowed only as documented search behavior;
- team and enterprise seats, invitations, SSO, SCIM, or organization
  administration;
- annual plans, device caps, and device-management UI;
- hosted transcript storage, hosted work graphs, or remote repository analysis;
- ask/brief agents, cloud research agents, or universal deterministic detector
  accuracy;
- a ctx history or work-graph browser UI;
- source repository modification;
- shell startup-file modification;
- write-capable SQL access;
- API-key requirements for core setup/import/search;
- provider-owned history daemons, hooks, or background collection outside
  documented ctx-owned daemon maintenance;
- self-upgrade for unmanaged source builds, package-manager installs, or copied
  binaries;
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

The local index, encrypted Pro graph, and JSON output are private by default.
Identity, billing, entitlement, and signed-artifact requests do not carry
history, repository, graph, citation, or query content. A user must review
copied output before sharing it outside the machine.
