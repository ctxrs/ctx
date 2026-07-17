package codex

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"sort"

	"github.com/ctxrs/ctx/internal/capture"
)

const SourceFormat = "codex_session_jsonl"

type Adapter struct{}

func NewAdapter() Adapter {
	return Adapter{}
}

func (Adapter) CaptureFile(ctx context.Context, path string, options capture.CaptureOptions) (*capture.CapturedBatch, error) {
	adapter := capture.JSONLAdapter{
		Provider:       capture.ProviderCodex,
		SourceFormat:   SourceFormat,
		ClassifyRecord: classifyRecord,
	}
	return adapter.CaptureFile(ctx, path, options)
}

func (adapter Adapter) CaptureTree(ctx context.Context, root string, options capture.CaptureOptions) ([]capture.CapturedBatch, error) {
	paths, err := jsonlFiles(root)
	if err != nil {
		return nil, err
	}
	rootURI := options.SourceRootURI
	if rootURI == "" {
		rootURI = capture.SourceRootURI(root)
	}
	batches := make([]capture.CapturedBatch, 0, len(paths))
	for _, path := range paths {
		fileOptions := options
		fileOptions.SourceRootURI = rootURI
		batch, err := adapter.CaptureFile(ctx, path, fileOptions)
		if err != nil {
			return nil, err
		}
		if batch != nil {
			batches = append(batches, *batch)
		}
	}
	return batches, nil
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
	switch entryType {
	case "session_meta":
		payload, _ := value["payload"].(map[string]any)
		if id, _ := payload["id"].(string); id != "" {
			classification.NativeSourceID = id
			classification.NativeID = id
		}
		if timestamp, _ := payload["timestamp"].(string); timestamp != "" {
			classification.Hints = map[string]string{"timestamp": timestamp}
		}
	case "response_item":
		payload, _ := value["payload"].(map[string]any)
		if itemType, _ := payload["type"].(string); itemType != "" {
			classification.Kind = "response_item:" + itemType
		}
		if callID, _ := payload["call_id"].(string); callID != "" {
			classification.NativeID = callID
		}
	case "event_msg":
		payload, _ := value["payload"].(map[string]any)
		if eventType, _ := payload["type"].(string); eventType != "" {
			classification.Kind = "event_msg:" + eventType
		}
	}
	return classification
}

func jsonlFiles(root string) ([]string, error) {
	info, err := os.Stat(root)
	if err != nil {
		return nil, err
	}
	if !info.IsDir() {
		return []string{root}, nil
	}
	var paths []string
	err = filepath.WalkDir(root, func(path string, entry os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if entry.IsDir() {
			return nil
		}
		if filepath.Ext(path) == ".jsonl" {
			paths = append(paths, path)
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	sort.Strings(paths)
	return paths, nil
}
