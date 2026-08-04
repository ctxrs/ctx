package rs.ctx.agenthistory;

import java.math.BigDecimal;
import java.math.BigInteger;
import java.nio.CharBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.LinkedHashMap;
import java.util.Map;

/** Content-governed MCP invocation and response capture. */
public final class McpExchange {
    public static final int MAX_IDENTITY_BYTES = 64 * 1024;
    public static final long MAX_SAFE_INTEGER = 9_007_199_254_740_991L;

    private final Map<String, Object> fields;
    private final String providerCallId;
    private final Invocation invocation;
    private final Response response;

    McpExchange(Map<String, Object> fields) {
        requireAllowed(fields, "exchange", "providerCallId", "invocation", "response");
        this.providerCallId = requiredIdentity(fields.get("providerCallId"), "providerCallId");
        this.invocation = fields.containsKey("invocation")
                ? Invocation.from(fields.get("invocation"))
                : null;
        this.response = fields.containsKey("response")
                ? Response.from(fields.get("response"))
                : null;
        if (invocation == null && response == null) {
            throw protocolError("requires invocation, response, or both", null);
        }
        this.fields = AgentHistoryValue.copyObject(fields);
    }

    static McpExchange from(Object value) {
        Map<String, Object> fields = AgentHistoryValue.objectOrNull(value);
        if (fields == null) {
            throw protocolError("must be an object when present", null);
        }
        return new McpExchange(fields);
    }

    static Map<String, Object> normalizeWire(Object value) {
        Map<String, Object> exchange = normalizeObject(
                value,
                "exchange",
                aliases(
                        "provider_call_id", "providerCallId",
                        "providerCallId", "providerCallId",
                        "invocation", "invocation",
                        "response", "response"));
        if (exchange.containsKey("invocation")) {
            exchange.put("invocation", normalizeInvocation(exchange.get("invocation")));
        }
        if (exchange.containsKey("response")) {
            exchange.put("response", normalizeResponse(exchange.get("response")));
        }
        return new McpExchange(exchange).asMap();
    }

    public String getProviderCallId() {
        return providerCallId;
    }

    public String providerCallId() {
        return providerCallId;
    }

    public Invocation getInvocation() {
        return invocation;
    }

    public Invocation invocation() {
        return invocation;
    }

    public Response getResponse() {
        return response;
    }

    public Response response() {
        return response;
    }

    public Map<String, Object> asMap() {
        return fields;
    }

    public enum ResponseStatus {
        SUCCEEDED("succeeded"),
        FAILED("failed"),
        CANCELLED("cancelled"),
        TIMED_OUT("timed_out"),
        UNKNOWN("unknown");

        private final String wireValue;

        ResponseStatus(String wireValue) {
            this.wireValue = wireValue;
        }

        public String wireValue() {
            return wireValue;
        }
    }

    public enum FailureKind {
        TOOL_REPORTED("tool_reported"),
        INVOCATION("invocation"),
        UNKNOWN("unknown");

        private final String wireValue;

        FailureKind(String wireValue) {
            this.wireValue = wireValue;
        }

        public String wireValue() {
            return wireValue;
        }
    }

    public enum JsonCaptureStatus {
        PRESENT("present"),
        ABSENT("absent"),
        UNAVAILABLE("unavailable"),
        OMITTED("omitted");

        private final String wireValue;

        JsonCaptureStatus(String wireValue) {
            this.wireValue = wireValue;
        }

        public String wireValue() {
            return wireValue;
        }
    }

    public enum TextCaptureStatus {
        NORMALIZED_BODY("normalized_body"),
        ABSENT("absent"),
        UNAVAILABLE("unavailable"),
        OMITTED("omitted");

        private final String wireValue;

        TextCaptureStatus(String wireValue) {
            this.wireValue = wireValue;
        }

        public String wireValue() {
            return wireValue;
        }
    }

    public enum OmissionReason {
        SIZE_LIMIT("size_limit");

        private final String wireValue;

        OmissionReason(String wireValue) {
            this.wireValue = wireValue;
        }

        public String wireValue() {
            return wireValue;
        }
    }

