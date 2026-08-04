package ctxagenthistory

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"strconv"
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
	snakeExchange, hasSnakeExchange := event["mcp_exchange"]
	camelExchange, hasCamelExchange := event["mcpExchange"]
	if hasSnakeExchange && hasCamelExchange {
		return nil, fmt.Errorf("agent-history-v1 MCP exchange has duplicate outer wire aliases")
	}

	out := make(map[string]any, len(event))
	for key, nested := range event {
		if key == "mcp_tool_call" || key == "mcpToolCall" || key == "mcp_exchange" || key == "mcpExchange" {
			continue
		}
		if snakeToCamel(key) == "mcpToolCall" {
			return nil, fmt.Errorf("event member %q collides with canonical mcpToolCall", key)
		}
		if snakeToCamel(key) == "mcpExchange" {
			return nil, fmt.Errorf("event member %q collides with canonical mcpExchange", key)
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
	if hasSnakeExchange || hasCamelExchange {
		exchange := snakeExchange
		if hasCamelExchange {
			exchange = camelExchange
		}
		normalized, err := normalizeMCPExchange(exchange)
		if err != nil {
			return nil, err
		}
		out["mcpExchange"] = normalized
		if response, ok := normalized["response"].(map[string]any); ok {
			if textCapture, ok := response["text"].(map[string]any); ok && textCapture["captureStatus"] == "normalized_body" {
				text, ok := event["text"].(string)
				if !ok || text == "" {
					return nil, fmt.Errorf("normalized MCP response body requires nonempty event text")
				}
			}
		}
	}
	return out, nil
}

type opaqueCapturedJSON struct{ value any }

func (value opaqueCapturedJSON) MarshalJSON() ([]byte, error) { return json.Marshal(value.value) }

func normalizeMCPExchange(value any) (map[string]any, error) {
	exchange, err := normalizeClosedMCPObject(value, "exchange", map[string]string{
		"provider_call_id": "providerCallId", "providerCallId": "providerCallId",
		"invocation": "invocation", "response": "response",
	})
	if err != nil {
		return nil, err
	}
	providerCallID, err := validateMCPExchangeIdentity(exchange["providerCallId"], "providerCallId")
	if err != nil {
		return nil, err
	}
	exchange["providerCallId"] = providerCallID
	if _, invocation := exchange["invocation"]; !invocation {
		if _, response := exchange["response"]; !response {
			return nil, fmt.Errorf("mcpExchange requires invocation, response, or both")
		}
	}
	if invocation, exists := exchange["invocation"]; exists {
		exchange["invocation"], err = normalizeMCPInvocation(invocation)
		if err != nil {
			return nil, err
		}
	}
	if response, exists := exchange["response"]; exists {
		exchange["response"], err = normalizeMCPResponse(response)
		if err != nil {
			return nil, err
		}
	}
	return exchange, nil
}

func normalizeMCPInvocation(value any) (map[string]any, error) {
	invocation, err := normalizeClosedMCPObject(value, "invocation", map[string]string{
		"server": "server", "tool": "tool", "arguments": "arguments",
	})
	if err != nil {
		return nil, err
	}
	if err := requireExactMCPMembers(invocation, "invocation", "arguments", "server", "tool"); err != nil {
		return nil, err
	}
	if invocation["server"], err = validateMCPExchangeIdentity(invocation["server"], "invocation.server"); err != nil {
		return nil, err
	}
	if invocation["tool"], err = validateMCPExchangeIdentity(invocation["tool"], "invocation.tool"); err != nil {
		return nil, err
	}
	invocation["arguments"], err = normalizeMCPCapture(invocation["arguments"], "invocation.arguments", true, false)
	return invocation, err
}

func normalizeMCPResponse(value any) (map[string]any, error) {
	response, err := normalizeClosedMCPObject(value, "response", map[string]string{
		"status": "status", "failure_kind": "failureKind", "failureKind": "failureKind",
		"duration_ns": "durationNs", "durationNs": "durationNs", "text": "text", "payload": "payload",
	})
	if err != nil {
		return nil, err
	}
	for _, required := range []string{"status", "text", "payload"} {
		if _, exists := response[required]; !exists {
			return nil, fmt.Errorf("mcpExchange response requires %s", required)
		}
	}
	status, ok := response["status"].(string)
	if !ok || !stringIn(status, "succeeded", "failed", "cancelled", "timed_out", "unknown") {
		return nil, fmt.Errorf("mcpExchange response.status is invalid")
	}
	if status == "failed" {
		failure, ok := response["failureKind"].(string)
		if !ok || !stringIn(failure, "tool_reported", "invocation", "unknown") {
			return nil, fmt.Errorf("failed mcpExchange response requires failureKind")
		}
	} else if _, exists := response["failureKind"]; exists {
		return nil, fmt.Errorf("mcpExchange failureKind is only valid for failed responses")
	}
	if duration, exists := response["durationNs"]; exists {
		if err := validateMCPSafeInteger(duration, "response.durationNs"); err != nil {
			return nil, err
		}
	}
	response["text"], err = normalizeMCPCapture(response["text"], "response.text", false, true)
	if err != nil {
		return nil, err
	}
	response["payload"], err = normalizeMCPCapture(response["payload"], "response.payload", false, false)
	return response, err
}

func normalizeMCPCapture(value any, context string, argumentsCapture, textCapture bool) (map[string]any, error) {
	capture, err := normalizeClosedMCPObject(value, context, map[string]string{
		"capture_status": "captureStatus", "captureStatus": "captureStatus", "value": "value",
		"reason": "reason", "observed_encoded_bytes": "observedEncodedBytes", "observedEncodedBytes": "observedEncodedBytes",
	})
	if err != nil {
		return nil, err
	}
	status, _ := capture["captureStatus"].(string)
	switch status {
	case "present":
		if textCapture {
			return nil, fmt.Errorf("%s cannot use present", context)
		}
		if err := requireExactMCPMembers(capture, context, "captureStatus", "value"); err != nil {
			return nil, err
		}
		if argumentsCapture {
			if _, ok := capture["value"].(map[string]any); !ok {
				return nil, fmt.Errorf("present MCP invocation arguments must be a JSON object")
			}
		}
		capture["value"] = opaqueCapturedJSON{value: capture["value"]}
	case "normalized_body":
		if !textCapture {
			return nil, fmt.Errorf("%s cannot use normalized_body", context)
		}
		if err := requireExactMCPMembers(capture, context, "captureStatus"); err != nil {
			return nil, err
		}
	case "absent", "unavailable":
		if err := requireExactMCPMembers(capture, context, "captureStatus"); err != nil {
			return nil, err
		}
	case "omitted":
		if capture["reason"] != "size_limit" {
			return nil, fmt.Errorf("%s.reason must be size_limit", context)
		}
		expected := []string{"captureStatus", "reason"}
		if observed, exists := capture["observedEncodedBytes"]; exists {
			if err := validateMCPSafeInteger(observed, context+".observedEncodedBytes"); err != nil {
				return nil, err
			}
			expected = append(expected, "observedEncodedBytes")
		}
		if err := requireExactMCPMembers(capture, context, expected...); err != nil {
			return nil, err
		}
	default:
		return nil, fmt.Errorf("%s.captureStatus is invalid", context)
	}
	return capture, nil
}

func normalizeClosedMCPObject(value any, context string, aliases map[string]string) (map[string]any, error) {
	object, ok := value.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("mcpExchange %s must be an object", context)
	}
	out := make(map[string]any, len(object))
	for key, nested := range object {
		canonical, known := aliases[key]
		if !known {
			return nil, fmt.Errorf("mcpExchange %s contains unknown member %q", context, key)
		}
		if _, collision := out[canonical]; collision {
			return nil, fmt.Errorf("mcpExchange %s contains colliding aliases for %s", context, canonical)
		}
		out[canonical] = nested
	}
	return out, nil
}

