using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

public sealed record SearchExecutionLimits
{
    internal SearchExecutionLimits(JsonObject json)
    {
        QueryBytes = Required(json, "query_bytes");
        Clauses = Required(json, "clauses");
        AnalyzedTokensPerClause = Required(json, "analyzed_tokens_per_clause");
        CandidatesPerPositiveSeed = Required(json, "candidates_per_positive_seed");
        CandidateRows = Required(json, "candidate_rows");
        RetainedCandidateIds = Required(json, "retained_candidate_ids");
        ResidualRows = Required(json, "residual_rows");
        VerificationBytes = Required(json, "verification_bytes");
        VerificationLookupBytes = Required(json, "verification_lookup_bytes");
        HydratedRows = Required(json, "hydrated_rows");
        HydrationInputBytes = Required(json, "hydration_input_bytes");
        HydrationInputBytesPerEvent = Required(json, "hydration_input_bytes_per_event");
        SnippetInputBytes = Required(json, "snippet_input_bytes");
        ReturnedTextBytes = Required(json, "returned_text_bytes");
        SerializedResponseBytes = Required(json, "serialized_response_bytes");
        Results = Required(json, "results");
        ElapsedMs = RequiredLong(json, "elapsed_ms");
    }

    public int QueryBytes { get; }
    public int Clauses { get; }
    public int AnalyzedTokensPerClause { get; }
    public int CandidatesPerPositiveSeed { get; }
    public int CandidateRows { get; }
    public int RetainedCandidateIds { get; }
    public int ResidualRows { get; }
    public int VerificationBytes { get; }
    public int VerificationLookupBytes { get; }
    public int HydratedRows { get; }
    public int HydrationInputBytes { get; }
    public int HydrationInputBytesPerEvent { get; }
    public int SnippetInputBytes { get; }
    public int ReturnedTextBytes { get; }
    public int SerializedResponseBytes { get; }
    public int Results { get; }
    public long ElapsedMs { get; }

    internal static int Required(JsonObject json, string key) => JsonHelpers.GetInt(json, key)
        ?? throw Missing(key);
    internal static long RequiredLong(JsonObject json, string key) => JsonHelpers.GetLong(json, key)
        ?? throw Missing(key);
    internal static CtxAgentHistoryProtocolException Missing(string key) => new(
        $"ctx search diagnostics are missing {key}", new JsonObject { ["field"] = key });
}

public sealed record SearchExecutionConsumption
{
    internal SearchExecutionConsumption(JsonObject json)
    {
        QueryBytes = R(json, "query_bytes"); Clauses = R(json, "clauses");
        AnalyzedTokens = R(json, "analyzed_tokens");
        LargestAnalyzedTokensPerClause = R(json, "largest_analyzed_tokens_per_clause");
        LargestPositiveSeedCandidates = R(json, "largest_positive_seed_candidates");
        CandidateRows = R(json, "candidate_rows"); RetainedCandidateIds = R(json, "retained_candidate_ids");
        ResidualRows = R(json, "residual_rows"); VerificationBytes = R(json, "verification_bytes");
        LargestVerificationLookupBytes = R(json, "largest_verification_lookup_bytes");
        HydratedRows = R(json, "hydrated_rows");
        HydrationInputBytes = R(json, "hydration_input_bytes");
        LargestHydrationInputBytes = R(json, "largest_hydration_input_bytes");
        SnippetInputBytes = R(json, "snippet_input_bytes"); ReturnedResults = R(json, "returned_results");
        ReturnedTextBytes = R(json, "returned_text_bytes"); SerializedResponseBytes = R(json, "serialized_response_bytes");
        ElapsedMs = SearchExecutionLimits.RequiredLong(json, "elapsed_ms");
    }

    public int QueryBytes { get; } public int Clauses { get; } public int AnalyzedTokens { get; }
    public int LargestAnalyzedTokensPerClause { get; } public int LargestPositiveSeedCandidates { get; }
    public int CandidateRows { get; } public int RetainedCandidateIds { get; } public int ResidualRows { get; }
    public int VerificationBytes { get; } public int LargestVerificationLookupBytes { get; }
    public int HydratedRows { get; } public int HydrationInputBytes { get; }
    public int LargestHydrationInputBytes { get; } public int SnippetInputBytes { get; }
    public int ReturnedResults { get; } public int ReturnedTextBytes { get; } public int SerializedResponseBytes { get; }
    public long ElapsedMs { get; }
    private static int R(JsonObject json, string key) => SearchExecutionLimits.Required(json, key);
}

public sealed record SearchSemanticCoverage
{
    internal SearchSemanticCoverage(JsonObject json)
    {
        IndexedDocuments = JsonHelpers.GetLong(json, "indexed_documents");
        SearchableDocuments = JsonHelpers.GetLong(json, "searchable_documents");
    }
    public long? IndexedDocuments { get; }
    public long? SearchableDocuments { get; }
}

