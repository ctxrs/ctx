package rs.ctx.agenthistory;

import java.util.Map;

/** Bounded result-window metadata for a search response. */
public final class SearchResultWindow {
    private final Map<String, Object> fields;

    SearchResultWindow(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
    }

    static SearchResultWindow from(Object value) {
        Map<String, Object> fields = AgentHistoryValue.objectOrNull(value);
        return fields == null ? null : new SearchResultWindow(fields);
    }

    public Integer getLimit() {
        return AgentHistoryValue.integer(fields.get("limit"));
    }

    public Integer limit() {
        return getLimit();
    }

    public Integer getReturned() {
        return AgentHistoryValue.integer(fields.get("returned"));
    }

    public Integer returned() {
        return getReturned();
    }

    public Boolean getMoreAvailable() {
        return AgentHistoryValue.bool(fields.get("moreAvailable"));
    }

    public Boolean moreAvailable() {
        return getMoreAvailable();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
