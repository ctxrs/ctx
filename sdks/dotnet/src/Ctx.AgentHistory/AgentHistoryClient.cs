using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

/// <summary>Client for the experimental ctx agent-history-v1 API.</summary>
public sealed class AgentHistoryClient
{
    private readonly IAgentHistoryTransport _transport;

    public AgentHistoryClient(IAgentHistoryTransport transport)
    {
        _transport = transport;
    }

    public IAgentHistoryTransport Transport => _transport;

    public static AgentHistoryClient Local(LocalAgentHistoryConfig? config = null)
    {
        return new AgentHistoryClient(new LocalCliAdapter(config));
    }

    public static AgentHistoryClient Hosted(HostedAgentHistoryConfig config)
    {
        return new AgentHistoryClient(new HostedAdapter(config));
    }

    public async Task<StatusResponse> StatusAsync(CancellationToken cancellationToken = default)
    {
        var raw = await InvokeAsync("status", ["status", "--json"], cancellationToken).ConfigureAwait(false);
        var envelope = AgentHistoryContract.Envelope("status", _transport.Backend(raw), "status", AgentHistoryContract.NormalizeStatus(raw));
        return new StatusResponse(envelope);
    }

    public async Task<InitResponse> InitAsync(InitOptions? options = null, CancellationToken cancellationToken = default)
    {
        options ??= new InitOptions();
        var args = new List<string> { "setup", "--json" };
        AddOption(args, "--progress", options.Progress);
        if (options.CatalogOnly)
        {
            args.Add("--catalog-only");
        }

        var raw = await InvokeAsync("init", args, cancellationToken).ConfigureAwait(false);
        var envelope = AgentHistoryContract.Envelope("init", _transport.Backend(raw), "status", AgentHistoryContract.NormalizeStatus(raw));
        return new InitResponse(envelope);
    }

    public async Task<SourcesResponse> SourcesAsync(CancellationToken cancellationToken = default)
    {
        var raw = await InvokeAsync("sources", ["sources", "--json"], cancellationToken).ConfigureAwait(false);
        var envelope = AgentHistoryContract.Envelope("sources", _transport.Backend(raw), "sources", AgentHistoryContract.NormalizeSources(raw));
        return new SourcesResponse(envelope);
    }

    public async Task<ImportResponse> ImportHistoryAsync(ImportOptions? options = null, CancellationToken cancellationToken = default)
    {
        return await ImportLikeAsync("import", options, cancellationToken).ConfigureAwait(false);
    }

    public async Task<ImportResponse> ImportAsync(ImportOptions? options = null, CancellationToken cancellationToken = default)
    {
        return await ImportHistoryAsync(options, cancellationToken).ConfigureAwait(false);
    }

    public async Task<ImportResponse> SyncAsync(ImportOptions? options = null, CancellationToken cancellationToken = default)
    {
        return await ImportLikeAsync("sync", options, cancellationToken).ConfigureAwait(false);
    }

    public async Task<SearchResponse> SearchAsync(SearchOptions? options = null, CancellationToken cancellationToken = default)
    {
        options ??= new SearchOptions();
        RequireSearchIntent(options);
        var args = new List<string> { "search" };
        if (options.Query is not null)
        {
            args.Add("--query-json");
            args.Add(options.Query.ToJson());
        }
        if (options.Limit is > 0)
        {
            AddOption(args, "--limit", options.Limit.Value.ToString(System.Globalization.CultureInfo.InvariantCulture));
        }
        AddOption(args, "--backend", options.Backend);
        AddOption(args, "--provider", options.Provider);
        AddOption(args, "--workspace", options.Workspace);
        AddOption(args, "--since", options.Since);
        if (options.PrimaryOnly)
        {
            args.Add("--primary-only");
        }
        if (options.IncludeSubagents)
        {
            args.Add("--include-subagents");
        }
        AddOption(args, "--event-type", options.EventType);
        AddOption(args, "--file", options.File);
        AddOption(args, "--session", options.Session);
        if (options.Events)
        {
            args.Add("--events");
        }
        AddOption(args, "--refresh", options.Refresh);
        if (options.IncludeCurrentSession)
        {
            args.Add("--include-current-session");
        }
        args.Add("--json");

        var raw = await InvokeAsync("search", args, cancellationToken).ConfigureAwait(false);
        var envelope = AgentHistoryContract.Envelope("search", _transport.Backend(raw), "search", AgentHistoryContract.NormalizeSearch(raw));
        return new SearchResponse(envelope);
    }

    public async Task<ShowEventResponse> ShowEventAsync(
        string eventId,
        ShowEventOptions? options = null,
        CancellationToken cancellationToken = default)
    {
        RequireValue(eventId, "event id");
        options ??= new ShowEventOptions();

        var args = new List<string> { "show", "event", eventId, "--format", "json" };
        AddNumberOption(args, "--before", options.Before);
        AddNumberOption(args, "--after", options.After);
        AddNumberOption(args, "--window", options.Window);

        var raw = await InvokeAsync("showEvent", args, cancellationToken).ConfigureAwait(false);
        var envelope = AgentHistoryContract.Envelope("showEvent", _transport.Backend(raw), "event", AgentHistoryContract.NormalizeEvent(raw));
        return new ShowEventResponse(envelope);
    }

    public async Task<ShowSessionResponse> ShowSessionAsync(
        string sessionId,
        ShowSessionOptions? options = null,
        CancellationToken cancellationToken = default)
    {
        options = (options ?? new ShowSessionOptions()) with { Id = sessionId };
        return await ShowSessionAsync(options, cancellationToken).ConfigureAwait(false);
    }

