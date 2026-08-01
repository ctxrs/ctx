using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

/// <summary>Common metadata carried by every agent-history-v1 response envelope.</summary>
public abstract record AgentHistoryResponse
{
    private readonly JsonObject _json;

    protected AgentHistoryResponse(JsonObject envelope)
    {
        _json = JsonHelpers.CloneObject(envelope);
        ContractVersion = JsonHelpers.GetString(envelope, "contractVersion") ?? CtxAgentHistoryVersions.ContractVersion;
        SchemaVersion = JsonHelpers.GetInt(envelope, "schemaVersion") ?? CtxAgentHistoryVersions.SchemaVersion;
        Operation = JsonHelpers.GetString(envelope, "operation") ?? "";
        Backend = AgentHistoryBackend.FromJson(envelope["backend"] as JsonObject);
    }

    public string ContractVersion { get; }
    public int SchemaVersion { get; }
    public string Operation { get; }
    public AgentHistoryBackend Backend { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);
}

public sealed record StatusResponse : AgentHistoryResponse
{
    internal StatusResponse(JsonObject envelope)
        : base(envelope)
    {
        Status = AgentHistoryStatus.FromJson(envelope["status"] as JsonObject);
    }

    public AgentHistoryStatus Status { get; }
}

public sealed record InitResponse : AgentHistoryResponse
{
    internal InitResponse(JsonObject envelope)
        : base(envelope)
    {
        Status = AgentHistoryStatus.FromJson(envelope["status"] as JsonObject);
    }

    public AgentHistoryStatus Status { get; }
}

public sealed record SourcesResponse : AgentHistoryResponse
{
    internal SourcesResponse(JsonObject envelope)
        : base(envelope)
    {
        Sources = ResponseReaders.ReadObjects(envelope["sources"] as JsonArray, ProviderSource.FromJson);
    }

    public IReadOnlyList<ProviderSource> Sources { get; }
}

public sealed record ImportResponse : AgentHistoryResponse
{
    internal ImportResponse(JsonObject envelope)
        : base(envelope)
    {
        Import = ImportResult.FromJson(envelope["import"] as JsonObject);
    }

    public ImportResult Import { get; }
}

public sealed record SearchResponse : AgentHistoryResponse
{
    internal SearchResponse(JsonObject envelope)
        : base(envelope)
    {
        Search = SearchResult.FromJson(envelope["search"] as JsonObject);
    }

    public SearchResult Search { get; }
}

public sealed record ShowEventResponse : AgentHistoryResponse
{
    internal ShowEventResponse(JsonObject envelope)
        : base(envelope)
    {
        Event = EventResult.FromJson(envelope["event"] as JsonObject);
    }

    public EventResult Event { get; }
}

public sealed record ShowSessionResponse : AgentHistoryResponse
{
    internal ShowSessionResponse(JsonObject envelope)
        : base(envelope)
    {
        Session = SessionResult.FromJson(envelope["session"] as JsonObject);
    }

    public SessionResult Session { get; }
}

public sealed record AgentHistoryBackend
{
    private readonly JsonObject _json;

    private AgentHistoryBackend(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        Kind = JsonHelpers.GetString(json, "kind") ?? "";
        DataRoot = JsonHelpers.GetString(json, "dataRoot");
        BaseUrl = JsonHelpers.GetString(json, "baseUrl");
    }

    public string Kind { get; }
    public string? DataRoot { get; }
    public string? BaseUrl { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static AgentHistoryBackend FromJson(JsonObject? json) => new(json ?? new JsonObject());
}

public sealed record AgentHistoryStatus
{
    private readonly JsonObject _json;

