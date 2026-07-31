package rs.ctx.agenthistory;

import java.util.List;
import java.util.Map;

/** A agent-history-v1 transcript event. */
public final class Event {
    private final Map<String, Object> fields;
    private final List<Citation> citations;

    Event(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
        this.citations = AgentHistoryValue.objectList(fields.get("citations"), Citation::new);
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

    public Integer getSequence() {
        return AgentHistoryValue.integer(fields.get("sequence"));
    }

    public Integer sequence() {
        return getSequence();
    }

    public String getEventType() {
        return AgentHistoryValue.string(fields.get("eventType"));
    }

    public String eventType() {
        return getEventType();
    }

    public String getRole() {
        return AgentHistoryValue.string(fields.get("role"));
    }

    public String role() {
        return getRole();
    }

    public String getOccurredAt() {
        return AgentHistoryValue.string(fields.get("occurredAt"));
    }

    public String occurredAt() {
        return getOccurredAt();
    }

    public String getText() {
        return AgentHistoryValue.string(fields.get("text"));
    }

    public String text() {
        return getText();
    }

    public String getPreview() {
        return AgentHistoryValue.string(fields.get("preview"));
    }

    public String preview() {
        return getPreview();
    }

    public List<Citation> getCitations() {
        return citations;
    }

    public List<Citation> citations() {
        return citations;
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
