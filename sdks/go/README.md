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
- `SDKVersion`

## Local CLI

```go
client := ctxagenthistory.NewLocalClient(
	ctxagenthistory.WithCLIPath("/usr/local/bin/ctx"),
	ctxagenthistory.WithDataRoot("/tmp/ctx-data"),
)
```

The adapter runs JSON-producing CLI commands such as `ctx status --json`,
`ctx search <query>|--term <term>|--file <path> --json`, and
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
