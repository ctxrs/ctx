package rs.ctx.agenthistory;

import java.util.Map;

/** Session metadata returned by show-session. */
public final class SessionSummary {
    private final Map<String, Object> fields;

    SessionSummary(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
    }

    static SessionSummary from(Object value) {
        Map<String, Object> fields = AgentHistoryValue.objectOrNull(value);
        return fields == null ? null : new SessionSummary(fields);
    }

    public String getCtxSessionId() {
        return AgentHistoryValue.string(fields.get("ctxSessionId"));
    }

    public String ctxSessionId() {
        return getCtxSessionId();
    }

    public String getProvider() {
        return AgentHistoryValue.string(fields.get("provider"));
    }

    public String provider() {
        return getProvider();
    }

    public String getProviderSessionId() {
        return AgentHistoryValue.string(fields.get("providerSessionId"));
    }

    public String providerSessionId() {
        return getProviderSessionId();
    }

    public String getSourceFormat() {
        return AgentHistoryValue.string(fields.get("sourceFormat"));
    }

    public String sourceFormat() {
        return getSourceFormat();
    }

    public String getTitle() {
        return AgentHistoryValue.string(fields.get("title"));
    }

    public String title() {
        return getTitle();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
