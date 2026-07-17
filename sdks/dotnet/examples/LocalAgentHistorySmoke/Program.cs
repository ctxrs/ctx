using System.Text.Json.Nodes;
using Ctx.AgentHistory;

const string EventId = "11111111-1111-4111-8111-111111111111";
const string SessionId = "22222222-2222-4222-8222-222222222222";

var client = CreateClient();

var status = await client.StatusAsync();
var initialized = await client.InitAsync(new InitOptions { CatalogOnly = true });
var imported = await client.ImportHistoryAsync(new ImportOptions { Provider = "codex", Resume = true });
var synced = await client.SyncAsync(new ImportOptions { All = true });
var search = await client.SearchAsync(new SearchOptions
{
    Query = new SearchQueryV1
    {
        Any = [SearchClause.All("local agent history")],
        Must = [SearchClause.All("codex")]
    },
    Provider = "codex",
    Limit = 5,
    Refresh = "off"
});
var showEvent = await client.ShowEventAsync(FirstEventId(search) ?? EventId, new ShowEventOptions { Window = 1 });
var showSession = await client.ShowSessionAsync(FirstSessionId(search) ?? SessionId, new ShowSessionOptions { Mode = "lite" });
var locateEvent = await client.LocateEventAsync(showEvent.Event.Event?.CtxEventId ?? EventId);
var locateSession = await client.LocateSessionAsync(showSession.Session.Session?.CtxSessionId ?? SessionId);

var output = new JsonObject
{
    ["status"] = status.ToJsonObject(),
    ["init"] = initialized.ToJsonObject(),
    ["import"] = imported.ToJsonObject(),
    ["sync"] = synced.ToJsonObject(),
    ["search"] = search.ToJsonObject(),
    ["showEvent"] = showEvent.ToJsonObject(),
    ["showSession"] = showSession.ToJsonObject(),
    ["locateEvent"] = locateEvent.ToJsonObject(),
    ["locateSession"] = locateSession.ToJsonObject()
};

Console.WriteLine(output.ToJsonString(new System.Text.Json.JsonSerializerOptions { WriteIndented = true }));

static AgentHistoryClient CreateClient()
{
    var ctxPath = Environment.GetEnvironmentVariable("CTX_AGENT_HISTORY_CTX");
    if (!string.IsNullOrWhiteSpace(ctxPath))
    {
        return AgentHistoryClient.Local(new LocalAgentHistoryConfig
        {
            CtxBinary = ctxPath,
            DataRoot = Environment.GetEnvironmentVariable("CTX_AGENT_HISTORY_DATA_ROOT") ?? "/tmp/ctx-agent-history-dotnet-smoke",
            Timeout = TimeSpan.FromSeconds(30)
        });
    }

    return new AgentHistoryClient(new FakeAgentHistoryTransport());
}

static string? FirstEventId(SearchResponse search)
{
    return search.Search.Results.FirstOrDefault(static hit => !string.IsNullOrWhiteSpace(hit.CtxEventId))?.CtxEventId;
}

static string? FirstSessionId(SearchResponse search)
{
    return search.Search.Results.FirstOrDefault(static hit => !string.IsNullOrWhiteSpace(hit.CtxSessionId))?.CtxSessionId;
}

internal sealed class FakeAgentHistoryTransport : IAgentHistoryTransport
{
    private const string DataRoot = "/tmp/ctx-agent-history-dotnet-smoke";

    public string Name => "fake-local";

    public JsonObject Backend(JsonObject? raw = null)
    {
        return new JsonObject
        {
            ["kind"] = "local",
            ["dataRoot"] = DataRoot
        };
    }

    public Task<JsonObject> ExecuteJsonAsync(
        string operation,
        IReadOnlyList<string> args,
        CancellationToken cancellationToken = default)
    {
        return Task.FromResult(operation switch
        {
            "status" => Status(),
            "init" => Init(),
            "import" => Import(),
            "sync" => Import(),
            "search" => Search(),
            "showEvent" => ShowEvent(args.Count > 2 ? args[2] : EventId),
            "showSession" => ShowSession(args.Count > 2 ? args[2] : SessionId),
            "locateEvent" => LocateEvent(args.Count > 2 ? args[2] : EventId),
            "locateSession" => LocateSession(args.Count > 2 ? args[2] : SessionId),
            _ => throw new CtxAgentHistoryValidationException($"fake transport does not implement {operation}")
        });
    }

    public Task<string?> GetCtxVersionAsync(CancellationToken cancellationToken = default)
    {
        return Task.FromResult<string?>("fake-ctx 0.0.0");
    }

    private static JsonObject Base()
    {
        return new JsonObject { ["schema_version"] = 1 };
    }

    private static JsonObject Status()
    {
        var payload = Base();
        payload["initialized"] = true;
        payload["local_only"] = true;
        payload["data_root"] = DataRoot;
        payload["indexed_items"] = 1;
        payload["indexed_sources"] = 1;
        return payload;
    }

    private static JsonObject Init()
    {
        var payload = Base();
        payload["data_root"] = DataRoot;
        payload["mode"] = "catalog_only";
        payload["indexed_items"] = 1;
        payload["network_required"] = false;
        return payload;
    }

    private static JsonObject Import()
    {
        var payload = Base();
        payload["resume"] = true;
        payload["totals"] = new JsonObject
        {
            ["imported_sources"] = 1,
            ["imported_sessions"] = 1,
            ["imported_events"] = 1
        };
        payload["sources"] = new JsonArray
        {
            new JsonObject
            {
                ["provider"] = "codex",
                ["path"] = $"{DataRoot}/session.jsonl",
                ["status"] = "imported",
                ["imported_sessions"] = 1,
                ["imported_events"] = 1
            }
        };
        return payload;
    }

