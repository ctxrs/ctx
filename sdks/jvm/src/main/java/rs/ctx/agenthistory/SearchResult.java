package rs.ctx.agenthistory;

import java.util.List;
import java.util.Map;

/** Search operation payload. */
public final class SearchResult {
    private final Map<String, Object> fields;
    private final int schemaVersion;
    private final SearchQuery query;
    private final SearchQueryExecution queryExecution;
    private final SearchFilters filters;
    private final Freshness freshness;
    private final List<SearchHit> results;
    private final SearchPagination pagination;
    private final SearchTruncation truncation;

    SearchResult(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
        Integer schema = AgentHistoryValue.integer(fields.get("schema_version"));
        if (schema == null || schema.intValue() != 2) {
            throw protocol("ctx search returned an unsupported schema version", "schema_version");
        }
        this.schemaVersion = schema.intValue();
        Map<String, Object> queryMap = AgentHistoryValue.objectOrNull(fields.get("query"));
        this.query = queryMap == null ? null : SearchQuery.fromMap(queryMap);
        Map<String, Object> execution = AgentHistoryValue.objectOrNull(fields.get("query_execution"));
        if (execution == null) throw protocol("ctx search response is missing query execution diagnostics", "query_execution");
        this.queryExecution = new SearchQueryExecution(execution);
        this.filters = SearchFilters.from(fields.get("filters"));
        this.freshness = Freshness.from(fields.get("freshness"));
        this.results = AgentHistoryValue.objectList(fields.get("results"), SearchHit::new);
        this.pagination = SearchPagination.from(fields.get("pagination"));
        this.truncation = SearchTruncation.from(fields.get("truncation"));
    }

    static SearchResult from(Object value) {
        return new SearchResult(AgentHistoryValue.object(value));
    }

    public int schemaVersion() { return schemaVersion; }
    public SearchQuery getQuery() { return query; }
    public SearchQuery query() { return query; }
    public SearchQueryExecution getQueryExecution() { return queryExecution; }
    public SearchQueryExecution queryExecution() { return queryExecution; }

    public SearchFilters getFilters() {
        return filters;
    }

    public SearchFilters filters() {
        return filters;
    }

    public Freshness getFreshness() {
        return freshness;
    }

    public Freshness freshness() {
        return freshness;
    }

    public String getGeneratedAt() {
        return AgentHistoryValue.string(fields.get("generatedAt"));
    }

    public String generatedAt() {
        return getGeneratedAt();
    }

    public Object getRetrieval() {
        return fields.get("retrieval");
    }

    public Object retrieval() {
        return getRetrieval();
    }

    public List<SearchHit> getResults() {
        return results;
    }

    public List<SearchHit> results() {
        return results;
    }

    public SearchPagination getPagination() {
        return pagination;
    }

    public SearchPagination pagination() {
        return pagination;
    }

    public SearchTruncation getTruncation() {
        return truncation;
    }

    public SearchTruncation truncation() {
        return truncation;
    }

    public Map<String, Object> asMap() {
        return fields;
    }

    private static CtxAgentHistoryException.Protocol protocol(String message, String field) {
        Map<String,Object> details = new java.util.LinkedHashMap<>(); details.put("field", field);
        return new CtxAgentHistoryException.Protocol(message, details, null);
    }
}
