# Rust SDK

`ctx-sdk` is an in-process Rust facade for local ctx history. It is intended for
Rust applications and agent hosts that want typed access to ctx without spawning
the `ctx` CLI.

The SDK opens the local SQLite store directly and uses the existing
`ctx-history-*` crates for provider import, search, and transcript lookup. Query
APIs use read-only SQLite connections. Import and initialization APIs open a
write connection only when they need to create or update the local store.

```rust
use ctx_sdk::{CaptureProvider, CtxClient, ImportOptions, SearchOptions};

fn main() -> ctx_sdk::Result<()> {
    let client = CtxClient::new()?;

    client.import_path(
        CaptureProvider::Codex,
        "/home/me/.codex/sessions",
        ImportOptions::default(),
    )?;

    let packet = client.search(
        "failed migration",
        SearchOptions::default().provider(CaptureProvider::Codex).limit(5),
    )?;

    for result in packet.results {
        println!("{} {}", result.rank, result.title);
    }

    Ok(())
}
```

Core entry points:

- `CtxClient::new()` uses `CTX_DATA_ROOT` or the default `~/.ctx` data root.
- `CtxClient::with_data_root(path)` isolates the SDK to a specific ctx data root.
- `status()` inspects store state without creating it.
- `init()` creates the local store if needed.
- `sources()` and `sources_for_provider()` discover local provider history.
- `import_path()`, `import_sources()`, and `import_available_sources()` import
  provider history in process.
- `search()` returns `ctx_history_search::SearchPacket` directly.
- `show_session()`, `show_event()`, `locate_session()`, and `locate_event()`
  return typed session/event structures without CLI rendering.

The SDK crate is currently workspace-local because the lower-level
`ctx-history-*` crates are still internal workspace crates. It can be used from
the repository or as a git dependency while the public crate packaging boundary
is finalized.

## Testing

Run the SDK tests from the workspace root:

```bash
cargo test -p ctx-sdk
```

The default SDK suite imports real local-history formats from sanitized Codex
and Pi fixtures. It does not call provider CLIs, read user history, require API
keys, or make network calls.

There is also an opt-in live LLM integration test. It calls OpenAI, writes the
model response into a temporary Codex-style local-history file, then verifies
that the SDK can import, search, and load the typed transcript without spawning
the `ctx` CLI. Set `OPENAI_API_KEY` and `CTX_SDK_LIVE_OPENAI_MODEL` in the
environment before opting in:

```bash
CTX_SDK_LIVE_OPENAI=1 \
cargo test -p ctx-sdk --test real_llm -- --ignored --nocapture
```

For OpenAI-compatible Chat Completions providers, set
`CTX_SDK_LIVE_OPENAI_API=chat` and `OPENAI_BASE_URL` to the provider's `/v1`
base URL.
