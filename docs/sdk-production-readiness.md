# SDK Production Readiness

The in-repo SDKs are product-facing agent history clients for
`agent-history-v1`. They are not package-manager artifacts yet, but they should
be boringly reliable before external consumers build on them.

## Public API Rules

- Return operation-specific typed envelopes: `StatusResponse`,
  `SearchResponse`, `ShowEventResponse`, and so on.
- Keep `contractVersion`, `schemaVersion`, `operation`, and `backend` visible in
  every response type.
- Use language-native enum/newtype patterns for known strings, while allowing
  additive future values where ctx can grow.
- Keep local adapter details out of the public product contract. CLI JSON,
  provider-native storage paths/schemas, migrations, and release tooling are
  adapter internals.
- Local mode must not perform network calls, provider API calls, or transcript
  upload.
- Hosted mode may be configurable, but until a hosted service exists operations
  must fail before network I/O with `not_supported`.

## Test Rules

Every SDK should have:

- unit tests for request construction and typed response decoding;
- fixture conformance tests for every JSON file in
  `contracts/agent-history-v1/fixtures`;
- structured error tests for all `agent-history-v1` error codes;
- timeout/cancellation tests using idiomatic language primitives;
- a dogfood toy app or example that exercises `init`, `import` or `sync`,
  `search`, `showEvent`, and `showSession`
  against fake transport by default, plus an opt-in real local ctx smoke where
  the toolchain supports it.

Tests must use temp data roots and sanitized fixtures. They must not require
network, API keys, package publishing, hosted ctx, or private user history.

## Commands

Rust SDK tests are native Bazel targets:

```bash
scripts/bazelw test \
  //crates/ctx-protocol:unit_tests \
  //crates/ctx-sdk:unit_tests \
  --config=test
```

Contract and non-Rust SDK checks:

```bash
./scripts/check-sdks.sh
```

`check-sdks.sh` does not run Rust or Cargo tests; the Bazel targets above are
the authoritative Rust SDK test path.

Strict toolchain mode for CI lanes that provision all non-Rust language
runtimes:

```bash
CTX_SDK_STRICT_TOOLCHAINS=1 ./scripts/check-sdks.sh
```

Opt-in local smoke through the built `ctx` CLI:

```bash
CTX_SDK_RUN_LOCAL_SMOKE=1 ./scripts/check-sdks.sh
```

Package dry-runs without publishing:

```bash
./scripts/sdk-package-dry-run.sh
```

No-publish guardrail:

```bash
./scripts/check-sdk-no-publish.sh
```

## Toolchain Floors

- Node.js 20+
- Python 3.10+
- Rust stable compatible with the workspace `rust-version` in `Cargo.toml`
- Go 1.22+
- Java 11+
- Swift 5.9+
- .NET 8+

The repository should keep these as CI configuration, not as assumptions hidden
inside one developer machine.