    public static final class Invocation {
        private final Map<String, Object> fields;
        private final String server;
        private final String tool;
        private final JsonCapture arguments;

        private Invocation(Map<String, Object> fields) {
            requireExactly(fields, "invocation", "server", "tool", "arguments");
            server = requiredIdentity(fields.get("server"), "invocation.server");
            tool = requiredIdentity(fields.get("tool"), "invocation.tool");
            arguments = JsonCapture.from(fields.get("arguments"), "invocation.arguments");
            if (arguments.status == JsonCaptureStatus.PRESENT
                    && !(arguments.value instanceof Map<?, ?>)) {
                throw protocolError("present invocation arguments must be a JSON object", null);
            }
            this.fields = AgentHistoryValue.copyObject(fields);
        }

        static Invocation from(Object value) {
            return new Invocation(requiredObject(value, "invocation"));
        }

        public String getServer() { return server; }
        public String server() { return server; }
        public String getTool() { return tool; }
        public String tool() { return tool; }
        public JsonCapture getArguments() { return arguments; }
        public JsonCapture arguments() { return arguments; }
        public Map<String, Object> asMap() { return fields; }
    }

    public static final class Response {
        private final Map<String, Object> fields;
        private final ResponseStatus status;
        private final FailureKind failureKind;
        private final Long durationNs;
        private final TextCapture text;
        private final JsonCapture payload;

        private Response(Map<String, Object> fields) {
            requireAllowed(fields, "response", "status", "failureKind", "durationNs", "text", "payload");
            requireMembers(fields, "response", "status", "text", "payload");
            status = enumValue(ResponseStatus.values(), fields.get("status"), "response.status");
            failureKind = fields.containsKey("failureKind")
                    ? enumValue(FailureKind.values(), fields.get("failureKind"), "response.failureKind")
                    : null;
            if ((status == ResponseStatus.FAILED) != (failureKind != null)) {
                throw protocolError("failureKind must be present exactly for failed responses", null);
            }
            durationNs = fields.containsKey("durationNs")
                    ? safeInteger(fields.get("durationNs"), "response.durationNs")
                    : null;
            text = TextCapture.from(fields.get("text"));
            payload = JsonCapture.from(fields.get("payload"), "response.payload");
            this.fields = AgentHistoryValue.copyObject(fields);
        }

        static Response from(Object value) {
            return new Response(requiredObject(value, "response"));
        }

        public ResponseStatus getStatus() { return status; }
        public ResponseStatus status() { return status; }
        public FailureKind getFailureKind() { return failureKind; }
        public FailureKind failureKind() { return failureKind; }
        public Long getDurationNs() { return durationNs; }
        public Long durationNs() { return durationNs; }
        public TextCapture getText() { return text; }
        public TextCapture text() { return text; }
        public JsonCapture getPayload() { return payload; }
        public JsonCapture payload() { return payload; }
        public Map<String, Object> asMap() { return fields; }
    }

    public static final class JsonCapture {
        private final Map<String, Object> fields;
        private final JsonCaptureStatus status;
        private final Object value;
        private final OmissionReason reason;
        private final Long observedEncodedBytes;

        private JsonCapture(Map<String, Object> fields, String context) {
            status = enumValue(JsonCaptureStatus.values(), fields.get("captureStatus"), context + ".captureStatus");
            if (status == JsonCaptureStatus.PRESENT) {
                requireExactly(fields, context, "captureStatus", "value");
                value = AgentHistoryValue.copy(fields.get("value"));
                reason = null;
                observedEncodedBytes = null;
            } else if (status == JsonCaptureStatus.OMITTED) {
                requireAllowed(fields, context, "captureStatus", "reason", "observedEncodedBytes");
                requireMembers(fields, context, "captureStatus", "reason");
                value = null;
                reason = enumValue(OmissionReason.values(), fields.get("reason"), context + ".reason");
                observedEncodedBytes = fields.containsKey("observedEncodedBytes")
                        ? safeInteger(fields.get("observedEncodedBytes"), context + ".observedEncodedBytes")
                        : null;
            } else {
                requireExactly(fields, context, "captureStatus");
                value = null;
                reason = null;
                observedEncodedBytes = null;
            }
            this.fields = AgentHistoryValue.copyObject(fields);
        }

