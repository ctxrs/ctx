using System.Text.Json.Nodes;
using Ctx.AgentHistory;

internal static class Program
{
    private static async Task<int> Main()
    {
        var tests = new (string Name, Func<Task> Body)[]
        {
            ("wraps status as agent-history-v1", WrapsStatus),
            ("preserves additive response fields", PreservesAdditiveFields),
            ("builds local CLI operation arguments", BuildsOperationArguments),
            ("normalizes setup init status", NormalizesSetupInitStatus),
            ("builds search flags", BuildsSearchFlags),
            ("rejects search without intent", RejectsSearchWithoutIntent),
            ("wraps show and locate commands", WrapsShowAndLocate),
            ("reports versioning metadata", ReportsVersioning),
            ("uses agent-history-v1 error codes", UsesAgentHistoryV1ErrorCodes),
            ("raises structured hosted placeholder errors", HostedPlaceholderError),
            ("loads shared agent-history-v1 fixtures", LoadsSharedFixtures)
        };

        var failures = 0;
        foreach (var test in tests)
        {
            try
            {
                await test.Body();
                Console.WriteLine($"ok - {test.Name}");
            }
            catch (Exception ex)
            {
                failures++;
                Console.Error.WriteLine($"not ok - {test.Name}: {ex.Message}");
                Console.Error.WriteLine(ex);
            }
        }

        return failures == 0 ? 0 : 1;
    }

    private static async Task NormalizesSetupInitStatus()
    {
        var transport = new RecordingTransport("""{"schema_version":1,"data_root":"/tmp/ctx","mode":"ready","indexed_items":9,"network_required":false}""");
        var client = new AgentHistoryClient(transport);

        var response = await client.InitAsync(new InitOptions { CatalogOnly = true });

        Equal("init", response.Operation);
        Equal(true, response.Status.Initialized);
        Equal(true, response.Status.LocalOnly);
        Equal(9, response.Status.IndexedItems ?? -1);
    }

    private static async Task WrapsStatus()
    {
        var transport = new RecordingTransport("""{"schema_version":1,"initialized":true,"data_root":"/tmp/ctx","database_path":"/tmp/ctx/history.sqlite3","indexed_items":4,"local_only":true}""");
        var client = new AgentHistoryClient(transport);

        var status = await client.StatusAsync();

        Equal("agent-history-v1", status.ContractVersion);
        Equal("status", status.Operation);
        Equal("local", status.Backend.Kind);
        Equal(true, status.Status.Initialized);
        Equal(4, status.Status.IndexedItems ?? -1);

        var envelope = status.ToJsonObject();
        Equal("agent-history-v1", envelope["contractVersion"]!.GetValue<string>());
        Equal(4, envelope["status"]!["indexedItems"]!.GetValue<int>());
    }

    private static async Task PreservesAdditiveFields()
    {
        var transport = new RecordingTransport("""{"schema_version":1,"initialized":true,"future_counter":7,"freshness":{"mode":"off"}}""");
        var client = new AgentHistoryClient(transport);

        var status = await client.StatusAsync();

        Equal(7, status.ToJsonObject()["status"]!["futureCounter"]!.GetValue<int>());
        Equal("off", status.Status.Freshness!.Mode ?? "");
    }

    private static async Task BuildsOperationArguments()
    {
        var transport = new RecordingTransport("""{"schema_version":1,"totals":{},"sources":[]}""");
        var client = new AgentHistoryClient(transport);

        await client.StatusAsync();
        await client.InitAsync(new InitOptions { CatalogOnly = true });
        await client.SourcesAsync();
        await client.ImportHistoryAsync(new ImportOptions { Provider = "codex", Resume = true });
        await client.SyncAsync(new ImportOptions { All = true });

        Equal("status --json", Join(transport.Calls[0]));
        Equal("setup --json --progress none --catalog-only", Join(transport.Calls[1]));
        Equal("sources --json", Join(transport.Calls[2]));
        Equal("import --json --progress none --provider codex --resume", Join(transport.Calls[3]));
        Equal("import --json --progress none --all", Join(transport.Calls[4]));
    }

    private static async Task BuildsSearchFlags()
    {
        var transport = new RecordingTransport("""{"schema_version":1,"query":"retry","results":[],"freshness":{"mode":"off"}}""");
        var client = new AgentHistoryClient(transport);

        var response = await client.SearchAsync(new SearchOptions
        {
            Query = "retry",
            Terms = ["timeout", "backoff"],
            Limit = 5,
            Provider = "codex",
            Workspace = "ctx",
            Since = "30d",
            PrimaryOnly = true,
            IncludeSubagents = true,
            EventType = "message",
            File = "src/lib.rs",
            Session = "session-1",
            Events = true,
            Refresh = "off",
            IncludeCurrentSession = true
        });

        Equal("search retry --term timeout --term backoff --limit 5 --provider codex --workspace ctx --since 30d --primary-only --include-subagents --event-type message --file src/lib.rs --session session-1 --events --refresh off --include-current-session --json", Join(transport.Calls[0]));
        Equal("search", response.Operation);
        Equal("retry", response.Search.Query ?? "");
        Equal("off", response.Search.Freshness!.Mode ?? "");
    }

