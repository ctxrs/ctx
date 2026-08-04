package ctxagenthistory

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"strings"
	"unicode/utf8"
)

func normalizePayload(op Operation, payload []byte) ([]byte, error) {
	if err := rejectDuplicateJSONMembers(payload); err != nil {
		return nil, err
	}
	var raw any
	decoder := json.NewDecoder(bytes.NewReader(payload))
	decoder.UseNumber()
	if err := decoder.Decode(&raw); err != nil {
		return nil, err
	}
	if object, ok := raw.(map[string]any); ok {
		if _, hasContractVersion := object["contractVersion"]; hasContractVersion {
			return payload, nil
		}
	}

	operation := agentHistoryOperationName(op.Name)
	if operation == "showEvent" || operation == "showSession" {
		normalized, err := normalizeEventPayload(operation, raw)
		if err != nil {
			return nil, err
		}
		raw = normalized
	}
	envelope := map[string]any{
		"contractVersion": APIVersion,
		"schemaVersion":   SchemaVersion,
		"operation":       operation,
		"backend":         map[string]any{"kind": "local"},
	}
	camel := camelize(raw)

	switch operation {
	case "status":
		envelope["status"] = normalizeStatus(camel)
	case "init":
		envelope["status"] = normalizeStatus(camel)
	case "sources":
		envelope["sources"] = get(camel, "sources")
	case "import", "sync":
		envelope["import"] = camel
	case "search":
		bridgeSearchPagination(camel)
		envelope["search"] = camel
	case "showEvent":
		envelope["event"] = map[string]any{
			"event":  get(camel, "event"),
			"events": get(camel, "events"),
		}
	case "showSession":
		envelope["session"] = map[string]any{
			"session": get(camel, "session"),
			"events":  get(camel, "events"),
			"mode":    get(camel, "mode"),
			"format":  get(camel, "format"),
		}
	}

	return json.Marshal(envelope)
}

func normalizeEventPayload(operation string, value any) (any, error) {
	object, ok := value.(map[string]any)
	if !ok {
		return value, nil
	}
	out := make(map[string]any, len(object))
	for key, nested := range object {
		out[key] = nested
	}
	if operation == "showEvent" {
		if event, exists := object["event"]; exists {
			normalized, err := normalizeEventRecord(event)
			if err != nil {
				return nil, err
			}
			out["event"] = normalized
		}
	}
	if events, exists := object["events"]; exists {
		normalized, err := normalizeEventRecords(events)
		if err != nil {
			return nil, err
		}
		out["events"] = normalized
	}
	return out, nil
}

func normalizeEventRecords(value any) (any, error) {
	events, ok := value.([]any)
	if !ok {
		return value, nil
	}
	out := make([]any, len(events))
	for index, event := range events {
		normalized, err := normalizeEventRecord(event)
		if err != nil {
			return nil, err
		}
		out[index] = normalized
	}
	return out, nil
}

func normalizeEventRecord(value any) (any, error) {
	event, ok := value.(map[string]any)
	if !ok {
		return value, nil
	}
	snake, hasSnake := event["mcp_tool_call"]
	camel, hasCamel := event["mcpToolCall"]
	if hasSnake && hasCamel {
		return nil, fmt.Errorf("agent-history-v1 MCP tool call has duplicate outer wire aliases")
	}

	out := make(map[string]any, len(event))
	for key, nested := range event {
		if key == "mcp_tool_call" || key == "mcpToolCall" {
			continue
		}
		if snakeToCamel(key) == "mcpToolCall" {
			return nil, fmt.Errorf("event member %q collides with canonical mcpToolCall", key)
		}
		out[key] = nested
	}
	if hasSnake || hasCamel {
		call := snake
		if hasCamel {
			call = camel
		}
		normalized, err := normalizeMCPToolCall(call)
		if err != nil {
			return nil, err
		}
		out["mcpToolCall"] = normalized
	}
	return out, nil
}

func normalizeMCPToolCall(value any) (map[string]any, error) {
	call, ok := value.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("mcpToolCall must be an object when present")
	}
	if len(call) != 2 {
		return nil, fmt.Errorf("mcpToolCall requires exactly server and tool")
	}
	out := make(map[string]any, 2)
	for _, field := range []string{"server", "tool"} {
		component, ok := call[field].(string)
		if !ok {
			return nil, fmt.Errorf("mcpToolCall.%s must be a string", field)
		}
		if component == "" {
			return nil, fmt.Errorf("mcpToolCall.%s must be nonempty", field)
		}
		if !utf8.ValidString(component) {
			return nil, fmt.Errorf("mcpToolCall.%s contains invalid UTF-8", field)
		}
		if len(component) > maxMCPToolCallComponentBytes {
			return nil, fmt.Errorf(
				"mcpToolCall.%s exceeds %d decoded UTF-8 bytes",
				field,
				maxMCPToolCallComponentBytes,
			)
		}
		out[field] = component
	}
	return out, nil
}

