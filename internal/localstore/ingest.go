package localstore

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"github.com/ctxrs/ctx/internal/capture"
)

func (s *Store) SaveCapturedBatch(ctx context.Context, batch capture.CapturedBatch) (int, error) {
	if len(batch.Records) == 0 {
		return 0, capture.ErrNoRecords
	}
	metadata, _ := json.Marshal(map[string]any{
		"batch_id":       batch.ID,
		"source_path":    batch.Source.Path,
		"source_root":    batch.Source.RootURI,
		"content_hash":   batch.ContentHash,
		"privacy_policy": batch.Privacy.PolicyID,
	})
	source, err := s.UpsertSource(ctx, SourceDescriptor{
		Key:          string(batch.Provider) + ":" + batch.Source.ID,
		Provider:     string(batch.Provider),
		Format:       batch.SourceFormat,
		URI:          firstNonEmpty(batch.Source.URI, capture.SourceURI(batch.Source.Path)),
		Identity:     batch.Revision.Identity,
		MetadataJSON: string(metadata),
	})
	if err != nil {
		return 0, err
	}
	gen, err := s.BeginGeneration(ctx, source.Key, GenerationOptions{
		Kind:           GenerationReplace,
		SourceIdentity: batch.Revision.Identity,
	})
	if err != nil {
		return 0, err
	}
	sessionTrees := codexSessionTrees(batch)
	total := 0
	prevSize := int64(0)
	prevTail := ""
	for start := 0; start < len(batch.Records); start += MaxAppendEvents {
		end := start + MaxAppendEvents
		if end > len(batch.Records) {
			end = len(batch.Records)
		}
		records := batch.Records[start:end]
		events := make([]Event, 0, len(records))
		for _, record := range records {
			events = append(events, mapCapturedRecordEvent(batch, record, sessionTrees))
		}
		newSize := records[len(records)-1].ByteEnd
		newTail := records[len(records)-1].ContentHash
		if end == len(batch.Records) {
			newTail = batch.ContentHash
		}
		result, err := s.AppendEvents(ctx, AppendRequest{
			SourceKey:        source.Key,
			GenerationID:     gen.ID,
			SourceIdentity:   batch.Revision.Identity,
			PreviousSize:     prevSize,
			PreviousTailHash: prevTail,
			NewSize:          newSize,
			NewTailHash:      newTail,
			PageStartOffset:  records[0].ByteStart,
			PageEndOffset:    records[len(records)-1].ByteEnd,
			Events:           events,
		})
		if err != nil {
			return total, err
		}
		total += result.InsertedEvents
		prevSize = newSize
		prevTail = newTail
	}
	if err := s.ActivateGeneration(ctx, gen.ID); err != nil {
		return total, err
	}
	return total, nil
}

func mapCapturedRecordEvent(batch capture.CapturedBatch, record capture.ProviderRecord, sessionTrees map[string]sessionTree) Event {
	projection := extractRecordProjection(batch, record)
	tree := sessionTrees[projection.SessionID]
	metadata, _ := json.Marshal(map[string]any{
		"batch_id":                batch.ID,
		"record_hash":             record.ContentHash,
		"native_id":               record.NativeID,
		"malformed":               record.Malformed,
		"parse_error":             record.ParseError,
		"source_path":             batch.Source.Path,
		"source_format":           batch.SourceFormat,
		"codex_session_id":        projection.SessionID,
		"codex_parent_session_id": tree.ParentID,
		"codex_root_session_id":   tree.RootID,
	})
	return Event{
		SourceEventID:      sourceEventID(record),
		ProviderSessionID:  projection.SessionID,
		ParentSessionID:    tree.ParentID,
		RootSessionID:      tree.RootID,
		ProviderEventIndex: record.Ordinal,
		Role:               projection.Role,
		Type:               projection.Type,
		OccurredAt:         projection.OccurredAt,
		Text:               projection.Text,
		MetadataJSON:       string(metadata),
		SourceOffset:       record.ByteStart,
		SourceEndOffset:    record.ByteEnd,
	}
}

type sessionTree struct {
	ParentID string
	RootID   string
}

func codexSessionTrees(batch capture.CapturedBatch) map[string]sessionTree {
	result := map[string]sessionTree{}
	if batch.Provider != capture.ProviderCodex {
		return result
	}
	for _, record := range batch.Records {
		var value map[string]any
		if err := json.Unmarshal(record.Raw, &value); err != nil || value["type"] != "session_meta" {
			continue
		}
		payload, _ := value["payload"].(map[string]any)
		id, _ := payload["id"].(string)
		if id == "" {
			continue
		}
		parentID := codexParentSessionID(payload)
		rootID := firstNonEmpty(
			stringFromMap(payload, "root_thread_id"),
			stringFromMap(payload, "root_session_id"),
			stringFromMap(payload, "root_id"),
			parentID,
			id,
		)
		result[id] = sessionTree{ParentID: parentID, RootID: rootID}
	}
	return result
}

