using System.Text;
using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

/// <summary>Status reported by an MCP response.</summary>
public enum McpResponseStatus
{
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Unknown
}

/// <summary>Failure category reported for a failed MCP response.</summary>
public enum McpFailureKind
{
    ToolReported,
    Invocation,
    Unknown
}

/// <summary>Capture state for MCP arguments and response JSON.</summary>
public enum McpJsonCaptureStatus
{
    Present,
    Absent,
    Unavailable,
    Omitted
}

/// <summary>Capture state for normalized MCP response text.</summary>
public enum McpTextCaptureStatus
{
    NormalizedBody,
    Absent,
    Unavailable,
    Omitted
}

/// <summary>Reason a complete MCP value was omitted.</summary>
public enum McpPayloadOmissionReason
{
    SizeLimit
}

/// <summary>Closed, content-governed MCP invocation and/or response capture.</summary>
public sealed record McpExchange
{
    public const int MaxIdentityBytes = 64 * 1024;
    public const ulong MaxSafeInteger = 9_007_199_254_740_991UL;

    private readonly JsonObject _json;

    private McpExchange(JsonObject json)
    {
        McpExchangeWire.RequireAllowed(json, "exchange", "providerCallId", "invocation", "response");
        ProviderCallId = McpExchangeWire.RequiredIdentity(json["providerCallId"], "providerCallId");
        Invocation = json.TryGetPropertyValue("invocation", out var invocation)
            ? McpInvocation.FromJson(invocation)
            : null;
        Response = json.TryGetPropertyValue("response", out var response)
            ? McpResponse.FromJson(response)
            : null;
        if (Invocation is null && Response is null)
        {
            throw McpExchangeWire.Invalid("requires invocation, response, or both");
        }
        _json = JsonHelpers.CloneObject(json);
    }

    public string ProviderCallId { get; }
    public McpInvocation? Invocation { get; }
    public McpResponse? Response { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static McpExchange FromJson(JsonNode? json) =>
        new(McpExchangeWire.RequiredObject(json, "exchange"));

    internal static JsonObject NormalizeWire(JsonNode? json)
    {
        var exchange = McpExchangeWire.NormalizeClosedObject(
            json,
            "exchange",
            new Dictionary<string, string>(StringComparer.Ordinal)
            {
                ["provider_call_id"] = "providerCallId",
                ["providerCallId"] = "providerCallId",
                ["invocation"] = "invocation",
                ["response"] = "response"
            });
        if (exchange.TryGetPropertyValue("invocation", out var invocation))
        {
            exchange["invocation"] = McpInvocation.NormalizeWire(invocation);
        }
        if (exchange.TryGetPropertyValue("response", out var response))
        {
            exchange["response"] = McpResponse.NormalizeWire(response);
        }
        return new McpExchange(exchange).ToJsonObject();
    }
}

/// <summary>Typed MCP invocation metadata and captured arguments.</summary>
public sealed record McpInvocation
{
    private readonly JsonObject _json;

    private McpInvocation(JsonObject json)
    {
        McpExchangeWire.RequireExactly(json, "invocation", "server", "tool", "arguments");
        Server = McpExchangeWire.RequiredIdentity(json["server"], "invocation.server");
        Tool = McpExchangeWire.RequiredIdentity(json["tool"], "invocation.tool");
        Arguments = McpArgumentsCapture.FromJson(json["arguments"]);
        _json = JsonHelpers.CloneObject(json);
    }

    public string Server { get; }
    public string Tool { get; }
    public McpArgumentsCapture Arguments { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static McpInvocation FromJson(JsonNode? json) =>
        new(McpExchangeWire.RequiredObject(json, "invocation"));

    internal static JsonObject NormalizeWire(JsonNode? json)
    {
        var invocation = McpExchangeWire.NormalizeClosedObject(
            json,
            "invocation",
            new Dictionary<string, string>(StringComparer.Ordinal)
            {
                ["server"] = "server",
                ["tool"] = "tool",
                ["arguments"] = "arguments"
            });
        if (invocation.TryGetPropertyValue("arguments", out var arguments))
        {
            invocation["arguments"] = McpArgumentsCapture.NormalizeWire(arguments);
        }
        return new McpInvocation(invocation).ToJsonObject();
    }
}

/// <summary>Typed MCP response metadata and capture dispositions.</summary>
public sealed record McpResponse
{
    private readonly JsonObject _json;

