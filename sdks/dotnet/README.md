# ctx Agent History SDK for .NET

Experimental C# SDK for the `agent-history-v1` ctx contract. The SDK is local-first by
default: it shells out to the `ctx` CLI, reads JSON from stdout, and wraps the
result in agent-history-v1 envelopes. Local mode does not make network calls or upload
transcripts.

The hosted configuration surface is present as a placeholder for a future ctx
service. Hosted operations currently throw a structured `not_supported` error.

## Projects

- `src/Ctx.AgentHistory/Ctx.AgentHistory.csproj` - SDK library, no NuGet publishing config.
- `tests/Ctx.AgentHistory.Tests/Ctx.AgentHistory.Tests.csproj` - dependency-free console
  smoke tests.
- `examples/LocalAgentHistorySmoke/LocalAgentHistorySmoke.csproj` - offline dogfood toy app
  that exercises status/search/show with a fake transport by default.

## Usage

```csharp
using Ctx.AgentHistory;

var client = AgentHistoryClient.Local(new LocalAgentHistoryConfig
{
    DataRoot = "/tmp/ctx-data",
    Timeout = TimeSpan.FromSeconds(30)
});

var status = await client.StatusAsync();
var sources = await client.SourcesAsync();
var imported = await client.ImportHistoryAsync(new ImportOptions
{
    Provider = "codex",
    Resume = true
});

var results = await client.SearchAsync(new SearchOptions
{
    Query = "local agent history",
    Provider = "codex",
    Refresh = "off",
    Limit = 10
});

Console.WriteLine(status.Status.Initialized);
Console.WriteLine(results.Search.Results.Count);
Console.WriteLine(results.ToJsonObject().ToJsonString());
```

## Public API

- `StatusAsync()`
- `InitAsync(InitOptions?)`
- `SourcesAsync()`
- `ImportHistoryAsync(ImportOptions?)`
- `SyncAsync(ImportOptions?)`
- `SearchAsync(SearchOptions)` with a query, term, or file option
- `ShowEventAsync(string, ShowEventOptions?)`
- `ShowSessionAsync(string, ShowSessionOptions?)`
- `ShowSessionAsync(ShowSessionOptions)`
- `VersionAsync()`
- `VersioningAsync()`

Agent history operations return hand-written response records/classes such as
`StatusResponse`, `SearchResponse`, `ShowEventResponse`, and
`ShowSessionResponse`. Each response exposes typed properties for stable
agent-history-v1 fields and `ToJsonObject()` for the canonical envelope, so unknown
future fields remain additive and accessible. SDK failures derive from
`CtxAgentHistoryException` and expose `Code`, `Retryable`, `Details`, and
`ToAgentHistoryError()`.

`SearchHit`, `AgentHistoryEvent`, and `SessionRecord` expose provider identity,
including `ProviderSessionId` and `SourceFormat`; for Codex,
`ProviderSessionId` is the resume UUID. Event `Content` carries typed Core
completeness and selected/redacted/omitted policy metadata. `Text` is the sole
body, and per-event source paths, cursors, source locations, and previews are
not exposed.

## Local CLI Adapter

`LocalCliAdapter` maps public operations to the local CLI:

- `ctx status --format json`
- `ctx setup --format json`
- `ctx sources --format json`
- `ctx import --format json`
- `ctx search <query>|--term <term>|--file <path> --format json`
- `ctx show event ... --format json`
- `ctx show session ... --format json`

Set `LocalAgentHistoryConfig.CtxBinary`, `DataRoot`, `WorkingDirectory`,
`Environment`, or `Timeout` to control command execution.

The adapter owns the complete CLI process tree for each request. Linux uses a
dedicated `setsid` process group (the `setsid` utility is required); Windows
assigns the suspended CLI root to a kill-on-close Job Object before it can run.
The local adapter fails closed on other operating systems with a structured
`CtxAgentHistoryCliException` (`adapter_error`); fake transports and typed
response parsing remain platform independent.
Timeouts cover both root-process exit and stdout/stderr EOF, and residual
descendants are terminated even after a successful root exit. Stdout is
accepted through the CLI's 64 MiB presentation ceiling; stderr remains bounded
to 16 MiB, and excess bytes are drained without unbounded retention.

## Tests

When the .NET SDK is installed:

```bash
dotnet build sdks/dotnet/src/Ctx.AgentHistory/Ctx.AgentHistory.csproj
dotnet run --project sdks/dotnet/tests/Ctx.AgentHistory.Tests/Ctx.AgentHistory.Tests.csproj
dotnet run --project sdks/dotnet/examples/LocalAgentHistorySmoke/LocalAgentHistorySmoke.csproj
```

The test project uses the shared fixtures under `contracts/agent-history-v1/fixtures`
and does not require a NuGet test framework.

`LocalAgentHistorySmoke` uses an in-process fake transport unless `CTX_AGENT_HISTORY_CTX` is
set to a local `ctx` binary path. Optional `CTX_AGENT_HISTORY_DATA_ROOT` controls the
data root for the env-configured local CLI mode.