    private AgentHistoryStatus(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        Initialized = JsonHelpers.GetBool(json, "initialized") ?? false;
        LocalOnly = JsonHelpers.GetBool(json, "localOnly") ?? true;
        DataRoot = JsonHelpers.GetString(json, "dataRoot");
        IndexedItems = JsonHelpers.GetInt(json, "indexedItems");
        IndexedSources = JsonHelpers.GetInt(json, "indexedSources");
        CatalogedSessions = JsonHelpers.GetInt(json, "catalogedSessions");
        IndexedCatalogSessions = JsonHelpers.GetInt(json, "indexedCatalogSessions");
        PendingCatalogSessions = JsonHelpers.GetInt(json, "pendingCatalogSessions");
        FailedCatalogSessions = JsonHelpers.GetInt(json, "failedCatalogSessions");
        StaleCatalogSessions = JsonHelpers.GetInt(json, "staleCatalogSessions");
        Freshness = Freshness.FromJson(json["freshness"] as JsonObject);
        Semantic = JsonHelpers.CloneObject(json["semantic"] as JsonObject);
        Daemon = JsonHelpers.CloneObject(json["daemon"] as JsonObject);
    }

    public bool Initialized { get; }
    public bool LocalOnly { get; }
    public string? DataRoot { get; }
    public int? IndexedItems { get; }
    public int? IndexedSources { get; }
    public int? CatalogedSessions { get; }
    public int? IndexedCatalogSessions { get; }
    public int? PendingCatalogSessions { get; }
    public int? FailedCatalogSessions { get; }
    public int? StaleCatalogSessions { get; }
    public Freshness? Freshness { get; }
    public JsonObject Semantic { get; }
    public JsonObject Daemon { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static AgentHistoryStatus FromJson(JsonObject? json) => new(json ?? new JsonObject());
}

public sealed record ProviderSource
{
    private readonly JsonObject _json;

    private ProviderSource(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        Provider = JsonHelpers.GetString(json, "provider");
        Path = JsonHelpers.GetString(json, "path");
        Exists = JsonHelpers.GetBool(json, "exists");
        SourceFormat = JsonHelpers.GetString(json, "sourceFormat");
        Status = JsonHelpers.GetString(json, "status");
        ImportSupport = JsonHelpers.GetString(json, "importSupport");
        NativeImport = JsonHelpers.GetBool(json, "nativeImport");
        Importable = JsonHelpers.GetBool(json, "importable");
        UnsupportedReason = JsonHelpers.GetString(json, "unsupportedReason");
    }

    public string? Provider { get; }
    public string? Path { get; }
    public bool? Exists { get; }
    public string? SourceFormat { get; }
    public string? Status { get; }
    public string? ImportSupport { get; }
    public bool? NativeImport { get; }
    public bool? Importable { get; }
    public string? UnsupportedReason { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static ProviderSource FromJson(JsonObject json) => new(json);
}

public sealed record ImportResult
{
    private readonly JsonObject _json;

    private ImportResult(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        Resume = JsonHelpers.GetBool(json, "resume") ?? false;
        ResumeMode = JsonHelpers.GetString(json, "resumeMode");
        Totals = Totals.FromJson(json["totals"] as JsonObject);
        Sources = JsonHelpers.GetObjectArray(json, "sources", ImportSource.FromJson);
    }

    public bool Resume { get; }
    public string? ResumeMode { get; }
    public Totals Totals { get; }
    public IReadOnlyList<ImportSource> Sources { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static ImportResult FromJson(JsonObject? json) => new(json ?? new JsonObject());
}

public sealed record ImportSource
{
    private readonly JsonObject _json;

    private ImportSource(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        Provider = JsonHelpers.GetString(json, "provider");
        Path = JsonHelpers.GetString(json, "path");
        SourceFormat = JsonHelpers.GetString(json, "sourceFormat");
        Status = JsonHelpers.GetString(json, "status");
        ImportedSessions = JsonHelpers.GetInt(json, "importedSessions");
        ImportedEvents = JsonHelpers.GetInt(json, "importedEvents");
        Skipped = JsonHelpers.GetInt(json, "skipped");
        Failed = JsonHelpers.GetInt(json, "failed");
        Error = JsonHelpers.GetString(json, "error");
    }

    public string? Provider { get; }
    public string? Path { get; }
    public string? SourceFormat { get; }
    public string? Status { get; }
    public int? ImportedSessions { get; }
    public int? ImportedEvents { get; }
    public int? Skipped { get; }
    public int? Failed { get; }
    public string? Error { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static ImportSource FromJson(JsonObject json) => new(json);
}

public sealed record Totals
{
    private readonly JsonObject _json;

