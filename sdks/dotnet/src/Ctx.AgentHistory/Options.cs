namespace Ctx.AgentHistory;

public sealed record InitOptions
{
    public string? Progress { get; init; } = "none";
}

public sealed record ImportOptions
{
    public bool All { get; init; }
    public string? Provider { get; init; }
    public string? Path { get; init; }
    public bool Resume { get; init; }
    public string? Progress { get; init; } = "none";
}

public enum SearchContentScope
{
    All,
    Transcript,
    Calls,
    Outputs,
}

public sealed record SearchOptions
{
    public string? Query { get; init; }
    public IReadOnlyList<string>? Terms { get; init; }
    public int? Limit { get; init; }
    public string? Backend { get; init; }
    public double? SemanticWeight { get; init; }
    public string? Provider { get; init; }
    public string? Workspace { get; init; }
    public string? Since { get; init; }
    public bool PrimaryOnly { get; init; }
    public bool IncludeSubagents { get; init; }
    public string? EventType { get; init; }
    public SearchContentScope? ContentScope { get; init; }
    public string? File { get; init; }
    public string? Session { get; init; }
    public bool Events { get; init; }
    public string? Refresh { get; init; }
    public bool IncludeCurrentSession { get; init; }
}

public sealed record ShowEventOptions
{
    public int? Before { get; init; }
    public int? After { get; init; }
    public int? Window { get; init; }
}

public record SessionLookupOptions
{
    public string? Id { get; init; }
    public string? Provider { get; init; }
    public string? ProviderSessionId { get; init; }
}

public sealed record ShowSessionOptions : SessionLookupOptions
{
    public string? Mode { get; init; }
}