        static JsonCapture from(Object value, String context) {
            return new JsonCapture(requiredObject(value, context), context);
        }

        public JsonCaptureStatus getCaptureStatus() { return status; }
        public JsonCaptureStatus captureStatus() { return status; }
        public Object getValue() { return value; }
        public Object value() { return value; }
        public OmissionReason getReason() { return reason; }
        public Long getObservedEncodedBytes() { return observedEncodedBytes; }
        public Map<String, Object> asMap() { return fields; }
    }

    public static final class TextCapture {
        private final Map<String, Object> fields;
        private final TextCaptureStatus status;
        private final OmissionReason reason;
        private final Long observedEncodedBytes;

        private TextCapture(Map<String, Object> fields) {
            status = enumValue(TextCaptureStatus.values(), fields.get("captureStatus"), "response.text.captureStatus");
            if (status == TextCaptureStatus.OMITTED) {
                requireAllowed(fields, "response.text", "captureStatus", "reason", "observedEncodedBytes");
                requireMembers(fields, "response.text", "captureStatus", "reason");
                reason = enumValue(OmissionReason.values(), fields.get("reason"), "response.text.reason");
                observedEncodedBytes = fields.containsKey("observedEncodedBytes")
                        ? safeInteger(fields.get("observedEncodedBytes"), "response.text.observedEncodedBytes")
                        : null;
            } else {
                requireExactly(fields, "response.text", "captureStatus");
                reason = null;
                observedEncodedBytes = null;
            }
            this.fields = AgentHistoryValue.copyObject(fields);
        }

        static TextCapture from(Object value) {
            return new TextCapture(requiredObject(value, "response.text"));
        }

        public TextCaptureStatus getCaptureStatus() { return status; }
        public TextCaptureStatus captureStatus() { return status; }
        public OmissionReason getReason() { return reason; }
        public Long getObservedEncodedBytes() { return observedEncodedBytes; }
        public Map<String, Object> asMap() { return fields; }
    }

    private static Map<String, Object> normalizeInvocation(Object value) {
        Map<String, Object> invocation = normalizeObject(
                value, "invocation", aliases("server", "server", "tool", "tool", "arguments", "arguments"));
        if (invocation.containsKey("arguments")) {
            invocation.put("arguments", normalizeCapture(invocation.get("arguments"), "invocation.arguments", false));
        }
        return invocation;
    }

    private static Map<String, Object> normalizeResponse(Object value) {
        Map<String, Object> response = normalizeObject(
                value,
                "response",
                aliases(
                        "status", "status",
                        "failure_kind", "failureKind",
                        "failureKind", "failureKind",
                        "duration_ns", "durationNs",
                        "durationNs", "durationNs",
                        "text", "text",
                        "payload", "payload"));
        if (response.containsKey("text")) {
            response.put("text", normalizeCapture(response.get("text"), "response.text", true));
        }
        if (response.containsKey("payload")) {
            response.put("payload", normalizeCapture(response.get("payload"), "response.payload", false));
        }
        return response;
    }

    private static Map<String, Object> normalizeCapture(Object value, String context, boolean text) {
        Map<String, Object> capture = normalizeObject(
                value,
                context,
                aliases(
                        "capture_status", "captureStatus",
                        "captureStatus", "captureStatus",
                        "value", "value",
                        "reason", "reason",
                        "observed_encoded_bytes", "observedEncodedBytes",
                        "observedEncodedBytes", "observedEncodedBytes"));
        if (text) {
            new TextCapture(capture);
        } else {
            new JsonCapture(capture, context);
        }
        return capture;
    }

    private static Map<String, Object> normalizeObject(
            Object value, String context, Map<String, String> aliases) {
        Map<String, Object> input = AgentHistoryValue.objectOrNull(value);
        if (input == null) {
            throw protocolError(context + " must be an object", null);
        }
        Map<String, Object> out = new LinkedHashMap<>();
        for (Map.Entry<String, Object> entry : input.entrySet()) {
            String canonical = aliases.get(entry.getKey());
            if (canonical == null) {
                throw protocolError(context + " contains unknown member " + entry.getKey(), null);
            }
            if (out.containsKey(canonical)) {
                throw protocolError(context + " contains colliding aliases for " + canonical, null);
            }
            out.put(canonical, AgentHistoryValue.copy(entry.getValue()));
        }
        return out;
    }