    private Totals(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        SourceFiles = JsonHelpers.GetInt(json, "sourceFiles");
        SourceBytes = JsonHelpers.GetInt(json, "sourceBytes");
        ImportedSources = JsonHelpers.GetInt(json, "importedSources");
        FailedSources = JsonHelpers.GetInt(json, "failedSources");
        ImportedSessions = JsonHelpers.GetInt(json, "importedSessions");
        ImportedEvents = JsonHelpers.GetInt(json, "importedEvents");
        ImportedEdges = JsonHelpers.GetInt(json, "importedEdges");
        Skipped = JsonHelpers.GetInt(json, "skipped");
        Failed = JsonHelpers.GetInt(json, "failed");
    }

    public int? SourceFiles { get; }
    public int? SourceBytes { get; }
    public int? ImportedSources { get; }
    public int? FailedSources { get; }
    public int? ImportedSessions { get; }
    public int? ImportedEvents { get; }
    public int? ImportedEdges { get; }
    public int? Skipped { get; }
    public int? Failed { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static Totals FromJson(JsonObject? json) => new(json ?? new JsonObject());
}

public sealed record Freshness
{
    private readonly JsonObject _json;

    private Freshness(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        Mode = JsonHelpers.GetString(json, "mode");
        Status = JsonHelpers.GetString(json, "status");
        Reason = JsonHelpers.GetString(json, "reason");
        BudgetReasons = JsonHelpers.GetStringArray(json, "budgetReasons");
        SourceCount = JsonHelpers.GetInt(json, "sourceCount");
        DaemonLastRunAtMs = JsonHelpers.GetLong(json, "daemonLastRunAtMs");
        Totals = json["totals"] is JsonObject totals ? Ctx.AgentHistory.Totals.FromJson(totals) : null;
        Error = JsonHelpers.GetString(json, "error");
    }

    public string? Mode { get; }
    public string? Status { get; }
    public string? Reason { get; }
    public IReadOnlyList<string> BudgetReasons { get; }
    public int? SourceCount { get; }
    public long? DaemonLastRunAtMs { get; }
    public Totals? Totals { get; }
    public string? Error { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static Freshness? FromJson(JsonObject? json) => json is null ? null : new Freshness(json);
}

public sealed record SearchResult
{
    private readonly JsonObject _json;

    private SearchResult(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        Query = JsonHelpers.GetString(json, "query");
        Filters = JsonHelpers.CloneObject(json["filters"] as JsonObject);
        Freshness = Ctx.AgentHistory.Freshness.FromJson(json["freshness"] as JsonObject);
        GeneratedAt = JsonHelpers.GetString(json, "generatedAt");
        Retrieval = JsonHelpers.Clone(json["retrieval"]);
        Results = JsonHelpers.GetObjectArray(json, "results", SearchHit.FromJson);
        ResultWindow = SearchResultWindow.FromJson(json["resultWindow"] as JsonObject);
        Pagination = JsonHelpers.CloneObject(json["pagination"] as JsonObject);
        Truncation = JsonHelpers.CloneObject(json["truncation"] as JsonObject);
    }

    public string? Query { get; }
    public JsonObject Filters { get; }
    public Freshness? Freshness { get; }
    public string? GeneratedAt { get; }
    public JsonNode? Retrieval { get; }
    public IReadOnlyList<SearchHit> Results { get; }
    public SearchResultWindow? ResultWindow { get; }
    public JsonObject Pagination { get; }
    public JsonObject Truncation { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static SearchResult FromJson(JsonObject? json) => new(json ?? new JsonObject());
}

public sealed record SearchResultWindow
{
    private readonly JsonObject _json;

    private SearchResultWindow(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        Limit = JsonHelpers.GetInt(json, "limit") ?? 0;
        Returned = JsonHelpers.GetInt(json, "returned") ?? 0;
        MoreAvailable = JsonHelpers.GetBool(json, "moreAvailable") ?? false;
    }

    public int Limit { get; }
    public int Returned { get; }
    public bool MoreAvailable { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static SearchResultWindow? FromJson(JsonObject? json) =>
        json is null ? null : new SearchResultWindow(json);
}

public sealed record SearchHit
{
    private readonly JsonObject _json;

