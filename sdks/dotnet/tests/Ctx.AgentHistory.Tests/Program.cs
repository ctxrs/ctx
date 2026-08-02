using System.Text.Json.Nodes;
using Ctx.AgentHistory;

internal static class Program
{
    private static async Task<int> Main(string[] args)
    {
        if (args.SequenceEqual(new[] { "status", "--format=json" }))
        {
            Console.WriteLine(new JsonObject
            {
                ["analyticsEnabled"] = Environment.GetEnvironmentVariable("CTX_ANALYTICS_ENABLED")
            }.ToJsonString());
            return 0;
        }

        var tests = new (string Name, Func<Task> Body)[]
        {
            ("wraps status as agent-history-v1", WrapsStatus),
            ("filters status to the current readiness contract", FiltersStatusFields),
            ("preserves legitimate source semantics", PreservesLegitimateSourceSemantics),
            ("builds local CLI operation arguments", BuildsOperationArguments),
            ("forces analytics off after ambient and user environment merging", ForcesAnalyticsOff),
            ("normalizes setup init status", NormalizesSetupInitStatus),
            ("builds search flags", BuildsSearchFlags),
            ("camelizes search retrieval json", CamelizesSearchRetrievalJson),
            ("rejects search without intent", RejectsSearchWithoutIntent),
            ("wraps show commands", WrapsShow),
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

    private static async Task ForcesAnalyticsOff()
    {
        const string analyticsEnabled = "CTX_ANALYTICS_ENABLED";
        var original = Environment.GetEnvironmentVariable(analyticsEnabled);
        try
        {
            Environment.SetEnvironmentVariable(analyticsEnabled, "true");
            Equal("true", Environment.GetEnvironmentVariable(analyticsEnabled));

            var executableName = OperatingSystem.IsWindows()
                ? "Ctx.AgentHistory.Tests.exe"
                : "Ctx.AgentHistory.Tests";
            var executable = Path.Combine(AppContext.BaseDirectory, executableName);
            True(File.Exists(executable), $"test helper executable not found: {executable}");

            var adapter = new LocalCliAdapter(new LocalAgentHistoryConfig
            {
                CtxBinary = executable,
                Environment = new Dictionary<string, string?>
                {
                    [analyticsEnabled] = "true"
                }
            });

            var raw = await adapter.ExecuteJsonAsync("status", ["status", "--format=json"]);
            Equal("false", raw["analyticsEnabled"]!.GetValue<string>());
        }
        finally
        {
            Environment.SetEnvironmentVariable(analyticsEnabled, original);
        }
    }

    private static async Task NormalizesSetupInitStatus()
    {
        var transport = new RecordingTransport("""{"schema_version":2,"initialized":true,"data_root":"/tmp/ctx","mode":"ready","indexed_items":2147483648,"indexed_sessions":2147483649,"indexed_events":2147483650,"indexed_sources":2147483651,"lexical":{"status":"ready","generation_id":"gen-64"},"refresh":{"status":"ready","generation_id":"gen-64"},"network_required":false}""");
        var client = new AgentHistoryClient(transport);

        var response = await client.InitAsync(new InitOptions());

        Equal("init", response.Operation);
        Equal(true, response.Status.Initialized);
        Equal(true, response.Status.LocalOnly);
        Equal(2147483648UL, response.Status.IndexedItems ?? 0UL);
        Equal(2147483649UL, response.Status.IndexedSessions ?? 0UL);
        Equal(2147483650UL, response.Status.IndexedEvents ?? 0UL);
        Equal(2147483651UL, response.Status.IndexedSources ?? 0UL);
    }

    private static async Task WrapsStatus()
    {
        var transport = new RecordingTransport("""{"schema_version":1,"data_root":"/tmp/ctx","indexed_items":4,"indexed_sessions":2,"indexed_events":2,"lexical":{"status":"ready","generation_id":"gen-4"},"refresh":{"status":"ready","generation_id":"gen-4"},"local_only":true}""");
        var client = new AgentHistoryClient(transport);

        var status = await client.StatusAsync();

        Equal("agent-history-v1", status.ContractVersion);
        Equal("status", status.Operation);
        Equal("local", status.Backend.Kind);
        Equal(true, status.Status.Initialized);
        Equal(4UL, status.Status.IndexedItems ?? 0UL);

        var envelope = status.ToJsonObject();
        Equal("agent-history-v1", envelope["contractVersion"]!.GetValue<string>());
        Equal(4UL, envelope["status"]!["indexedItems"]!.GetValue<ulong>());
    }

    private static async Task FiltersStatusFields()
    {
        var transport = new RecordingTransport("""{"schema_version":1,"future_counter":7,"lexical":{"status":"ready","generation_id":"gen-1"},"refresh":{"status":"ready"}}""");
        var client = new AgentHistoryClient(transport);

        var status = await client.StatusAsync();

        True(status.ToJsonObject()["status"]!["futureCounter"] is null, "unexpected future status field");
        Equal("gen-1", status.Status.Lexical["generationId"]!.GetValue<string>());
    }

    private static async Task PreservesLegitimateSourceSemantics()
    {
        var acquisition = """{"source":"local_scan","cursor":"opaque-checkpoint"}""";
        var sourceClient = new AgentHistoryClient(new RecordingTransport(
            $$"""{"sources":[{"provider":"codex","path":"/configured/root","status":"available","importable":true,"acquisition":{{acquisition}}}]}"""));
        var sources = await sourceClient.SourcesAsync();
        var normalizedAcquisition = sources.Sources[0].ToJsonObject()["acquisition"]!.AsObject();
        Equal("local_scan", normalizedAcquisition["source"]!.GetValue<string>());
        Equal("opaque-checkpoint", normalizedAcquisition["cursor"]!.GetValue<string>());

        var importClient = new AgentHistoryClient(new RecordingTransport(
            $$"""{"resume":false,"totals":{},"sources":[{"source":{{acquisition}}}]}"""));
        var imported = await importClient.ImportAsync();
        var normalizedSource = imported.Import.Sources[0].ToJsonObject()["source"]!.AsObject();
        Equal("local_scan", normalizedSource["source"]!.GetValue<string>());
        Equal("opaque-checkpoint", normalizedSource["cursor"]!.GetValue<string>());
    }

    private static async Task BuildsOperationArguments()
    {
        var transport = new RecordingTransport("""{"schema_version":1,"totals":{},"sources":[]}""");
        var client = new AgentHistoryClient(transport);

        await client.StatusAsync();
        await client.InitAsync(new InitOptions());
        await client.SourcesAsync();
        await client.ImportHistoryAsync(new ImportOptions { Provider = "codex", Resume = true });
        await client.SyncAsync(new ImportOptions { All = true });

        Equal("status --format=json", Join(transport.Calls[0]));
        Equal("setup --format=json --progress none", Join(transport.Calls[1]));
        Equal("sources --format=json", Join(transport.Calls[2]));
        Equal("import --format=json --progress none --provider codex --resume", Join(transport.Calls[3]));
        Equal("import --format=json --progress none --all", Join(transport.Calls[4]));
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
            Backend = "hybrid",
            SemanticWeight = 0.35,
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

        Equal("search retry --term timeout --term backoff --limit 5 --backend hybrid --semantic-weight 0.35 --provider codex --workspace ctx --since 30d --primary-only --include-subagents --event-type message --file src/lib.rs --session session-1 --events --refresh off --include-current-session --format=json", Join(transport.Calls[0]));
        Equal("search", response.Operation);
        Equal("retry", response.Search.Query ?? "");
        Equal("off", response.Search.Freshness!.Mode ?? "");
    }

    private static async Task CamelizesSearchRetrievalJson()
    {
        var transport = new RecordingTransport("""
            {
              "schema_version": 1,
              "payloadType": "search_results",
              "query": "agent history",
              "retrieval": {
                "requested_mode": "hybrid",
                "effective_mode": "lexical",
                "semantic_weight": 0.0,
                "semantic_fallback_code": "semantic_retrieval_failed",
                "semantic_fallback": "semantic_retrieval_failed",
                "coverage": {"embedded_items":4,"indexed_now":1},
                "diagnostics": {"query_embed_ms":2}
              },
              "results": [
                {
                  "result_type": "event",
                  "recordType": "event",
                  "itemType": "event",
                  "result_scope": "event",
                  "provider": "codex",
                  "provider_session_id": "codex-resume-uuid",
                  "source_format": "codex_session_jsonl",
                  "rank": 1,
                  "retrieval_score": 0.98,
                  "citations": [{"target_type":"event","label":"codex event"}]
                }
              ],
              "result_window": {"limit":1,"returned":1,"more_available":true}
            }
            """);
        var client = new AgentHistoryClient(transport);

        var response = await client.SearchAsync(new SearchOptions { Query = "agent history" });

        var retrieval = response.Search.Retrieval!.AsObject();
        Equal("hybrid", retrieval["requestedMode"]!.GetValue<string>());
        Equal("lexical", retrieval["effectiveMode"]!.GetValue<string>());
        Equal(0.0, retrieval["semanticWeight"]!.GetValue<double>());
        Equal("semantic_retrieval_failed", retrieval["semanticFallbackCode"]!.GetValue<string>());
        Equal("semantic_retrieval_failed", retrieval["semanticFallback"]!.GetValue<string>());
        Equal(4, retrieval["coverage"]!["embeddedItems"]!.GetValue<int>());
        Equal(1, retrieval["coverage"]!["indexedNow"]!.GetValue<int>());
        Equal(2, retrieval["diagnostics"]!["queryEmbedMs"]!.GetValue<int>());
        True(!response.Search.ToJsonObject().ContainsKey("payloadType"), "search payload leaked payloadType");
        True(!response.Search.Results[0].ToJsonObject().ContainsKey("recordType"), "search hit leaked recordType");
        True(!response.Search.Results[0].ToJsonObject().ContainsKey("itemType"), "search hit leaked itemType");
        Equal("event", response.Search.Results[0].ResultType ?? "");
        Equal("codex", response.Search.Results[0].Provider ?? "");
        Equal("codex-resume-uuid", response.Search.Results[0].ProviderSessionId ?? "");
        Equal("codex_session_jsonl", response.Search.Results[0].SourceFormat ?? "");
        Equal(1.0, response.Search.Results[0].Rank ?? 0.0);
        Equal(0.98, response.Search.Results[0].RetrievalScore ?? 0.0);
        Equal("event", response.Search.Results[0].Citations[0].TargetType ?? "");
        Equal(1, response.Search.ResultWindow!.Limit);
        Equal(1, response.Search.ResultWindow.Returned);
        Equal(true, response.Search.ResultWindow.MoreAvailable);
        Equal(1, response.Search.Pagination["limit"]!.GetValue<int>());
        Equal(true, response.Search.Pagination["hasMore"]!.GetValue<bool>());
        True(!response.Search.Pagination.ContainsKey("nextCursor"), "search pagination invented a cursor");
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

    private static async Task WrapsShow()
    {
        var transport = new RecordingTransport("""{"schema_version":1,"events":[],"ctx_session_id":"session-1","provider":"codex"}""");
        var client = new AgentHistoryClient(transport);

        await client.ShowEventAsync("event-1", new ShowEventOptions { Window = 2 });
        await client.ShowSessionAsync("session-1", new ShowSessionOptions { Mode = "full" });
        await client.ShowSessionAsync(new ShowSessionOptions { Provider = "codex", ProviderSessionId = "provider-session", Mode = "lite" });

        Equal("show event event-1 --format=json --window 2", Join(transport.Calls[0]));
        Equal("show session session-1 --mode full --format=json", Join(transport.Calls[1]));
        Equal("show session --provider codex --provider-session provider-session --mode lite --format=json", Join(transport.Calls[2]));

        await ThrowsAsync<CtxAgentHistoryValidationException>(() => client.ShowEventAsync(""));
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
                    var search = (await ClientFor(node["search"]).SearchAsync(new SearchOptions { Query = "fixture search" })).Search;
                    Equal(search.Results.Count, search.ResultWindow!.Returned);
                    Equal(search.ResultWindow.Limit, search.Pagination["limit"]!.GetValue<int>());
                    Equal(search.ResultWindow.MoreAvailable, search.Pagination["hasMore"]!.GetValue<bool>());
                    if (search.Results.Count > 0)
                    {
                        Equal(1.0, search.Results[0].Rank ?? 0.0);
                        Equal(0.98, search.Results[0].RetrievalScore ?? 0.0);
                    }
                    break;
                case "showEvent":
                    var shownEvent = (await ClientFor(node["event"]).ShowEventAsync("event-1")).Event.Event!;
                    Equal("codex-fixture-session", shownEvent.ProviderSessionId ?? "");
                    Equal("codex_session_jsonl", shownEvent.SourceFormat ?? "");
                    Equal(true, shownEvent.Content!.Complete);
                    Equal(CoreContentPolicyStatus.Selected, shownEvent.Content.PolicyStatus!.Value);
                    break;
                case "showSession":
                    var summary = (await ClientFor(node["session"]).ShowSessionAsync("session-1")).Session.Session!;
                    Equal("codex-fixture-session", summary.ProviderSessionId ?? "");
                    Equal("codex_session_jsonl", summary.SourceFormat ?? "");
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
