package rs.ctx.agenthistory;

import java.nio.CharBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.LinkedHashMap;
import java.util.Map;

/** MCP server and tool identity represented by an agent-history event. */
public final class McpToolCall {
    static final int MAX_COMPONENT_BYTES = 64 * 1024;

    private final Map<String, Object> fields;
    private final String server;
    private final String tool;

    McpToolCall(Map<String, Object> fields) {
        for (String name : fields.keySet()) {
            if (!"server".equals(name) && !"tool".equals(name)) {
                throw protocolError("contains unknown member " + name, name, null);
            }
        }
        this.fields = AgentHistoryValue.copyObject(fields);
        this.server = requiredString(fields, "server");
        this.tool = requiredString(fields, "tool");
    }

    static McpToolCall from(Object value) {
        Map<String, Object> fields = AgentHistoryValue.objectOrNull(value);
        if (fields == null) {
            throw protocolError("must be an object when present", null, null);
        }
        return new McpToolCall(fields);
    }

    public String getServer() {
        return server;
    }

    public String server() {
        return server;
    }

    public String getTool() {
        return tool;
    }

    public String tool() {
        return tool;
    }

    public Map<String, Object> asMap() {
        return fields;
    }

    private static String requiredString(Map<String, Object> fields, String name) {
        Object value = fields.get(name);
        if (!(value instanceof String)) {
            throw protocolError("is missing required string field " + name, name, null);
        }
        String text = (String) value;
        if (text.isEmpty()) {
            throw protocolError("field " + name + " must be nonempty", name, null);
        }
        try {
            int decodedBytes = StandardCharsets.UTF_8.newEncoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .encode(CharBuffer.wrap(text))
                    .remaining();
            if (decodedBytes > MAX_COMPONENT_BYTES) {
                throw protocolError(
                        "field " + name + " exceeds " + MAX_COMPONENT_BYTES + " decoded UTF-8 bytes",
                        name,
                        null);
            }
        } catch (CharacterCodingException cause) {
            throw protocolError("field " + name + " contains an invalid Unicode string", name, cause);
        }
        return text;
    }

    private static CtxAgentHistoryException.Protocol protocolError(
            String message,
            String field,
            Throwable cause) {
        Map<String, Object> details = new LinkedHashMap<>();
        details.put("field", field == null ? "mcpToolCall" : "mcpToolCall." + field);
        return new CtxAgentHistoryException.Protocol(
                "agent-history-v1 MCP tool call " + message,
                details,
                cause);
    }
}
