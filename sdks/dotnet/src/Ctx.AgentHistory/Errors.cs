using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

/// <summary>Base structured exception for ctx agent history SDK failures.</summary>
public class CtxAgentHistoryException : Exception
{
    public CtxAgentHistoryException(
        string message,
        string code = "unknown",
        bool retryable = false,
        JsonObject? details = null,
        Exception? innerException = null)
        : base(message, innerException)
    {
        Code = code;
        Retryable = retryable;
        Details = details ?? new JsonObject();
    }

    public string Code { get; }
    public bool Retryable { get; }
    public JsonObject Details { get; }

    public JsonObject ToAgentHistoryError()
    {
        return new JsonObject
        {
            ["code"] = Code,
            ["message"] = Message,
            ["retryable"] = Retryable,
            ["details"] = JsonHelpers.Clone(Details),
            ["cause"] = InnerException?.Message
        };
    }
}

public sealed class CtxAgentHistoryCliException : CtxAgentHistoryException
{
    public CtxAgentHistoryCliException(
        string message,
        IReadOnlyList<string> command,
        int exitCode,
        string stdout,
        string stderr,
        string code = "adapter_error",
        bool retryable = false,
        Exception? innerException = null)
        : base(
            message,
            code,
            retryable: retryable,
            details: BuildDetails(command, exitCode, stdout, stderr),
            innerException)
    {
        Command = command.ToArray();
        ExitCode = exitCode;
        Stdout = stdout;
        Stderr = stderr;
    }

    public IReadOnlyList<string> Command { get; }
    public int ExitCode { get; }
    public string Stdout { get; }
    public string Stderr { get; }

    private static JsonObject BuildDetails(IReadOnlyList<string> command, int exitCode, string stdout, string stderr)
    {
        return new JsonObject
        {
            ["command"] = JsonHelpers.ToJsonArray(command),
            ["exitCode"] = exitCode,
            ["stdout"] = stdout,
            ["stderr"] = stderr
        };
    }
}

public sealed class CtxAgentHistoryProtocolException : CtxAgentHistoryException
{
    public CtxAgentHistoryProtocolException(string message, JsonObject? details = null, Exception? innerException = null)
        : base(message, "decode_error", retryable: false, details, innerException)
    {
    }
}

public sealed class CtxAgentHistoryValidationException : CtxAgentHistoryException
{
    public CtxAgentHistoryValidationException(string message, JsonObject? details = null)
        : base(message, "invalid_request", retryable: false, details)
    {
    }
}

#pragma warning disable CS0618 // This obsolete exception retains the obsolete hosted configuration contract.
[Obsolete("Hosted SDK placeholders are deprecated and will be removed in the next breaking SDK revision; hosted operations remain unsupported.", error: false)]
public sealed class HostedTransportNotImplementedException : CtxAgentHistoryException
{
    public HostedTransportNotImplementedException(string method, HostedAgentHistoryConfig config)
        : base(
            "hosted ctx agent history backend is not available in this in-repo SDK",
            "not_supported",
            retryable: false,
            details: BuildDetails(method, config))
    {
        Method = method;
    }

    public string Method { get; }

    private static JsonObject BuildDetails(string method, HostedAgentHistoryConfig config)
    {
        var details = new JsonObject
        {
            ["backend"] = "hosted",
            ["method"] = method
        };
        if (!string.IsNullOrWhiteSpace(config.BaseUrl))
        {
            details["baseUrl"] = config.BaseUrl;
        }
        return details;
    }
}
#pragma warning restore CS0618
