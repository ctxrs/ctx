using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

internal static class JsonHelpers
{
    public static JsonArray ToJsonArray(IEnumerable<string> values)
    {
        var array = new JsonArray();
        foreach (var value in values)
        {
            array.Add(value);
        }
        return array;
    }

    public static JsonNode? Clone(JsonNode? node)
    {
        return node is null ? null : JsonNode.Parse(node.ToJsonString());
    }

    public static JsonObject CloneObject(JsonObject? obj)
    {
        return obj is null ? new JsonObject() : (JsonObject)Clone(obj)!;
    }

    public static string? GetString(JsonObject? obj, string key)
    {
        if (obj is null || !obj.TryGetPropertyValue(key, out var value) || value is null)
        {
            return null;
        }
        return value is JsonValue jsonValue && jsonValue.TryGetValue<string>(out var text) ? text : null;
    }

    public static int? GetInt(JsonObject? obj, string key)
    {
        if (obj is null || !obj.TryGetPropertyValue(key, out var value) || value is null)
        {
            return null;
        }
        return value is JsonValue jsonValue && jsonValue.TryGetValue<int>(out var number) ? number : null;
    }

    public static long? GetLong(JsonObject? obj, string key)
    {
        if (obj is null || !obj.TryGetPropertyValue(key, out var value) || value is null)
        {
            return null;
        }
        return value is JsonValue jsonValue && jsonValue.TryGetValue<long>(out var number) ? number : null;
    }

    public static ulong? GetUInt64(JsonObject? obj, string key)
    {
        if (obj is null || !obj.TryGetPropertyValue(key, out var value) || value is null)
        {
            return null;
        }
        return value is JsonValue jsonValue && jsonValue.TryGetValue<ulong>(out var number) ? number : null;
    }

    public static bool? GetBool(JsonObject? obj, string key)
    {
        if (obj is null || !obj.TryGetPropertyValue(key, out var value) || value is null)
        {
            return null;
        }
        return value is JsonValue jsonValue && jsonValue.TryGetValue<bool>(out var boolean) ? boolean : null;
    }

    public static double? GetDouble(JsonObject? obj, string key)
    {
        if (obj is null || !obj.TryGetPropertyValue(key, out var value) || value is null)
        {
            return null;
        }
        return value is JsonValue jsonValue && jsonValue.TryGetValue<double>(out var number) ? number : null;
    }

    public static IReadOnlyList<string> GetStringArray(JsonObject? obj, string key)
    {
        if (obj is null || obj[key] is not JsonArray array)
        {
            return Array.Empty<string>();
        }

        var result = new List<string>();
        foreach (var item in array)
        {
            if (item is JsonValue value && value.TryGetValue<string>(out var text))
            {
                result.Add(text);
            }
        }
        return result;
    }

    public static IReadOnlyList<T> GetObjectArray<T>(JsonObject? obj, string key, Func<JsonObject, T> factory)
    {
        if (obj is null || obj[key] is not JsonArray array)
        {
            return Array.Empty<T>();
        }

        var result = new List<T>();
        foreach (var item in array)
        {
            if (item is JsonObject objectItem)
            {
                result.Add(factory(objectItem));
            }
        }
        return result;
    }
}
