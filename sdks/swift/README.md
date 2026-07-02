# ctx Agent History Swift SDK

Experimental Swift SDK for the local ctx `agent-history-v1` API.

This package is intended for local development from this repository only. It has
no registry publishing configuration and no external package dependencies.

## Local Use

Add the package by path from another local Swift package:

```swift
.package(path: "../ctx/sdks/swift")
```

Then depend on the library product:

```swift
.product(name: "CtxAgentHistory", package: "CtxAgentHistory")
```

## Example

```swift
import CtxAgentHistory

let client = AgentHistoryClient.local(dataRoot: "/tmp/ctx")

let status = try client.status()
let results = try client.search(
    "retry handling",
    options: SearchOptions(provider: "codex", limit: 10, refresh: "off")
)

print(status.status.initialized)
print(results.search.results.map(\.snippet))
```

## API

The public client mirrors the `agent-history-v1` operations:

- `status()`
- `initialize()`
- `sources()`
- `importHistory()`
- `sync()`
- `search()` with a query, term, or file option
- `showEvent()`
- `showSession()`
- `locateEvent()`
- `locateSession()`
- `version()` / `versioning()`

Swift reserves `init` for initializers, so the agent-history-v1 `init` operation is
exposed as `initialize()`. Returned envelopes still use `operation: "init"`.

## Local CLI Adapter

`AgentHistoryClient.local(...)` shells out to a local `ctx` binary and never performs
network calls:

```swift
let client = AgentHistoryClient.local(
    ctxPath: "/usr/local/bin/ctx",
    dataRoot: "/tmp/ctx-data",
    cwd: "/workspace/repo",
    env: ["CTX_LOG": "warn"]
)
```

For tests, inject a `CommandRunner` through `LocalCLIAdapter` so no real ctx
binary is required.

## Hosted Placeholder

Hosted configuration is reserved for a future ctx service:

```swift
let client = AgentHistoryClient.hosted(
    HostedConfig(baseURL: URL(string: "https://ctx.example.invalid")!)
)
```

Data operations throw `CtxAgentHistorySDKError` with code `.notSupported`. No network
request is made.

## Errors

Failures throw `CtxAgentHistorySDKError`, which includes a stable `code`, human
message, retryability, optional details, and optional command diagnostics for
local CLI failures.

## Fixtures

The XCTest suite decodes all JSON files in `contracts/agent-history-v1/fixtures` through
the Swift envelope model. Run tests from this repository checkout so the shared
contract fixture directory is available.

## Local Smoke Example

The package includes a fake-by-default toy executable that exercises
`status`, `initialize`, `importHistory`, `sync`, `search`, `showEvent`,
`showSession`, `locateEvent`, and `locateSession` without reading private local
history:

```bash
swift run LocalAgentHistorySmoke
```

To point it at a local ctx binary explicitly, opt in with `--real` and pass an
isolated data root:

```bash
swift run LocalAgentHistorySmoke --real --ctx-path /path/to/ctx --data-root /tmp/ctx-smoke
```
