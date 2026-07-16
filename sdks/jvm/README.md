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
- `locateEvent(String)` -> `LocateEventResponse`
- `locateSession(String)` -> `LocateSessionResponse`
- `version()` -> `VersionInfo`

All data responses extend `AgentHistoryEnvelope`, with `contractVersion`,
`schemaVersion`, `operation`, backend metadata, `asMap()`, and operation payload
access. Local mode shells out to the `ctx` CLI and performs no network calls or
provider API calls.

Hosted configuration is present as `AgentHistoryClient.hosted(HostedConfig)` and
returns a structured `not_supported` error until a hosted ctx service exists.

Search uses the same bounded structured contract as the CLI:

```java
SearchQuery query = SearchQuery.builder()
        .any(SearchClause.all("disk io pressure"))
        .any(SearchClause.semantic("indexing made the workstation sluggish"))
        .must(SearchClause.all("codex"))
        .mustNot(SearchClause.literal("logs_2.db"))
        .build();
SearchResponse response = client.search(AgentHistoryOptions.search().query(query));
```

The adapter collapses Unicode whitespace in non-literal clauses, trims literals,
removes duplicates within each placement, and then validates the 32-clause cap,
UTF-8 byte budgets, and the 1 to 32 analyzed-token range. Explicit result limits
must be from 1 to 200. Source identity filters map directly to their CLI flags:

```java
AgentHistoryOptions.Search options = AgentHistoryOptions.search()
        .query(query)
        .provider("custom")
        .historySource("dorkos/default")
        .providerKey("dorkos")
        .sourceId("default")
        .sourceFormat("dorkos-history-v1")
        .limit(Integer.valueOf(20));
```

Queries are sent with `ctx search --query-json`. Search results require
`schema_version: 2` and expose typed bounded-execution, semantic readiness,
coverage, completeness, and truncation diagnostics through
`SearchQueryExecution`. Top-level retrieval diagnostics remain available with
camel-cased keys; obsolete semantic weight/fallback fields and per-hit retrieval
details are omitted from the canonical SDK response.

## Example

```bash
sdks/jvm/scripts/test
```

The test script also compiles and runs `examples/ToyAgentHistoryApp.java`, a fake
transport toy app that exercises `status`, `search`, `showEvent`, and
`locateEvent` without reading local private history.

## Tests

```bash
sdks/jvm/scripts/test
```

The script uses `javac` and `java` directly. It has no external dependencies.
