package rs.ctx.agenthistory;

import java.util.Map;

/** agent-history-v2 structured error payload. */
public final class AgentHistoryError {
    private final Map<String, Object> fields;

    AgentHistoryError(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
    }

    static AgentHistoryError from(Object value) {
        return new AgentHistoryError(AgentHistoryValue.object(value));
    }

    public String getCode() {
        return AgentHistoryValue.string(fields.get("code"));
    }

    public String code() {
        return getCode();
    }

    public String getMessage() {
        return AgentHistoryValue.string(fields.get("message"));
    }

    public String message() {
        return getMessage();
    }

    public Boolean getRetryable() {
        return AgentHistoryValue.bool(fields.get("retryable"));
    }

    public Boolean retryable() {
        return getRetryable();
    }

    public Map<String, Object> getDetails() {
        return AgentHistoryValue.object(fields.get("details"));
    }

    public Map<String, Object> details() {
        return getDetails();
    }

    public String getCause() {
        return AgentHistoryValue.string(fields.get("cause"));
    }

    public String cause() {
        return getCause();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