    private McpResponse(JsonObject json)
    {
        McpExchangeWire.RequireAllowed(
            json,
            "response",
            "status",
            "failureKind",
            "durationNs",
            "text",
            "payload");
        McpExchangeWire.RequireMembers(json, "response", "status", "text", "payload");

        Status = McpExchangeWire.ResponseStatus(json["status"]);
        FailureKind = json.TryGetPropertyValue("failureKind", out var failureKind)
            ? McpExchangeWire.FailureKind(failureKind)
            : null;
        if ((Status == McpResponseStatus.Failed) != (FailureKind is not null))
        {
            throw McpExchangeWire.Invalid("failureKind must be present exactly for failed responses");
        }
        DurationNs = json.TryGetPropertyValue("durationNs", out var durationNs)
            ? McpExchangeWire.SafeInteger(durationNs, "response.durationNs")
            : null;
        Text = McpTextCapture.FromJson(json["text"]);
        Payload = McpJsonCapture.FromJson(json["payload"], "response.payload");
        _json = JsonHelpers.CloneObject(json);
    }

    public McpResponseStatus Status { get; }
    public McpFailureKind? FailureKind { get; }
    public ulong? DurationNs { get; }
    public McpTextCapture Text { get; }
    public McpJsonCapture Payload { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static McpResponse FromJson(JsonNode? json) =>
        new(McpExchangeWire.RequiredObject(json, "response"));

    internal static JsonObject NormalizeWire(JsonNode? json)
    {
        var response = McpExchangeWire.NormalizeClosedObject(
            json,
            "response",
            new Dictionary<string, string>(StringComparer.Ordinal)
            {
                ["status"] = "status",
                ["failure_kind"] = "failureKind",
                ["failureKind"] = "failureKind",
                ["duration_ns"] = "durationNs",
                ["durationNs"] = "durationNs",
                ["text"] = "text",
                ["payload"] = "payload"
            });
        if (response.TryGetPropertyValue("text", out var text))
        {
            response["text"] = McpTextCapture.NormalizeWire(text);
        }
        if (response.TryGetPropertyValue("payload", out var payload))
        {
            response["payload"] = McpJsonCapture.NormalizeWire(payload, "response.payload");
        }
        return new McpResponse(response).ToJsonObject();
    }
}

/// <summary>MCP invocation-argument capture. Present values are JSON objects.</summary>
public sealed record McpArgumentsCapture
{
    private readonly JsonObject _json;

    private McpArgumentsCapture(JsonObject json)
    {
        CaptureStatus = McpExchangeWire.JsonCaptureStatus(json["captureStatus"], "invocation.arguments");
        switch (CaptureStatus)
        {
            case McpJsonCaptureStatus.Present:
                McpExchangeWire.RequireExactly(json, "invocation.arguments", "captureStatus", "value");
                Value = json["value"] is JsonObject value
                    ? JsonHelpers.CloneObject(value)
                    : throw McpExchangeWire.Invalid(
                        "present invocation arguments must be a JSON object",
                        "invocation.arguments.value");
                break;
            case McpJsonCaptureStatus.Omitted:
                (Reason, ObservedEncodedBytes) = McpExchangeWire.ReadOmission(json, "invocation.arguments");
                break;
            default:
                McpExchangeWire.RequireExactly(json, "invocation.arguments", "captureStatus");
                break;
        }
        _json = JsonHelpers.CloneObject(json);
    }

    public McpJsonCaptureStatus CaptureStatus { get; }
    public JsonObject? Value { get; }
    public McpPayloadOmissionReason? Reason { get; }
    public ulong? ObservedEncodedBytes { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static McpArgumentsCapture FromJson(JsonNode? json) =>
        new(McpExchangeWire.RequiredObject(json, "invocation.arguments"));

    internal static JsonObject NormalizeWire(JsonNode? json)
    {
        var capture = McpExchangeWire.NormalizeCaptureObject(json, "invocation.arguments");
        return new McpArgumentsCapture(capture).ToJsonObject();
    }
}

/// <summary>MCP JSON capture. Present values preserve arbitrary JSON keys and nulls.</summary>
public sealed record McpJsonCapture
{
    private readonly JsonObject _json;

    private McpJsonCapture(JsonObject json, string context)
    {
        CaptureStatus = McpExchangeWire.JsonCaptureStatus(json["captureStatus"], context);
        switch (CaptureStatus)
        {
            case McpJsonCaptureStatus.Present:
                McpExchangeWire.RequireExactly(json, context, "captureStatus", "value");
                Value = JsonHelpers.Clone(json["value"]);
                break;
            case McpJsonCaptureStatus.Omitted:
                (Reason, ObservedEncodedBytes) = McpExchangeWire.ReadOmission(json, context);
                break;
            default:
                McpExchangeWire.RequireExactly(json, context, "captureStatus");
                break;
        }
        _json = JsonHelpers.CloneObject(json);
    }

