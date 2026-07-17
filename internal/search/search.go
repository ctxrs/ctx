package search

import (
	"context"
	"errors"
	"time"
)

type Mode string

const (
	ModeLexical  Mode = "lexical"
	ModeSemantic Mode = "semantic"
	ModeHybrid   Mode = "hybrid"
)

var ErrSemanticUnavailable = errors.New("semantic search is unavailable")

type Query struct {
	Text      string
	Mode      Mode
	Limit     int
	Provider  string
	Workspace string
	// ExcludeActiveSessionID prevents returning the in-flight Codex session.
	ExcludeActiveSessionID string
	Since                  *time.Time
	Until                  *time.Time
}

type Results struct {
	Mode    Mode
	Items   []Result
	Elapsed time.Duration
}

type Result struct {
	SessionID string
	EventID   string
	Provider  string
	Title     string
	Snippet   string
	Score     float64
	CreatedAt time.Time
}

type Engine interface {
	Search(context.Context, Query) (Results, error)
}
