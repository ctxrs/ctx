package ctxagenthistory

import "encoding/json"

// Object stores JSON sub-documents whose shape can grow across ctx releases.
type Object map[string]any

// OperationName identifies a agent-history-v1 operation.
type OperationName string

const (
	OperationStatus      OperationName = "status"
	OperationInit        OperationName = "init"
	OperationSources     OperationName = "sources"
	OperationImport      OperationName = "import"
	OperationSync        OperationName = "sync"
	OperationSearch      OperationName = "search"
	OperationShowEvent   OperationName = "showEvent"
	OperationShowSession OperationName = "showSession"
	OperationError       OperationName = "error"
)

// BackendKind identifies whether a response came from local or hosted ctx.
type BackendKind string

const (
	BackendKindLocal  BackendKind = "local"
	BackendKindHosted BackendKind = "hosted"
)

// ProviderSourceStatus classifies source discovery state.
type ProviderSourceStatus string

const (
	ProviderSourceStatusReady       ProviderSourceStatus = "ready"
	ProviderSourceStatusMissing     ProviderSourceStatus = "missing"
	ProviderSourceStatusUnsupported ProviderSourceStatus = "unsupported"
)

// ImportSupport classifies source import support.
type ImportSupport string

const (
	ImportSupportNative      ImportSupport = "native"
	ImportSupportUnsupported ImportSupport = "unsupported"
)

// ImportSourceStatus classifies one import source result.
type ImportSourceStatus string

const (
	ImportSourceStatusImported ImportSourceStatus = "imported"
	ImportSourceStatusSkipped  ImportSourceStatus = "skipped"
	ImportSourceStatusFailed   ImportSourceStatus = "failed"
)

// FreshnessMode configures or reports search freshness behavior.
type FreshnessMode string

const (
	FreshnessModeBackground FreshnessMode = "background"
	FreshnessModeOff        FreshnessMode = "off"
	FreshnessModeWait       FreshnessMode = "wait"
)

// FreshnessStatus describes the outcome of a freshness pass.
type FreshnessStatus string

const (
	FreshnessStatusSkipped         FreshnessStatus = "skipped"
	FreshnessStatusNoSources       FreshnessStatus = "no_sources"
	FreshnessStatusCompleted       FreshnessStatus = "completed"
	FreshnessStatusReadOnly        FreshnessStatus = "read_only"
	FreshnessStatusBudgetExhausted FreshnessStatus = "budget_exhausted"
	FreshnessStatusFailed          FreshnessStatus = "failed"
)

// ResultScope classifies the granularity of a search hit.
type ResultScope string

const (
	ResultScopeEvent   ResultScope = "event"
	ResultScopeSession ResultScope = "session"
)

// CoreContentPolicyStatus reports how Core selected event content.
type CoreContentPolicyStatus string

const (
	CoreContentPolicyStatusSelected CoreContentPolicyStatus = "selected"
	CoreContentPolicyStatusRedacted CoreContentPolicyStatus = "redacted"
	CoreContentPolicyStatusOmitted  CoreContentPolicyStatus = "omitted"
)

// Envelope contains the fields common to every agent-history-v1 response.
type Envelope struct {
	ContractVersion string        `json:"contractVersion"`
	SchemaVersion   int           `json:"schemaVersion"`
	Operation       OperationName `json:"operation"`
	Backend         Backend       `json:"backend"`
}

// Backend describes the agent history backend that produced a response.
type Backend struct {
	Kind     BackendKind `json:"kind"`
	DataRoot string      `json:"dataRoot,omitempty"`
	BaseURL  string      `json:"baseUrl,omitempty"`
}

// AgentHistoryError is the agent-history-v1 error shape.
type AgentHistoryError struct {
	Code      ErrorKind `json:"code"`
	Message   string    `json:"message"`
	Retryable bool      `json:"retryable"`
	Details   Object    `json:"details,omitempty"`
	Cause     string    `json:"cause,omitempty"`
}

// StatusResponse is returned by Client.Status.
type StatusResponse struct {
	Envelope
	Status StatusRecord `json:"status"`
}

// StatusRecord describes local index state.
type StatusRecord struct {
	Initialized     bool   `json:"initialized"`
	LocalOnly       bool   `json:"localOnly"`
	ReadOnly        bool   `json:"readOnly,omitempty"`
	DataRoot        string `json:"dataRoot,omitempty"`
	IndexedItems    uint64 `json:"indexedItems,omitempty"`
	IndexedSessions uint64 `json:"indexedSessions,omitempty"`
	IndexedEvents   uint64 `json:"indexedEvents,omitempty"`
	IndexedSources  uint64 `json:"indexedSources,omitempty"`
	HistoryEpoch    Object `json:"historyEpoch,omitempty"`
	Lexical         Object `json:"lexical,omitempty"`
	Refresh         Object `json:"refresh,omitempty"`
	Semantic        Object `json:"semantic,omitempty"`
	Daemon          Object `json:"daemon,omitempty"`
}

