package rs.ctx.agenthistory;

import java.util.Map;

/** Source citation attached to search hits and events. */
public final class Citation {
    private final Map<String, Object> fields;

    Citation(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
    }

    public String getItemId() {
        return AgentHistoryValue.string(fields.get("itemId"));
    }

    public String itemId() {
        return getItemId();
    }

    public String getTargetType() {
        return AgentHistoryValue.string(fields.get("targetType"));
    }

    public String targetType() {
        return getTargetType();
    }

    public String getCtxEventId() {
        return AgentHistoryValue.string(fields.get("ctxEventId"));
    }

    public String ctxEventId() {
        return getCtxEventId();
    }

    public String getCtxSessionId() {
        return AgentHistoryValue.string(fields.get("ctxSessionId"));
    }

    public String ctxSessionId() {
        return getCtxSessionId();
    }

    public String getLabel() {
        return AgentHistoryValue.string(fields.get("label"));
    }

    public String label() {
        return getLabel();
    }

    public String getTime() {
        return AgentHistoryValue.string(fields.get("time"));
    }

    public String time() {
        return getTime();
    }

    public String getProvider() {
        return AgentHistoryValue.string(fields.get("provider"));
    }

    public String provider() {
        return getProvider();
    }

    public String getSessionId() {
        return AgentHistoryValue.string(fields.get("sessionId"));
    }

    public String sessionId() {
        return getSessionId();
    }

    public Integer getEventSeq() {
        return AgentHistoryValue.integer(fields.get("eventSeq"));
    }

    public Integer eventSeq() {
        return getEventSeq();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
