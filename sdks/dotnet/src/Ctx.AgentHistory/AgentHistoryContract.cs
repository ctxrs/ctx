using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

internal static class AgentHistoryContract
{
    private const ulong MaximumExactStatusCounter = 9_007_199_254_740_991UL;

    public static JsonObject Envelope(string operation, JsonObject backend, string payloadName, JsonNode? payload)
    {
        var result = new JsonObject
        {
            ["contractVersion"] = CtxAgentHistoryVersions.ContractVersion,
            ["schemaVersion"] = CtxAgentHistoryVersions.SchemaVersion,
            ["operation"] = operation,
            ["backend"] = JsonHelpers.Clone(backend)
        };
        result[payloadName] = payload;
        return result;
    }

    public static void EnsureSupportedSchema(JsonObject raw, string operation)
    {
        var schema = JsonHelpers.GetInt(raw, "schema_version") ?? JsonHelpers.GetInt(raw, "schemaVersion");
        if (schema is not null && schema != 1 && schema != 2)
        {
            throw new CtxAgentHistoryProtocolException(
                $"unsupported ctx schema version {schema}",
                new JsonObject
                {
                    ["operation"] = operation,
                    ["schemaVersion"] = schema
                });
        }
    }

    public static JsonObject NormalizeStatus(JsonObject raw)
    {
        var current = (JsonObject)CamelizePublic(raw)!;
        var lexical = current["lexical"] as JsonObject;
        var status = new JsonObject
        {
            ["initialized"] = JsonHelpers.GetBool(current, "initialized")
                ?? !string.IsNullOrWhiteSpace(JsonHelpers.GetString(lexical ?? new JsonObject(), "generationId")),
            ["localOnly"] = true
        };
        foreach (var key in new[]
        {
            "dataRoot", "readOnly", "indexedItems", "indexedSessions", "indexedEvents",
            "indexedSources", "historyEpoch", "lexical", "refresh", "semantic", "daemon"
        })
        {
            if (current[key] is JsonNode value)
            {
                if (key.StartsWith("indexed", StringComparison.Ordinal))
                {
                    ValidateStatusCounter(key, value);
                }
                status[key] = JsonHelpers.Clone(value);
            }
        }
        return status;
    }

    private static void ValidateStatusCounter(string key, JsonNode value)
    {
        if (value is not JsonValue jsonValue
            || !jsonValue.TryGetValue<ulong>(out var counter)
            || counter > MaximumExactStatusCounter)
        {
            throw new CtxAgentHistoryProtocolException(
                $"ctx status counter {key} is outside the exact JSON integer domain",
                new JsonObject
                {
                    ["field"] = key,
                    ["maximum"] = MaximumExactStatusCounter
                });
        }
    }

    public static JsonArray NormalizeSources(JsonObject raw)
    {
        var result = new JsonArray();
        if (raw["sources"] is JsonArray sources)
        {
            foreach (var source in sources)
            {
                result.Add(CamelizePublic(source));
            }
        }
        return result;
    }

    public static JsonObject NormalizeImport(JsonObject raw)
    {
        var import = (JsonObject)CamelizePublic(raw)!;
        var sources = new JsonArray();
        if (raw["sources"] is JsonArray rawSources)
        {
            foreach (var source in rawSources)
            {
                sources.Add(CamelizePublic(source));
            }
        }

        SetIfAbsent(import, "resume", JsonHelpers.GetBool(raw, "resume") ?? false);
        SetIfAbsent(import, "resumeMode", JsonHelpers.GetString(raw, "resume_mode") ?? JsonHelpers.GetString(raw, "resumeMode"));
        import["totals"] = CamelizePublic(raw["totals"] ?? new JsonObject());
        import["sources"] = sources;
        return import;
    }

    public static JsonObject NormalizeSearch(JsonObject raw)
    {
        var search = (JsonObject)CamelizePublic(raw)!;
        var results = new JsonArray();
        if (raw["results"] is JsonArray rawResults)
        {
            foreach (var result in rawResults)
            {
                results.Add(CamelizePublic(result));
            }
        }

        SetIfAbsent(search, "query", raw["query"]);
        search["filters"] = CamelizePublic(raw["filters"] ?? new JsonObject());
        search["freshness"] = CamelizePublic(raw["freshness"] ?? new JsonObject());
        SetIfAbsent(search, "generatedAt", JsonHelpers.Clone(raw["generated_at"] ?? raw["generatedAt"]));
        search["results"] = results;
        search["pagination"] = NormalizeSearchPagination(raw, search);
        search["truncation"] = CamelizePublic(raw["truncation"] ?? new JsonObject());
        return search;
    }

    private static JsonObject NormalizeSearchPagination(JsonObject raw, JsonObject search)
    {
        if (raw["pagination"] is JsonObject pagination)
        {
            return (JsonObject)CamelizePublic(pagination)!;
        }

        var compatibility = new JsonObject();
        if (search["resultWindow"] is not JsonObject resultWindow)
        {
            return compatibility;
        }
        if (resultWindow["limit"] is not null)
        {
            compatibility["limit"] = JsonHelpers.Clone(resultWindow["limit"]);
        }
        if (resultWindow["moreAvailable"] is not null)
        {
            compatibility["hasMore"] = JsonHelpers.Clone(resultWindow["moreAvailable"]);
        }
        return compatibility;
    }

    public static JsonObject NormalizeEvent(JsonObject raw)
    {
        var result = new JsonObject();
        var eventObject = NormalizeEventRecord(raw["event"]);
        var events = new JsonArray();
        if (raw["events"] is JsonArray rawEvents)
        {
            foreach (var item in rawEvents)
            {
                events.Add(NormalizeEventRecord(item));
            }
        }

        result["event"] = eventObject;
        result["events"] = events;
        return result;
    }