// InitResponse is returned by Client.Init.
type InitResponse struct {
	Envelope
	Status StatusRecord `json:"status,omitempty"`
}

// SourcesResponse is returned by Client.Sources.
type SourcesResponse struct {
	Envelope
	Sources []ProviderSource `json:"sources"`
}

// ProviderSource describes one discovered local history source.
type ProviderSource struct {
	Provider          string               `json:"provider"`
	Path              string               `json:"path"`
	Exists            bool                 `json:"exists"`
	SourceFormat      string               `json:"sourceFormat,omitempty"`
	Status            ProviderSourceStatus `json:"status"`
	ImportSupport     ImportSupport        `json:"importSupport,omitempty"`
	NativeImport      bool                 `json:"nativeImport"`
	Importable        bool                 `json:"importable"`
	UnsupportedReason *string              `json:"unsupportedReason,omitempty"`
}

// ImportResponse is returned by Client.Import and Client.Sync.
type ImportResponse struct {
	Envelope
	Import ImportResult `json:"import"`
}

// ImportResult describes an import/sync result.
type ImportResult struct {
	Resume     bool           `json:"resume"`
	ResumeMode string         `json:"resumeMode,omitempty"`
	Totals     Totals         `json:"totals"`
	Sources    []ImportSource `json:"sources,omitempty"`
}

// ImportSource summarizes one source handled by an import.
type ImportSource struct {
	Provider         string             `json:"provider,omitempty"`
	Path             string             `json:"path,omitempty"`
	SourceFormat     string             `json:"sourceFormat,omitempty"`
	Status           ImportSourceStatus `json:"status,omitempty"`
	ImportedSessions int                `json:"importedSessions,omitempty"`
	ImportedEvents   int                `json:"importedEvents,omitempty"`
	Skipped          int                `json:"skipped,omitempty"`
	Failed           int                `json:"failed,omitempty"`
	Error            string             `json:"error,omitempty"`
}

// Totals contains aggregate import counts.
type Totals struct {
	SourceFiles      int   `json:"sourceFiles,omitempty"`
	SourceBytes      int64 `json:"sourceBytes,omitempty"`
	ImportedSources  int   `json:"importedSources,omitempty"`
	FailedSources    int   `json:"failedSources,omitempty"`
	ImportedSessions int   `json:"importedSessions,omitempty"`
	ImportedEvents   int   `json:"importedEvents,omitempty"`
	ImportedEdges    int   `json:"importedEdges,omitempty"`
	Skipped          int   `json:"skipped,omitempty"`
	Failed           int   `json:"failed,omitempty"`
}

// SearchResponse is returned by Client.Search.
type SearchResponse struct {
	Envelope
	Search SearchResult `json:"search"`
}

// SearchResult contains agent history search results.
type SearchResult struct {
	Query        string              `json:"query,omitempty"`
	Filters      Object              `json:"filters,omitempty"`
	Freshness    *Freshness          `json:"freshness,omitempty"`
	GeneratedAt  string              `json:"generatedAt,omitempty"`
	Retrieval    any                 `json:"retrieval,omitempty"`
	Results      []SearchHit         `json:"results"`
	ResultWindow *SearchResultWindow `json:"resultWindow,omitempty"`
	Pagination   *SearchPagination   `json:"pagination,omitempty"`
	Truncation   *SearchTruncation   `json:"truncation,omitempty"`
}

// SearchResultWindow describes the bounded result window returned by search.
type SearchResultWindow struct {
	Limit         int  `json:"limit"`
	Returned      int  `json:"returned"`
	MoreAvailable bool `json:"moreAvailable"`
}

// SearchPagination describes paging metadata for search results.
type SearchPagination struct {
	Limit      int    `json:"limit,omitempty"`
	Offset     int    `json:"offset,omitempty"`
	Total      int    `json:"total,omitempty"`
	NextCursor string `json:"nextCursor,omitempty"`
	HasMore    bool   `json:"hasMore,omitempty"`
}

// SearchTruncation describes whether a search response was truncated.
type SearchTruncation struct {
	Truncated  bool   `json:"truncated"`
	Reason     string `json:"reason,omitempty"`
	MaxResults int    `json:"maxResults,omitempty"`
	MaxBytes   int64  `json:"maxBytes,omitempty"`
}

// Freshness describes an optional pre-search refresh.
type Freshness struct {
	Mode              FreshnessMode   `json:"mode,omitempty"`
	Status            FreshnessStatus `json:"status,omitempty"`
	Reason            string          `json:"reason,omitempty"`
	BudgetReasons     []string        `json:"budgetReasons,omitempty"`
	SourceCount       int             `json:"sourceCount,omitempty"`
	DaemonLastRunAtMs int64           `json:"daemonLastRunAtMs,omitempty"`
	Totals            Totals          `json:"totals,omitempty"`
	Error             string          `json:"error,omitempty"`
}