    private static async Task RejectsSearchWithoutIntent()
    {
        var transport = new RecordingTransport("""{"schema_version":1,"results":[]}""");
        var client = new AgentHistoryClient(transport);

        await ThrowsAsync<CtxAgentHistoryValidationException>(() => client.SearchAsync());
        await ThrowsAsync<CtxAgentHistoryValidationException>(() => client.SearchAsync(new SearchOptions
        {
            Refresh = "off",
            Limit = 5
        }));
        await ThrowsAsync<CtxAgentHistoryValidationException>(() => client.SearchAsync(new SearchOptions
        {
            Query = "   "
        }));

        Equal(0, transport.Calls.Count);
    }

    private static async Task WrapsShowAndLocate()
    {
        var transport = new RecordingTransport("""{"schema_version":1,"events":[],"source":{"path":"/tmp/source.jsonl"},"ctx_session_id":"session-1","provider":"codex"}""");
        var client = new AgentHistoryClient(transport);

        await client.ShowEventAsync("event-1", new ShowEventOptions { Window = 2 });
        await client.ShowSessionAsync("session-1", new ShowSessionOptions { Mode = "full" });
        await client.ShowSessionAsync(new ShowSessionOptions { Provider = "codex", ProviderSessionId = "provider-session", Mode = "lite" });
        await client.LocateEventAsync("event-1");
        await client.LocateSessionAsync(new SessionLookupOptions { Provider = "codex", ProviderSessionId = "provider-session" });

        Equal("show event event-1 --format json --window 2", Join(transport.Calls[0]));
        Equal("show session session-1 --mode full --format json", Join(transport.Calls[1]));
        Equal("show session --provider codex --provider-session provider-session --mode lite --format json", Join(transport.Calls[2]));
        Equal("locate event event-1 --format json", Join(transport.Calls[3]));
        Equal("locate session --provider codex --provider-session provider-session --format json", Join(transport.Calls[4]));

        await ThrowsAsync<CtxAgentHistoryValidationException>(() => client.ShowEventAsync(""));
        await ThrowsAsync<CtxAgentHistoryValidationException>(() => client.LocateSessionAsync(new SessionLookupOptions { Provider = "codex" }));
    }

    private static async Task ReportsVersioning()
    {
        var transport = new RecordingTransport("{}") { CtxVersion = "ctx 1.2.3" };
        var client = new AgentHistoryClient(transport);

        var version = await client.VersionAsync();
        Equal(CtxAgentHistoryVersions.ContractVersion, version.ApiVersion);
        Equal("test", version.Transport);
        Equal("ctx 1.2.3", version.CtxVersion ?? "");

        var versioning = await client.VersioningAsync();
        Equal(CtxAgentHistoryVersions.SdkVersion, versioning.SdkVersion);
    }

    private static Task HostedPlaceholderError()
    {
        var client = AgentHistoryClient.Hosted(new HostedAgentHistoryConfig("https://ctx.example.invalid"));
        return ThrowsAsync<HostedTransportNotImplementedException>(async () =>
        {
            try
            {
                await client.StatusAsync();
            }
            catch (HostedTransportNotImplementedException ex)
            {
                Equal("not_supported", ex.Code);
                Equal("hosted", ex.Details["backend"]!.GetValue<string>());
                Equal("status", ex.Details["method"]!.GetValue<string>());
                throw;
            }
        });
    }

    private static Task UsesAgentHistoryV1ErrorCodes()
    {
        Equal("invalid_request", new CtxAgentHistoryValidationException("bad").Code);
        Equal("decode_error", new CtxAgentHistoryProtocolException("bad").Code);
        Equal("adapter_error", new CtxAgentHistoryCliException("bad", ["ctx"], 1, "", "").Code);
        Equal("timeout", new CtxAgentHistoryCliException("timeout", ["ctx"], -1, "", "", code: "timeout", retryable: true).Code);
        Equal(true, new CtxAgentHistoryCliException("timeout", ["ctx"], -1, "", "", code: "timeout", retryable: true).Retryable);
        Equal("unknown", new CtxAgentHistoryException("unknown").Code);
        return Task.CompletedTask;
    }

