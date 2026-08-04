package rs.ctx.agenthistory;

import java.util.List;
import java.util.Map;

/** A agent-history-v1 transcript event. */
public final class Event {
    private final Map<String, Object> fields;
    private final List<Citation> citations;
    private final CoreContentMetadata content;
    private final McpToolCall mcpToolCall;

    Event(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
        this.citations = AgentHistoryValue.objectList(fields.get("citations"), Citation::new);
        this.content = CoreContentMetadata.from(fields.get("content"));
        this.mcpToolCall = fields.containsKey("mcpToolCall")
                ? McpToolCall.from(fields.get("mcpToolCall"))
                : null;
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

    public String getSourceFormat() {
        return AgentHistoryValue.string(fields.get("sourceFormat"));
    }

    public String sourceFormat() {
        return getSourceFormat();
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

    public McpToolCall getMcpToolCall() {
        return mcpToolCall;
    }

    public McpToolCall mcpToolCall() {
        return mcpToolCall;
    }

    public CoreContentMetadata getContent() {
        return content;
    }

    public CoreContentMetadata content() {
        return content;
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
