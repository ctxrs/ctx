using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

internal static class AgentHistoryContract
{
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
        var expected = operation == "search" ? 2 : CtxAgentHistoryVersions.SchemaVersion;
        if (schema is not null && schema != expected)
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
        var indexedCatalogSessions = JsonHelpers.GetInt(raw, "indexed_catalog_sessions")
            ?? JsonHelpers.GetInt(raw, "indexedCatalogSessions");

        var status = (JsonObject)CamelizePublic(raw)!;
        var setupMode = JsonHelpers.GetString(raw, "mode");
        SetIfAbsent(
            status,
            "initialized",
            JsonHelpers.GetBool(raw, "initialized") ?? setupMode is "ready" or "catalog_only");
        SetIfAbsent(status, "localOnly", JsonHelpers.GetBool(raw, "local_only") ?? JsonHelpers.GetBool(raw, "localOnly") ?? true);
        SetIfAbsent(status, "dataRoot", JsonHelpers.GetString(raw, "data_root") ?? JsonHelpers.GetString(raw, "dataRoot"));
        SetIfAbsent(status, "indexedItems", JsonHelpers.GetInt(raw, "indexed_items") ?? JsonHelpers.GetInt(raw, "indexedItems") ?? 0);
        SetIfAbsent(status, "indexedSources", JsonHelpers.GetInt(raw, "indexed_sources") ?? JsonHelpers.GetInt(raw, "indexedSources") ?? 0);
        SetIfAbsent(status, "catalogedSessions", JsonHelpers.GetInt(raw, "cataloged_sessions") ?? JsonHelpers.GetInt(raw, "catalogedSessions") ?? 0);
        SetIfAbsent(status, "pendingCatalogSessions", JsonHelpers.GetInt(raw, "pending_catalog_sessions") ?? JsonHelpers.GetInt(raw, "pendingCatalogSessions") ?? 0);
        SetIfAbsent(status, "failedCatalogSessions", JsonHelpers.GetInt(raw, "failed_catalog_sessions") ?? JsonHelpers.GetInt(raw, "failedCatalogSessions") ?? 0);
        SetIfAbsent(status, "staleCatalogSessions", JsonHelpers.GetInt(raw, "stale_catalog_sessions") ?? JsonHelpers.GetInt(raw, "staleCatalogSessions") ?? 0);
        if (indexedCatalogSessions is not null)
        {
            SetIfAbsent(status, "indexedCatalogSessions", indexedCatalogSessions.Value);
        }
        return status;
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
        var schema = JsonHelpers.GetInt(raw, "schema_version");
        if (schema != 2)
        {
            throw new CtxAgentHistoryProtocolException(
                "ctx search returned an unsupported schema version",
                new JsonObject { ["expectedSchemaVersion"] = 2, ["actualSchemaVersion"] = schema });
        }
        if (!raw.TryGetPropertyValue("query", out var rawQuery))
        {
            throw new CtxAgentHistoryProtocolException(
                "ctx search response is missing its canonical query",
                new JsonObject { ["field"] = "query" });
        }
        SearchQueryV1? query = null;
        if (rawQuery is JsonObject rawQueryObject)
        {
            try
            {
                query = SearchQueryV1.FromJson(rawQueryObject);
            }
            catch (CtxAgentHistoryValidationException error)
            {
                throw new CtxAgentHistoryProtocolException(
                    "ctx search returned an invalid canonical query",
                    new JsonObject { ["field"] = "query", ["validation"] = error.Message });
            }
        }
        else if (rawQuery is not null)
        {
            throw new CtxAgentHistoryProtocolException(
                "ctx search response contains a non-object canonical query",
                new JsonObject { ["field"] = "query" });
        }
        if (raw["query_execution"] is not JsonObject queryExecution)
        {
            throw new CtxAgentHistoryProtocolException(
                "ctx search response is missing query execution diagnostics",
                new JsonObject { ["field"] = "query_execution" });
        }
        if (raw["results"] is not JsonArray rawResults)
        {
            throw new CtxAgentHistoryProtocolException(
                "ctx search response is missing its results array",
                new JsonObject { ["field"] = "results" });
        }
        var search = (JsonObject)CamelizePublic(raw)!;
        var results = new JsonArray();
        foreach (var result in rawResults)
        {
            results.Add(CamelizePublic(result));
        }

        search["schema_version"] = 2;
        search["query"] = query?.ToJsonObject();
        search["query_execution"] = JsonHelpers.Clone(queryExecution);
        search.Remove("schemaVersion");
        search.Remove("queryExecution");
        search["filters"] = CamelizePublic(raw["filters"] ?? new JsonObject());
        search["freshness"] = CamelizePublic(raw["freshness"] ?? new JsonObject());
        SetIfAbsent(search, "generatedAt", JsonHelpers.Clone(raw["generated_at"] ?? raw["generatedAt"]));
        search["results"] = results;
        search["pagination"] = CamelizePublic(raw["pagination"] ?? new JsonObject());
        search["truncation"] = CamelizePublic(raw["truncation"] ?? new JsonObject());
        return search;
    }

    public static JsonObject NormalizeEvent(JsonObject raw)
    {
        var result = (JsonObject)CamelizePublic(raw)!;
        var eventObject = CamelizePublic(raw["event"]);
        var events = new JsonArray();
        if (raw["events"] is JsonArray rawEvents)
        {
            foreach (var item in rawEvents)
            {
                events.Add(CamelizePublic(item));
            }
        }

        var source = raw["source"];
        if (source is null && eventObject is JsonObject eventObj)
        {
            source = eventObj["source"];
        }

        result["event"] = eventObject;
        result["events"] = events;
        result["source"] = CamelizePublic(source);
        return result;
    }

    public static JsonObject NormalizeSession(JsonObject raw)
    {
        var result = (JsonObject)CamelizePublic(raw)!;
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
                events.Add(CamelizePublic(item));
            }
        }

        result["session"] = session;
        result["events"] = events;
        result["source"] = CamelizePublic(raw["source"]);
        SetIfAbsent(result, "mode", raw["mode"]);
        SetIfAbsent(result, "format", raw["format"]);
        return result;
    }

    public static JsonObject NormalizeLocation(JsonObject raw)
    {
        return (JsonObject)CamelizePublic(raw)!;
    }

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
                if (pair.Key is
                    "schema_version" or "schemaVersion" or "contractVersion" or "operation" or
                    "backend" or "target" or "item_type" or "itemType" or "payload_type" or
                    "payloadType" or "record_type" or "recordType" or
                    "semantic_weight" or "semanticWeight" or
                    "semantic_fallback_code" or "semanticFallbackCode" or
                    "semantic_fallback" or "semanticFallback")
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
