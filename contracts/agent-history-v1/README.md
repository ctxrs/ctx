# ctx Agent History Contract v1

`agent-history-v1` is the experimental in-repo SDK contract for embedding ctx as
agent history search infrastructure. It is intentionally product-shaped rather
than a mirror of ctx storage internals.

The contract supports two backends:

- `local`: shells out to a local `ctx` CLI and never performs network calls,
  provider API calls, or transcript uploads.
- `hosted`: reserved for a future hosted ctx API. Current SDKs accept hosted
  configuration but return a structured `not_supported` error for operations.

SDKs must not expose SQLite schema details, migration internals, Buildkite or
release tooling, or raw Rust crate shapes as their public API.

## Versioning

- Contract id: `agent-history-v1`
- Current schema version: `1`
- SDKs expose their own SDK version separately from `contractVersion`.
- Unknown JSON fields are additive and must be ignored or preserved.
- Required fields can only change in a future contract id.

## Public Operations

All operations return JSON objects with `contractVersion: "agent-history-v1"` and
`schemaVersion: 1`, or raise/return a structured SDK error.

| Operation | Purpose |
| --- | --- |
| `status()` | Read local index status and freshness metadata. |
| `init()` | Initialize local ctx storage, optionally catalog-only. |
| `sources()` | List local provider sources and importability. |
| `importHistory()` / `sync()` | Import local provider history into ctx. |
| `search()` | Search indexed agent history. |
| `showEvent()` | Return one event or an event window. |
| `showSession()` | Return a session transcript. |

## Privacy

Local mode is local-first. SDKs must not make network calls in local mode and
must not upload transcript content. CLI stderr progress can contain local paths
and is not included in successful SDK responses unless a language exposes it as
debug metadata outside this contract.

Every local adapter launches every `ctx` child process with
`CTX_ANALYTICS_ENABLED=false`. The adapter applies this reserved value after
inheriting the ambient environment and merging caller-supplied environment
overrides, so SDK callers cannot re-enable network analytics. This guard is
process-scoped to SDK-launched local CLI commands, including version probes; it
does not change the analytics configuration or environment contract for users
who invoke the standalone `ctx` CLI directly.

## Shapes

The authoritative machine-readable shape lives in
[`schema.json`](./schema.json). Golden fixtures in [`fixtures`](./fixtures) are
shared by all SDK tests.

Important reusable records:

- `ProviderSource`: provider, path, availability, and importability.
- `Freshness`: pre-search refresh mode/status/totals.
- `Status.semantic` and `Status.daemon`: extensible local diagnostic objects for
  semantic coverage and the ctx-owned daemon coordinator.
- `SearchRetrieval`: requested/effective retrieval mode, applied semantic
  weight, semantic coverage, optional fallback code/message, and optional
  diagnostics. The CLI adapter camel-cases raw CLI retrieval fields for this
  contract.
- `Citation`: stable ctx event/session citation fields. Public search/show
  records do not expose provider source paths, cursors, or source-location
  objects.
- `CoreContentMetadata`: whether a shown event has complete Core content, the
  selected/redacted/omitted policy status, and an optional policy reason. `text`
  is the only textual body; there is no alternate `preview` body.
- `SessionSummary`: typed Core session identity, including `provider`,
  `providerSessionId`, and `sourceFormat`.
- Search hits, shown events, and session summaries expose `providerSessionId`
  when Core retained the provider-owned session identity. For Codex, this is
  the resume UUID used by Codex tooling.
- Structured error: `code`, `message`, `retryable`, optional `details`, and
  optional `cause`.

## CLI Adapter Mapping

Current SDK local adapters call these private CLI JSON commands and normalize
them into `agent-history-v1` wrappers:

- `ctx status --format json`
- `ctx setup --format json`
- `ctx sources --format json`
- `ctx import --format json`
- `ctx search <query>|--term <term>|--file <path> --format json`
- `ctx show event ... --format json`
- `ctx show session ... --format json`

This mapping is an adapter detail. SDK consumers should depend on
`agent-history-v1`, not on CLI rendering or SQLite storage.

Local CLI output can include absolute diagnostic paths such as `vectorPath`,
`lockPath`, or `statusPath` inside extensible semantic/daemon/retrieval objects.
They are troubleshooting metadata for the current machine, not portable IDs; SDK
consumers should not persist them as contract state or send them to hosted
services unless deliberately collecting local diagnostics.