// SearchHit is one agent history search hit.
type SearchHit struct {
	CtxEventID            string      `json:"ctxEventId,omitempty"`
	CtxSessionID          string      `json:"ctxSessionId,omitempty"`
	ProviderSessionID     string      `json:"providerSessionId,omitempty"`
	EventSeq              int         `json:"eventSeq,omitempty"`
	Title                 string      `json:"title,omitempty"`
	Snippet               string      `json:"snippet,omitempty"`
	Rank                  float64     `json:"rank,omitempty"`
	RetrievalScore        *float64    `json:"retrievalScore,omitempty"`
	ResultType            string      `json:"resultType,omitempty"`
	ResultScope           ResultScope `json:"resultScope"`
	Provider              string      `json:"provider,omitempty"`
	SourceFormat          string      `json:"sourceFormat,omitempty"`
	Timestamp             string      `json:"timestamp,omitempty"`
	CWD                   string      `json:"cwd,omitempty"`
	WhyMatched            []string    `json:"whyMatched,omitempty"`
	Citations             []Citation  `json:"citations,omitempty"`
	SuggestedNextCommands []string    `json:"suggestedNextCommands,omitempty"`
	Visibility            string      `json:"visibility,omitempty"`
}

// Citation identifies source material for a agent history result.
type Citation struct {
	ItemID       string `json:"itemId,omitempty"`
	TargetType   string `json:"targetType,omitempty"`
	CtxEventID   string `json:"ctxEventId,omitempty"`
	CtxSessionID string `json:"ctxSessionId,omitempty"`
	Label        string `json:"label,omitempty"`
	Time         string `json:"time,omitempty"`
	Provider     string `json:"provider,omitempty"`
	SessionID    string `json:"sessionId,omitempty"`
	EventSeq     int    `json:"eventSeq,omitempty"`
}

// ShowEventResponse is returned by Client.ShowEvent.
type ShowEventResponse struct {
	Envelope
	Event EventResult `json:"event"`
}

// EventResult contains one selected event and its surrounding window.
type EventResult struct {
	Event  *Event  `json:"event,omitempty"`
	Events []Event `json:"events"`
}

// ShowSessionResponse is returned by Client.ShowSession.
type ShowSessionResponse struct {
	Envelope
	Session SessionResult `json:"session"`
}

// SessionResult contains a session transcript.
type SessionResult struct {
	Session *SessionRecord `json:"session,omitempty"`
	Events  []Event        `json:"events,omitempty"`
	Mode    string         `json:"mode,omitempty"`
	Format  string         `json:"format,omitempty"`
}

// SessionRecord identifies a agent history session.
type SessionRecord struct {
	CtxSessionID      string `json:"ctxSessionId,omitempty"`
	Provider          string `json:"provider,omitempty"`
	ProviderSessionID string `json:"providerSessionId,omitempty"`
	SourceFormat      string `json:"sourceFormat,omitempty"`
	Title             string `json:"title,omitempty"`
	StartedAt         string `json:"startedAt,omitempty"`
	UpdatedAt         string `json:"updatedAt,omitempty"`
	CWD               string `json:"cwd,omitempty"`
	Visibility        string `json:"visibility,omitempty"`
}

// Event is the agent-history-v1 event shape.
type Event struct {
	CtxEventID        string               `json:"ctxEventId,omitempty"`
	CtxSessionID      string               `json:"ctxSessionId,omitempty"`
	Provider          string               `json:"provider,omitempty"`
	ProviderSessionID string               `json:"providerSessionId,omitempty"`
	SourceFormat      string               `json:"sourceFormat,omitempty"`
	Sequence          int                  `json:"sequence,omitempty"`
	EventType         string               `json:"eventType,omitempty"`
	Role              string               `json:"role,omitempty"`
	OccurredAt        string               `json:"occurredAt,omitempty"`
	Text              string               `json:"text,omitempty"`
	StructuredContent json.RawMessage      `json:"structuredContent,omitempty"`
	Content           *CoreContentMetadata `json:"content,omitempty"`
	Citations         []Citation           `json:"citations,omitempty"`
}

// CoreContentMetadata describes completeness and Core policy for shown content.
type CoreContentMetadata struct {
	Complete     bool                    `json:"complete"`
	PolicyStatus CoreContentPolicyStatus `json:"policyStatus"`
	PolicyReason *string                 `json:"policyReason,omitempty"`
}

// ErrorResponse is the agent-history-v1 structured error envelope.
type ErrorResponse struct {
	Envelope
	Error AgentHistoryError `json:"error"`
}
