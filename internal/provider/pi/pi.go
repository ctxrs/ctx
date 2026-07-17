package pi

import (
	"context"
	"encoding/json"

	"github.com/ctxrs/ctx/internal/capture"
)

const SourceFormat = "pi_session_jsonl"

type Adapter struct{}

func NewAdapter() Adapter {
	return Adapter{}
}

func (Adapter) CaptureFile(ctx context.Context, path string, options capture.CaptureOptions) (*capture.CapturedBatch, error) {
	adapter := capture.JSONLAdapter{
		Provider:       capture.ProviderPi,
		SourceFormat:   SourceFormat,
		ClassifyRecord: classifyRecord,
	}
	return adapter.CaptureFile(ctx, path, options)
}

func classifyRecord(raw json.RawMessage) capture.RecordClassification {
	var value map[string]any
	if err := json.Unmarshal(raw, &value); err != nil {
		return capture.RecordClassification{}
	}
	entryType, _ := value["type"].(string)
	classification := capture.RecordClassification{
		Kind: entryType,
	}
	if id, _ := value["id"].(string); id != "" {
		classification.NativeID = id
	}
	if entryType == "session" {
		if id, _ := value["id"].(string); id != "" {
			classification.NativeSourceID = id
		}
	}
	if message, _ := value["message"].(map[string]any); message != nil {
		if role, _ := message["role"].(string); role != "" {
			classification.Hints = map[string]string{"role": role}
		}
	}
	return classification
}