func codexParentSessionID(payload map[string]any) string {
	for _, key := range []string{"parent_thread_id", "parent_session_id", "parent_id"} {
		if value := stringFromMap(payload, key); value != "" {
			return value
		}
	}
	source, _ := payload["source"].(map[string]any)
	subagent, _ := source["subagent"].(map[string]any)
	spawn, _ := subagent["thread_spawn"].(map[string]any)
	return stringFromMap(spawn, "parent_thread_id")
}

func stringFromMap(value map[string]any, key string) string {
	if value == nil {
		return ""
	}
	result, _ := value[key].(string)
	return result
}

type recordProjection struct {
	SessionID  string
	Role       string
	Type       string
	OccurredAt time.Time
	Text       string
}

func extractRecordProjection(batch capture.CapturedBatch, record capture.ProviderRecord) recordProjection {
	projection := recordProjection{
		SessionID: firstNonEmpty(batch.Source.NativeID, batch.Source.ID),
		Role:      firstNonEmpty(record.Hints["role"], "unknown"),
		Type:      firstNonEmpty(record.Kind, "record"),
		Text:      strings.TrimSpace(string(record.Raw)),
	}
	var value map[string]any
	if err := json.Unmarshal(record.Raw, &value); err != nil {
		return projection
	}
	projection.OccurredAt = parseJSONTime(value["timestamp"])
	switch batch.Provider {
	case capture.ProviderCodex:
		applyCodexProjection(&projection, value)
	case capture.ProviderPi:
		applyPiProjection(&projection, value)
	}
	if projection.Text == "" {
		encoded, _ := json.Marshal(value)
		projection.Text = string(encoded)
	}
	return projection
}

func applyCodexProjection(projection *recordProjection, value map[string]any) {
	if sessionID, _ := value["session_id"].(string); sessionID != "" {
		projection.SessionID = sessionID
	}
	payload, _ := value["payload"].(map[string]any)
	switch value["type"] {
	case "session_meta":
		if id, _ := payload["id"].(string); id != "" {
			projection.SessionID = id
		}
		projection.Role = "system"
		projection.Text = "session metadata " + compactJSON(payload)
	case "response_item":
		itemType, _ := payload["type"].(string)
		projection.Type = firstNonEmpty("response_item:"+itemType, projection.Type)
		if role, _ := payload["role"].(string); role != "" {
			projection.Role = role
		} else {
			projection.Role = "assistant"
		}
		projection.Text = codexPayloadText(payload)
	case "event_msg":
		projection.Role = "system"
		projection.Text = compactJSON(payload)
	default:
		if text, _ := value["text"].(string); text != "" {
			projection.Text = text
			projection.Role = "user"
			projection.Type = "history"
		}
	}
}

func applyPiProjection(projection *recordProjection, value map[string]any) {
	if id, _ := value["id"].(string); value["type"] == "session" && id != "" {
		projection.SessionID = id
		projection.Role = "system"
		projection.Text = "session metadata " + compactJSON(value)
		return
	}
	message, _ := value["message"].(map[string]any)
	if role, _ := message["role"].(string); role != "" {
		projection.Role = role
	}
	if text := textFromAny(message["content"]); text != "" {
		projection.Text = text
	}
}

func codexPayloadText(payload map[string]any) string {
	switch payload["type"] {
	case "message":
		return textFromAny(payload["content"])
	case "function_call":
		return strings.TrimSpace(fmt.Sprintf("%v %v", payload["name"], payload["arguments"]))
	case "function_call_output":
		return fmt.Sprint(payload["output"])
	default:
		return compactJSON(payload)
	}
}

func textFromAny(value any) string {
	switch typed := value.(type) {
	case string:
		return typed
	case []any:
		var parts []string
		for _, item := range typed {
			if object, ok := item.(map[string]any); ok {
				if text, _ := object["text"].(string); text != "" {
					parts = append(parts, text)
				} else {
					parts = append(parts, compactJSON(object))
				}
			} else {
				parts = append(parts, fmt.Sprint(item))
			}
		}
		return strings.Join(parts, "\n")
	case map[string]any:
		return compactJSON(typed)
	default:
		return ""
	}
}

func sourceEventID(record capture.ProviderRecord) string {
	if record.NativeID != "" {
		return fmt.Sprintf("%012d:%s", record.Ordinal, record.NativeID)
	}
	return fmt.Sprintf("%012d:%s", record.Ordinal, record.ContentHash)
}

func compactJSON(value any) string {
	encoded, err := json.Marshal(value)
	if err != nil {
		return fmt.Sprint(value)
	}
	return string(encoded)
}

func parseJSONTime(value any) time.Time {
	switch typed := value.(type) {
	case string:
		for _, layout := range []string{time.RFC3339Nano, time.RFC3339} {
			if parsed, err := time.Parse(layout, typed); err == nil {
				return parsed.UTC()
			}
		}
	case float64:
		if typed > 1_000_000_000_000 {
			return time.UnixMilli(int64(typed)).UTC()
		}
		return time.Unix(int64(typed), 0).UTC()
	}
	return time.Time{}
}

func firstNonEmpty[T ~string](values ...T) string {
	for _, value := range values {
		if value != "" {
			return string(value)
		}
	}
	return ""
}
