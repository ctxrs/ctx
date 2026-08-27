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

Search includes primary and subagent sessions by default, groups exact
root-session claims, and returns one best result per group before repeats;
sessions without a root claim remain their own groups. Primary evidence is
slightly preferred only near ties; stronger child evidence can win. Set
`SearchOptions.PrimaryOnly` only for a deliberately primary-only search.

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

The adapter runs JSON-producing CLI commands such as `ctx status --format json`,
`ctx search <query>|--term <term>|--file <path> --format json`, and
`ctx show event --format json`, then normalizes CLI JSON into
`agent-history-v1` wrappers with `contractVersion` and `schemaVersion`.

Search hits, shown events, and `SessionRecord` expose provider identity,
including `ProviderSessionID` and `SourceFormat`; for Codex,
`ProviderSessionID` is the resume UUID. Shown event `Content` reports Core
completeness and selected/redacted/omitted policy. `Text` is the sole body, and
per-event source paths, cursors, source locations, and previews are omitted.

## Errors

SDK calls return `*ctxagenthistory.Error` for structured failures. Use
`ctxagenthistory.IsErrorKind(err, ctxagenthistory.ErrorKindCommandFailed)` when branching on
failure classes.

## Hosted Placeholder

`HostedConfig` and `NewHostedClient` reserve the hosted transport API. The
Hosted SDK placeholders are deprecated and will be removed in the next breaking
SDK revision; hosted operations remain unsupported. Calls continue to return
`ErrorKindHostedNotImplemented` without making network calls.