    private static async Task LoadsSharedFixtures()
    {
        var fixtures = FindFixtures();
        var seen = 0;
        foreach (var path in Directory.EnumerateFiles(fixtures, "*.json").Order())
        {
            seen++;
            var node = JsonNode.Parse(File.ReadAllText(path))?.AsObject()
                ?? throw new InvalidOperationException($"{path} did not contain a JSON object");
            Equal("agent-history-v1", node["contractVersion"]!.GetValue<string>());
            Equal(1, node["schemaVersion"]!.GetValue<int>());
            var operation = node["operation"]!.GetValue<string>();
            switch (operation)
            {
                case "status":
                    True((await ClientFor(node["status"]).StatusAsync()).Status.Initialized, $"{path} status not initialized");
                    break;
                case "init":
                    True((await ClientFor(node["status"]).InitAsync()).Status.Initialized, $"{path} init not initialized");
                    break;
                case "sources":
                    True((await ClientFor(new JsonObject { ["sources"] = Clone(node["sources"]) }).SourcesAsync()).Sources.Count > 0, $"{path} sources empty");
                    break;
                case "import":
                case "sync":
                    if (operation == "import")
                    {
                        _ = (await ClientFor(node["import"]).ImportHistoryAsync()).Import.Totals.ImportedEvents;
                    }
                    else
                    {
                        _ = (await ClientFor(node["import"]).SyncAsync()).Import.Totals.ImportedEvents;
                    }
                    break;
                case "search":
                    _ = (await ClientFor(node["search"]).SearchAsync(new SearchOptions { Query = "fixture search" })).Search.Results;
                    break;
                case "showEvent":
                    _ = (await ClientFor(node["event"]).ShowEventAsync("event-1")).Event.Events;
                    break;
                case "showSession":
                    _ = (await ClientFor(node["session"]).ShowSessionAsync("session-1")).Session.Events;
                    break;
                case "locateEvent":
                    _ = (await ClientFor(node["location"]).LocateEventAsync("event-1")).Location.Source;
                    break;
                case "locateSession":
                    _ = (await ClientFor(node["location"]).LocateSessionAsync("session-1")).Location.Source;
                    break;
                case "error":
                    True(node.ContainsKey("error"), $"{path} missing error payload");
                    break;
                default:
                    throw new InvalidOperationException($"unknown fixture operation {operation} in {path}");
            }
        }
        True(seen > 0, "expected shared agent-history-v1 fixtures");
    }

    private static AgentHistoryClient ClientFor(JsonNode? payload)
    {
        return new AgentHistoryClient(new RecordingTransport(Clone(payload)?.ToJsonString() ?? "{}"));
    }

    private static JsonNode? Clone(JsonNode? node)
    {
        return node is null ? null : JsonNode.Parse(node.ToJsonString());
    }

    private static string FindFixtures()
    {
        foreach (var start in new[] { Directory.GetCurrentDirectory(), AppContext.BaseDirectory })
        {
            var dir = new DirectoryInfo(start);
            while (dir is not null)
            {
                var candidate = Path.Combine(dir.FullName, "contracts", "agent-history-v1", "fixtures");
                if (Directory.Exists(candidate))
                {
                    return candidate;
                }
                dir = dir.Parent;
            }
        }
        throw new DirectoryNotFoundException("contracts/agent-history-v1/fixtures");
    }

    private static string Join(IReadOnlyList<string> values) => string.Join(" ", values);

    private static void Equal<T>(T expected, T actual)
    {
        if (!EqualityComparer<T>.Default.Equals(expected, actual))
        {
            throw new InvalidOperationException($"expected {expected}, got {actual}");
        }
    }

    private static void True(bool value, string message)
    {
        if (!value)
        {
            throw new InvalidOperationException(message);
        }
    }

    private static async Task ThrowsAsync<T>(Func<Task> action) where T : Exception
    {
        try
        {
            await action();
        }
        catch (T)
        {
            return;
        }
        throw new InvalidOperationException($"expected {typeof(T).Name}");
    }

    private sealed class RecordingTransport : IAgentHistoryTransport
    {
        private readonly string _response;

        public RecordingTransport(string response)
        {
            _response = response;
        }

        public string Name => "test";
        public string? CtxVersion { get; init; }
        public List<IReadOnlyList<string>> Calls { get; } = [];

        public JsonObject Backend(JsonObject? raw = null)
        {
            return new JsonObject
            {
                ["kind"] = "local",
                ["dataRoot"] = raw?["data_root"]?.GetValue<string>() ?? "/tmp/ctx-test"
            };
        }

        public Task<JsonObject> ExecuteJsonAsync(string operation, IReadOnlyList<string> args, CancellationToken cancellationToken = default)
        {
            Calls.Add(args.ToArray());
            return Task.FromResult(JsonNode.Parse(_response)!.AsObject());
        }

        public Task<string?> GetCtxVersionAsync(CancellationToken cancellationToken = default)
        {
            return Task.FromResult(CtxVersion);
        }
    }
}
