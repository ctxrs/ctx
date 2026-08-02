package rs.ctx.agenthistory;

import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Map;

/** Canonical agent-history-v1 envelope shared by all typed responses. */
public class AgentHistoryEnvelope {
    public static final String CONTRACT_VERSION = "agent-history-v1";
    public static final int SCHEMA_VERSION = 1;

    private final String contractVersion;
    private final int schemaVersion;
    private final String operation;
    private final Backend backend;
    private final Map<String, Object> fields;
    private final Map<String, Object> envelope;

    AgentHistoryEnvelope(Map<String, Object> canonical) {
        this.contractVersion = AgentHistoryValue.string(canonical.get("contractVersion"));
        Integer version = AgentHistoryValue.integer(canonical.get("schemaVersion"));
        this.schemaVersion = version == null ? SCHEMA_VERSION : version.intValue();
        this.operation = AgentHistoryValue.string(canonical.get("operation"));
        this.backend = new Backend(AgentHistoryValue.objectAt(canonical, "backend"));
        Map<String, Object> payloadFields = new LinkedHashMap<>();
        for (Map.Entry<String, Object> entry : canonical.entrySet()) {
            if (!isCommonField(entry.getKey())) {
                payloadFields.put(entry.getKey(), AgentHistoryValue.copy(entry.getValue()));
            }
        }
        this.fields = Collections.unmodifiableMap(payloadFields);
        this.envelope = AgentHistoryValue.copyObject(canonical);
    }

    AgentHistoryEnvelope(String operation, Backend backend, Map<String, Object> fields) {
        this(buildCanonical(operation, backend, fields));
    }

    public String getContractVersion() {
        return contractVersion;
    }

    public String contractVersion() {
        return contractVersion;
    }

    public int getSchemaVersion() {
        return schemaVersion;
    }

    public int schemaVersion() {
        return schemaVersion;
    }

    public String getOperation() {
        return operation;
    }

    public String operation() {
        return operation;
    }

    public Backend getBackend() {
        return backend;
    }

    public Map<String, Object> backend() {
        return backend.asMap();
    }

    public Object payload(String name) {
        return fields.get(name);
    }

    public Map<String, Object> fields() {
        return fields;
    }

    public Map<String, Object> asMap() {
        return envelope;
    }

    static AgentHistoryEnvelope wrap(String operation, Backend backend, Map<String, Object> raw) {
        return new AgentHistoryEnvelope(normalize(operation, backend, raw));
    }

    static Map<String, Object> normalize(String operation, Backend backend, Map<String, Object> raw) {
        if (CONTRACT_VERSION.equals(raw.get("contractVersion"))) {
            return AgentHistoryValue.copyObject(raw);
        }

        Map<String, Object> camel = new LinkedHashMap<>(AgentHistoryValue.camelizeObject(raw));
        Map<String, Object> fields = new LinkedHashMap<>();
        switch (operation) {
            case "status":
            case "init":
                fields.put("status", normalizeStatus(camel));
                break;
            case "sources":
                fields.put("sources", camel.containsKey("sources")
                        ? camel.get("sources")
                        : Collections.emptyList());
                break;
            case "import":
            case "sync":
                fields.put("import", camel);
                break;
            case "search":
                bridgeSearchPagination(camel);
                fields.put("search", camel);
                break;
            case "showEvent":
                fields.put("event", eventResult(camel));
                break;
            case "showSession":
                fields.put("session", pick(camel, "session", "events", "mode", "format"));
                break;
            default:
                Map<String, Object> error = new LinkedHashMap<>();
                error.put("code", "not_supported");
                error.put("message", "unsupported operation");
                error.put("retryable", Boolean.FALSE);
                fields.put("error", error);
                operation = "error";
                break;
        }
        return buildCanonical(operation, backend, fields);
    }

    private static Map<String, Object> normalizeStatus(Map<String, Object> current) {
        Map<String, Object> status = pick(
                current,
                "initialized", "dataRoot", "readOnly", "indexedItems", "indexedSessions",
                "indexedEvents", "indexedSources", "historyEpoch", "lexical", "refresh",
                "semantic", "daemon");
        if (!status.containsKey("initialized")) {
            Map<String, Object> lexical = AgentHistoryValue.objectOrNull(current.get("lexical"));
            status.put("initialized", Boolean.valueOf(
                    lexical != null && AgentHistoryValue.string(lexical.get("generationId")) != null));
        }
        status.put("localOnly", Boolean.TRUE);
        return status;
    }

    private static void bridgeSearchPagination(Map<String, Object> search) {
        if (search.containsKey("pagination")) {
            return;
        }
        Map<String, Object> resultWindow = AgentHistoryValue.objectOrNull(search.get("resultWindow"));
        if (resultWindow == null) {
            return;
        }
        Map<String, Object> pagination = new LinkedHashMap<>();
        if (resultWindow.containsKey("limit")) {
            pagination.put("limit", resultWindow.get("limit"));
        }
        if (resultWindow.containsKey("moreAvailable")) {
            pagination.put("hasMore", resultWindow.get("moreAvailable"));
        }
        search.put("pagination", pagination);
    }

    private static Map<String, Object> buildCanonical(
            String operation,
            Backend backend,
            Map<String, Object> fields) {
        Map<String, Object> canonical = new LinkedHashMap<>();
        canonical.put("contractVersion", CONTRACT_VERSION);
        canonical.put("schemaVersion", Integer.valueOf(SCHEMA_VERSION));
        canonical.put("operation", operation);
        canonical.put("backend", backend.asMap());
        canonical.putAll(fields);
        return AgentHistoryValue.copyObject(canonical);
    }

    private static Map<String, Object> eventResult(Map<String, Object> camel) {
        return pick(camel, "event", "events");
    }

    private static Map<String, Object> pick(Map<String, Object> raw, String... keys) {
        Map<String, Object> out = new LinkedHashMap<>();
        for (String key : keys) {
            if (raw.containsKey(key)) {
                out.put(key, raw.get(key));
            }
        }
        return out;
    }

    private static boolean isCommonField(String name) {
        return "contractVersion".equals(name)
                || "schemaVersion".equals(name)
                || "operation".equals(name)
                || "backend".equals(name);
    }
}
