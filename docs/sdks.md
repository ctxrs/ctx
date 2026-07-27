# SDKs

ctx includes experimental in-repo SDKs for using agent history search from
tools, scripts, editors, and services.

The SDKs all target the same `agent-history-v1` contract. They are thin clients over
agent-history search primitives, not wrappers around SQLite tables, migrations, release
tooling, or internal Rust crate shapes.

## Status

The SDKs are in-repo only for now. Their APIs are intended to be stable enough
to dogfood and review, but package-manager publishing is intentionally deferred
while the contract settles.

Do not expect npm, PyPI, crates.io, Maven Central, Swift package registry,
NuGet, or Go module tag releases yet. Use the source checkout directly.

## SDK directories

| Language | Directory |
| --- | --- |
| TypeScript / JavaScript | `sdks/typescript` |
| Python | `sdks/python` |
| Rust | `crates/ctx-sdk` |
| Go | `sdks/go` |
| Java / Kotlin JVM | `sdks/jvm` |
| Swift | `sdks/swift` |
| .NET / C# | `sdks/dotnet` |

Shared contract files live under `contracts/agent-history-v1`.

## API shape

Each SDK exposes typed operation-specific responses for:

- `status`
- `init`
- `sources`
- `import` or `sync`
- `search`
- `showEvent`
- `showSession`
- `locateEvent`
- `locateSession`
- version metadata
- structured errors

Responses include the common `agent-history-v1` envelope fields:

- `contractVersion`
- `schemaVersion`
- `operation`
- `backend`

Payloads include typed agent history data such as freshness, citations, sessions,
events, and source locations.

## Local and hosted backends

Local clients execute the local `ctx` CLI and adapt its JSON into the public
`agent-history-v1` contract. Local mode stays local-first: it does not make network
calls, call provider APIs, require API keys, or upload transcripts.

Hosted client configuration is reserved for future ctx service support. Until a
hosted service exists, hosted operations fail before network I/O with a
structured `not_supported` error.

## Dogfood examples

Each SDK includes a fake-by-default toy app or example that exercises the agent history
workflow without reading private local history:

`status -> init -> import/sync -> search -> showEvent -> showSession -> locateEvent -> locateSession`

The examples can be pointed at a real local ctx binary explicitly when the
language toolchain is installed and an isolated `CTX_DATA_ROOT` is provided.

## Checks

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

Opt-in local smoke:

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

Use strict toolchain mode in CI lanes that provision every non-Rust language
runtime:

```bash
CTX_SDK_STRICT_TOOLCHAINS=1 ./scripts/check-sdks.sh
```

## Related docs

- [`contracts/agent-history-v1/README.md`](../contracts/agent-history-v1/README.md)
- [`docs/sdk-production-readiness.md`](sdk-production-readiness.md)
- [`docs/agent-skill-install.md`](agent-skill-install.md)
