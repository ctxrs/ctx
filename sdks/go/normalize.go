package ctxagenthistory

import (
	"encoding/json"
	"strings"
)

func normalizePayload(op Operation, payload []byte) ([]byte, error) {
	var raw any
	if err := json.Unmarshal(payload, &raw); err != nil {
		return nil, err
	}
	if object, ok := raw.(map[string]any); ok {
		if _, hasContractVersion := object["contractVersion"]; hasContractVersion {
			return payload, nil
		}
	}

	operation := agentHistoryOperationName(op.Name)
	envelope := map[string]any{
		"contractVersion": APIVersion,
		"schemaVersion":   SchemaVersion,
		"operation":       operation,
		"backend":         map[string]any{"kind": "local"},
	}
	camel := camelize(raw)

	switch operation {
	case "status":
		envelope["status"] = camel
	case "init":
		envelope["status"] = camel
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
			if camelKey == "databasePath" || camelKey == "configPath" || camelKey == "itemType" || camelKey == "payloadType" || camelKey == "recordType" {
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
