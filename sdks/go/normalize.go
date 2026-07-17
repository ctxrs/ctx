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
	rawObject, _ := raw.(map[string]any)
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
		envelope["search"] = normalizeSearchPayload(rawObject)
	case "showEvent":
		envelope["event"] = map[string]any{
			"event":  normalizeEventRecord(get(camel, "event")),
			"events": normalizeEventRecords(get(camel, "events")),
			"source": sourceLocationFromShow(rawObject),
		}
	case "showSession":
		envelope["session"] = map[string]any{
			"session": get(camel, "session"),
			"events":  normalizeEventRecords(get(camel, "events")),
			"source":  get(camel, "source"),
			"mode":    get(camel, "mode"),
			"format":  get(camel, "format"),
		}
	case "locateEvent", "locateSession":
		envelope["location"] = locationFromLocate(camel)
	}

	return json.Marshal(envelope)
}

func normalizeSearchPayload(raw map[string]any) map[string]any {
	normalized, _ := camelize(raw).(map[string]any)
	delete(normalized, "schemaVersion")
	delete(normalized, "queryExecution")
	if value, ok := raw["schema_version"]; ok {
		normalized["schema_version"] = value
	}
	if value, ok := raw["query"]; ok {
		normalized["query"] = value
	}
	if value, ok := raw["query_execution"]; ok {
		normalized["query_execution"] = value
	}
	return normalized
}

func agentHistoryOperationName(name string) string {
	switch name {
	case "show_event":
		return "showEvent"
	case "show_session":
		return "showSession"
	case "locate_event":
		return "locateEvent"
	case "locate_session":
		return "locateSession"
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
			if omittedPublicKey(camelKey) {
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

func omittedPublicKey(key string) bool {
	switch key {
	case "databasePath", "configPath", "itemType", "payloadType", "recordType",
		"semanticWeight", "semanticFallbackCode", "semanticFallback":
		return true
	default:
		return false
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

func normalizeEventRecord(value any) any {
	object, ok := value.(map[string]any)
	if !ok {
		return value
	}
	source := object["source"]
	switch typed := source.(type) {
	case nil:
	case string:
	default:
		if label := sourceName(typed); label != "" {
			object["source"] = label
		} else {
			delete(object, "source")
		}
	}
	return object
}

func normalizeEventRecords(value any) any {
	events, ok := value.([]any)
	if !ok {
		return value
	}
	for i, event := range events {
		events[i] = normalizeEventRecord(event)
	}
	return events
}

func sourceName(value any) string {
	object, ok := value.(map[string]any)
	if !ok {
		return ""
	}
	for _, key := range []string{"provider", "source", "kind"} {
		if name, ok := object[key].(string); ok {
			return name
		}
	}
	return ""
}

func locationFromLocate(camel any) map[string]any {
	object, _ := camel.(map[string]any)
	return map[string]any{
		"ctxSessionId":      object["ctxSessionId"],
		"ctxEventId":        object["ctxEventId"],
		"provider":          object["provider"],
		"providerSessionId": object["providerSessionId"],
		"source":            object["source"],
		"resume":            object["resume"],
	}
}

func sourceLocationFromShow(raw map[string]any) any {
	if raw == nil {
		return nil
	}
	source := raw["source"]
	if source != nil {
		return camelize(source)
	}
	event, _ := raw["event"].(map[string]any)
	if event == nil {
		return nil
	}
	return camelize(event["source"])
}
