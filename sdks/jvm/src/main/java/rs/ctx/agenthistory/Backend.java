package rs.ctx.agenthistory;

import java.util.LinkedHashMap;
import java.util.Map;

/** Backend metadata for a agent-history-v2 response. */
public final class Backend {
    private final String kind;
    private final String dataRoot;
    private final String baseUrl;
    private final Map<String, Object> fields;

    public Backend(String kind, String dataRoot, String baseUrl) {
        Map<String, Object> fields = new LinkedHashMap<>();
        fields.put("kind", kind);
        if (dataRoot != null) {
            fields.put("dataRoot", dataRoot);
        }
        if (baseUrl != null) {
            fields.put("baseUrl", baseUrl);
        }
        this.kind = kind;
        this.dataRoot = dataRoot;
        this.baseUrl = baseUrl;
        this.fields = AgentHistoryValue.copyObject(fields);
    }

    Backend(Map<String, Object> fields) {
        this.kind = AgentHistoryValue.string(fields.get("kind"));
        this.dataRoot = AgentHistoryValue.string(fields.get("dataRoot"));
        this.baseUrl = AgentHistoryValue.string(fields.get("baseUrl"));
        this.fields = AgentHistoryValue.copyObject(fields);
    }

    public String getKind() {
        return kind;
    }

    public String kind() {
        return kind;
    }

    public String getDataRoot() {
        return dataRoot;
    }

    public String dataRoot() {
        return dataRoot;
    }

    public String getBaseUrl() {
        return baseUrl;
    }

    public String baseUrl() {
        return baseUrl;
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
