# ctx-sdk for Rust

Experimental in-repo Rust SDK for the ctx `agent-history-v1` contract.

This crate is not published to crates.io. Its API may change while the SDK
contract is being shaped in-repo.

## Use

```rust
use ctx_sdk::{
    AgentHistoryClient, LocalBackendConfig, SearchClause, SearchOptions, SearchQuery,
    SearchRefresh,
};

let client = AgentHistoryClient::local(LocalBackendConfig::default());
let status = client.status()?;
let results = client.search(SearchOptions {
    query: Some(SearchQuery::new(vec![SearchClause::all("release notes")])),
    refresh: SearchRefresh::Off,
    ..SearchOptions::default()
})?;
# Ok::<(), ctx_sdk::AgentHistoryError>(())
```

## Backends

- Local backend: shells out to `ctx` JSON commands and never performs network
  calls or provider API calls.
- Hosted backend: accepted for future compatibility but currently returns a
  structured `not_supported` error.

## Public Operations

`status`, `init`, `sources`, `import_history`, `sync`, `search`, `show_event`,
`show_session`, `locate_event`, and `locate_session`.

The SDK returns `AgentHistoryEnvelope` values from `ctx-protocol` with stable
`agent-history-v1` fields. CLI JSON remains an adapter detail.

## Structured Search

Search accepts only `ctx-search-v1` structured queries or a file-only search.
The Rust SDK re-exports `SearchQuery`, the externally tagged `SearchClause`
enum, and the typed `SearchExecutionDiagnostics` tree directly from
`ctx-protocol`; it does not maintain a second DTO.

Queries are canonicalized and validated before the local adapter invokes
`ctx search --query-json <json> --json`. Search responses must use nested
schema version 2 and include `query_execution`; those machine fields retain
their exact `snake_case` wire names. `SearchOptions` exposes provider/source,
workspace/time, event/subagent, file, and session filters; result limits are
validated in `1..=200` before the CLI is started.
