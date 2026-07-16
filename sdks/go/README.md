# ctx Go SDK

Experimental Go SDK for the local `ctx` agent-history-v1 JSON contract.

The SDK has no third-party dependencies and defaults to the local `ctx` CLI. It
does not require network access or API keys.

```go
package main

import (
	"context"
	"fmt"
	"log"

	ctxagenthistory "github.com/ctxrs/ctx/sdks/go"
)

func main() {
	client := ctxagenthistory.NewLocalClient()

	status, err := client.Status(context.Background())
	if err != nil {
		log.Fatal(err)
	}
	fmt.Println(status.Status.IndexedItems)
}
```

## API

The public client mirrors agent-history-v1 operations:

- `Status(ctx)`
- `Init(ctx, InitOptions)`
- `Sources(ctx)`
- `Import(ctx, ImportOptions)`
- `Sync(ctx, ImportOptions)`, an alias for local import/index refresh
- `Search(ctx, SearchOptions)`
- `ShowEvent(ctx, ShowEventOptions)`
- `ShowSession(ctx, ShowSessionOptions)`
- `LocateEvent(ctx, LocateEventOptions)`
- `LocateSession(ctx, LocateSessionOptions)`

Version constants:

- `APIVersion`
- `SchemaVersion`
- `SearchQueryVersion`
- `SearchSchemaVersion`
- `SDKVersion`

## Search

Search accepts only the structured `ctx-search-v1` query contract (or a
file-only search). `any` clauses are alternatives, `must` clauses are global
requirements, and `must_not` clauses are global exclusions. Semantic clauses
are allowed once under `any` only.

```go
query := ctxagenthistory.NewSearchQuery(
	ctxagenthistory.SearchAll("disk io pressure"),
	ctxagenthistory.SearchSemantic("storage contention during indexing"),
)
query.Must = []ctxagenthistory.SearchClause{ctxagenthistory.SearchAll("codex")}

response, err := client.Search(context.Background(), ctxagenthistory.SearchOptions{
	Query:   &query,
	Backend: "hybrid",
	Limit:   20,
})
```

The SDK validates and canonicalizes queries before invoking `ctx`, passes them
with `--query-json`, requires nested search schema version 2, and exposes the
bounded `query_execution` diagnostics with their exact `snake_case` JSON keys.

## Local CLI

```go
client := ctxagenthistory.NewLocalClient(
	ctxagenthistory.WithCLIPath("/usr/local/bin/ctx"),
	ctxagenthistory.WithDataRoot("/tmp/ctx-data"),
)
```

The adapter runs JSON-producing CLI commands such as `ctx status --json`,
`ctx search --query-json <json> --json`, and
`ctx show event --format json`, then normalizes CLI JSON into
`agent-history-v1` wrappers with `contractVersion` and `schemaVersion`.

## Errors

SDK calls return `*ctxagenthistory.Error` for structured failures. Use
`ctxagenthistory.IsErrorKind(err, ctxagenthistory.ErrorKindCommandFailed)` when branching on
failure classes.

## Hosted Placeholder

`HostedConfig` and `NewHostedClient` reserve the hosted transport API. The
hosted transport is not implemented yet; operations return
`ErrorKindHostedNotImplemented` without making network calls.