    public McpJsonCaptureStatus CaptureStatus { get; }
    public JsonNode? Value { get; }
    public McpPayloadOmissionReason? Reason { get; }
    public ulong? ObservedEncodedBytes { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static McpJsonCapture FromJson(JsonNode? json, string context) =>
        new(McpExchangeWire.RequiredObject(json, context), context);

    internal static JsonObject NormalizeWire(JsonNode? json, string context)
    {
        var capture = McpExchangeWire.NormalizeCaptureObject(json, context);
        return new McpJsonCapture(capture, context).ToJsonObject();
    }
}

/// <summary>MCP response-text capture disposition.</summary>
public sealed record McpTextCapture
{
    private readonly JsonObject _json;

    private McpTextCapture(JsonObject json)
    {
        CaptureStatus = McpExchangeWire.TextCaptureStatus(json["captureStatus"]);
        if (CaptureStatus == McpTextCaptureStatus.Omitted)
        {
            (Reason, ObservedEncodedBytes) = McpExchangeWire.ReadOmission(json, "response.text");
        }
        else
        {
            McpExchangeWire.RequireExactly(json, "response.text", "captureStatus");
        }
        _json = JsonHelpers.CloneObject(json);
    }

    public McpTextCaptureStatus CaptureStatus { get; }
    public McpPayloadOmissionReason? Reason { get; }
    public ulong? ObservedEncodedBytes { get; }

    public JsonObject ToJsonObject() => JsonHelpers.CloneObject(_json);

    internal static McpTextCapture FromJson(JsonNode? json) =>
        new(McpExchangeWire.RequiredObject(json, "response.text"));

    internal static JsonObject NormalizeWire(JsonNode? json)
    {
        var capture = McpExchangeWire.NormalizeCaptureObject(json, "response.text");
        return new McpTextCapture(capture).ToJsonObject();
    }
}

internal static class McpExchangeWire
{
    private static readonly UTF8Encoding StrictUtf8 = new(false, true);

    public static JsonObject NormalizeCaptureObject(JsonNode? json, string context) =>
        NormalizeClosedObject(
            json,
            context,
            new Dictionary<string, string>(StringComparer.Ordinal)
            {
                ["capture_status"] = "captureStatus",
                ["captureStatus"] = "captureStatus",
                ["value"] = "value",
                ["reason"] = "reason",
                ["observed_encoded_bytes"] = "observedEncodedBytes",
                ["observedEncodedBytes"] = "observedEncodedBytes"
            });

    public static JsonObject NormalizeClosedObject(
        JsonNode? json,
        string context,
        IReadOnlyDictionary<string, string> aliases)
    {
        var input = RequiredObject(json, context);
        var result = new JsonObject();
        foreach (var pair in input)
        {
            if (!aliases.TryGetValue(pair.Key, out var canonical))
            {
                throw Invalid($"{context} contains unknown member {pair.Key}", context);
            }
            if (result.ContainsKey(canonical))
            {
                throw Invalid($"{context} contains colliding aliases for {canonical}", context);
            }
            result[canonical] = JsonHelpers.Clone(pair.Value);
        }
        return result;
    }

    public static JsonObject RequiredObject(JsonNode? json, string context) =>
        json is JsonObject value
            ? value
            : throw Invalid($"{context} must be an object", context);

    public static void RequireExactly(JsonObject json, string context, params string[] fields)
    {
        RequireAllowed(json, context, fields);
        RequireMembers(json, context, fields);
    }

    public static void RequireAllowed(JsonObject json, string context, params string[] fields)
    {
        var allowed = new HashSet<string>(fields, StringComparer.Ordinal);
        var unknown = json.Select(pair => pair.Key).Where(key => !allowed.Contains(key)).ToArray();
        if (unknown.Length > 0)
        {
            throw Invalid($"{context} contains unknown members: {string.Join(", ", unknown)}", context);
        }
    }

    public static void RequireMembers(JsonObject json, string context, params string[] fields)
    {
        foreach (var field in fields)
        {
            if (!json.ContainsKey(field))
            {
                throw Invalid($"{context} requires {field}", $"{context}.{field}");
            }
        }
    }

