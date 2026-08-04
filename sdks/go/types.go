package ctxagenthistory

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"strconv"
	"unicode/utf8"
)

const maxMCPToolCallComponentBytes = 64 * 1024
const maxMCPExchangeIdentityBytes = 64 * 1024

// MaxSafeInteger is the largest exact JSON integer in every supported SDK.
const MaxSafeInteger uint64 = (1 << 53) - 1

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

// MaxSafeStatusCounter is retained as the status-specific name for MaxSafeInteger.
const MaxSafeStatusCounter uint64 = MaxSafeInteger

// UnmarshalJSON rejects counters that other SDKs cannot represent exactly.
func (s *StatusRecord) UnmarshalJSON(data []byte) error {
	type statusRecord StatusRecord
	var decoded statusRecord
	if err := json.Unmarshal(data, &decoded); err != nil {
		return err
	}
	for name, value := range map[string]uint64{
		"indexedItems":    decoded.IndexedItems,
		"indexedSessions": decoded.IndexedSessions,
		"indexedEvents":   decoded.IndexedEvents,
		"indexedSources":  decoded.IndexedSources,
	} {
		if value > MaxSafeStatusCounter {
			return fmt.Errorf("status counter %s exceeds maximum %d", name, MaxSafeStatusCounter)
		}
	}
	*s = StatusRecord(decoded)
	return nil
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
	MCPToolCall       *MCPToolCall         `json:"mcpToolCall,omitempty"`
	MCPExchange       *MCPExchange         `json:"mcpExchange,omitempty"`
	StructuredContent json.RawMessage      `json:"structuredContent,omitempty"`
	Content           *CoreContentMetadata `json:"content,omitempty"`
	Citations         []Citation           `json:"citations,omitempty"`
}

// UnmarshalJSON preserves Event's permissive outer-field policy while requiring
// a present mcpToolCall value to be the exact nested object rather than null.
func (value *Event) UnmarshalJSON(data []byte) error {
	if err := rejectDuplicateJSONMembers(data); err != nil {
		return err
	}
	type eventWire Event
	var wire eventWire
	if err := json.Unmarshal(data, &wire); err != nil {
		return err
	}
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(data, &fields); err != nil {
		return err
	}
	if raw, exists := fields["mcpToolCall"]; exists && bytes.Equal(bytes.TrimSpace(raw), []byte("null")) {
		return fmt.Errorf("mcpToolCall must be an object when present")
	}
	if raw, exists := fields["mcpExchange"]; exists && bytes.Equal(bytes.TrimSpace(raw), []byte("null")) {
		return fmt.Errorf("mcpExchange must be an object when present")
	}
	decoded := Event(wire)
	if decoded.MCPExchange != nil && decoded.MCPExchange.Response != nil &&
		decoded.MCPExchange.Response.Text.CaptureStatus == MCPTextCaptureStatusNormalizedBody && decoded.Text == "" {
		return fmt.Errorf("normalized MCP response body requires nonempty event text")
	}
	*value = decoded
	return nil
}

// MCP response and capture enums retain the contract's snake_case wire values.
type MCPResponseStatus string
type MCPFailureKind string
type MCPPayloadOmissionReason string

const (
	MCPResponseStatusSucceeded MCPResponseStatus = "succeeded"
	MCPResponseStatusFailed    MCPResponseStatus = "failed"
	MCPResponseStatusCancelled MCPResponseStatus = "cancelled"
	MCPResponseStatusTimedOut  MCPResponseStatus = "timed_out"
	MCPResponseStatusUnknown   MCPResponseStatus = "unknown"

	MCPFailureKindToolReported MCPFailureKind = "tool_reported"
	MCPFailureKindInvocation   MCPFailureKind = "invocation"
	MCPFailureKindUnknown      MCPFailureKind = "unknown"

	MCPPayloadOmissionReasonSizeLimit MCPPayloadOmissionReason = "size_limit"

	MCPJSONCaptureStatusPresent        = "present"
	MCPJSONCaptureStatusAbsent         = "absent"
	MCPJSONCaptureStatusUnavailable    = "unavailable"
	MCPJSONCaptureStatusOmitted        = "omitted"
	MCPTextCaptureStatusNormalizedBody = "normalized_body"
)

// MCPExchange is the closed, content-governed MCP invocation/response record.
type MCPExchange struct {
	ProviderCallID string         `json:"providerCallId"`
	Invocation     *MCPInvocation `json:"invocation,omitempty"`
	Response       *MCPResponse   `json:"response,omitempty"`
}

type MCPInvocation struct {
	Server    string         `json:"server"`
	Tool      string         `json:"tool"`
	Arguments MCPJSONCapture `json:"arguments"`
}

type MCPResponse struct {
	Status      MCPResponseStatus `json:"status"`
	FailureKind *MCPFailureKind   `json:"failureKind,omitempty"`
	DurationNS  *uint64           `json:"durationNs,omitempty"`
	Text        MCPTextCapture    `json:"text"`
	Payload     MCPJSONCapture    `json:"payload"`
}

