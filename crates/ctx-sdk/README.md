# ctx-sdk for Rust

Experimental in-repo Rust SDK for the ctx `agent-history-v1` contract.

This crate is not published to crates.io. Its API may change while the SDK
contract is being shaped in-repo.

## Use

```rust
use ctx_sdk::{LocalBackendConfig, AgentHistoryClient, SearchOptions, SearchRefresh};

let client = AgentHistoryClient::local(LocalBackendConfig::default());
let status = client.status()?;
let results = client.search(SearchOptions {
    query: Some("release notes".to_owned()),
    refresh: SearchRefresh::Off,
    ..SearchOptions::default()
})?;
# Ok::<(), ctx_sdk::AgentHistoryError>(())
```

## Backends

- Local backend: shells out to `ctx` JSON commands and never performs network
  calls or provider API calls.
- Hosted backend: Hosted SDK placeholders are deprecated and will be removed in
  the next breaking SDK revision; hosted operations remain unsupported. Valid
  operations continue to return a structured `not_supported` error.

## Public Operations

`status`, `init`, `sources`, `import_history`, `sync`, `search`, `show_event`,
and `show_session`.

The SDK returns `AgentHistoryEnvelope` values from `ctx-protocol` with stable
`agent-history-v1` fields. CLI JSON remains an adapter detail.

Search hits, shown events, and typed session summaries expose Core `provider`,
`provider_session_id`, and `source_format` identity where applicable. For
Codex, `provider_session_id` is the directly usable resume UUID. Shown events
carry typed completeness and selected/redacted/omitted policy metadata; `text`
is the sole body and no path, cursor, source-location, or preview body is
published.
