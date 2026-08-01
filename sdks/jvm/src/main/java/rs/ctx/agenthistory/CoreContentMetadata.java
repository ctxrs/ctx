package rs.ctx.agenthistory;

import java.util.Map;

/** Completeness and Core policy metadata for a shown event body. */
public final class CoreContentMetadata {
    private final Map<String, Object> fields;

    CoreContentMetadata(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
    }

    static CoreContentMetadata from(Object value) {
        Map<String, Object> fields = AgentHistoryValue.objectOrNull(value);
        return fields == null ? null : new CoreContentMetadata(fields);
    }

    public Boolean getComplete() {
        return AgentHistoryValue.bool(fields.get("complete"));
    }

    public Boolean complete() {
        return getComplete();
    }

    public CoreContentPolicyStatus getPolicyStatus() {
        return CoreContentPolicyStatus.fromWireName(
                AgentHistoryValue.string(fields.get("policyStatus")));
    }

    public CoreContentPolicyStatus policyStatus() {
        return getPolicyStatus();
    }

    public String getPolicyReason() {
        return AgentHistoryValue.string(fields.get("policyReason"));
    }

    public String policyReason() {
        return getPolicyReason();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
