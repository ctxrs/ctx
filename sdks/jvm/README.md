# ctx JVM SDK

Experimental in-repo JVM SDK for the ctx `agent-history-v1` contract.

This SDK is not published to Maven Central or any package registry. It is plain
Java source for now so Java and Kotlin callers can evaluate the API without a
large dependency footprint.

## API

`AgentHistoryClient.local()` exposes typed Java 11 response classes for:

- `status()` -> `StatusResponse`
- `init(InitOptions)` -> `InitResponse`
- `sources()` -> `SourcesResponse`
- `importHistory(ImportOptions)` / `sync(ImportOptions)` -> `ImportResponse`
- `search(SearchOptions)` -> `SearchResponse`
- `showEvent(String, ShowEventOptions)` -> `ShowEventResponse`
- `showSession(String, ShowSessionOptions)` -> `ShowSessionResponse`
- `version()` -> `VersionInfo`

`SearchHit`, `Event`, and `SessionSummary` expose provider identity, including
`providerSessionId` and `sourceFormat`; Codex uses `providerSessionId` as its
resume UUID. `Event.content()` returns typed Core completeness and
selected/redacted/omitted policy metadata. Event source paths, cursors,
source-location objects, and preview bodies are not exposed.

All data responses extend `AgentHistoryEnvelope`, with `contractVersion`,
`schemaVersion`, `operation`, backend metadata, `asMap()`, and operation payload
access. Local mode shells out to the `ctx` CLI and performs no network calls or
provider API calls.

The local adapter currently supports POSIX systems with either `setsid` or
`bash` available for race-free process-group ownership (including standard
Linux and macOS installations). It fails closed on Windows and on POSIX systems
without either containment launcher. Fake transports and typed response parsing
remain platform independent.

Hosted configuration is present as `AgentHistoryClient.hosted(HostedConfig)` and
returns a structured `not_supported` error until a hosted ctx service exists.

## Example

```bash
sdks/jvm/scripts/test
```

The test script also compiles and runs `examples/ToyAgentHistoryApp.java`, a fake
transport toy app that exercises `status`, `search`, `showEvent`, and
`showSession` without reading local private history.

## Tests

```bash
sdks/jvm/scripts/test
```

The script uses `javac` and `java` directly. It has no external dependencies.
