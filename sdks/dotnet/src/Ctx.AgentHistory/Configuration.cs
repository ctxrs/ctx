namespace Ctx.AgentHistory;

/// <summary>Configuration for the local ctx CLI adapter.</summary>
public sealed record LocalAgentHistoryConfig
{
    public string CtxBinary { get; init; } = "ctx";
    public string? DataRoot { get; init; }
    public string? WorkingDirectory { get; init; }
    public IReadOnlyDictionary<string, string?>? Environment { get; init; }
    public TimeSpan? Timeout { get; init; }
}

/// <summary>Placeholder configuration for a future hosted agent-history-v1 transport.</summary>
[Obsolete("Hosted SDK placeholders are deprecated and will be removed in the next breaking SDK revision; hosted operations remain unsupported.", error: false)]
public sealed record HostedAgentHistoryConfig
{
    public HostedAgentHistoryConfig(string baseUrl)
    {
        BaseUrl = baseUrl;
    }

    public string BaseUrl { get; init; }
    public string? ApiKey { get; init; }
    public TimeSpan? Timeout { get; init; }
}
