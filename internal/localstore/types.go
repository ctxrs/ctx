package localstore

import (
	"errors"
	"time"
)

const SchemaVersion = 1

const (
	MaxAppendEvents = 64
	MaxAppendBytes  = 8 * 1024 * 1024
)

var (
	ErrNotFound           = errors.New("localstore: not found")
	ErrCheckpointMismatch = errors.New("localstore: append checkpoint mismatch")
	ErrInvalidAppend      = errors.New("localstore: invalid append")
)

type SourceDescriptor struct {
	Key          string
	Provider     string
	Format       string
	URI          string
	Identity     string
	MetadataJSON string
}

type Source struct {
	ID                 int64
	Key                string
	Provider           string
	Format             string
	URI                string
	Identity           string
	MetadataJSON       string
	SizeBytes          int64
	TailHash           string
	ActiveGenerationID int64
	CreatedAt          time.Time
	UpdatedAt          time.Time
}

type GenerationKind string

const (
	GenerationReplace GenerationKind = "replace"
	GenerationAppend  GenerationKind = "append"
)

type GenerationState string

const (
	GenerationBuilding GenerationState = "building"
	GenerationActive   GenerationState = "active"
	GenerationStale    GenerationState = "stale"
)

type GenerationOptions struct {
	Kind           GenerationKind
	SourceIdentity string
	BaseSizeBytes  int64
	BaseTailHash   string
}

type Generation struct {
	ID              int64
	SourceID        int64
	SourceKey       string
	Kind            GenerationKind
	State           GenerationState
	SourceIdentity  string
	BaseSizeBytes   int64
	BaseTailHash    string
	HighWaterBytes  int64
	TailHash        string
	CreatedAt       time.Time
	ActivatedAt     time.Time
	StaleAt         time.Time
	CleanupMarkedAt time.Time
}

type Event struct {
	SourceEventID      string
	ProviderSessionID  string
	ProviderEventIndex int64
	Role               string
	Type               string
	OccurredAt         time.Time
	Text               string
	MetadataJSON       string
	SourceOffset       int64
	SourceEndOffset    int64
}

type AppendRequest struct {
	SourceKey        string
	GenerationID     int64
	SourceIdentity   string
	PreviousSize     int64
	PreviousTailHash string
	NewSize          int64
	NewTailHash      string
	PageStartOffset  int64
	PageEndOffset    int64
	Events           []Event
}

type AppendResult struct {
	InsertedEvents int
	Replayed       bool
	HighWaterBytes int64
}

type SearchHit struct {
	EventID            int64
	GenerationID       int64
	SourceKey          string
	Provider           string
	ProviderSessionID  string
	ProviderEventIndex int64
	Role               string
	Type               string
	OccurredAt         time.Time
	Text               string
	Rank               float64
}

type Status struct {
	Initialized             bool
	SchemaVersion           int
	SourceCount             int
	ActiveGenerationCount   int
	InactiveGenerationCount int
	StaleGenerationCount    int
	EventCount              int
	ActiveEventCount        int
	PendingJobCount         int
	RunningJobCount         int
	FailedJobCount          int
	CompletedJobCount       int
}

type Job struct {
	ID          int64
	Key         string
	Type        string
	State       string
	PayloadJSON string
	Attempts    int
	AvailableAt time.Time
	LeasedUntil time.Time
	LastError   string
	CreatedAt   time.Time
	UpdatedAt   time.Time
}