    private static JsonObject Search()
    {
        var payload = new JsonObject { ["schema_version"] = 2 };
        payload["query"] = new SearchQueryV1
        {
            Any = [SearchClause.All("local agent history")],
            Must = [SearchClause.All("codex")]
        }.ToJsonObject();
        payload["query_execution"] = QueryExecution();
        payload["filters"] = new JsonObject { ["provider"] = "codex" };
        payload["freshness"] = new JsonObject { ["mode"] = "off", ["status"] = "skipped" };
        payload["results"] = new JsonArray
        {
            new JsonObject
            {
                ["ctx_event_id"] = EventId,
                ["ctx_session_id"] = SessionId,
                ["provider_session_id"] = "codex-fixture-session",
                ["event_seq"] = 1,
                ["result_scope"] = "event",
                ["provider"] = "codex",
                ["snippet"] = "local agent history smoke result",
                ["source_path"] = $"{DataRoot}/session.jsonl",
                ["source_exists"] = true
            }
        };
        return payload;
    }

    private static JsonObject QueryExecution()
    {
        var limits = JsonNode.Parse("{\"query_bytes\":8192,\"clauses\":32,\"analyzed_tokens_per_clause\":32,\"candidates_per_positive_seed\":1024,\"candidate_rows\":16384,\"retained_candidate_ids\":8192,\"residual_rows\":8192,\"verification_bytes\":16777216,\"verification_lookup_bytes\":16384,\"hydrated_rows\":256,\"hydration_input_bytes\":8388608,\"hydration_input_bytes_per_event\":65536,\"snippet_input_bytes\":8388608,\"returned_text_bytes\":524288,\"serialized_response_bytes\":2097152,\"results\":200,\"elapsed_ms\":1000}")!;
        var consumed = JsonNode.Parse("{\"query_bytes\":24,\"clauses\":2,\"analyzed_tokens\":4,\"largest_analyzed_tokens_per_clause\":3,\"largest_positive_seed_candidates\":1,\"candidate_rows\":1,\"retained_candidate_ids\":1,\"residual_rows\":1,\"verification_bytes\":16,\"largest_verification_lookup_bytes\":16,\"hydrated_rows\":1,\"hydration_input_bytes\":32,\"largest_hydration_input_bytes\":32,\"snippet_input_bytes\":32,\"returned_results\":1,\"returned_text_bytes\":32,\"serialized_response_bytes\":1000,\"elapsed_ms\":2}")!;
        return new JsonObject
        {
            ["query_version"] = "ctx-search-v1", ["candidate_strategy"] = "bounded_fts",
            ["resolved"] = limits, ["consumed"] = consumed,
            ["semantic"] = new JsonObject { ["attempted"] = false, ["required"] = false, ["readiness"] = "unavailable", ["effective_backend"] = "lexical", ["requested_candidates"] = 0, ["eligible_candidates"] = 0, ["candidates_supplied"] = 0, ["candidates_consumed"] = 0, ["candidates_used"] = 0, ["coverage"] = new JsonObject(), ["completeness"] = "not_attempted", ["positive_text_rule_version"] = "ctx-search-positive-text-v1" },
            ["rrf_k"] = 60, ["per_branch_candidate_rows"] = 1024, ["requested_result_limit"] = 5,
            ["result_limit"] = 5, ["max_result_limit"] = 200, ["clauses_executed"] = 2,
            ["verification_dropped"] = 0, ["filter_verification_dropped"] = 0,
            ["candidate_budget_exhausted"] = false, ["timed_out"] = false, ["truncated"] = false
        };
    }

    private static JsonObject ShowEvent(string eventId)
    {
        var payload = Base();
        payload["event"] = new JsonObject
        {
            ["ctx_event_id"] = eventId,
            ["ctx_session_id"] = SessionId,
            ["sequence"] = 1,
            ["event_type"] = "message",
            ["role"] = "assistant",
            ["text"] = "local agent history smoke result"
        };
        payload["events"] = new JsonArray();
        payload["source"] = Source();
        return payload;
    }

    private static JsonObject ShowSession(string sessionId)
    {
        var payload = Base();
        payload["ctx_session_id"] = sessionId;
        payload["provider_session_id"] = "codex-fixture-session";
        payload["session"] = new JsonObject
        {
            ["ctx_session_id"] = sessionId,
            ["provider"] = "codex",
            ["title"] = "LocalAgentHistorySmoke fixture"
        };
        payload["events"] = new JsonArray();
        payload["source"] = Source();
        payload["mode"] = "lite";
        payload["format"] = "json";
        return payload;
    }

    private static JsonObject LocateEvent(string eventId)
    {
        var payload = Base();
        payload["ctx_session_id"] = SessionId;
        payload["ctx_event_id"] = eventId;
        payload["provider"] = "codex";
        payload["provider_session_id"] = "codex-fixture-session";
        payload["source"] = Source();
        payload["resume"] = new JsonObject { ["cursor"] = "line:1" };
        return payload;
    }

    private static JsonObject LocateSession(string sessionId)
    {
        var payload = Base();
        payload["ctx_session_id"] = sessionId;
        payload["provider"] = "codex";
        payload["provider_session_id"] = "codex-fixture-session";
        payload["source"] = Source();
        return payload;
    }

    private static JsonObject Source()
    {
        return new JsonObject
        {
            ["path"] = $"{DataRoot}/session.jsonl",
            ["cursor"] = "line:1",
            ["exists"] = true,
            ["source_format"] = "codex_session_jsonl"
        };
    }
}
