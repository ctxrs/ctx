package rs.ctx.agenthistory;

import java.util.LinkedHashMap;
import java.util.Map;

/** One externally tagged ctx-search-v1 matcher. */
public final class SearchClause {
    private final String matcher;
    private final String value;

    private SearchClause(String matcher, String value) {
        this.matcher = matcher;
        this.value = value;
    }

    public static SearchClause all(String value) { return new SearchClause("all", value); }
    public static SearchClause phrase(String value) { return new SearchClause("phrase", value); }
    public static SearchClause literal(String value) { return new SearchClause("literal", value); }
    public static SearchClause semantic(String value) { return new SearchClause("semantic", value); }

    public String matcher() { return matcher; }
    public String value() { return value; }

    public Map<String, Object> asMap() {
        Map<String, Object> out = new LinkedHashMap<>();
        out.put(matcher, value);
        return AgentHistoryValue.copyObject(out);
    }

    static SearchClause fromMap(Map<String, Object> raw, String placement) {
        if (raw.size() != 1) {
            throw invalid("search clause must contain exactly one matcher");
        }
        Map.Entry<String, Object> entry = raw.entrySet().iterator().next();
        if (!(entry.getValue() instanceof String)) {
            throw invalid("search clause value must be a string");
        }
        String key = entry.getKey();
        String text = (String) entry.getValue();
        if ("all".equals(key)) return all(text);
        if ("phrase".equals(key)) return phrase(text);
        if ("literal".equals(key)) return literal(text);
        if ("semantic".equals(key) && "any".equals(placement)) return semantic(text);
        throw invalid("matcher '" + key + "' is not allowed in " + placement);
    }

    private static CtxAgentHistoryException.Validation invalid(String message) {
        return new CtxAgentHistoryException.Validation(message);
    }
}
