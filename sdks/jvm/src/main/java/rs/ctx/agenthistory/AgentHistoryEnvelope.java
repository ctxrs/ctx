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
                if (!camel.containsKey("initialized")) {
                    Object mode = camel.get("mode");
                    camel.put("initialized", Boolean.valueOf("ready".equals(mode) || "catalog_only".equals(mode) || mode == null));
                }
                if (!camel.containsKey("localOnly")) {
                    camel.put("localOnly", Boolean.TRUE);
                }
                fields.put("status", camel);
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
                fields.put("search", normalizeSearch(raw));
                break;
            case "showEvent":
                fields.put("event", eventResult(camel));
                break;
            case "showSession":
                fields.put("session", pick(camel, "session", "events", "source", "mode", "format"));
                break;
            case "locateEvent":
            case "locateSession":
                fields.put("location", pick(camel,
                        "ctxSessionId",
                        "ctxEventId",
                        "provider",
                        "providerSessionId",
                        "source",
                        "resume"));
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
        Map<String, Object> out = pick(camel, "event", "events", "source");
        if (out.get("source") == null) {
            Map<String, Object> event = AgentHistoryValue.objectOrNull(camel.get("event"));
            if (event != null) {
                out.put("source", event.get("source"));
            }
        }
        return out;
    }

    private static Map<String, Object> normalizeSearch(Map<String, Object> raw) {
        Integer schema = AgentHistoryValue.integer(raw.get("schema_version"));
        if (schema == null || schema.intValue() != 2) {
            Map<String,Object> details = new LinkedHashMap<>();
            details.put("expectedSchemaVersion", Integer.valueOf(2)); details.put("actualSchemaVersion", schema);
            throw new CtxAgentHistoryException.Protocol("ctx search returned an unsupported schema version", details, null);
        }
        Object rawQuery = raw.get("query");
        SearchQuery query = null;
        if (rawQuery != null) {
            Map<String,Object> queryMap = AgentHistoryValue.objectOrNull(rawQuery);
            if (queryMap == null) {
                Map<String,Object> details = new LinkedHashMap<>(); details.put("field", "query");
                throw new CtxAgentHistoryException.Protocol("ctx search response contains a non-object canonical query", details, null);
            }
            try {
                query = SearchQuery.fromMap(queryMap);
            } catch (CtxAgentHistoryException.Validation error) {
                Map<String,Object> details = new LinkedHashMap<>(); details.put("field", "query"); details.put("validation", error.getMessage());
                throw new CtxAgentHistoryException.Protocol("ctx search returned an invalid canonical query", details, error);
            }
        }
        Map<String,Object> execution = AgentHistoryValue.objectOrNull(raw.get("query_execution"));
        if (execution == null) {
            Map<String,Object> details = new LinkedHashMap<>(); details.put("field", "query_execution");
            throw new CtxAgentHistoryException.Protocol("ctx search response is missing query execution diagnostics", details, null);
        }
        Map<String,Object> search = new LinkedHashMap<>(AgentHistoryValue.camelizeObject(raw));
        search.remove("schemaVersion"); search.remove("queryExecution");
        search.put("schema_version", Integer.valueOf(2));
        search.put("query", query == null ? null : query.asMap());
        search.put("query_execution", AgentHistoryValue.copyObject(execution));
        return AgentHistoryValue.copyObject(search);
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