public sealed record SearchSemanticExecution
{
    internal SearchSemanticExecution(JsonObject json)
    {
        Attempted = JsonHelpers.GetBool(json, "attempted") ?? throw SearchExecutionLimits.Missing("attempted");
        Required = JsonHelpers.GetBool(json, "required") ?? throw SearchExecutionLimits.Missing("required");
        Readiness = JsonHelpers.GetString(json, "readiness") ?? throw SearchExecutionLimits.Missing("readiness");
        EffectiveBackend = JsonHelpers.GetString(json, "effective_backend") ?? throw SearchExecutionLimits.Missing("effective_backend");
        Backend = JsonHelpers.GetString(json, "backend"); RequestedCandidates = R(json, "requested_candidates");
        EligibleCandidates = R(json, "eligible_candidates"); CandidatesSupplied = R(json, "candidates_supplied");
        CandidatesConsumed = R(json, "candidates_consumed"); CandidatesUsed = R(json, "candidates_used");
        Coverage = new SearchSemanticCoverage(json["coverage"] as JsonObject ?? new JsonObject());
        Completeness = JsonHelpers.GetString(json, "completeness") ?? throw SearchExecutionLimits.Missing("completeness");
        IncompletenessReasons = JsonHelpers.GetStringArray(json, "incompleteness_reasons");
        SkipReason = JsonHelpers.GetString(json, "skip_reason");
        PositiveTextRuleVersion = JsonHelpers.GetString(json, "positive_text_rule_version")
            ?? throw SearchExecutionLimits.Missing("positive_text_rule_version");
    }
    public bool Attempted { get; } public bool Required { get; } public string Readiness { get; }
    public string EffectiveBackend { get; } public string? Backend { get; } public int RequestedCandidates { get; }
    public int EligibleCandidates { get; } public int CandidatesSupplied { get; } public int CandidatesConsumed { get; }
    public int CandidatesUsed { get; } public SearchSemanticCoverage Coverage { get; } public string Completeness { get; }
    public IReadOnlyList<string> IncompletenessReasons { get; } public string? SkipReason { get; }
    public string PositiveTextRuleVersion { get; }
    private static int R(JsonObject json, string key) => SearchExecutionLimits.Required(json, key);
}

public sealed record SearchQueryExecution
{
    internal SearchQueryExecution(JsonObject json)
    {
        QueryVersion = JsonHelpers.GetString(json, "query_version") ?? throw SearchExecutionLimits.Missing("query_version");
        CandidateStrategy = JsonHelpers.GetString(json, "candidate_strategy") ?? throw SearchExecutionLimits.Missing("candidate_strategy");
        Resolved = new SearchExecutionLimits(json["resolved"] as JsonObject ?? throw SearchExecutionLimits.Missing("resolved"));
        Consumed = new SearchExecutionConsumption(json["consumed"] as JsonObject ?? throw SearchExecutionLimits.Missing("consumed"));
        Semantic = new SearchSemanticExecution(json["semantic"] as JsonObject ?? throw SearchExecutionLimits.Missing("semantic"));
        RrfK = R(json, "rrf_k"); PerBranchCandidateRows = R(json, "per_branch_candidate_rows");
        RequestedResultLimit = R(json, "requested_result_limit"); ResultLimit = R(json, "result_limit");
        MaxResultLimit = R(json, "max_result_limit"); ClausesExecuted = R(json, "clauses_executed");
        VerificationDropped = R(json, "verification_dropped"); FilterVerificationDropped = R(json, "filter_verification_dropped");
        CandidateBudgetExhausted = JsonHelpers.GetBool(json, "candidate_budget_exhausted") ?? throw SearchExecutionLimits.Missing("candidate_budget_exhausted");
        TimedOut = JsonHelpers.GetBool(json, "timed_out") ?? throw SearchExecutionLimits.Missing("timed_out");
        Truncated = JsonHelpers.GetBool(json, "truncated") ?? throw SearchExecutionLimits.Missing("truncated");
        TruncationReasons = JsonHelpers.GetStringArray(json, "truncation_reasons");
    }
    public string QueryVersion { get; } public string CandidateStrategy { get; }
    public SearchExecutionLimits Resolved { get; } public SearchExecutionConsumption Consumed { get; }
    public SearchSemanticExecution Semantic { get; } public int RrfK { get; } public int PerBranchCandidateRows { get; }
    public int RequestedResultLimit { get; } public int ResultLimit { get; } public int MaxResultLimit { get; }
    public int ClausesExecuted { get; } public int VerificationDropped { get; } public int FilterVerificationDropped { get; }
    public bool CandidateBudgetExhausted { get; } public bool TimedOut { get; } public bool Truncated { get; }
    public IReadOnlyList<string> TruncationReasons { get; }
    private static int R(JsonObject json, string key) => SearchExecutionLimits.Required(json, key);
}
