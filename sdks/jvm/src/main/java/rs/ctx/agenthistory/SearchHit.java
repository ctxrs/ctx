package rs.ctx.agenthistory;

import java.util.List;
import java.util.Map;

/** One agent history search hit. */
public final class SearchHit {
    private final Map<String, Object> fields;
    private final List<String> whyMatched;
    private final List<Citation> citations;
    private final List<String> suggestedNextCommands;

    SearchHit(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
        this.whyMatched = AgentHistoryValue.stringList(fields.get("whyMatched"));
        this.citations = AgentHistoryValue.objectList(fields.get("citations"), Citation::new);
        this.suggestedNextCommands = AgentHistoryValue.stringList(fields.get("suggestedNextCommands"));
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

    public String getProviderSessionId() {
        return AgentHistoryValue.string(fields.get("providerSessionId"));
    }

    public String providerSessionId() {
        return getProviderSessionId();
    }

    public Integer getEventSeq() {
        return AgentHistoryValue.integer(fields.get("eventSeq"));
    }

    public Integer eventSeq() {
        return getEventSeq();
    }

    public String getTitle() {
        return AgentHistoryValue.string(fields.get("title"));
    }

    public String title() {
        return getTitle();
    }

    public String getSnippet() {
        return AgentHistoryValue.string(fields.get("snippet"));
    }

    public String snippet() {
        return getSnippet();
    }

    public Double getRank() {
        return AgentHistoryValue.doubleValue(fields.get("rank"));
    }

    public Double rank() {
        return getRank();
    }

    public Double getRetrievalScore() {
        return AgentHistoryValue.doubleValue(fields.get("retrievalScore"));
    }

    public Double retrievalScore() {
        return getRetrievalScore();
    }

    public String getResultType() {
        return AgentHistoryValue.string(fields.get("resultType"));
    }

    public String resultType() {
        return getResultType();
    }

    public String getResultScope() {
        return AgentHistoryValue.string(fields.get("resultScope"));
    }

    public String resultScope() {
        return getResultScope();
    }

    public String getProvider() {
        return AgentHistoryValue.string(fields.get("provider"));
    }

    public String provider() {
        return getProvider();
    }

    public String getTimestamp() {
        return AgentHistoryValue.string(fields.get("timestamp"));
    }

    public String timestamp() {
        return getTimestamp();
    }

    public String getCwd() {
        return AgentHistoryValue.string(fields.get("cwd"));
    }

    public String cwd() {
        return getCwd();
    }

    public List<String> getWhyMatched() {
        return whyMatched;
    }

    public List<String> whyMatched() {
        return whyMatched;
    }

    public List<Citation> getCitations() {
        return citations;
    }

    public List<Citation> citations() {
        return citations;
    }

    public List<String> getSuggestedNextCommands() {
        return suggestedNextCommands;
    }

    public List<String> suggestedNextCommands() {
        return suggestedNextCommands;
    }

    public String getVisibility() {
        return AgentHistoryValue.string(fields.get("visibility"));
    }

    public String visibility() {
        return getVisibility();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