    private SearchHit(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        CtxEventId = JsonHelpers.GetString(json, "ctxEventId");
        CtxSessionId = JsonHelpers.GetString(json, "ctxSessionId");
        ProviderSessionId = JsonHelpers.GetString(json, "providerSessionId");
        EventSeq = JsonHelpers.GetInt(json, "eventSeq");
        Title = JsonHelpers.GetString(json, "title");
        Snippet = JsonHelpers.GetString(json, "snippet");
        Rank = JsonHelpers.GetDouble(json, "rank");
        RetrievalScore = JsonHelpers.GetDouble(json, "retrievalScore");
        ResultType = JsonHelpers.GetString(json, "resultType");
        ResultScope = JsonHelpers.GetString(json, "resultScope");
        Provider = JsonHelpers.GetString(json, "provider");
        SourceFormat = JsonHelpers.GetString(json, "sourceFormat");
        Timestamp = JsonHelpers.GetString(json, "timestamp");
        Cwd = JsonHelpers.GetString(json, "cwd");
        WhyMatched = JsonHelpers.GetStringArray(json, "whyMatched");
        Citations = JsonHelpers.GetObjectArray(json, "citations", Citation.FromJson);
        SuggestedNextCommands = JsonHelpers.GetStringArray(json, "suggestedNextCommands");
        Visibility = JsonHelpers.GetString(json, "visibility");
    }

    public string? CtxEventId { get; }
    public string? CtxSessionId { get; }
    public string? ProviderSessionId { get; }
    public int? EventSeq { get; }
    public string? Title { get; }
    public string? Snippet { get; }
    public double? Rank { get; }
    public double? RetrievalScore { get; }
    public string? ResultType { get; }
    public string? ResultScope { get; }
    public string? Provider { get; }
    public string? SourceFormat { get; }
    public string? Timestamp { get; }
    public string? Cwd { get; }
    public IReadOnlyList<string> WhyMatched { get; }
    public IReadOnlyList<Citation> Citations { get; }
    public IReadOnlyList<string> SuggestedNextCommands { get; }
    public string? Visibility { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static SearchHit FromJson(JsonObject json) => new(json);
}

public sealed record Citation
{
    private readonly JsonObject _json;

    private Citation(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        ItemId = JsonHelpers.GetString(json, "itemId");
        TargetType = JsonHelpers.GetString(json, "targetType");
        CtxEventId = JsonHelpers.GetString(json, "ctxEventId");
        CtxSessionId = JsonHelpers.GetString(json, "ctxSessionId");
        Label = JsonHelpers.GetString(json, "label");
        Time = JsonHelpers.GetString(json, "time");
        Provider = JsonHelpers.GetString(json, "provider");
        SessionId = JsonHelpers.GetString(json, "sessionId");
        EventSeq = JsonHelpers.GetInt(json, "eventSeq");
    }

    public string? ItemId { get; }
    public string? TargetType { get; }
    public string? CtxEventId { get; }
    public string? CtxSessionId { get; }
    public string? Label { get; }
    public string? Time { get; }
    public string? Provider { get; }
    public string? SessionId { get; }
    public int? EventSeq { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static Citation FromJson(JsonObject json) => new(json);
}

public sealed record EventResult
{
    private readonly JsonObject _json;

    private EventResult(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        Event = AgentHistoryEvent.FromJson(json["event"] as JsonObject);
        Events = JsonHelpers.GetObjectArray(json, "events", AgentHistoryEvent.FromJsonRequired);
    }

    public AgentHistoryEvent? Event { get; }
    public IReadOnlyList<AgentHistoryEvent> Events { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static EventResult FromJson(JsonObject? json) => new(json ?? new JsonObject());
}

public sealed record SessionResult
{
    private readonly JsonObject _json;

    private SessionResult(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        Session = SessionRecord.FromJson(json["session"] as JsonObject);
        Events = JsonHelpers.GetObjectArray(json, "events", AgentHistoryEvent.FromJsonRequired);
        Mode = JsonHelpers.GetString(json, "mode");
        Format = JsonHelpers.GetString(json, "format");
    }

    public SessionRecord? Session { get; }
    public IReadOnlyList<AgentHistoryEvent> Events { get; }
    public string? Mode { get; }
    public string? Format { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static SessionResult FromJson(JsonObject? json) => new(json ?? new JsonObject());
}

public sealed record SessionRecord
{
    private readonly JsonObject _json;