    public static JsonObject NormalizeSession(JsonObject raw)
    {
        var result = new JsonObject();
        var session = CamelizePublic(raw["session"] ?? new JsonObject());
        if (session is JsonObject sessionObj)
        {
            CopyIfAbsent(sessionObj, "ctxSessionId", raw["ctx_session_id"]);
            CopyIfAbsent(sessionObj, "providerSessionId", raw["provider_session_id"]);
        }

        var events = new JsonArray();
        if (raw["events"] is JsonArray rawEvents)
        {
            foreach (var item in rawEvents)
            {
                events.Add(NormalizeEventRecord(item));
            }
        }

        result["session"] = session;
        result["events"] = events;
        SetIfAbsent(result, "mode", raw["mode"]);
        SetIfAbsent(result, "format", raw["format"]);
        return result;
    }

    private static JsonNode? NormalizeEventRecord(JsonNode? value)
    {
        if (value is not JsonObject eventObject)
        {
            return CamelizePublic(value);
        }

        var hasSnake = eventObject.TryGetPropertyValue("mcp_tool_call", out var snake);
        var hasCamel = eventObject.TryGetPropertyValue("mcpToolCall", out var camel);
        if (hasSnake && hasCamel)
        {
            throw InvalidMcpWire("duplicate outer wire aliases");
        }
        var hasSnakeExchange = eventObject.TryGetPropertyValue("mcp_exchange", out var snakeExchange);
        var hasCamelExchange = eventObject.TryGetPropertyValue("mcpExchange", out var camelExchange);
        if (hasSnakeExchange && hasCamelExchange)
        {
            throw InvalidMcpExchangeWire("duplicate outer wire aliases");
        }

        var outer = new JsonObject();
        foreach (var pair in eventObject)
        {
            if (pair.Key is "mcp_tool_call" or "mcpToolCall" or "mcp_exchange" or "mcpExchange")
            {
                continue;
            }
            if (SnakeToCamel(pair.Key) == "mcpToolCall")
            {
                throw InvalidMcpWire($"outer member {pair.Key} collides with canonical mcpToolCall");
            }
            if (SnakeToCamel(pair.Key) == "mcpExchange")
            {
                throw InvalidMcpExchangeWire($"outer member {pair.Key} collides with canonical mcpExchange");
            }
            outer[pair.Key] = JsonHelpers.Clone(pair.Value);
        }

        var normalized = (JsonObject)CamelizePublic(outer)!;
        if (hasSnake || hasCamel)
        {
            normalized["mcpToolCall"] = McpToolCall.FromJson(hasSnake ? snake : camel).ToJsonObject();
        }
        if (hasSnakeExchange || hasCamelExchange)
        {
            normalized["mcpExchange"] = McpExchange.NormalizeWire(
                hasSnakeExchange ? snakeExchange : camelExchange);
        }
        return normalized;
    }

    private static CtxAgentHistoryProtocolException InvalidMcpWire(string message) =>
        new(
            $"agent-history-v1 MCP tool call {message}",
            new JsonObject { ["field"] = "mcpToolCall" });

    private static CtxAgentHistoryProtocolException InvalidMcpExchangeWire(string message) =>
        new(
            $"agent-history-v1 MCP exchange {message}",
            new JsonObject { ["field"] = "mcpExchange" });

    public static JsonNode? CamelizePublic(JsonNode? value)
    {
        if (value is null)
        {
            return null;
        }

        if (value is JsonArray array)
        {
            var result = new JsonArray();
            foreach (var item in array)
            {
                result.Add(CamelizePublic(item));
            }
            return result;
        }

        if (value is JsonObject obj)
        {
            var result = new JsonObject();
            foreach (var pair in obj)
            {
                if (pair.Key is "schema_version" or "schemaVersion" or "contractVersion" or "operation" or "backend" or "target" or "item_type" or "itemType" or "payload_type" or "payloadType" or "record_type" or "recordType")
                {
                    continue;
                }
                result[SnakeToCamel(pair.Key)] = CamelizePublic(pair.Value);
            }
            return result;
        }

        return JsonHelpers.Clone(value);
    }

    private static string SnakeToCamel(string value)
    {
        var parts = value.Split('_');
        if (parts.Length == 1)
        {
            return value;
        }

        return parts[0] + string.Concat(parts.Skip(1).Select(part =>
            part.Length == 0 ? "" : char.ToUpperInvariant(part[0]) + part[1..]));
    }

    private static void CopyIfAbsent(JsonObject target, string key, JsonNode? value)
    {
        if (!target.ContainsKey(key) && value is not null)
        {
            target[key] = JsonHelpers.Clone(value);
        }
    }

    private static void SetIfAbsent(JsonObject target, string key, JsonNode? value)
    {
        if (!target.ContainsKey(key))
        {
            target[key] = JsonHelpers.Clone(value);
        }
    }

    private static void SetIfAbsent(JsonObject target, string key, string? value)
    {
        if (!target.ContainsKey(key) && value is not null)
        {
            target[key] = value;
        }
    }

    private static void SetIfAbsent(JsonObject target, string key, int value)
    {
        if (!target.ContainsKey(key))
        {
            target[key] = value;
        }
    }

    private static void SetIfAbsent(JsonObject target, string key, bool value)
    {
        if (!target.ContainsKey(key))
        {
            target[key] = value;
        }
    }
}