// MCPJSONCapture preserves a present value as raw JSON without rewriting nested keys.
type MCPJSONCapture struct {
	CaptureStatus        string                    `json:"captureStatus"`
	Value                json.RawMessage           `json:"value,omitempty"`
	Reason               *MCPPayloadOmissionReason `json:"reason,omitempty"`
	ObservedEncodedBytes *uint64                   `json:"observedEncodedBytes,omitempty"`
}

type MCPTextCapture struct {
	CaptureStatus        string                    `json:"captureStatus"`
	Reason               *MCPPayloadOmissionReason `json:"reason,omitempty"`
	ObservedEncodedBytes *uint64                   `json:"observedEncodedBytes,omitempty"`
}

func (value *MCPExchange) UnmarshalJSON(data []byte) error {
	raw, err := decodeMCPExactObject(data)
	if err != nil {
		return err
	}
	normalized, err := normalizeMCPExchange(raw)
	if err != nil {
		return err
	}
	decoded := MCPExchange{ProviderCallID: normalized["providerCallId"].(string)}
	if invocation, exists := normalized["invocation"]; exists {
		decoded.Invocation = &MCPInvocation{}
		if err := decodeMCPNormalized(invocation, decoded.Invocation); err != nil {
			return err
		}
	}
	if response, exists := normalized["response"]; exists {
		decoded.Response = &MCPResponse{}
		if err := decodeMCPNormalized(response, decoded.Response); err != nil {
			return err
		}
	}
	*value = decoded
	return nil
}

func (value *MCPInvocation) UnmarshalJSON(data []byte) error {
	raw, err := decodeMCPExactObject(data)
	if err != nil {
		return err
	}
	normalized, err := normalizeMCPInvocation(raw)
	if err != nil {
		return err
	}
	decoded := MCPInvocation{
		Server: normalized["server"].(string),
		Tool:   normalized["tool"].(string),
	}
	if err := decodeMCPNormalized(normalized["arguments"], &decoded.Arguments); err != nil {
		return err
	}
	if decoded.Arguments.CaptureStatus == MCPJSONCaptureStatusPresent {
		var arguments map[string]json.RawMessage
		if err := json.Unmarshal(decoded.Arguments.Value, &arguments); err != nil {
			return fmt.Errorf("present MCP invocation arguments must be a JSON object: %w", err)
		}
		if arguments == nil {
			return fmt.Errorf("present MCP invocation arguments must be a JSON object")
		}
	}
	*value = decoded
	return nil
}

func (value *MCPResponse) UnmarshalJSON(data []byte) error {
	raw, err := decodeMCPExactObject(data)
	if err != nil {
		return err
	}
	normalized, err := normalizeMCPResponse(raw)
	if err != nil {
		return err
	}
	decoded := MCPResponse{Status: MCPResponseStatus(normalized["status"].(string))}
	if failure, exists := normalized["failureKind"].(string); exists {
		typed := MCPFailureKind(failure)
		decoded.FailureKind = &typed
	}
	if duration, exists := normalized["durationNs"]; exists {
		parsed, err := mcpSafeIntegerValue(duration)
		if err != nil {
			return err
		}
		decoded.DurationNS = &parsed
	}
	if err := decodeMCPNormalized(normalized["text"], &decoded.Text); err != nil {
		return err
	}
	if err := decodeMCPNormalized(normalized["payload"], &decoded.Payload); err != nil {
		return err
	}
	*value = decoded
	return nil
}

func (value *MCPJSONCapture) UnmarshalJSON(data []byte) error {
	raw, err := decodeMCPExactObject(data)
	if err != nil {
		return err
	}
	normalized, err := normalizeMCPCapture(raw, "JSON capture", false, false)
	if err != nil {
		return err
	}
	decoded := MCPJSONCapture{CaptureStatus: normalized["captureStatus"].(string)}
	if captured, exists := normalized["value"]; exists {
		decoded.Value, err = json.Marshal(captured)
		if err != nil {
			return err
		}
	}
	if err := decodeMCPCommonCapture(normalized, &decoded.Reason, &decoded.ObservedEncodedBytes); err != nil {
		return err
	}
	*value = decoded
	return nil
}

func (value *MCPTextCapture) UnmarshalJSON(data []byte) error {
	raw, err := decodeMCPExactObject(data)
	if err != nil {
		return err
	}
	normalized, err := normalizeMCPCapture(raw, "text capture", false, true)
	if err != nil {
		return err
	}
	decoded := MCPTextCapture{CaptureStatus: normalized["captureStatus"].(string)}
	if err := decodeMCPCommonCapture(normalized, &decoded.Reason, &decoded.ObservedEncodedBytes); err != nil {
		return err
	}
	*value = decoded
	return nil
}

func decodeMCPCommonCapture(
	value map[string]any,
	reason **MCPPayloadOmissionReason,
	observed **uint64,
) error {
	if raw, exists := value["reason"].(string); exists {
		typed := MCPPayloadOmissionReason(raw)
		*reason = &typed
	}
	if raw, exists := value["observedEncodedBytes"]; exists {
		parsed, err := mcpSafeIntegerValue(raw)
		if err != nil {
			return err
		}
		*observed = &parsed
	}
	return nil
}

