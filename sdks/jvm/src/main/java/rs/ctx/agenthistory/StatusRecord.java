package rs.ctx.agenthistory;

import java.util.Map;

/** Local agent history index status. */
public final class StatusRecord {
    private final Map<String, Object> fields;

    StatusRecord(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
    }

    static StatusRecord from(Object value) {
        return new StatusRecord(AgentHistoryValue.object(value));
    }

    public Boolean getInitialized() {
        return AgentHistoryValue.bool(fields.get("initialized"));
    }

    public Boolean initialized() {
        return getInitialized();
    }

    public Boolean getLocalOnly() {
        return AgentHistoryValue.bool(fields.get("localOnly"));
    }

    public Boolean localOnly() {
        return getLocalOnly();
    }

    public Boolean getReadOnly() {
        return AgentHistoryValue.bool(fields.get("readOnly"));
    }

    public Boolean readOnly() {
        return getReadOnly();
    }

    public String getDataRoot() {
        return AgentHistoryValue.string(fields.get("dataRoot"));
    }

    public String dataRoot() {
        return getDataRoot();
    }

    public Integer getIndexedItems() {
        return AgentHistoryValue.integer(fields.get("indexedItems"));
    }

    public Integer indexedItems() {
        return getIndexedItems();
    }

    public Integer getIndexedSessions() {
        return AgentHistoryValue.integer(fields.get("indexedSessions"));
    }

    public Integer indexedSessions() {
        return getIndexedSessions();
    }

    public Integer getIndexedEvents() {
        return AgentHistoryValue.integer(fields.get("indexedEvents"));
    }

    public Integer indexedEvents() {
        return getIndexedEvents();
    }

    public Integer getIndexedSources() {
        return AgentHistoryValue.integer(fields.get("indexedSources"));
    }

    public Integer indexedSources() {
        return getIndexedSources();
    }

    public Map<String, Object> getHistoryEpoch() {
        return AgentHistoryValue.objectAt(fields, "historyEpoch");
    }

    public Map<String, Object> historyEpoch() {
        return getHistoryEpoch();
    }

    public Map<String, Object> getLexical() {
        return AgentHistoryValue.objectAt(fields, "lexical");
    }

    public Map<String, Object> lexical() {
        return getLexical();
    }

    public Map<String, Object> getRefresh() {
        return AgentHistoryValue.objectAt(fields, "refresh");
    }

    public Map<String, Object> refresh() {
        return getRefresh();
    }

    public Map<String, Object> getSemantic() {
        return AgentHistoryValue.objectAt(fields, "semantic");
    }

    public Map<String, Object> semantic() {
        return getSemantic();
    }

    public Map<String, Object> getDaemon() {
        return AgentHistoryValue.objectAt(fields, "daemon");
    }

    public Map<String, Object> daemon() {
        return getDaemon();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
