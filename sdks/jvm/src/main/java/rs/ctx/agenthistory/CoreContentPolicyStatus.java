package rs.ctx.agenthistory;

/** Core policy outcome for shown event content. */
public enum CoreContentPolicyStatus {
    SELECTED("selected"),
    REDACTED("redacted"),
    OMITTED("omitted");

    private final String wireName;

    CoreContentPolicyStatus(String wireName) {
        this.wireName = wireName;
    }

    public String wireName() {
        return wireName;
    }

    static CoreContentPolicyStatus fromWireName(String value) {
        for (CoreContentPolicyStatus status : values()) {
            if (status.wireName.equals(value)) {
                return status;
            }
        }
        return null;
    }
}