    public async Task<ShowSessionResponse> ShowSessionAsync(
        ShowSessionOptions options,
        CancellationToken cancellationToken = default)
    {
        var args = BuildSessionLookupArgs("show", "session", options);
        AddOption(args, "--mode", options.Mode);
        args.Add("--format");
        args.Add("json");

        var raw = await InvokeAsync("showSession", args, cancellationToken).ConfigureAwait(false);
        var envelope = AgentHistoryContract.Envelope("showSession", _transport.Backend(raw), "session", AgentHistoryContract.NormalizeSession(raw));
        return new ShowSessionResponse(envelope);
    }

    public async Task<LocateEventResponse> LocateEventAsync(string eventId, CancellationToken cancellationToken = default)
    {
        RequireValue(eventId, "event id");
        var raw = await InvokeAsync(
            "locateEvent",
            ["locate", "event", eventId, "--format", "json"],
            cancellationToken).ConfigureAwait(false);
        var envelope = AgentHistoryContract.Envelope("locateEvent", _transport.Backend(raw), "location", AgentHistoryContract.NormalizeLocation(raw));
        return new LocateEventResponse(envelope);
    }

    public async Task<LocateSessionResponse> LocateSessionAsync(string sessionId, CancellationToken cancellationToken = default)
    {
        return await LocateSessionAsync(new SessionLookupOptions { Id = sessionId }, cancellationToken).ConfigureAwait(false);
    }

    public async Task<LocateSessionResponse> LocateSessionAsync(
        SessionLookupOptions options,
        CancellationToken cancellationToken = default)
    {
        var args = BuildSessionLookupArgs("locate", "session", options);
        args.Add("--format");
        args.Add("json");

        var raw = await InvokeAsync("locateSession", args, cancellationToken).ConfigureAwait(false);
        var envelope = AgentHistoryContract.Envelope("locateSession", _transport.Backend(raw), "location", AgentHistoryContract.NormalizeLocation(raw));
        return new LocateSessionResponse(envelope);
    }

    public async Task<VersionInfo> VersionAsync(CancellationToken cancellationToken = default)
    {
        var ctxVersion = await _transport.GetCtxVersionAsync(cancellationToken).ConfigureAwait(false);
        return new VersionInfo(
            CtxAgentHistoryVersions.SdkVersion,
            CtxAgentHistoryVersions.ContractVersion,
            _transport.Name,
            ctxVersion);
    }

    public async Task<VersionInfo> VersioningAsync(CancellationToken cancellationToken = default)
    {
        return await VersionAsync(cancellationToken).ConfigureAwait(false);
    }

    private async Task<ImportResponse> ImportLikeAsync(
        string operation,
        ImportOptions? options,
        CancellationToken cancellationToken)
    {
        options ??= new ImportOptions();
        var args = new List<string> { "import", "--json" };
        AddOption(args, "--progress", options.Progress);
        if (options.All)
        {
            args.Add("--all");
        }
        AddOption(args, "--provider", options.Provider);
        AddOption(args, "--path", options.Path);
        if (options.Resume)
        {
            args.Add("--resume");
        }

        var raw = await InvokeAsync(operation, args, cancellationToken).ConfigureAwait(false);
        var envelope = AgentHistoryContract.Envelope(operation, _transport.Backend(raw), "import", AgentHistoryContract.NormalizeImport(raw));
        return new ImportResponse(envelope);
    }

    private async Task<JsonObject> InvokeAsync(
        string operation,
        IReadOnlyList<string> args,
        CancellationToken cancellationToken)
    {
        var raw = await _transport.ExecuteJsonAsync(operation, args, cancellationToken).ConfigureAwait(false);
        AgentHistoryContract.EnsureSupportedSchema(raw, operation);
        return raw;
    }

    private static List<string> BuildSessionLookupArgs(string command, string kind, SessionLookupOptions options)
    {
        var args = new List<string> { command, kind };
        if (!string.IsNullOrWhiteSpace(options.Id))
        {
            args.Add(options.Id);
        }
        else if (!string.IsNullOrWhiteSpace(options.ProviderSessionId))
        {
            AddOption(args, "--provider", options.Provider);
            AddOption(args, "--provider-session", options.ProviderSessionId);
        }
        else
        {
            throw new CtxAgentHistoryValidationException($"{kind} lookup requires an id or provider session id");
        }
        return args;
    }

    private static void RequireSearchIntent(SearchOptions options)
    {
        if (options.Query is not null)
        {
            options.Query.Validate();
            return;
        }
        if (!string.IsNullOrWhiteSpace(options.File))
        {
            return;
        }
        throw new CtxAgentHistoryValidationException("search requires a ctx-search-v1 query or file option");
    }

    private static void RequireValue(string value, string name)
    {
        if (string.IsNullOrWhiteSpace(value))
        {
            throw new CtxAgentHistoryValidationException($"{name} is required");
        }
    }

    private static void AddOption(List<string> args, string flag, string? value)
    {
        if (!string.IsNullOrWhiteSpace(value))
        {
            args.Add(flag);
            args.Add(value);
        }
    }

    private static void AddNumberOption(List<string> args, string flag, int? value)
    {
        if (value is > 0)
        {
            args.Add(flag);
            args.Add(value.Value.ToString(System.Globalization.CultureInfo.InvariantCulture));
        }
    }

}
