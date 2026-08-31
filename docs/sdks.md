# SDKs

ctx includes experimental in-repo SDKs for using agent history search from
tools, scripts, editors, and services.

The SDKs all target the same `agent-history-v1` contract. They are thin clients
over agent-history search primitives, not wrappers around provider-native
storage schemas, migrations, release tooling, or internal Rust crate shapes.

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
- version metadata
- structured errors

Ordinary SDK searches include primary and subagent sessions. Sessions with the
same exact root-session claim are grouped together, while sessions without one
remain their own groups; one best result is returned per group before repeats.
Primary evidence is slightly preferred only when nearly as relevant; stronger
child evidence can win. The language-specific `primaryOnly`,
`primary_only`, or `PrimaryOnly` option is the sole narrow-scope override.

Responses include the common `agent-history-v1` envelope fields:

- `contractVersion`
- `schemaVersion`
- `operation`
- `backend`

Payloads include typed agent history data such as freshness, citations,
sessions, events, and provider-owned session IDs. For Codex,
`providerSessionId` is the resume UUID.

Shown events can include full-content `activity` with exact typed provider call
identity, invocation and/or result channels, and literal provider facts. It is
an additive event field; keys inside captured JSON arguments and structured
results remain unchanged. See
[`mcp-exchange-capture.md`](mcp-exchange-capture.md).

## Local and hosted backends

Local clients execute the local `ctx` CLI and adapt its JSON into the public
`agent-history-v1` contract. The SDK adapter does not call provider APIs or
upload transcripts on its own. The local CLI stays on-machine with the built-in
executor, but a search can use the network and send raw query text and eligible
ctx-created document chunks when the data root has an explicitly selected
external semantic executor.

Hosted client configuration is reserved for future ctx service support. Until a
hosted service exists, hosted operations fail before network I/O with a
structured `not_supported` error.

## Dogfood examples

Each SDK includes a fake-by-default toy app or example that exercises the agent history
workflow without reading private local history:

`status -> init -> import/sync -> search -> showEvent -> showSession`

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