func decodeMCPExactObject(data []byte) (map[string]any, error) {
	if err := rejectDuplicateJSONMembers(data); err != nil {
		return nil, err
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	var value map[string]any
	if err := decoder.Decode(&value); err != nil {
		return nil, err
	}
	if value == nil {
		return nil, fmt.Errorf("MCP value must be an object")
	}
	return value, ensureJSONEnd(decoder)
}

func decodeMCPNormalized(value any, target any) error {
	data, err := json.Marshal(value)
	if err != nil {
		return err
	}
	return json.Unmarshal(data, target)
}

func mcpSafeIntegerValue(value any) (uint64, error) {
	if err := validateMCPSafeInteger(value, "numeric value"); err != nil {
		return 0, err
	}
	switch typed := value.(type) {
	case json.Number:
		return strconv.ParseUint(string(typed), 10, 64)
	case uint64:
		return typed, nil
	case int:
		return uint64(typed), nil
	default:
		return 0, fmt.Errorf("invalid MCP integer")
	}
}

// MCPToolCall identifies the MCP server and tool represented by an event.
type MCPToolCall struct {
	Server string `json:"server"`
	Tool   string `json:"tool"`
}

// UnmarshalJSON enforces the exact closed MCP tool-call pair.
func (value *MCPToolCall) UnmarshalJSON(data []byte) error {
	decoder := json.NewDecoder(bytes.NewReader(data))
	opening, err := decoder.Token()
	if err != nil {
		return err
	}
	if delimiter, ok := opening.(json.Delim); !ok || delimiter != '{' {
		return fmt.Errorf("mcpToolCall must be an object")
	}

	components := make(map[string]string, 2)
	for decoder.More() {
		keyToken, err := decoder.Token()
		if err != nil {
			return err
		}
		key, ok := keyToken.(string)
		if !ok {
			return fmt.Errorf("mcpToolCall member name must be a string")
		}
		if key != "server" && key != "tool" {
			return fmt.Errorf("mcpToolCall contains unknown member %q", key)
		}
		if _, duplicate := components[key]; duplicate {
			return fmt.Errorf("mcpToolCall contains duplicate member %q", key)
		}
		var raw json.RawMessage
		if err := decoder.Decode(&raw); err != nil {
			return err
		}
		component, err := decodeMCPToolCallComponent(raw, key)
		if err != nil {
			return err
		}
		components[key] = component
	}
	if _, err := decoder.Token(); err != nil {
		return err
	}
	if err := ensureJSONEnd(decoder); err != nil {
		return err
	}

	server, hasServer := components["server"]
	if !hasServer {
		return fmt.Errorf("mcpToolCall.server is required")
	}
	tool, hasTool := components["tool"]
	if !hasTool {
		return fmt.Errorf("mcpToolCall.tool is required")
	}
	*value = MCPToolCall{Server: server, Tool: tool}
	return nil
}

func decodeMCPToolCallComponent(raw json.RawMessage, field string) (string, error) {
	if !utf8.Valid(raw) {
		return "", fmt.Errorf("mcpToolCall.%s contains invalid UTF-8", field)
	}
	var value string
	if err := json.Unmarshal(raw, &value); err != nil {
		return "", fmt.Errorf("mcpToolCall.%s must be a string: %w", field, err)
	}
	if hasUnpairedJSONSurrogate(raw) {
		return "", fmt.Errorf("mcpToolCall.%s contains an invalid Unicode surrogate", field)
	}
	if value == "" {
		return "", fmt.Errorf("mcpToolCall.%s must be nonempty", field)
	}
	if len(value) > maxMCPToolCallComponentBytes {
		return "", fmt.Errorf(
			"mcpToolCall.%s exceeds %d decoded UTF-8 bytes",
			field,
			maxMCPToolCallComponentBytes,
		)
	}
	return value, nil
}

func hasUnpairedJSONSurrogate(raw []byte) bool {
	for index := 1; index+1 < len(raw); index++ {
		if raw[index] != '\\' {
			continue
		}
		index++
		if index >= len(raw)-1 || raw[index] != 'u' {
			continue
		}
		if index+4 >= len(raw) {
			return true
		}
		code, err := strconv.ParseUint(string(raw[index+1:index+5]), 16, 16)
		if err != nil {
			return true
		}
		index += 4
		if code >= 0xd800 && code <= 0xdbff {
			if index+6 >= len(raw) || raw[index+1] != '\\' || raw[index+2] != 'u' {
				return true
			}
			low, err := strconv.ParseUint(string(raw[index+3:index+7]), 16, 16)
			if err != nil || low < 0xdc00 || low > 0xdfff {
				return true
			}
			index += 6
		} else if code >= 0xdc00 && code <= 0xdfff {
			return true
		}
	}
	return false
}

func ensureJSONEnd(decoder *json.Decoder) error {
	var trailing any
	if err := decoder.Decode(&trailing); err != io.EOF {
		if err == nil {
			return fmt.Errorf("mcpToolCall has trailing JSON data")
		}
		return err
	}
	return nil
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