func rejectDuplicateJSONMembers(payload []byte) error {
	decoder := json.NewDecoder(bytes.NewReader(payload))
	decoder.UseNumber()
	if err := scanExactJSONValue(decoder); err != nil {
		return err
	}
	var trailing any
	if err := decoder.Decode(&trailing); err != io.EOF {
		if err == nil {
			return fmt.Errorf("trailing JSON data")
		}
		return err
	}
	return nil
}

func scanExactJSONValue(decoder *json.Decoder) error {
	token, err := decoder.Token()
	if err != nil {
		return err
	}
	delimiter, ok := token.(json.Delim)
	if !ok {
		return nil
	}
	switch delimiter {
	case '{':
		members := make(map[string]struct{})
		for decoder.More() {
			keyToken, err := decoder.Token()
			if err != nil {
				return err
			}
			key, ok := keyToken.(string)
			if !ok {
				return fmt.Errorf("JSON object member name must be a string")
			}
			if _, duplicate := members[key]; duplicate {
				return fmt.Errorf("duplicate JSON object member %q", key)
			}
			members[key] = struct{}{}
			if err := scanExactJSONValue(decoder); err != nil {
				return err
			}
		}
		closing, err := decoder.Token()
		if err != nil {
			return err
		}
		if closing != json.Delim('}') {
			return fmt.Errorf("expected JSON object end")
		}
	case '[':
		for decoder.More() {
			if err := scanExactJSONValue(decoder); err != nil {
				return err
			}
		}
		closing, err := decoder.Token()
		if err != nil {
			return err
		}
		if closing != json.Delim(']') {
			return fmt.Errorf("expected JSON array end")
		}
	default:
		return fmt.Errorf("unexpected JSON delimiter %q", delimiter)
	}
	return nil
}

func normalizeStatus(value any) map[string]any {
	status, _ := value.(map[string]any)
	out := map[string]any{"localOnly": true}
	for _, key := range []string{
		"dataRoot", "indexedItems", "indexedSessions", "indexedEvents", "indexedSources",
		"historyEpoch", "lexical", "refresh", "semantic", "daemon", "readOnly",
	} {
		if nested, exists := status[key]; exists {
			out[key] = nested
		}
	}
	lexical, _ := status["lexical"].(map[string]any)
	generationID, _ := lexical["generationId"].(string)
	initialized, exact := status["initialized"].(bool)
	if !exact {
		initialized = generationID != ""
	}
	out["initialized"] = initialized
	return out
}

func bridgeSearchPagination(value any) {
	search, ok := value.(map[string]any)
	if !ok {
		return
	}
	if _, exists := search["pagination"]; exists {
		return
	}
	resultWindow, ok := search["resultWindow"].(map[string]any)
	if !ok {
		return
	}
	pagination := map[string]any{}
	if limit, exists := resultWindow["limit"]; exists {
		pagination["limit"] = limit
	}
	if moreAvailable, exists := resultWindow["moreAvailable"]; exists {
		pagination["hasMore"] = moreAvailable
	}
	search["pagination"] = pagination
}

func agentHistoryOperationName(name string) string {
	switch name {
	case "show_event":
		return "showEvent"
	case "show_session":
		return "showSession"
	case "setup":
		return "init"
	default:
		return name
	}
}

func camelize(value any) any {
	switch typed := value.(type) {
	case map[string]any:
		out := make(map[string]any, len(typed))
		for key, nested := range typed {
			camelKey := snakeToCamel(key)
			if camelKey == "configPath" || camelKey == "itemType" || camelKey == "payloadType" || camelKey == "recordType" {
				continue
			}
			out[camelKey] = camelize(nested)
		}
		return out
	case []any:
		out := make([]any, len(typed))
		for i, nested := range typed {
			out[i] = camelize(nested)
		}
		return out
	default:
		return value
	}
}

func snakeToCamel(value string) string {
	if !strings.Contains(value, "_") {
		return value
	}
	parts := strings.Split(value, "_")
	out := parts[0]
	for _, part := range parts[1:] {
		if part == "" {
			continue
		}
		out += strings.ToUpper(part[:1]) + part[1:]
	}
	return out
}

func get(value any, key string) any {
	object, ok := value.(map[string]any)
	if !ok {
		return nil
	}
	return object[key]
}
