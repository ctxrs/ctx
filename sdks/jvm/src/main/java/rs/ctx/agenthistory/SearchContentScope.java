package rs.ctx.agenthistory;

/** Event content classes that a search can match. */
public enum SearchContentScope {
    ALL("all"),
    TRANSCRIPT("transcript"),
    CALLS("calls"),
    OUTPUTS("outputs");

    private final String wireName;

    SearchContentScope(String wireName) {
        this.wireName = wireName;
    }

    public String wireName() {
        return wireName;
    }
}
