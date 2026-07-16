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

public sealed record LocateEventResponse : AgentHistoryResponse
{
    internal LocateEventResponse(JsonObject envelope)
        : base(envelope)
    {
        Location = LocationResult.FromJson(envelope["location"] as JsonObject);
    }

    public LocationResult Location { get; }
}

public sealed record LocateSessionResponse : AgentHistoryResponse
{
    internal LocateSessionResponse(JsonObject envelope)
        : base(envelope)
    {
        Location = LocationResult.FromJson(envelope["location"] as JsonObject);
    }

    public LocationResult Location { get; }
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
        SchemaVersion = JsonHelpers.GetInt(json, "schema_version")
            ?? throw SearchExecutionLimits.Missing("schema_version");
        if (SchemaVersion != 2)
        {
            throw new CtxAgentHistoryProtocolException("unsupported search schema version", new JsonObject { ["schema_version"] = SchemaVersion });
        }
        Query = json["query"] is JsonObject query ? SearchQueryV1.FromJson(query) : null;
        QueryExecution = new SearchQueryExecution(json["query_execution"] as JsonObject
            ?? throw SearchExecutionLimits.Missing("query_execution"));
        Filters = JsonHelpers.CloneObject(json["filters"] as JsonObject);
        Freshness = Ctx.AgentHistory.Freshness.FromJson(json["freshness"] as JsonObject);
        GeneratedAt = JsonHelpers.GetString(json, "generatedAt");
        Retrieval = JsonHelpers.Clone(json["retrieval"]);
        Results = JsonHelpers.GetObjectArray(json, "results", SearchHit.FromJson);
        Pagination = JsonHelpers.CloneObject(json["pagination"] as JsonObject);
        Truncation = JsonHelpers.CloneObject(json["truncation"] as JsonObject);
    }

    public int SchemaVersion { get; }
    public SearchQueryV1? Query { get; }
    public SearchQueryExecution QueryExecution { get; }
    public JsonObject Filters { get; }
    public Freshness? Freshness { get; }
    public string? GeneratedAt { get; }
    public JsonNode? Retrieval { get; }
    public IReadOnlyList<SearchHit> Results { get; }
    public JsonObject Pagination { get; }
    public JsonObject Truncation { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static SearchResult FromJson(JsonObject? json) => new(json ?? new JsonObject());
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
        ResultType = JsonHelpers.GetString(json, "resultType");
        ResultScope = JsonHelpers.GetString(json, "resultScope");
        Provider = JsonHelpers.GetString(json, "provider");
        Timestamp = JsonHelpers.GetString(json, "timestamp");
        Cwd = JsonHelpers.GetString(json, "cwd");
        SourcePath = JsonHelpers.GetString(json, "sourcePath");
        SourceExists = JsonHelpers.GetBool(json, "sourceExists");
        Cursor = JsonHelpers.GetString(json, "cursor");
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
    public string? ResultType { get; }
    public string? ResultScope { get; }
    public string? Provider { get; }
    public string? Timestamp { get; }
    public string? Cwd { get; }
    public string? SourcePath { get; }
    public bool? SourceExists { get; }
    public string? Cursor { get; }
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
        SourcePath = JsonHelpers.GetString(json, "sourcePath");
        SourceExists = JsonHelpers.GetBool(json, "sourceExists");
        Cursor = JsonHelpers.GetString(json, "cursor");
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
    public string? SourcePath { get; }
    public bool? SourceExists { get; }
    public string? Cursor { get; }

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
        Source = SourceLocation.FromJson(json["source"] as JsonObject);
    }

    public AgentHistoryEvent? Event { get; }
    public IReadOnlyList<AgentHistoryEvent> Events { get; }
    public SourceLocation? Source { get; }

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
        Source = SourceLocation.FromJson(json["source"] as JsonObject);
        Mode = JsonHelpers.GetString(json, "mode");
        Format = JsonHelpers.GetString(json, "format");
    }

    public SessionRecord? Session { get; }
    public IReadOnlyList<AgentHistoryEvent> Events { get; }
    public SourceLocation? Source { get; }
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
        Title = JsonHelpers.GetString(json, "title");
        StartedAt = JsonHelpers.GetString(json, "startedAt");
        UpdatedAt = JsonHelpers.GetString(json, "updatedAt");
        Cwd = JsonHelpers.GetString(json, "cwd");
        SourcePath = JsonHelpers.GetString(json, "sourcePath");
        Visibility = JsonHelpers.GetString(json, "visibility");
    }

    public string? CtxSessionId { get; }
    public string? Provider { get; }
    public string? ProviderSessionId { get; }
    public string? Title { get; }
    public string? StartedAt { get; }
    public string? UpdatedAt { get; }
    public string? Cwd { get; }
    public string? SourcePath { get; }
    public string? Visibility { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static SessionRecord? FromJson(JsonObject? json) => json is null ? null : new SessionRecord(json);
}

