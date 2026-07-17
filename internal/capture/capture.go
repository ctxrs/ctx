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
	ID       string
	Provider Provider
	Kind     string
	Path     string
}

type Cursor struct {
	Opaque []byte
}

type SourceRevision struct {
	Source     SourceRef
	Generation string
	Identity   string
	SizeBytes  int64
	ModifiedAt time.Time
}

type CapturedBatch struct {
	Provider Provider
	Source   SourceRef
	Revision SourceRevision
	Cursor   Cursor
	Sessions []CapturedSession
	Events   []CapturedEvent
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
