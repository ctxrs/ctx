package rs.ctx.agenthistory;

import java.util.List;
import java.util.Map;

/** Typed snake_case query_execution diagnostics for schema-v2 search. */
public final class SearchQueryExecution {
    private final String queryVersion;
    private final String candidateStrategy;
    private final Limits resolved;
    private final Consumption consumed;
    private final Semantic semantic;
    private final int rrfK;
    private final int perBranchCandidateRows;
    private final int requestedResultLimit;
    private final int resultLimit;
    private final int maxResultLimit;
    private final int clausesExecuted;
    private final int verificationDropped;
    private final int filterVerificationDropped;
    private final boolean candidateBudgetExhausted;
    private final boolean timedOut;
    private final boolean truncated;
    private final List<String> truncationReasons;

    SearchQueryExecution(Map<String, Object> raw) {
        queryVersion = string(raw, "query_version"); candidateStrategy = string(raw, "candidate_strategy");
        resolved = new Limits(object(raw, "resolved")); consumed = new Consumption(object(raw, "consumed"));
        semantic = new Semantic(object(raw, "semantic")); rrfK = integer(raw, "rrf_k");
        perBranchCandidateRows = integer(raw, "per_branch_candidate_rows");
        requestedResultLimit = integer(raw, "requested_result_limit"); resultLimit = integer(raw, "result_limit");
        maxResultLimit = integer(raw, "max_result_limit"); clausesExecuted = integer(raw, "clauses_executed");
        verificationDropped = integer(raw, "verification_dropped");
        filterVerificationDropped = integer(raw, "filter_verification_dropped");
        candidateBudgetExhausted = bool(raw, "candidate_budget_exhausted"); timedOut = bool(raw, "timed_out");
        truncated = bool(raw, "truncated"); truncationReasons = AgentHistoryValue.stringList(raw.get("truncation_reasons"));
    }

    public String queryVersion() { return queryVersion; } public String candidateStrategy() { return candidateStrategy; }
    public Limits resolved() { return resolved; } public Consumption consumed() { return consumed; }
    public Semantic semantic() { return semantic; } public int rrfK() { return rrfK; }
    public int perBranchCandidateRows() { return perBranchCandidateRows; }
    public int requestedResultLimit() { return requestedResultLimit; } public int resultLimit() { return resultLimit; }
    public int maxResultLimit() { return maxResultLimit; } public int clausesExecuted() { return clausesExecuted; }
    public int verificationDropped() { return verificationDropped; }
    public int filterVerificationDropped() { return filterVerificationDropped; }
    public boolean candidateBudgetExhausted() { return candidateBudgetExhausted; }
    public boolean timedOut() { return timedOut; } public boolean truncated() { return truncated; }
    public List<String> truncationReasons() { return truncationReasons; }

    public static final class Limits {
        private final Map<String, Object> raw;
        Limits(Map<String, Object> raw) { this.raw = raw; require(raw, LIMIT_KEYS); }
        public int queryBytes() { return integer(raw, "query_bytes"); } public int clauses() { return integer(raw, "clauses"); }
        public int analyzedTokensPerClause() { return integer(raw, "analyzed_tokens_per_clause"); }
        public int candidatesPerPositiveSeed() { return integer(raw, "candidates_per_positive_seed"); }
        public int candidateRows() { return integer(raw, "candidate_rows"); }
        public int retainedCandidateIds() { return integer(raw, "retained_candidate_ids"); }
        public int residualRows() { return integer(raw, "residual_rows"); }
        public int verificationBytes() { return integer(raw, "verification_bytes"); }
        public int verificationLookupBytes() { return integer(raw, "verification_lookup_bytes"); }
        public int hydratedRows() { return integer(raw, "hydrated_rows"); }
        public int hydrationInputBytes() { return integer(raw, "hydration_input_bytes"); }
        public int hydrationInputBytesPerEvent() { return integer(raw, "hydration_input_bytes_per_event"); }
        public int snippetInputBytes() { return integer(raw, "snippet_input_bytes"); }
        public int returnedTextBytes() { return integer(raw, "returned_text_bytes"); }
        public int serializedResponseBytes() { return integer(raw, "serialized_response_bytes"); }
        public int results() { return integer(raw, "results"); } public long elapsedMs() { return longValue(raw, "elapsed_ms"); }
    }