public sealed record AgentHistoryEvent
{
    private readonly JsonObject _json;

    private AgentHistoryEvent(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        CtxEventId = JsonHelpers.GetString(json, "ctxEventId");
        CtxSessionId = JsonHelpers.GetString(json, "ctxSessionId");
        Sequence = JsonHelpers.GetInt(json, "sequence");
        EventType = JsonHelpers.GetString(json, "eventType");
        Role = JsonHelpers.GetString(json, "role");
        OccurredAt = JsonHelpers.GetString(json, "occurredAt");
        Source = JsonHelpers.Clone(json["source"]);
        Cursor = JsonHelpers.GetString(json, "cursor");
        Text = JsonHelpers.GetString(json, "text");
        Preview = JsonHelpers.GetString(json, "preview");
        Citations = JsonHelpers.GetObjectArray(json, "citations", Citation.FromJson);
    }

    public string? CtxEventId { get; }
    public string? CtxSessionId { get; }
    public int? Sequence { get; }
    public string? EventType { get; }
    public string? Role { get; }
    public string? OccurredAt { get; }
    public JsonNode? Source { get; }
    public string? Cursor { get; }
    public string? Text { get; }
    public string? Preview { get; }
    public IReadOnlyList<Citation> Citations { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static AgentHistoryEvent? FromJson(JsonObject? json) => json is null ? null : new AgentHistoryEvent(json);
    internal static AgentHistoryEvent FromJsonRequired(JsonObject json) => new(json);
}

public sealed record LocationResult
{
    private readonly JsonObject _json;

    private LocationResult(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        CtxSessionId = JsonHelpers.GetString(json, "ctxSessionId");
        CtxEventId = JsonHelpers.GetString(json, "ctxEventId");
        Provider = JsonHelpers.GetString(json, "provider");
        ProviderSessionId = JsonHelpers.GetString(json, "providerSessionId");
        Source = SourceLocation.FromJson(json["source"] as JsonObject);
        Resume = JsonHelpers.CloneObject(json["resume"] as JsonObject);
    }

    public string? CtxSessionId { get; }
    public string? CtxEventId { get; }
    public string? Provider { get; }
    public string? ProviderSessionId { get; }
    public SourceLocation? Source { get; }
    public JsonObject Resume { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static LocationResult FromJson(JsonObject? json) => new(json ?? new JsonObject());
}

public sealed record SourceLocation
{
    private readonly JsonObject _json;

    private SourceLocation(JsonObject json)
    {
        _json = JsonHelpers.CloneObject(json);
        Path = JsonHelpers.GetString(json, "path");
        Cursor = JsonHelpers.GetString(json, "cursor");
        Exists = JsonHelpers.GetBool(json, "exists");
        SourceId = JsonHelpers.GetString(json, "sourceId");
        SourceFormat = JsonHelpers.GetString(json, "sourceFormat");
    }

    public string? Path { get; }
    public string? Cursor { get; }
    public bool? Exists { get; }
    public string? SourceId { get; }
    public string? SourceFormat { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static SourceLocation? FromJson(JsonObject? json) => json is null ? null : new SourceLocation(json);
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
