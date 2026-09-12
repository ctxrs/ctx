using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

/// <summary>Executes adapter-specific agent-history-v2 operations.</summary>
public interface IAgentHistoryTransport
{
    string Name { get; }

    JsonObject Backend(JsonObject? raw = null);

    Task<JsonObject> ExecuteJsonAsync(
        string operation,
        IReadOnlyList<string> args,
        CancellationToken cancellationToken = default);

    Task<string?> GetCtxVersionAsync(CancellationToken cancellationToken = default);
}