func requireExactMCPMembers(value map[string]any, context string, expected ...string) error {
	if len(value) != len(expected) {
		return fmt.Errorf("mcpExchange %s has invalid members", context)
	}
	for _, key := range expected {
		if _, exists := value[key]; !exists {
			return fmt.Errorf("mcpExchange %s requires %s", context, key)
		}
	}
	return nil
}

func validateMCPExchangeIdentity(value any, field string) (string, error) {
	text, ok := value.(string)
	if !ok || text == "" {
		return "", fmt.Errorf("mcpExchange %s must be a nonempty string", field)
	}
	if !utf8.ValidString(text) || len(text) > maxMCPExchangeIdentityBytes {
		return "", fmt.Errorf("mcpExchange %s exceeds the valid 64 KiB UTF-8 domain", field)
	}
	return text, nil
}

func validateMCPSafeInteger(value any, field string) error {
	var number uint64
	switch typed := value.(type) {
	case json.Number:
		parsed, err := parseExactUint64(string(typed))
		if err != nil {
			return fmt.Errorf("mcpExchange %s is outside the exact JSON integer domain", field)
		}
		number = parsed
	case uint64:
		number = typed
	case int:
		if typed < 0 {
			return fmt.Errorf("mcpExchange %s is outside the exact JSON integer domain", field)
		}
		number = uint64(typed)
	default:
		return fmt.Errorf("mcpExchange %s is outside the exact JSON integer domain", field)
	}
	if number > MaxSafeInteger {
		return fmt.Errorf("mcpExchange %s exceeds maximum %d", field, MaxSafeInteger)
	}
	return nil
}

func parseExactUint64(value string) (uint64, error) {
	if value == "" || (len(value) > 1 && value[0] == '0') || strings.ContainsAny(value, ".eE-") {
		return 0, fmt.Errorf("not an unsigned integer")
	}
	return strconv.ParseUint(value, 10, 64)
}

func stringIn(value string, choices ...string) bool {
	for _, choice := range choices {
		if value == choice {
			return true
		}
	}
	return false
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
