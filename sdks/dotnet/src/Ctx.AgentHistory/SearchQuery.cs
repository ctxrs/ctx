using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

public abstract record SearchClause
{
    private SearchClause(string matcher, string value)
    {
        Matcher = matcher;
        Value = value;
    }

    public string Matcher { get; }
    public string Value { get; }

    public static SearchClause All(string value) => new AllClause(value);
    public static SearchClause Phrase(string value) => new PhraseClause(value);
    public static SearchClause Literal(string value) => new LiteralClause(value);
    public static SearchClause Semantic(string value) => new SemanticClause(value);

    internal JsonObject ToJsonObject() => new() { [Matcher] = Value };

    internal static SearchClause FromJson(JsonObject json, string placement)
    {
        if (json.Count != 1)
        {
            throw Invalid("search clause must contain exactly one matcher");
        }
        var pair = json.First();
        var value = pair.Value is JsonValue node && node.TryGetValue<string>(out var text)
            ? text
            : throw Invalid("search clause value must be a string");
        return pair.Key switch
        {
            "all" => All(value),
            "phrase" => Phrase(value),
            "literal" => Literal(value),
            "semantic" when placement == "any" => Semantic(value),
            _ => throw Invalid($"matcher '{pair.Key}' is not allowed in {placement}")
        };
    }

    private static CtxAgentHistoryValidationException Invalid(string message) => new(message);

    private sealed record AllClause : SearchClause { internal AllClause(string value) : base("all", value) { } }
    private sealed record PhraseClause : SearchClause { internal PhraseClause(string value) : base("phrase", value) { } }
    private sealed record LiteralClause : SearchClause { internal LiteralClause(string value) : base("literal", value) { } }
    private sealed record SemanticClause : SearchClause { internal SemanticClause(string value) : base("semantic", value) { } }
}

public sealed record SearchQueryV1
{
    public const string CurrentVersion = "ctx-search-v1";
    public const int MaxClauses = 32;
    public const int MaxClauseBytes = 1_024;
    public const int MaxTotalClauseBytes = 8_192;
    public const int MaxJsonBytes = 64 * 1_024;
    public const int MinLiteralBytes = 3;
    public const int MaxLiteralBytes = 256;

    public string Version { get; init; } = CurrentVersion;
    public IReadOnlyList<SearchClause> Any { get; init; } = Array.Empty<SearchClause>();
    public IReadOnlyList<SearchClause> Must { get; init; } = Array.Empty<SearchClause>();
    public IReadOnlyList<SearchClause> MustNot { get; init; } = Array.Empty<SearchClause>();

    public SearchQueryV1 Validate()
    {
        if (Version != CurrentVersion)
        {
            throw Invalid("search query version must be ctx-search-v1");
        }
        var placements = new[] { ("any", Any), ("must", Must), ("must_not", MustNot) };
        var totalClauses = 0;
        var totalBytes = 0;
        var semanticClauses = 0;
        foreach (var (placement, clauses) in placements)
        {
            if (clauses is null)
            {
                throw Invalid($"search query {placement} must be an array");
            }
            foreach (var clause in clauses)
            {
                if (clause is null)
                {
                    throw Invalid("search clause cannot be null");
                }
                if (placement != "any" && clause.Matcher == "semantic")
                {
                    throw Invalid("semantic clauses are allowed only in any");
                }
                if (clause.Matcher == "semantic" && ++semanticClauses > 1)
                {
                    throw Invalid("search query allows at most one semantic clause in any");
                }
                if (string.IsNullOrWhiteSpace(clause.Value))
                {
                    throw Invalid("search clause value must be a non-empty string");
                }
                var bytes = Encoding.UTF8.GetByteCount(clause.Value);
                if (bytes > MaxClauseBytes)
                {
                    throw Invalid("search clause exceeds the 1024-byte limit");
                }
                if (clause.Matcher == "literal" && (bytes < MinLiteralBytes || bytes > MaxLiteralBytes))
                {
                    throw Invalid("literal search clause must be between 3 and 256 bytes");
                }
                totalClauses++;
                totalBytes += bytes;
            }
        }
        if (Any.Count + Must.Count == 0)
        {
            throw Invalid("search query needs a positive any or must clause");
        }
        if (totalClauses > MaxClauses)
        {
            throw Invalid("search query exceeds the 32-clause limit");
        }
        if (totalBytes > MaxTotalClauseBytes)
        {
            throw Invalid("search query exceeds the 8192-byte clause limit");
        }
        return this;
    }

    public JsonObject ToJsonObject()
    {
        Validate();
        var json = new JsonObject { ["version"] = Version };
        AddPlacement(json, "any", Any);
        AddPlacement(json, "must", Must);
        AddPlacement(json, "must_not", MustNot);
        return json;
    }

    public string ToJson()
    {
        var serialized = ToJsonObject().ToJsonString(new JsonSerializerOptions { WriteIndented = false });
        if (Encoding.UTF8.GetByteCount(serialized) > MaxJsonBytes)
        {
            throw Invalid("search query JSON exceeds the 65536-byte limit");
        }
        return serialized;
    }

    internal static SearchQueryV1 FromJson(JsonObject json)
    {
        var allowed = new HashSet<string>(StringComparer.Ordinal) { "version", "any", "must", "must_not" };
        var unknown = json.Select(pair => pair.Key).FirstOrDefault(key => !allowed.Contains(key));
        if (unknown is not null)
        {
            throw Invalid($"search query contains unknown field '{unknown}'");
        }
        var query = new SearchQueryV1
        {
            Version = JsonHelpers.GetString(json, "version") ?? "",
            Any = ReadPlacement(json, "any"),
            Must = ReadPlacement(json, "must"),
            MustNot = ReadPlacement(json, "must_not")
        };
        return query.Validate();
    }

    private static IReadOnlyList<SearchClause> ReadPlacement(JsonObject json, string placement)
    {
        if (!json.TryGetPropertyValue(placement, out var node))
        {
            return Array.Empty<SearchClause>();
        }
        if (node is not JsonArray array)
        {
            throw Invalid($"search query {placement} must be an array");
        }
        return array.Select(item => item is JsonObject clause
            ? SearchClause.FromJson(clause, placement)
            : throw Invalid("search clause must be an object")).ToArray();
    }

    private static void AddPlacement(JsonObject json, string name, IReadOnlyList<SearchClause> clauses)
    {
        if (clauses.Count == 0)
        {
            return;
        }
        var array = new JsonArray();
        foreach (var clause in clauses)
        {
            array.Add(clause.ToJsonObject());
        }
        json[name] = array;
    }

    private static CtxAgentHistoryValidationException Invalid(string message) => new(message);
}