    public static final class Consumption {
        private final Map<String, Object> raw;
        Consumption(Map<String, Object> raw) { this.raw = raw; require(raw, CONSUMED_KEYS); }
        public int queryBytes() { return integer(raw, "query_bytes"); } public int clauses() { return integer(raw, "clauses"); }
        public int analyzedTokens() { return integer(raw, "analyzed_tokens"); }
        public int largestAnalyzedTokensPerClause() { return integer(raw, "largest_analyzed_tokens_per_clause"); }
        public int largestPositiveSeedCandidates() { return integer(raw, "largest_positive_seed_candidates"); }
        public int candidateRows() { return integer(raw, "candidate_rows"); }
        public int retainedCandidateIds() { return integer(raw, "retained_candidate_ids"); }
        public int residualRows() { return integer(raw, "residual_rows"); }
        public int verificationBytes() { return integer(raw, "verification_bytes"); }
        public int largestVerificationLookupBytes() { return integer(raw, "largest_verification_lookup_bytes"); }
        public int hydratedRows() { return integer(raw, "hydrated_rows"); }
        public int hydrationInputBytes() { return integer(raw, "hydration_input_bytes"); }
        public int largestHydrationInputBytes() { return integer(raw, "largest_hydration_input_bytes"); }
        public int snippetInputBytes() { return integer(raw, "snippet_input_bytes"); }
        public int returnedResults() { return integer(raw, "returned_results"); }
        public int returnedTextBytes() { return integer(raw, "returned_text_bytes"); }
        public int serializedResponseBytes() { return integer(raw, "serialized_response_bytes"); }
        public long elapsedMs() { return longValue(raw, "elapsed_ms"); }
    }

    public static final class Coverage {
        private final Map<String, Object> raw;
        Coverage(Map<String, Object> raw) { this.raw = raw; }
        public Long indexedDocuments() { return AgentHistoryValue.longValue(raw.get("indexed_documents")); }
        public Long searchableDocuments() { return AgentHistoryValue.longValue(raw.get("searchable_documents")); }
    }

    public static final class Semantic {
        private final Map<String, Object> raw; private final Coverage coverage;
        Semantic(Map<String, Object> raw) { this.raw = raw; require(raw, SEMANTIC_KEYS); coverage = new Coverage(AgentHistoryValue.object(raw.get("coverage"))); }
        public boolean attempted() { return bool(raw, "attempted"); } public boolean required() { return bool(raw, "required"); }
        public String readiness() { return string(raw, "readiness"); }
        public String effectiveBackend() { return string(raw, "effective_backend"); }
        public String backend() { return AgentHistoryValue.string(raw.get("backend")); }
        public int requestedCandidates() { return integer(raw, "requested_candidates"); }
        public int eligibleCandidates() { return integer(raw, "eligible_candidates"); }
        public int candidatesSupplied() { return integer(raw, "candidates_supplied"); }
        public int candidatesConsumed() { return integer(raw, "candidates_consumed"); }
        public int candidatesUsed() { return integer(raw, "candidates_used"); }
        public Coverage coverage() { return coverage; } public String completeness() { return string(raw, "completeness"); }
        public List<String> incompletenessReasons() { return AgentHistoryValue.stringList(raw.get("incompleteness_reasons")); }
        public String skipReason() { return AgentHistoryValue.string(raw.get("skip_reason")); }
        public String positiveTextRuleVersion() { return string(raw, "positive_text_rule_version"); }
    }

    private static final String[] LIMIT_KEYS = {"query_bytes","clauses","analyzed_tokens_per_clause","candidates_per_positive_seed","candidate_rows","retained_candidate_ids","residual_rows","verification_bytes","verification_lookup_bytes","hydrated_rows","hydration_input_bytes","hydration_input_bytes_per_event","snippet_input_bytes","returned_text_bytes","serialized_response_bytes","results","elapsed_ms"};
    private static final String[] CONSUMED_KEYS = {"query_bytes","clauses","analyzed_tokens","largest_analyzed_tokens_per_clause","largest_positive_seed_candidates","candidate_rows","retained_candidate_ids","residual_rows","verification_bytes","largest_verification_lookup_bytes","hydrated_rows","hydration_input_bytes","largest_hydration_input_bytes","snippet_input_bytes","returned_results","returned_text_bytes","serialized_response_bytes","elapsed_ms"};
    private static final String[] SEMANTIC_KEYS = {"attempted","required","readiness","effective_backend","requested_candidates","eligible_candidates","candidates_supplied","candidates_consumed","candidates_used","coverage","completeness","positive_text_rule_version"};

    private static void require(Map<String, Object> raw, String[] keys) { for (String key : keys) if (!raw.containsKey(key)) throw missing(key); }
    private static Map<String, Object> object(Map<String, Object> raw, String key) { Map<String, Object> value = AgentHistoryValue.objectOrNull(raw.get(key)); if (value == null) throw missing(key); return value; }
    private static String string(Map<String, Object> raw, String key) { String value = AgentHistoryValue.string(raw.get(key)); if (value == null) throw missing(key); return value; }
    private static int integer(Map<String, Object> raw, String key) { Integer value = AgentHistoryValue.integer(raw.get(key)); if (value == null) throw missing(key); return value.intValue(); }
    private static long longValue(Map<String, Object> raw, String key) { Long value = AgentHistoryValue.longValue(raw.get(key)); if (value == null) throw missing(key); return value.longValue(); }
    private static boolean bool(Map<String, Object> raw, String key) { Boolean value = AgentHistoryValue.bool(raw.get(key)); if (value == null) throw missing(key); return value.booleanValue(); }
    private static CtxAgentHistoryException.Protocol missing(String key) { Map<String,Object> d = new java.util.LinkedHashMap<>(); d.put("field", key); return new CtxAgentHistoryException.Protocol("ctx search diagnostics are missing " + key, d, null); }
}
