package ctxagenthistory

// Object stores JSON sub-documents whose shape can grow across ctx releases.
type Object map[string]any

// OperationName identifies a agent-history-v1 operation.
type OperationName string

const (
	OperationStatus        OperationName = "status"
	OperationInit          OperationName = "init"
	OperationSources       OperationName = "sources"
	OperationImport        OperationName = "import"
	OperationSync          OperationName = "sync"
	OperationSearch        OperationName = "search"
	OperationShowEvent     OperationName = "showEvent"
	OperationShowSession   OperationName = "showSession"
	OperationLocateEvent   OperationName = "locateEvent"
	OperationLocateSession OperationName = "locateSession"
	OperationError         OperationName = "error"
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

// SearchSemanticReadiness reports whether semantic retrieval can run.
type SearchSemanticReadiness string

const (
	SearchSemanticReady       SearchSemanticReadiness = "ready"
	SearchSemanticNotReady    SearchSemanticReadiness = "not_ready"
	SearchSemanticUnsupported SearchSemanticReadiness = "unsupported"
	SearchSemanticUnavailable SearchSemanticReadiness = "unavailable"
)

// SearchEffectiveBackend reports the backend that actually contributed results.
type SearchEffectiveBackend string

const (
	SearchEffectiveNone     SearchEffectiveBackend = "none"
	SearchEffectiveLexical  SearchEffectiveBackend = "lexical"
	SearchEffectiveSemantic SearchEffectiveBackend = "semantic"
	SearchEffectiveHybrid   SearchEffectiveBackend = "hybrid"
)

// SearchSemanticCompleteness reports semantic retrieval coverage.
type SearchSemanticCompleteness string

const (
	SearchSemanticNotAttempted SearchSemanticCompleteness = "not_attempted"
	SearchSemanticComplete     SearchSemanticCompleteness = "complete"
	SearchSemanticPartial      SearchSemanticCompleteness = "partial"
	SearchSemanticSkipped      SearchSemanticCompleteness = "skipped"
)

// SearchSemanticSkipReason explains why semantic retrieval did not run.
type SearchSemanticSkipReason string

const (
	SearchSemanticSkipDisabled     SearchSemanticSkipReason = "disabled"
	SearchSemanticSkipUnavailable  SearchSemanticSkipReason = "unavailable"
	SearchSemanticSkipNotReady     SearchSemanticSkipReason = "not_ready"
	SearchSemanticSkipUnsupported  SearchSemanticSkipReason = "unsupported"
	SearchSemanticSkipNoCandidates SearchSemanticSkipReason = "no_lexical_candidates"
	SearchSemanticSkipIneligible   SearchSemanticSkipReason = "query_shape_not_eligible"
)

// ResultScope classifies the granularity of a search hit.
type ResultScope string

const (
	ResultScopeEvent   ResultScope = "event"
	ResultScopeSession ResultScope = "session"
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
	Initialized            bool       `json:"initialized"`
	LocalOnly              bool       `json:"localOnly"`
	DataRoot               string     `json:"dataRoot,omitempty"`
	IndexedItems           int        `json:"indexedItems,omitempty"`
	IndexedSources         int        `json:"indexedSources,omitempty"`
	CatalogedSessions      int        `json:"catalogedSessions,omitempty"`
	IndexedCatalogSessions int        `json:"indexedCatalogSessions,omitempty"`
	PendingCatalogSessions int        `json:"pendingCatalogSessions,omitempty"`
	FailedCatalogSessions  int        `json:"failedCatalogSessions,omitempty"`
	StaleCatalogSessions   int        `json:"staleCatalogSessions,omitempty"`
	Freshness              *Freshness `json:"freshness,omitempty"`
	Semantic               Object     `json:"semantic,omitempty"`
	Daemon                 Object     `json:"daemon,omitempty"`
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
	SchemaVersion  int                  `json:"schema_version"`
	Query          *SearchQuery         `json:"query"`
	QueryExecution SearchQueryExecution `json:"query_execution"`
	Filters        Object               `json:"filters,omitempty"`
	Freshness      *Freshness           `json:"freshness,omitempty"`
	GeneratedAt    string               `json:"generatedAt,omitempty"`
	Retrieval      any                  `json:"retrieval,omitempty"`
	Results        []SearchHit          `json:"results"`
	Pagination     *SearchPagination    `json:"pagination,omitempty"`
	Truncation     *SearchTruncation    `json:"truncation,omitempty"`
}

// SearchExecutionLimits reports the resolved hard work envelope for a search.
type SearchExecutionLimits struct {
	QueryBytes                  int   `json:"query_bytes"`
	Clauses                     int   `json:"clauses"`
	AnalyzedTokensPerClause     int   `json:"analyzed_tokens_per_clause"`
	CandidatesPerPositiveSeed   int   `json:"candidates_per_positive_seed"`
	CandidateRows               int   `json:"candidate_rows"`
	RetainedCandidateIDs        int   `json:"retained_candidate_ids"`
	ResidualRows                int   `json:"residual_rows"`
	VerificationBytes           int   `json:"verification_bytes"`
	VerificationLookupBytes     int   `json:"verification_lookup_bytes"`
	HydratedRows                int   `json:"hydrated_rows"`
	HydrationInputBytes         int   `json:"hydration_input_bytes"`
	HydrationInputBytesPerEvent int   `json:"hydration_input_bytes_per_event"`
	SnippetInputBytes           int   `json:"snippet_input_bytes"`
	ReturnedTextBytes           int   `json:"returned_text_bytes"`
	SerializedResponseBytes     int   `json:"serialized_response_bytes"`
	Results                     int   `json:"results"`
	ElapsedMS                   int64 `json:"elapsed_ms"`
}

// SearchExecutionConsumption reports work actually consumed by a search.
type SearchExecutionConsumption struct {
	QueryBytes                     int   `json:"query_bytes"`
	Clauses                        int   `json:"clauses"`
	AnalyzedTokens                 int   `json:"analyzed_tokens"`
	LargestAnalyzedTokensPerClause int   `json:"largest_analyzed_tokens_per_clause"`
	LargestPositiveSeedCandidates  int   `json:"largest_positive_seed_candidates"`
	CandidateRows                  int   `json:"candidate_rows"`
	RetainedCandidateIDs           int   `json:"retained_candidate_ids"`
	ResidualRows                   int   `json:"residual_rows"`
	HydratedRows                   int   `json:"hydrated_rows"`
	LegacyFallbackRows             int   `json:"legacy_fallback_rows"`
	VerificationBytes              int   `json:"verification_bytes"`
	LargestVerificationLookupBytes int   `json:"largest_verification_lookup_bytes"`
	HydrationInputBytes            int   `json:"hydration_input_bytes"`
	LargestHydrationInputBytes     int   `json:"largest_hydration_input_bytes"`
	SnippetInputBytes              int   `json:"snippet_input_bytes"`
	ReturnedResults                int   `json:"returned_results"`
	ReturnedTextBytes              int   `json:"returned_text_bytes"`
	SerializedResponseBytes        int   `json:"serialized_response_bytes"`
	ElapsedMS                      int64 `json:"elapsed_ms"`
}

// SearchSemanticCoverage reports semantic index coverage relevant to a query.
type SearchSemanticCoverage struct {
	IndexedDocuments    *uint64 `json:"indexed_documents,omitempty"`
	SearchableDocuments *uint64 `json:"searchable_documents,omitempty"`
}

// SearchSemanticExecution reports semantic readiness, use, and completeness.
type SearchSemanticExecution struct {
	Attempted               bool                       `json:"attempted"`
	Required                bool                       `json:"required"`
	Readiness               SearchSemanticReadiness    `json:"readiness"`
	EffectiveBackend        SearchEffectiveBackend     `json:"effective_backend"`
	Backend                 *string                    `json:"backend,omitempty"`
	RequestedCandidates     int                        `json:"requested_candidates"`
	EligibleCandidates      int                        `json:"eligible_candidates"`
	CandidatesSupplied      int                        `json:"candidates_supplied"`
	CandidatesConsumed      int                        `json:"candidates_consumed"`
	CandidatesUsed          int                        `json:"candidates_used"`
	Coverage                SearchSemanticCoverage     `json:"coverage"`
	Completeness            SearchSemanticCompleteness `json:"completeness"`
	IncompletenessReasons   []string                   `json:"incompleteness_reasons,omitempty"`
	SkipReason              *SearchSemanticSkipReason  `json:"skip_reason,omitempty"`
	PositiveTextRuleVersion string                     `json:"positive_text_rule_version"`
}

// SearchQueryExecution is the typed, snake_case schema-v2 execution diagnostic block.
type SearchQueryExecution struct {
	QueryVersion              string                     `json:"query_version"`
	CandidateStrategy         string                     `json:"candidate_strategy"`
	Resolved                  SearchExecutionLimits      `json:"resolved"`
	Consumed                  SearchExecutionConsumption `json:"consumed"`
	Semantic                  SearchSemanticExecution    `json:"semantic"`
	RRFK                      uint32                     `json:"rrf_k"`
	PerBranchCandidateRows    int                        `json:"per_branch_candidate_rows"`
	RequestedResultLimit      int                        `json:"requested_result_limit"`
	ResultLimit               int                        `json:"result_limit"`
	MaxResultLimit            int                        `json:"max_result_limit"`
	ClausesExecuted           int                        `json:"clauses_executed"`
	VerificationDropped       int                        `json:"verification_dropped"`
	FilterVerificationDropped int                        `json:"filter_verification_dropped"`
	CandidateBudgetExhausted  bool                       `json:"candidate_budget_exhausted"`
	TimedOut                  bool                       `json:"timed_out"`
	Truncated                 bool                       `json:"truncated"`
	TruncationReasons         []string                   `json:"truncation_reasons,omitempty"`
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
	ResultType            string      `json:"resultType,omitempty"`
	ResultScope           ResultScope `json:"resultScope"`
	Provider              string      `json:"provider,omitempty"`
	Timestamp             string      `json:"timestamp,omitempty"`
	CWD                   string      `json:"cwd,omitempty"`
	SourcePath            string      `json:"sourcePath,omitempty"`
	SourceExists          *bool       `json:"sourceExists,omitempty"`
	Cursor                string      `json:"cursor,omitempty"`
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
	SourcePath   string `json:"sourcePath,omitempty"`
	SourceExists *bool  `json:"sourceExists,omitempty"`
	Cursor       string `json:"cursor,omitempty"`
}

// ShowEventResponse is returned by Client.ShowEvent.
type ShowEventResponse struct {
	Envelope
	Event EventResult `json:"event"`
}

// EventResult contains one selected event and its surrounding window.
type EventResult struct {
	Event  *Event          `json:"event,omitempty"`
	Events []Event         `json:"events"`
	Source *SourceLocation `json:"source,omitempty"`
}

// ShowSessionResponse is returned by Client.ShowSession.
type ShowSessionResponse struct {
	Envelope
	Session SessionResult `json:"session"`
}

// SessionResult contains a session transcript.
type SessionResult struct {
	Session *SessionRecord  `json:"session,omitempty"`
	Events  []Event         `json:"events,omitempty"`
	Source  *SourceLocation `json:"source,omitempty"`
	Mode    string          `json:"mode,omitempty"`
	Format  string          `json:"format,omitempty"`
}

// SessionRecord identifies a agent history session.
type SessionRecord struct {
	CtxSessionID      string `json:"ctxSessionId,omitempty"`
	Provider          string `json:"provider,omitempty"`
	ProviderSessionID string `json:"providerSessionId,omitempty"`
	Title             string `json:"title,omitempty"`
	StartedAt         string `json:"startedAt,omitempty"`
	UpdatedAt         string `json:"updatedAt,omitempty"`
	CWD               string `json:"cwd,omitempty"`
	SourcePath        string `json:"sourcePath,omitempty"`
	Visibility        string `json:"visibility,omitempty"`
}

// Event is the agent-history-v1 event shape.
type Event struct {
	CtxEventID   string     `json:"ctxEventId,omitempty"`
	CtxSessionID string     `json:"ctxSessionId,omitempty"`
	Sequence     int        `json:"sequence,omitempty"`
	EventType    string     `json:"eventType,omitempty"`
	Role         string     `json:"role,omitempty"`
	OccurredAt   string     `json:"occurredAt,omitempty"`
	Source       string     `json:"source,omitempty"`
	Cursor       string     `json:"cursor,omitempty"`
	Text         string     `json:"text,omitempty"`
	Preview      string     `json:"preview,omitempty"`
	Citations    []Citation `json:"citations,omitempty"`
}

// LocateEventResponse is returned by Client.LocateEvent.
type LocateEventResponse struct {
	Envelope
	Location LocationResult `json:"location"`
}

// LocateSessionResponse is returned by Client.LocateSession.
type LocateSessionResponse struct {
	Envelope
	Location LocationResult `json:"location"`
}

// LocationResult contains event or session source provenance.
type LocationResult struct {
	CtxSessionID      string          `json:"ctxSessionId"`
	CtxEventID        string          `json:"ctxEventId,omitempty"`
	Provider          string          `json:"provider"`
	ProviderSessionID string          `json:"providerSessionId,omitempty"`
	Source            *SourceLocation `json:"source"`
	Resume            *ResumeLocation `json:"resume,omitempty"`
}

// ResumeLocation contains enough source information for a caller to resume.
type ResumeLocation struct {
	Cursor string `json:"cursor,omitempty"`
	Path   string `json:"path,omitempty"`
}

// SourceLocation identifies source provenance for show/locate results.
type SourceLocation struct {
	Path         string `json:"path,omitempty"`
	Cursor       string `json:"cursor,omitempty"`
	Exists       *bool  `json:"exists,omitempty"`
	SourceID     string `json:"sourceId,omitempty"`
	SourceFormat string `json:"sourceFormat,omitempty"`
}

// ErrorResponse is the agent-history-v1 structured error envelope.
type ErrorResponse struct {
	Envelope
	Error AgentHistoryError `json:"error"`
}