    public static string RequiredIdentity(JsonNode? json, string field)
    {
        if (json is not JsonValue value || !value.TryGetValue<string>(out var text) || text.Length == 0)
        {
            throw Invalid($"{field} must be a nonempty string", field);
        }
        try
        {
            if (StrictUtf8.GetByteCount(text) > McpExchange.MaxIdentityBytes)
            {
                throw Invalid(
                    $"{field} exceeds {McpExchange.MaxIdentityBytes} decoded UTF-8 bytes",
                    field);
            }
        }
        catch (EncoderFallbackException exception)
        {
            throw Invalid($"{field} contains an invalid Unicode string", field, exception);
        }
        return text;
    }

    public static ulong SafeInteger(JsonNode? json, string field)
    {
        if (json is not JsonValue value
            || !value.TryGetValue<ulong>(out var number)
            || number > McpExchange.MaxSafeInteger)
        {
            throw Invalid(
                $"{field} is outside the exact JSON integer domain",
                field);
        }
        return number;
    }

    public static McpResponseStatus ResponseStatus(JsonNode? json) =>
        RequiredWireString(json, "response.status") switch
        {
            "succeeded" => McpResponseStatus.Succeeded,
            "failed" => McpResponseStatus.Failed,
            "cancelled" => McpResponseStatus.Cancelled,
            "timed_out" => McpResponseStatus.TimedOut,
            "unknown" => McpResponseStatus.Unknown,
            _ => throw Invalid("response.status is invalid", "response.status")
        };

    public static McpFailureKind FailureKind(JsonNode? json) =>
        RequiredWireString(json, "response.failureKind") switch
        {
            "tool_reported" => McpFailureKind.ToolReported,
            "invocation" => McpFailureKind.Invocation,
            "unknown" => McpFailureKind.Unknown,
            _ => throw Invalid("response.failureKind is invalid", "response.failureKind")
        };

    public static McpJsonCaptureStatus JsonCaptureStatus(JsonNode? json, string context) =>
        RequiredWireString(json, $"{context}.captureStatus") switch
        {
            "present" => McpJsonCaptureStatus.Present,
            "absent" => McpJsonCaptureStatus.Absent,
            "unavailable" => McpJsonCaptureStatus.Unavailable,
            "omitted" => McpJsonCaptureStatus.Omitted,
            _ => throw Invalid($"{context}.captureStatus is invalid", $"{context}.captureStatus")
        };

    public static McpTextCaptureStatus TextCaptureStatus(JsonNode? json) =>
        RequiredWireString(json, "response.text.captureStatus") switch
        {
            "normalized_body" => McpTextCaptureStatus.NormalizedBody,
            "absent" => McpTextCaptureStatus.Absent,
            "unavailable" => McpTextCaptureStatus.Unavailable,
            "omitted" => McpTextCaptureStatus.Omitted,
            _ => throw Invalid("response.text.captureStatus is invalid", "response.text.captureStatus")
        };

    public static (McpPayloadOmissionReason Reason, ulong? ObservedEncodedBytes) ReadOmission(
        JsonObject json,
        string context)
    {
        RequireAllowed(json, context, "captureStatus", "reason", "observedEncodedBytes");
        RequireMembers(json, context, "captureStatus", "reason");
        var reason = RequiredWireString(json["reason"], $"{context}.reason") switch
        {
            "size_limit" => McpPayloadOmissionReason.SizeLimit,
            _ => throw Invalid($"{context}.reason is invalid", $"{context}.reason")
        };
        ulong? observed = json.TryGetPropertyValue("observedEncodedBytes", out var value)
            ? SafeInteger(value, $"{context}.observedEncodedBytes")
            : null;
        return (reason, observed);
    }

    private static string RequiredWireString(JsonNode? json, string field) =>
        json is JsonValue value && value.TryGetValue<string>(out var text)
            ? text
            : throw Invalid($"{field} must be a string", field);

    public static CtxAgentHistoryProtocolException Invalid(
        string message,
        string? field = null,
        Exception? exception = null) =>
        new(
            $"agent-history-v1 MCP exchange {message}",
            ErrorDetails(message, field),
            exception);

    private static JsonObject ErrorDetails(string message, string? field)
    {
        var details = new JsonObject
        {
            ["field"] = field is null ? "mcpExchange" : $"mcpExchange.{field}"
        };
        if (message.Contains("exact JSON integer domain", StringComparison.Ordinal))
        {
            details["maximum"] = McpExchange.MaxSafeInteger;
        }
        return details;
    }
}