    private static Map<String, String> aliases(String... entries) {
        Map<String, String> aliases = new LinkedHashMap<>();
        for (int index = 0; index < entries.length; index += 2) {
            aliases.put(entries[index], entries[index + 1]);
        }
        return aliases;
    }

    private static Map<String, Object> requiredObject(Object value, String context) {
        Map<String, Object> object = AgentHistoryValue.objectOrNull(value);
        if (object == null) {
            throw protocolError(context + " must be an object", null);
        }
        return object;
    }

    private static void requireExactly(Map<String, Object> value, String context, String... fields) {
        requireAllowed(value, context, fields);
        requireMembers(value, context, fields);
    }

    private static void requireAllowed(Map<String, Object> value, String context, String... fields) {
        java.util.Set<String> allowed = java.util.Set.of(fields);
        for (String field : value.keySet()) {
            if (!allowed.contains(field)) {
                throw protocolError(context + " contains unknown member " + field, null);
            }
        }
    }

    private static void requireMembers(Map<String, Object> value, String context, String... fields) {
        for (String field : fields) {
            if (!value.containsKey(field)) {
                throw protocolError(context + " requires " + field, null);
            }
        }
    }

    private static String requiredIdentity(Object value, String field) {
        if (!(value instanceof String) || ((String) value).isEmpty()) {
            throw protocolError(field + " must be a nonempty string", null);
        }
        String text = (String) value;
        try {
            int bytes = StandardCharsets.UTF_8.newEncoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .encode(CharBuffer.wrap(text))
                    .remaining();
            if (bytes > MAX_IDENTITY_BYTES) {
                throw protocolError(field + " exceeds " + MAX_IDENTITY_BYTES + " decoded UTF-8 bytes", null);
            }
        } catch (CharacterCodingException cause) {
            throw protocolError(field + " contains invalid Unicode", cause);
        }
        return text;
    }

    private static Long safeInteger(Object value, String field) {
        BigInteger integer;
        try {
            if (value instanceof BigDecimal) {
                integer = ((BigDecimal) value).toBigIntegerExact();
            } else if (value instanceof Byte || value instanceof Short
                    || value instanceof Integer || value instanceof Long) {
                integer = BigInteger.valueOf(((Number) value).longValue());
            } else {
                throw new ArithmeticException();
            }
        } catch (ArithmeticException ignored) {
            throw protocolError(field + " is outside the exact JSON integer domain", null);
        }
        if (integer.signum() < 0 || integer.compareTo(BigInteger.valueOf(MAX_SAFE_INTEGER)) > 0) {
            throw protocolError(field + " is outside the exact JSON integer domain", null);
        }
        return Long.valueOf(integer.longValue());
    }

    private static <T extends Enum<T>> T enumValue(T[] values, Object value, String field) {
        if (!(value instanceof String)) {
            throw protocolError(field + " must be a string", null);
        }
        for (T candidate : values) {
            if (candidate instanceof ResponseStatus
                    && ((ResponseStatus) candidate).wireValue().equals(value)
                    || candidate instanceof FailureKind
                    && ((FailureKind) candidate).wireValue().equals(value)
                    || candidate instanceof JsonCaptureStatus
                    && ((JsonCaptureStatus) candidate).wireValue().equals(value)
                    || candidate instanceof TextCaptureStatus
                    && ((TextCaptureStatus) candidate).wireValue().equals(value)
                    || candidate instanceof OmissionReason
                    && ((OmissionReason) candidate).wireValue().equals(value)) {
                return candidate;
            }
        }
        throw protocolError(field + " has unknown value " + value, null);
    }

    static CtxAgentHistoryException.Protocol protocolError(String message, Throwable cause) {
        Map<String, Object> details = new LinkedHashMap<>();
        details.put("field", "mcpExchange");
        return new CtxAgentHistoryException.Protocol(
                "agent-history-v1 MCP exchange " + message,
                details,
                cause);
    }
}
