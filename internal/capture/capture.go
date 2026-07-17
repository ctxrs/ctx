package capture

import (
	"context"
	"time"
)

type Provider string

const (
	ProviderClaude     Provider = "claude"
	ProviderCodex      Provider = "codex"
	ProviderCursor     Provider = "cursor"
	ProviderPi         Provider = "pi"
	ProviderCopilotCLI Provider = "copilot-cli"
	ProviderOpenCode   Provider = "opencode"
	ProviderGemini     Provider = "gemini"
	ProviderQwen       Provider = "qwen"
)

type SourceRef struct {
	ID       string   `json:"id"`
	Provider Provider `json:"provider"`
	Kind     string   `json:"kind"`
	Path     string   `json:"path,omitempty"`
	URI      string   `json:"uri,omitempty"`
	RootURI  string   `json:"rootUri,omitempty"`
	NativeID string   `json:"nativeId,omitempty"`
}

type Cursor struct {
	Opaque []byte
}

type SourceRevision struct {
	Source      SourceRef `json:"source"`
	Generation  string    `json:"generation,omitempty"`
	Identity    string    `json:"identity,omitempty"`
	ID          string    `json:"id"`
	Kind        string    `json:"kind"`
	SizeBytes   int64     `json:"sizeBytes"`
	ModifiedAt  time.Time `json:"modifiedAt,omitempty"`
	ContentHash string    `json:"contentHash"`
}

type CapturedBatch struct {
	SchemaVersion       int                 `json:"schemaVersion"`
	ID                  string              `json:"id"`
	Provider            Provider            `json:"provider"`
	Source              SourceRef           `json:"source"`
	SourceFormat        string              `json:"sourceFormat"`
	Revision            SourceRevision      `json:"revision"`
	Range               RecordRange         `json:"range"`
	ContentHash         string              `json:"contentHash"`
	RecordSchemaVersion int                 `json:"recordSchemaVersion"`
	Records             []ProviderRecord    `json:"records"`
	Hints               DeterministicHints  `json:"hints"`
	Privacy             PrivacyFilterStatus `json:"privacy"`
	Checkpoint          SourceCheckpoint    `json:"checkpoint"`
	Cursor              Cursor              `json:"cursor,omitempty"`
	Sessions            []CapturedSession   `json:"sessions,omitempty"`
	Events              []CapturedEvent     `json:"events,omitempty"`
}

type CapturedSession struct {
	ID        string
	Provider  Provider
	SourceID  string
	Title     string
	StartedAt time.Time
	EndedAt   *time.Time
	Metadata  map[string]string
}

type CapturedEvent struct {
	ID        string
	SessionID string
	Provider  Provider
	SourceID  string
	Role      string
	Text      string
	CreatedAt time.Time
	Metadata  map[string]string
}

type Capturer interface {
	Discover(context.Context) ([]SourceRef, error)
	Capture(context.Context, SourceRef, Cursor) (CapturedBatch, error)
}
