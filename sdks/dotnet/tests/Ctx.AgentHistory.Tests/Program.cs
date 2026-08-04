using System.Diagnostics;
using System.Text;
using System.Text.Json.Nodes;
using Ctx.AgentHistory;

internal static class Program
{
    private const string RawResponseEnvironment = "CTX_AGENT_HISTORY_TEST_RAW_RESPONSE";
    private const string RawResponseHexEnvironment = "CTX_AGENT_HISTORY_TEST_RAW_RESPONSE_HEX";
    private const string ProcessFixtureMode = "CTX_MCP289_DOTNET_PROCESS_FIXTURE_MODE";
    private const string ProcessFixtureRootPid = "CTX_MCP289_DOTNET_PROCESS_FIXTURE_ROOT_PID";
    private const string ProcessFixtureDescendantPid = "CTX_MCP289_DOTNET_PROCESS_FIXTURE_DESCENDANT_PID";
    private const int RetainedStdoutLimit = 64 * 1024 * 1024;
    private const int RetainedStderrLimit = 16 * 1024 * 1024;
    private const int LargeValidPayloadSize = 17 * 1024 * 1024;

    private static async Task<int> Main(string[] args)
    {
        if (Environment.GetEnvironmentVariable(ProcessFixtureMode) is { } processFixture)
        {
            return await RunMcp289ProcessFixture(processFixture);
        }
        if (Environment.GetEnvironmentVariable(RawResponseHexEnvironment) is { } rawResponseHex)
        {
            await Console.OpenStandardOutput().WriteAsync(Convert.FromHexString(rawResponseHex));
            return 0;
        }
        if (Environment.GetEnvironmentVariable(RawResponseEnvironment) is { } rawResponse)
        {
            Console.Write(rawResponse);
            return 0;
        }

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
            ("preserves missing CLI launch errors", PreservesMissingCliLaunchError),
            ("wraps unsupported local platforms as SDK errors", WrapsUnsupportedLocalPlatformError),
            ("accepts valid CLI JSON above the legacy 16 MiB cap", DrainsLargeOutputWithinBound),
            ("bounds retained local CLI output while continuing to drain", BoundsOutputWhileDraining),
            ("rejects oversize successful output as a protocol failure", RejectsOversizeSuccessfulOutput),
            ("owns and force-cleans ignored process-tree pipe owners", OwnsAndCleansProcessTree),
            ("applies the deadline to EOF from a detached pipe owner", CleansDetachedPipeOwnerAtDeadline),
            ("cleans closed-pipe descendants after successful root exit", CleansClosedPipeDescendantAfterSuccess),
            ("normalizes setup init status", NormalizesSetupInitStatus),
            ("enforces the exact status counter domain", EnforcesExactStatusCounterDomain),
            ("builds search flags", BuildsSearchFlags),
            ("keeps search content scope values closed", SearchContentScopeValuesAreClosed),
            ("forwards search content scope once", SearchForwardsContentScopeOnce),
            ("rejects conflicting search content filters before transport", RejectsContentScopeEventTypeConflictBeforeTransport),
            ("camelizes search retrieval json", CamelizesSearchRetrievalJson),
            ("rejects search without intent", RejectsSearchWithoutIntent),
            ("wraps show commands", WrapsShow),
            ("exposes optional MCP tool-call metadata", ExposesOptionalMcpToolCallMetadata),
            ("rejects raw MCP tool-call duplicate members", RejectsRawMcpToolCallDuplicateMembers),
            ("strictly decodes spawned stdout UTF-8", StrictlyDecodesSpawnedStdoutUtf8),
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

    private static async Task<int> RunMcp289ProcessFixture(string mode)
    {
        switch (mode)
        {
            case "large-stdout":
                var largeStdout = Console.OpenStandardOutput();
                await largeStdout.WriteAsync("{\"payload\":\""u8.ToArray());
                await WriteRepeatedAsync(largeStdout, (byte)'d', LargeValidPayloadSize);
                await largeStdout.WriteAsync("\"}"u8.ToArray());
                return 0;
            case "oversize-nonzero":
                await WriteRepeatedAsync(Console.OpenStandardOutput(), (byte)'o', RetainedStdoutLimit + 64 * 1024);
                await WriteRepeatedAsync(Console.OpenStandardError(), (byte)'e', RetainedStderrLimit + 64 * 1024);
                return 23;
            case "oversize-success":
                var oversizeStdout = Console.OpenStandardOutput();
                await oversizeStdout.WriteAsync("{\"payload\":\""u8.ToArray());
                await WriteRepeatedAsync(oversizeStdout, (byte)'s', RetainedStdoutLimit + 64 * 1024);
                await oversizeStdout.WriteAsync("\"}"u8.ToArray());
                return 0;
            case "pipe-owner-tree":
                return await RunPipeOwnerTreeFixture();
            case "detached-pipe-owner":
                return await RunDetachedPipeOwnerFixture();
            case "closed-pipe-descendant":
                return await RunClosedPipeDescendantFixture();
            case "pipe-owner-descendant":
                File.WriteAllText(RequiredFixturePath(ProcessFixtureDescendantPid), Environment.ProcessId.ToString());
                await Task.Delay(Timeout.InfiniteTimeSpan);
                return 0;
            default:
                return 64;
        }
    }

    private static async Task<int> RunPipeOwnerTreeFixture()
    {
        File.WriteAllText(RequiredFixturePath(ProcessFixtureRootPid), Environment.ProcessId.ToString());
        _ = StartFixtureDescendant(redirectOutput: false);
        await Task.Delay(Timeout.InfiniteTimeSpan);
        return 0;
    }

    private static async Task<int> RunDetachedPipeOwnerFixture()
    {
        File.WriteAllText(RequiredFixturePath(ProcessFixtureRootPid), Environment.ProcessId.ToString());
        _ = StartFixtureDescendant(redirectOutput: false);
        await WaitForFixtureFile(RequiredFixturePath(ProcessFixtureDescendantPid));
        await Task.Delay(250);
        return 0;
    }

    private static async Task<int> RunClosedPipeDescendantFixture()
    {
        File.WriteAllText(RequiredFixturePath(ProcessFixtureRootPid), Environment.ProcessId.ToString());
        _ = StartFixtureDescendant(redirectOutput: true);
        await WaitForFixtureFile(RequiredFixturePath(ProcessFixtureDescendantPid));
        Console.Write("{\"completed\":true}");
        return 0;
    }

    private static Process StartFixtureDescendant(bool redirectOutput)
    {
        var child = new ProcessStartInfo
        {
            FileName = TestExecutable(),
            RedirectStandardOutput = redirectOutput,
            RedirectStandardError = redirectOutput,
            UseShellExecute = false
        };
        child.Environment[ProcessFixtureMode] = "pipe-owner-descendant";
        child.Environment[ProcessFixtureDescendantPid] = RequiredFixturePath(ProcessFixtureDescendantPid);
        return Process.Start(child)
            ?? throw new InvalidOperationException("failed to start fixture descendant");
    }

    private static async Task WaitForFixtureFile(string path)
    {
        var deadline = Stopwatch.StartNew();
        while (deadline.Elapsed < TimeSpan.FromSeconds(2))
        {
            if (File.Exists(path) && new FileInfo(path).Length > 0)
            {
                return;
            }
            await Task.Delay(10);
        }
        throw new InvalidOperationException($"fixture PID file was not written: {path}");
    }

    private static async Task WriteRepeatedAsync(Stream stream, byte value, int count)
    {
        var chunk = Enumerable.Repeat(value, 8 * 1024).ToArray();
        while (count > 0)
        {
            var length = Math.Min(count, chunk.Length);
            await stream.WriteAsync(chunk.AsMemory(0, length));
            count -= length;
        }
        await stream.FlushAsync();
    }

    private static string RequiredFixturePath(string name)
    {
        return Environment.GetEnvironmentVariable(name)
            ?? throw new InvalidOperationException($"missing process fixture path {name}");
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

    private static async Task PreservesMissingCliLaunchError()
    {
        var missing = Path.Combine(Path.GetTempPath(), $"ctx-missing-{Guid.NewGuid():N}");
        var adapter = new LocalCliAdapter(new LocalAgentHistoryConfig { CtxBinary = missing });
        try
        {
            await adapter.ExecuteJsonAsync("status", ["status"]);
            throw new InvalidOperationException("expected a missing CLI launch to fail");
        }
        catch (CtxAgentHistoryCliException ex)
        {
            Equal("failed to execute ctx CLI", ex.Message);
            Equal("adapter_error", ex.Code);
            Equal(-1, ex.ExitCode);
            Equal(missing, ex.Command[0]);
        }
    }

    private static async Task WrapsUnsupportedLocalPlatformError()
    {
        if (OperatingSystem.IsWindows() || OperatingSystem.IsLinux())
        {
            return;
        }

        var adapter = new LocalCliAdapter(new LocalAgentHistoryConfig
        {
            CtxBinary = "ctx-should-not-spawn"
        });
        try
        {
            await adapter.ExecuteJsonAsync("status", ["status"]);
            throw new InvalidOperationException("expected the unsupported platform to fail closed");
        }
        catch (CtxAgentHistoryCliException ex)
        {
            Equal("failed to execute ctx CLI", ex.Message);
            Equal("adapter_error", ex.Code);
            Equal(-1, ex.ExitCode);
            True(ex.InnerException is PlatformNotSupportedException,
                "unsupported platform cause was not preserved");
        }
    }

    private static async Task DrainsLargeOutputWithinBound()
    {
        var adapter = ProcessFixtureAdapter("large-stdout", TimeSpan.FromSeconds(15));

        var output = await adapter.ExecuteJsonAsync("status", ["status"]);

        Equal(LargeValidPayloadSize, output["payload"]!.GetValue<string>().Length);
    }

    private static async Task BoundsOutputWhileDraining()
    {
        var adapter = ProcessFixtureAdapter("oversize-nonzero", TimeSpan.FromSeconds(20));
        var started = Stopwatch.StartNew();

        try
        {
            await adapter.ExecuteJsonAsync("status", ["status"]);
            throw new InvalidOperationException("expected oversized nonzero fixture to fail");
        }
        catch (CtxAgentHistoryCliException ex)
        {
            Equal(23, ex.ExitCode);
            Equal(RetainedStdoutLimit, Encoding.UTF8.GetByteCount(ex.Stdout));
            Equal(RetainedStderrLimit, Encoding.UTF8.GetByteCount(ex.Stderr));
        }
        True(started.Elapsed < TimeSpan.FromSeconds(18), $"oversize output drain took {started.Elapsed}");
    }

    private static async Task RejectsOversizeSuccessfulOutput()
    {
        var adapter = ProcessFixtureAdapter("oversize-success", TimeSpan.FromSeconds(20));
        try
        {
            await adapter.ExecuteJsonAsync("status", ["status"]);
            throw new InvalidOperationException("expected oversized successful output to fail closed");
        }
        catch (CtxAgentHistoryProtocolException ex)
        {
            Equal("decode_error", ex.Code);
            Equal(RetainedStdoutLimit, ex.Details["maximumBytes"]!.GetValue<int>());
        }
    }

    private static async Task OwnsAndCleansProcessTree()
    {
        var rootPidPath = Path.Combine(Path.GetTempPath(), $"ctx-mcp289-dotnet-root-{Guid.NewGuid():N}.pid");
        var descendantPidPath = Path.Combine(Path.GetTempPath(), $"ctx-mcp289-dotnet-descendant-{Guid.NewGuid():N}.pid");
        var adapter = ProcessFixtureAdapter(
            "pipe-owner-tree",
            TimeSpan.FromMilliseconds(600),
            new Dictionary<string, string?>
            {
                [ProcessFixtureRootPid] = rootPidPath,
                [ProcessFixtureDescendantPid] = descendantPidPath
            });
        var started = Stopwatch.StartNew();
        try
        {
            try
            {
                await adapter.ExecuteJsonAsync("status", ["status"]);
                throw new InvalidOperationException("expected pipe-owning process tree to time out");
            }
            catch (CtxAgentHistoryCliException ex)
            {
                Equal("timeout", ex.Code);
                Equal(true, ex.Retryable);
            }

            True(started.Elapsed < TimeSpan.FromSeconds(4), $"absolute process/EOF deadline took {started.Elapsed}");
            var rootPid = ReadFixturePid(rootPidPath);
            var descendantPid = ReadFixturePid(descendantPidPath);
            await AssertProcessStops(rootPid, "fixture root");
            await AssertProcessStops(descendantPid, "pipe-owning descendant");
        }
        finally
        {
            ForceStopFixture(rootPidPath);
            ForceStopFixture(descendantPidPath);
            File.Delete(rootPidPath);
            File.Delete(descendantPidPath);
        }
    }

    private static async Task CleansDetachedPipeOwnerAtDeadline()
    {
        var rootPidPath = Path.Combine(Path.GetTempPath(), $"ctx-mcp289-dotnet-detached-root-{Guid.NewGuid():N}.pid");
        var descendantPidPath = Path.Combine(Path.GetTempPath(), $"ctx-mcp289-dotnet-detached-descendant-{Guid.NewGuid():N}.pid");
        var adapter = ProcessFixtureAdapter(
            "detached-pipe-owner",
            TimeSpan.FromMilliseconds(900),
            new Dictionary<string, string?>
            {
                [ProcessFixtureRootPid] = rootPidPath,
                [ProcessFixtureDescendantPid] = descendantPidPath
            });
        var started = Stopwatch.StartNew();
        try
        {
            try
            {
                await adapter.ExecuteJsonAsync("status", ["status"]);
                throw new InvalidOperationException("expected detached pipe owner to exhaust the EOF deadline");
            }
            catch (CtxAgentHistoryCliException ex)
            {
                Equal("timeout", ex.Code);
                Equal(true, ex.Retryable);
            }

            True(started.Elapsed < TimeSpan.FromSeconds(4), $"detached EOF deadline took {started.Elapsed}");
            await AssertProcessStops(ReadFixturePid(rootPidPath), "detached fixture root");
            await AssertProcessStops(ReadFixturePid(descendantPidPath), "detached pipe owner");
        }
        finally
        {
            ForceStopFixture(rootPidPath);
            ForceStopFixture(descendantPidPath);
            File.Delete(rootPidPath);
            File.Delete(descendantPidPath);
        }
    }

    private static async Task CleansClosedPipeDescendantAfterSuccess()
    {
        var rootPidPath = Path.Combine(Path.GetTempPath(), $"ctx-mcp289-dotnet-success-root-{Guid.NewGuid():N}.pid");
        var descendantPidPath = Path.Combine(Path.GetTempPath(), $"ctx-mcp289-dotnet-success-descendant-{Guid.NewGuid():N}.pid");
        var adapter = ProcessFixtureAdapter(
            "closed-pipe-descendant",
            TimeSpan.FromSeconds(5),
            new Dictionary<string, string?>
            {
                [ProcessFixtureRootPid] = rootPidPath,
                [ProcessFixtureDescendantPid] = descendantPidPath
            });
        var started = Stopwatch.StartNew();
        try
        {
            var result = await adapter.ExecuteJsonAsync("status", ["status"]);

            Equal(true, result["completed"]!.GetValue<bool>());
            True(started.Elapsed < TimeSpan.FromSeconds(4), $"successful tree cleanup took {started.Elapsed}");
            await AssertProcessStops(ReadFixturePid(rootPidPath), "successful fixture root");
            await AssertProcessStops(ReadFixturePid(descendantPidPath), "closed-pipe descendant");
        }
        finally
        {
            ForceStopFixture(rootPidPath);
            ForceStopFixture(descendantPidPath);
            File.Delete(rootPidPath);
            File.Delete(descendantPidPath);
        }
    }

    private static LocalCliAdapter ProcessFixtureAdapter(
        string mode,
        TimeSpan timeout,
        IReadOnlyDictionary<string, string?>? extraEnvironment = null)
    {
        var environment = new Dictionary<string, string?>
        {
            [ProcessFixtureMode] = mode
        };
        if (extraEnvironment is not null)
        {
            foreach (var pair in extraEnvironment)
            {
                environment[pair.Key] = pair.Value;
            }
        }
        return new LocalCliAdapter(new LocalAgentHistoryConfig
        {
            CtxBinary = TestExecutable(),
            Environment = environment,
            Timeout = timeout
        });
    }

    private static string TestExecutable()
    {
        var executableName = OperatingSystem.IsWindows()
            ? "Ctx.AgentHistory.Tests.exe"
            : "Ctx.AgentHistory.Tests";
        var executable = Path.Combine(AppContext.BaseDirectory, executableName);
        True(File.Exists(executable), $"test helper executable not found: {executable}");
        return executable;
    }

    private static int ReadFixturePid(string path)
    {
        True(File.Exists(path), $"fixture did not write PID file {path}");
        return int.Parse(File.ReadAllText(path).Trim());
    }

    private static async Task AssertProcessStops(int pid, string label)
    {
        var deadline = Stopwatch.StartNew();
        while (deadline.Elapsed < TimeSpan.FromSeconds(2))
        {
            if (!IsProcessAlive(pid))
            {
                return;
            }
            await Task.Delay(10);
        }
        throw new InvalidOperationException($"{label} remained alive: {pid}");
    }

    private static bool IsProcessAlive(int pid)
    {
        try
        {
            using var process = Process.GetProcessById(pid);
            return !process.HasExited;
        }
        catch (ArgumentException)
        {
            return false;
        }
    }

    private static void ForceStopFixture(string pidPath)
    {
        if (!File.Exists(pidPath) || !int.TryParse(File.ReadAllText(pidPath).Trim(), out var pid))
        {
            return;
        }
        try
        {
            using var process = Process.GetProcessById(pid);
            process.Kill(entireProcessTree: true);
            process.WaitForExit(2_000);
        }
        catch (Exception ex) when (ex is ArgumentException or InvalidOperationException or System.ComponentModel.Win32Exception)
        {
            // The process was already gone.
        }
    }

    private static async Task NormalizesSetupInitStatus()
    {
        var transport = new RecordingTransport("""{"schema_version":2,"initialized":true,"data_root":"/tmp/ctx","mode":"ready","indexed_items":9007199254740991,"indexed_sessions":9007199254740991,"indexed_events":9007199254740991,"indexed_sources":9007199254740991,"lexical":{"status":"ready","generation_id":"gen-64"},"refresh":{"status":"ready","generation_id":"gen-64"},"network_required":false}""");
        var client = new AgentHistoryClient(transport);

        var response = await client.InitAsync(new InitOptions());

        Equal("init", response.Operation);
        Equal(true, response.Status.Initialized);
        Equal(true, response.Status.LocalOnly);
        Equal(9007199254740991UL, response.Status.IndexedItems ?? 0UL);
        Equal(9007199254740991UL, response.Status.IndexedSessions ?? 0UL);
        Equal(9007199254740991UL, response.Status.IndexedEvents ?? 0UL);
        Equal(9007199254740991UL, response.Status.IndexedSources ?? 0UL);
    }

    private static async Task EnforcesExactStatusCounterDomain()
    {
        foreach (var rejected in new[] { "9007199254740993", "18446744073709551615" })
        {
            var client = new AgentHistoryClient(new RecordingTransport(
                $$"""{"initialized":true,"indexed_items":{{rejected}}}"""));
            await ThrowsAsync<CtxAgentHistoryProtocolException>(() => client.StatusAsync());
        }
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

    private static async Task SearchContentScopeValuesAreClosed()
    {
        Equal(4, Enum.GetValues<SearchContentScope>().Length);

        var transport = new RecordingTransport("""{"schema_version":1,"results":[]}""");
        var client = new AgentHistoryClient(transport);
        await ThrowsAsync<CtxAgentHistoryValidationException>(() => client.SearchAsync(new SearchOptions
        {
            Query = "agent history",
            ContentScope = (SearchContentScope)int.MaxValue
        }));
        Equal(0, transport.Calls.Count);
    }

    private static async Task SearchForwardsContentScopeOnce()
    {
        var transport = new RecordingTransport("""{"schema_version":1,"results":[]}""");
        var client = new AgentHistoryClient(transport);
        var cases = new[]
        {
            (SearchContentScope.All, "all"),
            (SearchContentScope.Transcript, "transcript"),
            (SearchContentScope.Calls, "calls"),
            (SearchContentScope.Outputs, "outputs"),
        };

        foreach (var (scope, wireName) in cases)
        {
            await client.SearchAsync(new SearchOptions
            {
                Query = "agent history",
                ContentScope = scope
            });
            var call = transport.Calls[^1];
            Equal(1, call.Count(arg => arg == "--content-scope"));
            var flagIndex = call.ToList().IndexOf("--content-scope");
            Equal(wireName, call[flagIndex + 1]);
        }
    }

    private static async Task RejectsContentScopeEventTypeConflictBeforeTransport()
    {
        var transport = new RecordingTransport("""{"schema_version":1,"results":[]}""");
        var client = new AgentHistoryClient(transport);

        await ThrowsAsync<CtxAgentHistoryValidationException>(() => client.SearchAsync(new SearchOptions
        {
            Query = "agent history",
            EventType = "message",
            ContentScope = SearchContentScope.All
        }));
        Equal(0, transport.Calls.Count);
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

        Equal("show event event-1 --format json --window 2", Join(transport.Calls[0]));
        Equal("show session session-1 --mode full --format json", Join(transport.Calls[1]));
        Equal("show session --provider codex --provider-session provider-session --mode lite --format json", Join(transport.Calls[2]));

        await ThrowsAsync<CtxAgentHistoryValidationException>(() => client.ShowEventAsync(""));
    }

    private static async Task ExposesOptionalMcpToolCallMetadata()
    {
        var fixtures = FindFixtures();
        var oldFixture = JsonNode.Parse(
            File.ReadAllText(Path.Combine(fixtures, "show-event.window.json")))!.AsObject();
        var oldResponse = await ClientFor(oldFixture["event"]).ShowEventAsync("event-1");
        var oldEvent = oldResponse.Event.Event
            ?? throw new InvalidOperationException("legacy selected event did not decode");
        Equal<McpToolCall?>(null, oldEvent.McpToolCall);
        True(!oldEvent.ToJsonObject().ContainsKey("mcpToolCall"), "absent MCP metadata was serialized");

        var newFixture = JsonNode.Parse(
            File.ReadAllText(Path.Combine(fixtures, "show-event.mcp-tool-call.json")))!.AsObject();
        var fixturePayload = newFixture["event"]
            ?? throw new InvalidOperationException("fixture omitted event payload");
        var fixtureResponse = await ClientFor(fixturePayload).ShowEventAsync("event-1");
        var newEvent = fixtureResponse.Event.Event
            ?? throw new InvalidOperationException("fixture selected event did not decode");
        var call = newEvent.McpToolCall
            ?? throw new InvalidOperationException("fixture MCP tool call did not decode");
        Equal("mcp-サーバー-🦀", call.Server);
        Equal("検索/工具/🛠️", call.Tool);
        Equal(true, newEvent.ToJsonObject()["futureEventField"]?["preserved"]?.GetValue<bool>() ?? false);

        var normalizedRaw = JsonNode.Parse("""
            {
              "event": {
                  "mcp_tool_call": {
                    "server": "mcp-サーバー-🦀",
                    "tool": "検索/工具/🛠️"
                  },
                  "future_event_field": true
              },
              "events": [{}]
            }
            """)!.AsObject();
        var normalized = (await ClientFor(normalizedRaw).ShowEventAsync("event-1")).Event;
        var normalizedEvent = normalized.Event
            ?? throw new InvalidOperationException("normalized selected event did not decode");
        var normalizedCall = normalizedEvent.McpToolCall
            ?? throw new InvalidOperationException("normalized MCP tool call did not decode");
        Equal("mcp-サーバー-🦀", normalizedCall.Server);
        Equal("検索/工具/🛠️", normalizedCall.Tool);
        Equal(true, normalizedEvent.ToJsonObject()["futureEventField"]?.GetValue<bool>() ?? false);
        Equal<McpToolCall?>(null, normalized.Events[0].McpToolCall);

        var exact = new JsonObject
        {
            ["event"] = new JsonObject
            {
                ["mcp_tool_call"] = new JsonObject
                {
                    ["server"] = " ",
                    ["tool"] = string.Concat(Enumerable.Repeat("🦀", 16_384)),
                },
            },
            ["events"] = new JsonArray(),
        };
        var exactEvent = (await ClientFor(exact).ShowEventAsync("event-1")).Event.Event
            ?? throw new InvalidOperationException("exact-bound event did not decode");
        var exactCall = exactEvent.McpToolCall
            ?? throw new InvalidOperationException("exact-bound MCP tool call did not decode");
        Equal(64 * 1024, Encoding.UTF8.GetByteCount(exactCall.Tool));

        foreach (var invalid in new[]
        {
            "{\"event\":{\"mcp_tool_call\":{\"server\":\"only-server\"}},\"events\":[]}",
            "{\"event\":{\"mcp_tool_call\":{\"tool\":\"only-tool\"}},\"events\":[]}",
            "{\"event\":{\"mcp_tool_call\":{\"server\":\"server\",\"tool\":\"tool\",\"future\":true}},\"events\":[]}",
            "{\"event\":{\"mcp_tool_call\":{\"server\":\"\",\"tool\":\"tool\"}},\"events\":[]}",
            "{\"event\":{\"mcp_tool_call\":{\"server\":\"server\",\"tool\":7}},\"events\":[]}",
            "{\"event\":{\"mcp_tool_call\":null},\"events\":[]}",
            "{\"event\":{\"mcp_tool_call\":{\"server\":\"server\",\"tool\":\"" + new string('a', 64 * 1024 + 1) + "\"}},\"events\":[]}"
        })
        {
            await ThrowsAsync<CtxAgentHistoryProtocolException>(
                () => ClientFor(JsonNode.Parse(invalid)).ShowEventAsync("event-1"));
        }
    }

    private static async Task RejectsRawMcpToolCallDuplicateMembers()
    {
        var fixtureDirectory = Path.Combine(FindFixtures(), "adversarial");
        foreach (var name in new[]
        {
            "duplicate-event-mcp-tool-call-snake.json",
            "duplicate-event-mcp-tool-call-camel.json",
            "duplicate-mcp-tool-call-server.json",
            "duplicate-mcp-tool-call-tool.json",
            "invalid-mcp-tool-call-transformed-server.json",
            "invalid-mcp-tool-call-transformed-tool.json",
            "invalid-mcp-tool-call-transformed-collision.json",
            "invalid-mcp-tool-call-outer-alias-collision.json",
            "invalid-mcp-tool-call-outer-mixed-case.json",
            "invalid-mcp-tool-call-outer-repeated-separator.json",
            "invalid-mcp-tool-call-outer-trailing-separator.json",
            "invalid-mcp-tool-call-outer-camel-snake.json"
        })
        {
            var raw = File.ReadAllText(Path.Combine(fixtureDirectory, name));
            await ThrowsAsync<CtxAgentHistoryProtocolException>(
                () => LocalClientForRaw(raw).ShowEventAsync("event-1"));
        }

        var repeated = File.ReadAllText(
            Path.Combine(fixtureDirectory, "valid-repeated-string-contents.json"));
        var response = await LocalClientForRaw(repeated).ShowEventAsync("event-1");
        Equal("server server", response.Event.Event?.McpToolCall?.Server ?? "");
        Equal("tool tool", response.Event.Event?.McpToolCall?.Tool ?? "");

        var aliases = File.ReadAllText(
            Path.Combine(fixtureDirectory, "valid-mcp-tool-call-outer-aliases.json"));
        var aliasResponse = await LocalClientForRaw(aliases).ShowEventAsync("event-1");
        Equal("snake-server", aliasResponse.Event.Event?.McpToolCall?.Server ?? "");
        Equal("snake-extra", aliasResponse.Event.Event?.ToJsonObject()["futureEventField"]?.GetValue<string>() ?? "");
        Equal("camel-server", aliasResponse.Event.Events[0].McpToolCall?.Server ?? "");
        Equal("camel-extra", aliasResponse.Event.Events[0].ToJsonObject()["futureEventField"]?.GetValue<string>() ?? "");
    }

    private static async Task StrictlyDecodesSpawnedStdoutUtf8()
    {
        var prefix = Encoding.UTF8.GetBytes("{\"event\":{\"mcp_tool_call\":{\"server\":\"");
        var suffix = Encoding.UTF8.GetBytes("\",\"tool\":\"tool\"}},\"events\":[]}");
        var invalid = new byte[prefix.Length + 1 + suffix.Length];
        prefix.CopyTo(invalid, 0);
        invalid[prefix.Length] = 0xff;
        suffix.CopyTo(invalid, prefix.Length + 1);
        await ThrowsAsync<CtxAgentHistoryProtocolException>(
            () => LocalClientForRawBytes(invalid).ShowEventAsync("event-1"));

        const string valid = "{\"event\":{\"mcp_tool_call\":{\"server\":\"�\",\"tool\":\"tool\"}},\"events\":[]}";
        var response = await LocalClientForRawBytes(Encoding.UTF8.GetBytes(valid)).ShowEventAsync("event-1");
        Equal("�", response.Event.Event?.McpToolCall?.Server ?? "");
    }

    private static AgentHistoryClient LocalClientForRaw(string raw)
    {
        var executableName = OperatingSystem.IsWindows()
            ? "Ctx.AgentHistory.Tests.exe"
            : "Ctx.AgentHistory.Tests";
        var executable = Path.Combine(AppContext.BaseDirectory, executableName);
        True(File.Exists(executable), $"test helper executable not found: {executable}");
        return AgentHistoryClient.Local(new LocalAgentHistoryConfig
        {
            CtxBinary = executable,
            Environment = new Dictionary<string, string?>
            {
                [RawResponseEnvironment] = raw
            }
        });
    }

    private static AgentHistoryClient LocalClientForRawBytes(byte[] raw)
    {
        var executableName = OperatingSystem.IsWindows()
            ? "Ctx.AgentHistory.Tests.exe"
            : "Ctx.AgentHistory.Tests";
        var executable = Path.Combine(AppContext.BaseDirectory, executableName);
        True(File.Exists(executable), $"test helper executable not found: {executable}");
        return AgentHistoryClient.Local(new LocalAgentHistoryConfig
        {
            CtxBinary = executable,
            Environment = new Dictionary<string, string?>
            {
                [RawResponseHexEnvironment] = Convert.ToHexString(raw)
            }
        });
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
