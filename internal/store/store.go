package store

import (
	"context"
	"errors"
	"time"

	"github.com/ctxrs/ctx/internal/capture"
)

var ErrNotFound = errors.New("not found")

type Store interface {
	Setup(context.Context, SetupOptions) error
	Status(context.Context) (Status, error)
	ListSources(context.Context, SourceFilter) ([]Source, error)
	SaveBatch(context.Context, capture.CapturedBatch) error
	ShowSession(context.Context, string) (Transcript, error)
	ShowEvent(context.Context, string, EventWindowOptions) (Transcript, error)
	LocateSession(context.Context, string) (Location, error)
	LocateEvent(context.Context, string) (Location, error)
	QuerySQL(context.Context, SQLQuery) (SQLResult, error)
	Close() error
}

type SetupOptions struct {
	CatalogOnly bool
}

type Status struct {
	Ready           bool
	DataRoot        string
	SchemaVersion   int
	Sources         int
	IndexedSessions int
	IndexedEvents   int
	UpdatedAt       time.Time
}

type SourceFilter struct {
	Provider string
	All      bool
}

type Source struct {
	ID        string
	Provider  string
	Path      string
	Available bool
	UpdatedAt time.Time
}

type Transcript struct {
	ID       string
	Title    string
	Provider string
	Events   []TranscriptEvent
}

type TranscriptEvent struct {
	ID        string
	Role      string
	Text      string
	CreatedAt time.Time
}

type EventWindowOptions struct {
	Before int
	After  int
}

type Location struct {
	ID       string
	Provider string
	Path     string
	Line     int
	Column   int
}

type SQLQuery struct {
	Text string
	Args []string
}

type SQLResult struct {
	Columns []string
	Rows    [][]string
}
