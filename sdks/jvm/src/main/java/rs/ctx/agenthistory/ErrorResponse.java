package rs.ctx.agenthistory;

import java.util.Map;

/** Response wrapper for canonical agent-history-v2 error fixtures. */
public final class ErrorResponse extends AgentHistoryEnvelope {
    private final AgentHistoryError error;

    ErrorResponse(Map<String, Object> canonical) {
        super(canonical);
        this.error = AgentHistoryError.from(payload("error"));
    }

    public AgentHistoryError getError() {
        return error;
    }

    public AgentHistoryError error() {
        return error;
    }
}
