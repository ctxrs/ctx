using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

#pragma warning disable CS0618 // This obsolete placeholder composes only obsolete hosted API types.
/// <summary>Hosted agent-history-v1 placeholder. It performs no network I/O.</summary>
[Obsolete("Hosted SDK placeholders are deprecated and will be removed in the next breaking SDK revision; hosted operations remain unsupported.", error: false)]
public sealed class HostedAdapter : IAgentHistoryTransport
{
    public HostedAdapter(HostedAgentHistoryConfig config)
    {
        Config = config;
    }

    public string Name => "hosted";
    public HostedAgentHistoryConfig Config { get; }

    public JsonObject Backend(JsonObject? raw = null)
    {
        var backend = new JsonObject { ["kind"] = "hosted" };
        if (!string.IsNullOrWhiteSpace(Config.BaseUrl))
        {
            backend["baseUrl"] = Config.BaseUrl;
        }
        return backend;
    }

    public Task<JsonObject> ExecuteJsonAsync(
        string operation,
        IReadOnlyList<string> args,
        CancellationToken cancellationToken = default)
    {
        throw new HostedTransportNotImplementedException(operation, Config);
    }

    public Task<string?> GetCtxVersionAsync(CancellationToken cancellationToken = default)
    {
        return Task.FromResult<string?>(null);
    }
}
#pragma warning restore CS0618