    private SessionRecord(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        CtxSessionId = JsonHelpers.GetString(json, "ctxSessionId");
        Provider = JsonHelpers.GetString(json, "provider");
        ProviderSessionId = JsonHelpers.GetString(json, "providerSessionId");
        SourceFormat = JsonHelpers.GetString(json, "sourceFormat");
        Title = JsonHelpers.GetString(json, "title");
        StartedAt = JsonHelpers.GetString(json, "startedAt");
        UpdatedAt = JsonHelpers.GetString(json, "updatedAt");
        Cwd = JsonHelpers.GetString(json, "cwd");
        Visibility = JsonHelpers.GetString(json, "visibility");
    }

    public string? CtxSessionId { get; }
    public string? Provider { get; }
    public string? ProviderSessionId { get; }
    public string? SourceFormat { get; }
    public string? Title { get; }
    public string? StartedAt { get; }
    public string? UpdatedAt { get; }
    public string? Cwd { get; }
    public string? Visibility { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static SessionRecord? FromJson(JsonObject? json) => json is null ? null : new SessionRecord(json);
}

public enum CoreContentPolicyStatus
{
    Selected,
    Redacted,
    Omitted,
}

public sealed record CoreContentMetadata
{
    private readonly JsonObject _json;

    private CoreContentMetadata(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        Complete = JsonHelpers.GetBool(json, "complete") ?? false;
        PolicyStatus = JsonHelpers.GetString(json, "policyStatus") switch
        {
            "selected" => CoreContentPolicyStatus.Selected,
            "redacted" => CoreContentPolicyStatus.Redacted,
            "omitted" => CoreContentPolicyStatus.Omitted,
            _ => null,
        };
        PolicyReason = JsonHelpers.GetString(json, "policyReason");
    }

    public bool Complete { get; }
    public CoreContentPolicyStatus? PolicyStatus { get; }
    public string? PolicyReason { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static CoreContentMetadata? FromJson(JsonObject? json) =>
        json is null ? null : new CoreContentMetadata(json);
}

public sealed record AgentHistoryEvent
{
    private readonly JsonObject _json;

    private AgentHistoryEvent(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        CtxEventId = JsonHelpers.GetString(json, "ctxEventId");
        CtxSessionId = JsonHelpers.GetString(json, "ctxSessionId");
        Provider = JsonHelpers.GetString(json, "provider");
        ProviderSessionId = JsonHelpers.GetString(json, "providerSessionId");
        SourceFormat = JsonHelpers.GetString(json, "sourceFormat");
        Sequence = JsonHelpers.GetInt(json, "sequence");
        EventType = JsonHelpers.GetString(json, "eventType");
        Role = JsonHelpers.GetString(json, "role");
        OccurredAt = JsonHelpers.GetString(json, "occurredAt");
        Text = JsonHelpers.GetString(json, "text");
        Content = CoreContentMetadata.FromJson(json["content"] as JsonObject);
        Citations = JsonHelpers.GetObjectArray(json, "citations", Citation.FromJson);
    }

    public string? CtxEventId { get; }
    public string? CtxSessionId { get; }
    public string? Provider { get; }
    public string? ProviderSessionId { get; }
    public string? SourceFormat { get; }
    public int? Sequence { get; }
    public string? EventType { get; }
    public string? Role { get; }
    public string? OccurredAt { get; }
    public string? Text { get; }
    public CoreContentMetadata? Content { get; }
    public IReadOnlyList<Citation> Citations { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static AgentHistoryEvent? FromJson(JsonObject? json) => json is null ? null : new AgentHistoryEvent(json);
    internal static AgentHistoryEvent FromJsonRequired(JsonObject json) => new(json);
}

internal static class ResponseReaders
{
    public static IReadOnlyList<T> ReadObjects<T>(JsonArray? array, Func<JsonObject, T> factory)
    {
        if (array is null)
        {
            return Array.Empty<T>();
        }

        var result = new List<T>();
        foreach (var item in array)
        {
            if (item is JsonObject obj)
            {
                result.Add(factory(obj));
            }
        }
        return result;
    }
}
