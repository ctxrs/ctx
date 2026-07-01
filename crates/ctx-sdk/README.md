# ctx-sdk

`ctx-sdk` is an in-process Rust facade over ctx's local history primitives.
It opens the local SQLite store directly and calls the provider import and search
crates directly. It does not shell out to the `ctx` binary and does not parse CLI
JSON.

```rust,no_run
use ctx_sdk::{CaptureProvider, CtxClient, ImportOptions, SearchOptions};

# fn main() -> ctx_sdk::Result<()> {
let client = CtxClient::new()?;

client.import_path(
    CaptureProvider::Codex,
    "/home/me/.codex/sessions",
    ImportOptions::default(),
)?;

let results = client.search(
    "failed migration",
    SearchOptions::default().provider(CaptureProvider::Codex).limit(5),
)?;

for result in results.results {
    println!("{} {}", result.rank, result.title);
}
# Ok(())
# }
```

The SDK uses read-only SQLite connections for query APIs and opens a write
connection only for initialization or import calls.

## Tests

```bash
cargo test -p ctx-sdk
```

The default tests use sanitized real local-history formats. To run the opt-in
live OpenAI test, set `OPENAI_API_KEY` and `CTX_SDK_LIVE_OPENAI_MODEL` in the
environment, then opt in explicitly:

```bash
CTX_SDK_LIVE_OPENAI=1 \
cargo test -p ctx-sdk --test real_llm -- --ignored --nocapture
```

For OpenAI-compatible Chat Completions providers, also set
`CTX_SDK_LIVE_OPENAI_API=chat` and `OPENAI_BASE_URL`.
