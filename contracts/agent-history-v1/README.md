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
| `locateEvent()` | Return event provenance and source location. |
| `locateSession()` | Return session provenance and source location. |

## Privacy

Local mode is local-first. SDKs must not make network calls in local mode and
must not upload transcript content. CLI stderr progress can contain local paths
and is not included in successful SDK responses unless a language exposes it as
debug metadata outside this contract.

## Shapes

The authoritative machine-readable shape lives in
[`schema.json`](./schema.json). Golden fixtures in [`fixtures`](./fixtures) are
shared by all SDK tests.

Important reusable records:

- `ProviderSource`: provider, path, availability, importability, raw retention.
- `Freshness`: pre-search refresh mode/status/totals.
- `Citation`: ctx event/session/file/source citation fields.
- `SourceLocation`: path/cursor/source id/source format/existence.
- Structured error: `code`, `message`, `retryable`, optional `details`, and
  optional `cause`.

## CLI Adapter Mapping

Current SDK local adapters call these private CLI JSON commands and normalize
them into `agent-history-v1` wrappers:

- `ctx status --json`
- `ctx setup --json`
- `ctx sources --json`
- `ctx import --json`
- `ctx search <query>|--term <term>|--file <path> --json`
- `ctx show event ... --format json`
- `ctx show session ... --format json`
- `ctx locate event ... --format json`
- `ctx locate session ... --format json`

This mapping is an adapter detail. SDK consumers should depend on
`agent-history-v1`, not on CLI rendering or SQLite storage.
