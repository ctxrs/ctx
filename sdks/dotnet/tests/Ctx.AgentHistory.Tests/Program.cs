using System.Diagnostics;
using System.Text.Json.Nodes;
using Ctx.AgentHistory;

internal static class Program
{
    private static async Task<int> Main(string[] args)
    {
        if (args.FirstOrDefault() == "__ctx_sdk_helper")
        {
            return await RunProcessHelper(args.Skip(1).ToArray());
        }
        var tests = new (string Name, Func<Task> Body)[]
        {
            ("wraps status as agent-history-v1", WrapsStatus),
            ("preserves additive response fields", PreservesAdditiveFields),
            ("builds local CLI operation arguments", BuildsOperationArguments),
            ("normalizes setup init status", NormalizesSetupInitStatus),
            ("builds search flags", BuildsSearchFlags),
            ("canonicalizes search queries", CanonicalizesSearchQueries),
            ("enforces bounded search query limits", EnforcesSearchQueryLimits),
            ("validates search result limits before transport", ValidatesSearchResultLimits),
            ("decodes schema-v2 search diagnostics", DecodesSearchDiagnostics),
            ("rejects noncanonical schema-v2 responses", RejectsNoncanonicalSearchSchemas),
            ("bounds local CLI output capture", BoundsLocalCliCapture),
            ("handles adversarial local CLI process capture", AdversarialLocalCliCapture),
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
        var transport = new RecordingTransport(EmptySearchJson());
        var client = new AgentHistoryClient(transport);

        var response = await client.SearchAsync(new SearchOptions
        {
            Query = new SearchQueryV1
            {
                Any = [SearchClause.All("retry"), SearchClause.Semantic("timeout backoff behavior")],
                Must = [SearchClause.All("ctx")]
            },
            Limit = 5,
            Backend = "hybrid",
            Provider = "codex",
            HistorySource = "codex/default",
            ProviderKey = "codex",
            SourceId = "default",
            SourceFormat = "codex_session_jsonl",
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

        Equal("search --query-json {\"version\":\"ctx-search-v1\",\"any\":[{\"all\":\"retry\"},{\"semantic\":\"timeout backoff behavior\"}],\"must\":[{\"all\":\"ctx\"}]} --limit 5 --backend hybrid --provider codex --history-source codex/default --provider-key codex --source-id default --source-format codex_session_jsonl --workspace ctx --since 30d --primary-only --include-subagents --event-type message --file src/lib.rs --session session-1 --events --refresh off --include-current-session --json", Join(transport.Calls[0]));
        Equal("search", response.Operation);
        Equal("agent history", response.Search.Query!.Any[0].Value);
        Equal("off", response.Search.Freshness!.Mode ?? "");
    }

    private static Task CanonicalizesSearchQueries()
    {
        var query = new SearchQueryV1
        {
            Any =
            [
                SearchClause.All("  cafe\u0301\u00A0\u4E16\u754C  "),
                SearchClause.All("cafe\u0301 \u4E16\u754C"),
                SearchClause.Phrase("\u2003retry\t path\u3000")
            ],
            MustNot = [SearchClause.Literal("\u00A0logs_2.db  \t backup\u3000")]
        };

        var canonical = query.Validate();
        Equal(2, canonical.Any.Count);
        Equal("cafe\u0301 \u4E16\u754C", canonical.Any[0].Value);
        Equal("retry path", canonical.Any[1].Value);
        Equal("logs_2.db  \t backup", canonical.MustNot[0].Value);

        var serialized = JsonNode.Parse(query.ToJson())!.AsObject();
        Equal("cafe\u0301 \u4E16\u754C", serialized["any"]![0]!["all"]!.GetValue<string>());
        var duplicates = new SearchQueryV1
        {
            Any = Enumerable.Repeat(SearchClause.All(" duplicate "), 33).ToArray()
        };
        Equal(1, duplicates.Validate().Any.Count);
        return Task.CompletedTask;
    }

    private static Task EnforcesSearchQueryLimits()
    {
        var exactClauses = Enumerable.Range(0, SearchQueryV1.MaxClauses)
            .Select(index => SearchClause.All($"term{index}"))
            .ToArray();
        Equal(SearchQueryV1.MaxClauses, new SearchQueryV1 { Any = exactClauses }.Validate().Any.Count);
        Throws<CtxAgentHistoryValidationException>(() => new SearchQueryV1
        {
            Any = [.. exactClauses, SearchClause.All("overflow")]
        }.Validate());

        var exactTokens = string.Join(" ", new[] { "cafe\u0301ine\u200Dword" }.Concat(
            Enumerable.Range(1, SearchQueryV1.MaxAnalyzedTokensPerClause - 1)
                .Select(index => $"t{index}")));
        _ = new SearchQueryV1 { Any = [SearchClause.All(exactTokens)] }.Validate();
        Throws<CtxAgentHistoryValidationException>(() => new SearchQueryV1
        {
            Any = [SearchClause.All($"{exactTokens} overflow")]
        }.Validate());
        Throws<CtxAgentHistoryValidationException>(() => new SearchQueryV1
        {
            Any = [SearchClause.All("\u0301\u200D---")]
        }.Validate());

        _ = new SearchQueryV1 { Any = [SearchClause.All(new string('a', SearchQueryV1.MaxClauseBytes))] }.Validate();
        Throws<CtxAgentHistoryValidationException>(() => new SearchQueryV1
        {
            Any = [SearchClause.All(new string('a', SearchQueryV1.MaxClauseBytes + 1))]
        }.Validate());
        _ = new SearchQueryV1 { Any = [SearchClause.Literal(new string('a', SearchQueryV1.MinLiteralBytes))] }.Validate();
        _ = new SearchQueryV1 { Any = [SearchClause.Literal(new string('a', SearchQueryV1.MaxLiteralBytes))] }.Validate();
        Throws<CtxAgentHistoryValidationException>(() => new SearchQueryV1
        {
            Any = [SearchClause.Literal(new string('a', SearchQueryV1.MinLiteralBytes - 1))]
        }.Validate());
        Throws<CtxAgentHistoryValidationException>(() => new SearchQueryV1
        {
            Any = [SearchClause.Literal(new string('a', SearchQueryV1.MaxLiteralBytes + 1))]
        }.Validate());

        var fullSizeClauses = Enumerable.Range(0, 9)
            .Select(index =>
            {
                var prefix = $"term{index}";
                return SearchClause.All(prefix + new string('a', SearchQueryV1.MaxClauseBytes - prefix.Length));
            })
            .ToArray();
        _ = new SearchQueryV1 { Any = fullSizeClauses[..8] }.Validate();
        Throws<CtxAgentHistoryValidationException>(() => new SearchQueryV1 { Any = fullSizeClauses }.Validate());
        return Task.CompletedTask;
    }

    private static async Task ValidatesSearchResultLimits()
    {
        var transport = new RecordingTransport(EmptySearchJson());
        var client = new AgentHistoryClient(transport);
        foreach (var limit in new[] { -1, 0, 201 })
        {
            await ThrowsAsync<CtxAgentHistoryValidationException>(() => client.SearchAsync(new SearchOptions
            {
                Query = SearchQuery(),
                Limit = limit
            }));
        }
        Equal(0, transport.Calls.Count);

        await client.SearchAsync(new SearchOptions { Query = SearchQuery(), Limit = 1 });
        await client.SearchAsync(new SearchOptions { Query = SearchQuery(), Limit = 200 });
        True(Join(transport.Calls[0]).Contains("--limit 1"), "minimum search limit was not forwarded");
        True(Join(transport.Calls[1]).Contains("--limit 200"), "maximum search limit was not forwarded");
    }

    private static async Task DecodesSearchDiagnostics()
    {
        var transport = new RecordingTransport(EmptySearchJson());
        var client = new AgentHistoryClient(transport);
        var response = await client.SearchAsync(new SearchOptions { Query = SearchQuery() });
        Equal(2, response.Search.SchemaVersion);
        Equal("ctx-search-v1", response.Search.QueryExecution.QueryVersion);
        Equal(16_384, response.Search.QueryExecution.Resolved.CandidateRows);
        Equal("lexical", response.Search.QueryExecution.Semantic.EffectiveBackend);
        True(response.Search.ToJsonObject().ContainsKey("query_execution"), "query_execution lost snake_case wire name");
        var retrieval = response.Search.Retrieval?.AsObject()
            ?? throw new InvalidOperationException("search retrieval diagnostics missing");
        Equal("hybrid", retrieval["requestedMode"]!.GetValue<string>());
        foreach (var key in new[]
        {
            "semantic_weight", "semanticWeight", "semantic_fallback_code",
            "semanticFallbackCode", "semantic_fallback", "semanticFallback"
        })
        {
            True(!retrieval.ContainsKey(key), $"obsolete retrieval field {key} was retained");
        }
    }

    private static async Task RejectsNoncanonicalSearchSchemas()
    {
        var schemaOne = EmptySearchObject();
        schemaOne["schema_version"] = 1;

        var stringQuery = EmptySearchObject();
        stringQuery["query"] = "agent history";

        var camelSchema = EmptySearchObject();
        camelSchema.Remove("schema_version");
        camelSchema["schemaVersion"] = 2;

        var camelExecution = EmptySearchObject();
        camelExecution["queryExecution"] = Clone(camelExecution["query_execution"]);
        camelExecution.Remove("query_execution");

        var missingQuery = EmptySearchObject();
        missingQuery.Remove("query");

        var missingResults = EmptySearchObject();
        missingResults.Remove("results");

        var fractionalSchema = EmptySearchObject();
        fractionalSchema["schema_version"] = 2.5;

        foreach (var response in new[]
        {
            schemaOne, stringQuery, camelSchema, camelExecution,
            missingQuery, missingResults, fractionalSchema
        })
        {
            var client = new AgentHistoryClient(new RecordingTransport(response.ToJsonString()));
            await ThrowsAsync<CtxAgentHistoryProtocolException>(() => client.SearchAsync(new SearchOptions
            {
                Query = SearchQuery()
            }));
        }
    }

    private static async Task RejectsSearchWithoutIntent()
    {
        var transport = new RecordingTransport(EmptySearchJson());
        var client = new AgentHistoryClient(transport);

        await ThrowsAsync<CtxAgentHistoryValidationException>(() => client.SearchAsync());
        await ThrowsAsync<CtxAgentHistoryValidationException>(() => client.SearchAsync(new SearchOptions
        {
            Refresh = "off",
            Limit = 5
        }));
        await ThrowsAsync<CtxAgentHistoryValidationException>(() => client.SearchAsync(new SearchOptions
        {
            Query = new SearchQueryV1 { MustNot = [SearchClause.All("negative only")] }
        }));
        await ThrowsAsync<CtxAgentHistoryValidationException>(() => client.SearchAsync(new SearchOptions
        {
            Query = new SearchQueryV1 { Must = [SearchClause.Semantic("invalid placement")] }
        }));

        Equal(0, transport.Calls.Count);
    }

    private static async Task BoundsLocalCliCapture()
    {
        if (OperatingSystem.IsWindows())
        {
            return;
        }
        var directory = Path.Combine(Path.GetTempPath(), $"ctx-dotnet-capture-{Guid.NewGuid():N}");
        Directory.CreateDirectory(directory);
        var script = Path.Combine(directory, "ctx-overflow");
        try
        {
            await File.WriteAllTextAsync(script, "#!/bin/sh\nexec dd if=/dev/zero bs=65536 count=33 2>/dev/null\n");
            File.SetUnixFileMode(
                script,
                UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
            var adapter = new LocalCliAdapter(new LocalAgentHistoryConfig
            {
                CtxBinary = script,
                Timeout = TimeSpan.FromSeconds(5)
            });
            var error = await ThrowsAsync<CtxAgentHistoryException>(() =>
                adapter.ExecuteJsonAsync("status", ["status", "--json"]));
            Equal("capture_limit", error.Code);
            Equal("stdout", error.Details["stream"]!.GetValue<string>());
            Equal(2 * 1024 * 1024, error.Details["capBytes"]!.GetValue<int>());
            True(!error.Details.ContainsKey("stdout"), "capture error exposed retained stdout");
            True(!error.Details.ContainsKey("stderr"), "capture error exposed retained stderr");
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    private static async Task AdversarialLocalCliCapture()
    {
        var hasLauncher = !string.IsNullOrWhiteSpace(
            Environment.GetEnvironmentVariable("CTX_SDK_PROCESS_SCOPE_LAUNCHER"));
        var hasSetsid = !OperatingSystem.IsWindows()
            && new[] { "/usr/bin/setsid", "/bin/setsid" }.Any(File.Exists);
        var nativeScope = hasLauncher || hasSetsid;
        var executable = Environment.ProcessPath
            ?? throw new InvalidOperationException("test executable path is unavailable");
        LocalCliAdapter Adapter(TimeSpan timeout) => new(new LocalAgentHistoryConfig
        {
            CtxBinary = executable,
            Timeout = timeout
        });
        var directory = Path.Combine(Path.GetTempPath(), $"ctx-dotnet-process-{Guid.NewGuid():N}");
        Directory.CreateDirectory(directory);
        try
        {
            if (!nativeScope)
            {
                var unavailable = await ThrowsAsync<CtxAgentHistoryException>(() =>
                    Adapter(TimeSpan.FromSeconds(2))
                        .ExecuteJsonAsync("__ctx_sdk_helper", ["dual"]));
                Equal("capture_failure", unavailable.Code);
                Equal("process_scope", unavailable.Details["stream"]!.GetValue<string>());
                return;
            }
            _ = await Adapter(TimeSpan.FromSeconds(2))
                .ExecuteJsonAsync("__ctx_sdk_helper", ["dual"]);

            var started = Stopwatch.StartNew();
            var overflow = await ThrowsAsync<CtxAgentHistoryException>(() =>
                Adapter(TimeSpan.FromSeconds(2))
                    .ExecuteJsonAsync("__ctx_sdk_helper", ["stderr-first"]));
            Equal("capture_limit", overflow.Code);
            Equal("stderr", overflow.Details["stream"]!.GetValue<string>());
            True(started.Elapsed < TimeSpan.FromSeconds(2), "stderr-first overflow exceeded bounded teardown");

            var inheritedAlive = Path.Combine(directory, "inherited.alive");
            var inheritedPid = Path.Combine(directory, "inherited.pid");
            started.Restart();
            var inheritedError = await ThrowsAsync<CtxAgentHistoryException>(() =>
                Adapter(TimeSpan.FromSeconds(5)).ExecuteJsonAsync(
                    "__ctx_sdk_helper",
                    ["inherit", inheritedAlive, inheritedPid]));
            Equal("capture_failure", inheritedError.Code);
            True(started.Elapsed < TimeSpan.FromSeconds(2), "inherited-pipe teardown exceeded its deadline");
            await Task.Delay(700);
            True(!File.Exists(inheritedAlive), "owned inherited-handle descendant survived teardown");

            var successAlive = Path.Combine(directory, "success.alive");
            var successPid = Path.Combine(directory, "success.pid");
            _ = await Adapter(TimeSpan.FromSeconds(2)).ExecuteJsonAsync(
                "__ctx_sdk_helper",
                ["success-child", successAlive, successPid]);
            await Task.Delay(700);
            True(!File.Exists(successAlive), "successful scoped command left a silent child alive");
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    private static async Task<int> RunProcessHelper(string[] args)
    {
        switch (args[0])
        {
            case "dual":
                var stdout = Console.OpenStandardOutput();
                var stderr = Console.OpenStandardError();
                var block = Enumerable.Repeat((byte)' ', 8192).ToArray();
                await Task.WhenAll(
                    Task.Run(async () =>
                    {
                        for (var index = 0; index < 30; index++) await stdout.WriteAsync(block);
                    }),
                    Task.Run(async () =>
                    {
                        for (var index = 0; index < 30; index++) await stderr.WriteAsync(block);
                    }));
                Console.Write("{}");
                return 0;
            case "stderr-first":
                await Console.OpenStandardError().WriteAsync(new byte[256 * 1024 + 1]);
                await Task.Delay(TimeSpan.FromMinutes(1));
                return 0;
            case "inherit":
            case "success-child":
                var child = Process.Start(ChildProcess(args[0] == "success-child", args[1]))
                    ?? throw new InvalidOperationException("failed to start process fixture child");
                await File.WriteAllTextAsync(args[2], child.Id.ToString());
                if (args[0] == "success-child") Console.Write("{}");
                return 0;
            case "linger":
                if (!OperatingSystem.IsWindows()) IgnoreSignal(15, new IntPtr(1));
                await Task.Delay(500);
                await File.WriteAllTextAsync(args[1], "alive");
                await Task.Delay(TimeSpan.FromMinutes(1));
                return 0;
            default:
                return 97;
        }
    }

    private static ProcessStartInfo ChildProcess(bool detach, string alivePath)
    {
        var executable = Environment.ProcessPath
            ?? throw new InvalidOperationException("test executable path is unavailable");
        var info = new ProcessStartInfo
        {
            FileName = executable,
            UseShellExecute = false,
            RedirectStandardInput = detach,
            RedirectStandardOutput = detach,
            RedirectStandardError = detach
        };
        info.ArgumentList.Add("__ctx_sdk_helper");
        info.ArgumentList.Add("linger");
        info.ArgumentList.Add(alivePath);
        return info;
    }

    [System.Runtime.InteropServices.DllImport("libc", EntryPoint = "signal")]
    private static extern IntPtr IgnoreSignal(int signal, IntPtr handler);

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
                    _ = (await ClientFor(node["search"]).SearchAsync(new SearchOptions { Query = SearchQuery() })).Search.Results;
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

    private static SearchQueryV1 SearchQuery() => new()
    {
        Any = [SearchClause.All("agent history")]
    };

    private static string EmptySearchJson()
    {
        const string limits = "{\"query_bytes\":8192,\"clauses\":32,\"analyzed_tokens_per_clause\":32,\"candidates_per_positive_seed\":1024,\"candidate_rows\":16384,\"retained_candidate_ids\":8192,\"residual_rows\":8192,\"verification_bytes\":16777216,\"verification_lookup_bytes\":16384,\"hydrated_rows\":256,\"hydration_input_bytes\":8388608,\"hydration_input_bytes_per_event\":65536,\"snippet_input_bytes\":8388608,\"returned_text_bytes\":524288,\"serialized_response_bytes\":2097152,\"results\":200,\"elapsed_ms\":1000}";
        const string consumed = "{\"query_bytes\":13,\"clauses\":1,\"analyzed_tokens\":2,\"largest_analyzed_tokens_per_clause\":2,\"largest_positive_seed_candidates\":0,\"candidate_rows\":0,\"retained_candidate_ids\":0,\"residual_rows\":0,\"verification_bytes\":0,\"largest_verification_lookup_bytes\":0,\"hydrated_rows\":0,\"hydration_input_bytes\":0,\"largest_hydration_input_bytes\":0,\"snippet_input_bytes\":0,\"returned_results\":0,\"returned_text_bytes\":0,\"serialized_response_bytes\":0,\"elapsed_ms\":1}";
        return "{\"schema_version\":2,\"query\":{\"version\":\"ctx-search-v1\",\"any\":[{\"all\":\"agent history\"}]},\"query_execution\":{\"query_version\":\"ctx-search-v1\",\"candidate_strategy\":\"bounded_fts\",\"resolved\":" + limits + ",\"consumed\":" + consumed + ",\"semantic\":{\"attempted\":false,\"required\":false,\"readiness\":\"unavailable\",\"effective_backend\":\"lexical\",\"requested_candidates\":0,\"eligible_candidates\":0,\"candidates_supplied\":0,\"candidates_consumed\":0,\"candidates_used\":0,\"coverage\":{},\"completeness\":\"not_attempted\",\"positive_text_rule_version\":\"ctx-search-positive-text-v1\"},\"rrf_k\":60,\"per_branch_candidate_rows\":0,\"requested_result_limit\":20,\"result_limit\":20,\"max_result_limit\":200,\"clauses_executed\":1,\"verification_dropped\":0,\"filter_verification_dropped\":0,\"candidate_budget_exhausted\":false,\"timed_out\":false,\"truncated\":false},\"retrieval\":{\"requested_mode\":\"hybrid\",\"effective_mode\":\"lexical\",\"semantic_status\":\"unavailable\",\"semantic_weight\":0.25,\"semanticWeight\":0.5,\"semantic_fallback_code\":\"old\",\"semanticFallbackCode\":\"old\",\"semantic_fallback\":\"old\",\"semanticFallback\":\"old\"},\"results\":[]}";
    }

    private static JsonObject EmptySearchObject() => JsonNode.Parse(EmptySearchJson())!.AsObject();

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

    private static async Task<T> ThrowsAsync<T>(Func<Task> action) where T : Exception
    {
        try
        {
            await action();
        }
        catch (T error)
        {
            return error;
        }
        throw new InvalidOperationException($"expected {typeof(T).Name}");
    }

    private static void Throws<T>(Action action) where T : Exception
    {
        try
        {
            action();
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
