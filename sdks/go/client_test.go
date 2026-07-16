package ctxagenthistory

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

func TestStatusDecodesAgentHistoryV1(t *testing.T) {
	client := NewClient(WithTransport(fakeTransport{
		response: `{
			"schema_version": 1,
			"initialized": true,
			"data_root": "/tmp/ctx",
			"database_path": "/tmp/ctx/history.sqlite3",
			"config_path": "/tmp/ctx/config.toml",
			"indexed_items": 7,
			"indexed_sources": 2,
			"cataloged_sessions": 3,
			"indexed_catalog_sessions": 2,
			"pending_catalog_sessions": 1,
			"failed_catalog_sessions": 0,
			"stale_catalog_sessions": 0,
			"local_only": true
		}`,
	}))

	status, err := client.Status(context.Background())
	if err != nil {
		t.Fatalf("Status returned error: %v", err)
	}
	if status.ContractVersion != APIVersion || status.Operation != "status" {
		t.Fatalf("unexpected envelope: %+v", status)
	}
	if !status.Status.Initialized || status.Status.IndexedItems != 7 || !status.Status.LocalOnly {
		t.Fatalf("unexpected status: %+v", status)
	}
}

func TestSearchBuildsAgentHistoryV1Operation(t *testing.T) {
	transport := &recordingTransport{response: `{
			"schema_version": 2,
			"query": {"version":"ctx-search-v1","any":[{"all":"panic"},{"semantic":"sqlite retry"}]},
			"query_execution": {"query_version":"ctx-search-v1","candidate_strategy":"bounded_union_rrf_v1"},
		"filters": {},
		"freshness": {"mode": "off", "status": "skipped", "source_count": 0, "totals": {}},
		"generated_at": "2026-01-01T00:00:00Z",
		"results": [],
		"pagination": {},
		"truncation": {}
	}`}
	client := NewClient(WithTransport(transport))
	query := NewSearchQuery(SearchAll("panic"), SearchSemantic("sqlite retry"))

	_, err := client.Search(context.Background(), SearchOptions{
		Query:                 &query,
		Limit:                 5,
		Backend:               "hybrid",
		Provider:              "codex",
		Workspace:             "ctx",
		Since:                 "30d",
		EventType:             "message",
		File:                  "crates/ctx-cli/src/main.rs",
		Session:               "00000000-0000-0000-0000-000000000001",
		Events:                true,
		Refresh:               "off",
		IncludeCurrentSession: true,
	})
	if err != nil {
		t.Fatalf("Search returned error: %v", err)
	}

	want := []string{
		"search", "--query-json", `{"version":"ctx-search-v1","any":[{"all":"panic"},{"semantic":"sqlite retry"}]}`, "--json", "--limit", "5",
		"--backend", "hybrid",
		"--provider", "codex",
		"--workspace", "ctx",
		"--since", "30d",
		"--event-type", "message",
		"--file", "crates/ctx-cli/src/main.rs",
		"--session", "00000000-0000-0000-0000-000000000001",
		"--refresh", "off",
		"--events",
		"--include-current-session",
	}
	if !reflect.DeepEqual(transport.op.Args, want) {
		t.Fatalf("args mismatch\nwant: %#v\n got: %#v", want, transport.op.Args)
	}
}

func TestSearchCamelizesRetrievalJSON(t *testing.T) {
	client := NewClient(WithTransport(fakeTransport{response: `{
			"schema_version": 2,
			"query": {"version":"ctx-search-v1","any":[{"all":"agent history"}]},
			"query_execution": {
				"query_version":"ctx-search-v1",
				"candidate_strategy":"bounded_union_rrf_v1",
				"resolved":{"query_bytes":8192},
				"consumed":{"query_bytes":13},
				"semantic":{"attempted":false,"required":false,"readiness":"unavailable","effective_backend":"lexical","positive_text_rule_version":"ctx-search-positive-text-v1"}
			},
		"retrieval": {
			"requested_mode": "hybrid",
			"effective_mode": "lexical",
			"semantic_weight": 0.0,
			"semantic_fallback_code": "semantic_retrieval_failed",
			"semantic_fallback": "semantic_retrieval_failed",
			"coverage": {"embedded_items": 4, "indexed_now": 1},
			"diagnostics": {"query_embed_ms": 2}
		},
		"results": [{
			"result_scope": "event"
		}]
		}`}))
	query := NewSearchQuery(SearchAll("agent history"))
	response, err := client.Search(context.Background(), SearchOptions{Query: &query})
	if err != nil {
		t.Fatalf("Search returned error: %v", err)
	}
	retrieval, ok := response.Search.Retrieval.(map[string]any)
	if !ok {
		t.Fatalf("top-level retrieval was not decoded: %#v", response.Search.Retrieval)
	}
	if retrieval["requestedMode"] != "hybrid" || retrieval["effectiveMode"] != "lexical" || retrieval["semanticWeight"] != 0.0 {
		t.Fatalf("top-level retrieval was not camelized: %#v", retrieval)
	}
	if retrieval["semanticFallbackCode"] != "semantic_retrieval_failed" {
		t.Fatalf("retrieval fallback code was not camelized: %#v", retrieval)
	}
	coverage, ok := retrieval["coverage"].(map[string]any)
	if !ok || coverage["embeddedItems"] != float64(4) || coverage["indexedNow"] != float64(1) {
		t.Fatalf("retrieval coverage was not camelized: %#v", retrieval)
	}
	diagnostics, ok := retrieval["diagnostics"].(map[string]any)
	if !ok || diagnostics["queryEmbedMs"] != float64(2) {
		t.Fatalf("retrieval diagnostics were not camelized: %#v", retrieval)
	}
	if response.Search.SchemaVersion != SearchSchemaVersion || response.Search.QueryExecution.QueryVersion != SearchQueryVersion {
		t.Fatalf("schema-v2 search contract was not preserved: %+v", response.Search)
	}
	if response.Search.Query == nil || response.Search.Query.Any[0].Value() != "agent history" {
		t.Fatalf("canonical query was not decoded: %+v", response.Search.Query)
	}
}

func TestSearchRequiresQueryOrFileBeforeTransport(t *testing.T) {
	transport := &recordingTransport{response: `{"schema_version":2,"query":null,"query_execution":{},"results":[]}`}
	client := NewClient(WithTransport(transport))

	for name, opts := range map[string]SearchOptions{
		"empty":        {},
		"filters only": {Refresh: "off", Limit: 5},
	} {
		t.Run(name, func(t *testing.T) {
			if _, err := client.Search(context.Background(), opts); !IsErrorKind(err, ErrorKindInvalidArgument) {
				t.Fatalf("Search error kind mismatch: %v", err)
			}
		})
	}
	if transport.op.Args != nil {
		t.Fatalf("Search invoked transport despite invalid input: %#v", transport.op.Args)
	}
}

func TestSearchQueryValidationRejectsInvalidShapesBeforeTransport(t *testing.T) {
	transport := &recordingTransport{response: `{"schema_version":2,"query":null,"query_execution":{},"results":[]}`}
	client := NewClient(WithTransport(transport))
	invalid := []SearchQuery{
		{Version: SearchQueryVersion},
		{Version: SearchQueryVersion, MustNot: []SearchClause{SearchAll("excluded")}},
		{Version: SearchQueryVersion, Must: []SearchClause{SearchSemantic("not allowed")}},
		{Version: SearchQueryVersion, Any: []SearchClause{SearchSemantic("one"), SearchSemantic("two")}},
		NewSearchQuery(SearchLiteral("x")),
	}
	for _, query := range invalid {
		if _, err := client.Search(context.Background(), SearchOptions{Query: &query}); !IsErrorKind(err, ErrorKindInvalidArgument) {
			t.Fatalf("expected invalid query error for %+v, got %v", query, err)
		}
	}
	if transport.op.Args != nil {
		t.Fatalf("invalid query invoked transport: %#v", transport.op.Args)
	}
}

func TestSearchQueryJSONIsClosedAndCanonical(t *testing.T) {
	var query SearchQuery
	if err := json.Unmarshal([]byte(`{"version":"ctx-search-v1","any":[{"all":"  disk   pressure "},{"all":"disk pressure"}],"must_not":[{"literal":" logs_2.db "}]}`), &query); err != nil {
		t.Fatalf("decode canonical query: %v", err)
	}
	serialized, err := SerializeSearchQuery(query)
	if err != nil {
		t.Fatalf("serialize canonical query: %v", err)
	}
	want := `{"version":"ctx-search-v1","any":[{"all":"disk pressure"}],"must_not":[{"literal":"logs_2.db"}]}`
	if serialized != want {
		t.Fatalf("canonical query mismatch\nwant: %s\n got: %s", want, serialized)
	}
	for _, raw := range []string{
		`{"version":"ctx-search-v1","unknown":true,"any":[{"all":"ctx"}]}`,
		`{"version":"ctx-search-v1","any":[{"all":"ctx","phrase":"ctx"}]}`,
		`{"version":"ctx-search-v1","must":[{"semantic":"ctx"}]}`,
	} {
		if err := json.Unmarshal([]byte(raw), &query); err == nil {
			t.Fatalf("expected closed-query rejection for %s", raw)
		}
	}
}

func TestSearchRejectsLegacyStringAndCamelCaseSchemaV2Fields(t *testing.T) {
	query := NewSearchQuery(SearchAll("ctx"))
	for _, response := range []string{
		`{"schema_version":2,"query":"ctx","query_execution":{},"results":[]}`,
		`{"schemaVersion":2,"query":{"version":"ctx-search-v1","any":[{"all":"ctx"}]},"query_execution":{},"results":[]}`,
		`{"schema_version":2,"query":{"version":"ctx-search-v1","any":[{"all":"ctx"}]},"queryExecution":{},"results":[]}`,
	} {
		client := NewClient(WithTransport(fakeTransport{response: response}))
		if _, err := client.Search(context.Background(), SearchOptions{Query: &query}); !IsErrorKind(err, ErrorKindDecode) {
			t.Fatalf("expected schema-v2 decode rejection, got %v", err)
		}
	}
}

func TestSearchExecutionDiagnosticsUseExactSnakeCaseKeys(t *testing.T) {
	data, err := json.Marshal(SearchQueryExecution{
		QueryVersion:      SearchQueryVersion,
		CandidateStrategy: "bounded_union_rrf_v1",
		Semantic: SearchSemanticExecution{
			Readiness:               SearchSemanticReady,
			EffectiveBackend:        SearchEffectiveHybrid,
			PositiveTextRuleVersion: "ctx-search-positive-text-v1",
		},
	})
	if err != nil {
		t.Fatalf("marshal diagnostics: %v", err)
	}
	encoded := string(data)
	for _, key := range []string{`"query_version"`, `"candidate_strategy"`, `"effective_backend"`, `"positive_text_rule_version"`} {
		if !strings.Contains(encoded, key) {
			t.Fatalf("diagnostics missing snake_case key %s: %s", key, encoded)
		}
	}
	if strings.Contains(encoded, "queryVersion") || strings.Contains(encoded, "effectiveBackend") {
		t.Fatalf("diagnostics emitted a camelCase alias: %s", encoded)
	}
}

func TestShowAndLocateValidateRequiredEventID(t *testing.T) {
	client := NewClient(WithTransport(fakeTransport{response: `{}`}))
	if _, err := client.ShowEvent(context.Background(), ShowEventOptions{}); !IsErrorKind(err, ErrorKindInvalidArgument) {
		t.Fatalf("ShowEvent error kind mismatch: %v", err)
	}
	if _, err := client.LocateEvent(context.Background(), LocateEventOptions{}); !IsErrorKind(err, ErrorKindInvalidArgument) {
		t.Fatalf("LocateEvent error kind mismatch: %v", err)
	}
}

func TestRejectsWrongCanonicalEnvelope(t *testing.T) {
	client := NewClient(WithTransport(fakeTransport{response: `{
		"contractVersion": "agent-history-v2",
		"schemaVersion": 1,
		"operation": "status",
		"backend": {"kind": "local"},
		"status": {"initialized": true, "localOnly": true}
	}`}))
	if _, err := client.Status(context.Background()); !IsErrorKind(err, ErrorKindUnsupportedSchema) {
		t.Fatalf("expected unsupported schema error, got %v", err)
	}

	client = NewClient(WithTransport(fakeTransport{response: `{
		"contractVersion": "agent-history-v1",
		"schemaVersion": 1,
		"operation": "search",
		"backend": {"kind": "local"},
		"status": {"initialized": true, "localOnly": true}
	}`}))
	if _, err := client.Status(context.Background()); !IsErrorKind(err, ErrorKindDecode) {
		t.Fatalf("expected operation decode error, got %v", err)
	}
}

func TestLegacyShowEventSourceObjectNormalizesToTypedEvent(t *testing.T) {
	client := NewClient(WithTransport(fakeTransport{response: `{
		"event": {
			"ctx_event_id": "event-1",
			"ctx_session_id": "session-1",
			"sequence": 1,
			"event_type": "message",
			"source": {
				"path": "/tmp/session.jsonl",
				"cursor": "line:1",
				"exists": true,
				"source_id": "source-1",
				"source_format": "codex_session_jsonl"
			},
			"text": "hello"
		},
		"events": []
	}`}))

	response, err := client.ShowEvent(context.Background(), ShowEventOptions{ID: "event-1"})
	if err != nil {
		t.Fatalf("ShowEvent returned error: %v", err)
	}
	if response.Event.Event == nil || response.Event.Event.Source != "" {
		t.Fatalf("unexpected normalized event source: %+v", response.Event.Event)
	}
	if response.Event.Source == nil || response.Event.Source.Path != "/tmp/session.jsonl" {
		t.Fatalf("expected source location from legacy event source, got %+v", response.Event.Source)
	}
}

func TestLocalCLIAdapterCommandFailureIsStructured(t *testing.T) {
	adapter := NewLocalCLIAdapter(WithCLIPath("ctx"))
	adapter.runner = fakeRunner{
		result: commandResult{
			Stderr:   []byte("no importable provider history sources found\n"),
			ExitCode: 1,
			Err:      errors.New("exit status 1"),
		},
	}

	_, err := adapter.Do(context.Background(), Operation{Name: "import", Args: []string{"import", "--json"}})
	var sdkErr *Error
	if !errors.As(err, &sdkErr) {
		t.Fatalf("expected structured error, got %T %v", err, err)
	}
	if sdkErr.Kind != ErrorKindCommandFailed || sdkErr.ExitCode != 1 || len(sdkErr.Command) != 3 {
		t.Fatalf("unexpected structured error: %+v", sdkErr)
	}
}

func TestLocalCLIAdapterClassifiesContextTimeout(t *testing.T) {
	adapter := NewLocalCLIAdapter(WithCLIPath("ctx"))
	adapter.runner = fakeRunner{result: commandResult{Err: context.DeadlineExceeded, ExitCode: -1}}

	_, err := adapter.Do(context.Background(), Operation{Name: "status", Args: []string{"status", "--json"}})
	if !IsErrorKind(err, ErrorKindTimeout) {
		t.Fatalf("expected timeout error, got %v", err)
	}
}

func TestLocalCLIAdapterAddsDataRootEnvironment(t *testing.T) {
	runner := &recordingRunner{result: commandResult{Stdout: []byte(`{"schema_version":1}`)}}
	adapter := NewLocalCLIAdapter(WithCLIPath("ctx"), WithDataRoot("/tmp/ctx-data"))
	adapter.runner = runner

	_, err := adapter.Do(context.Background(), Operation{Name: "status", Args: []string{"status", "--json"}})
	if err != nil {
		t.Fatalf("Do returned error: %v", err)
	}
	if !contains(runner.env, "CTX_DATA_ROOT=/tmp/ctx-data") {
		t.Fatalf("CTX_DATA_ROOT missing from env: %#v", runner.env)
	}
}

func TestHostedClientPlaceholder(t *testing.T) {
	client := NewHostedClient(HostedConfig{BaseURL: "https://example.invalid", APIKey: "test"})
	_, err := client.Status(context.Background())
	if !IsErrorKind(err, ErrorKindHostedNotImplemented) {
		t.Fatalf("unexpected hosted error: %v", err)
	}
	version, err := client.Version(context.Background())
	if err != nil {
		t.Fatalf("hosted Version returned error: %v", err)
	}
	if version.APIVersion != APIVersion || version.Transport != "hosted-placeholder" || version.CtxVersion != "" {
		t.Fatalf("unexpected hosted version: %+v", version)
	}
}

func TestVersionUsesTransport(t *testing.T) {
	client := NewClient(WithTransport(fakeTransport{response: "ctx 9.9.9\n"}))
	version, err := client.Version(context.Background())
	if err != nil {
		t.Fatalf("Version returned error: %v", err)
	}
	if version.APIVersion != APIVersion || version.SDKVersion != SDKVersion || version.CtxVersion != "ctx 9.9.9" {
		t.Fatalf("unexpected version: %+v", version)
	}
}

func TestContractErrorKindsArePublicConstants(t *testing.T) {
	for _, kind := range []ErrorKind{
		ErrorKindInvalidArgument,
		ErrorKindNotFound,
		ErrorKindNotInitialized,
		ErrorKindUnavailable,
		ErrorKindTimeout,
		ErrorKindCancelled,
		ErrorKindHostedNotImplemented,
		ErrorKindCommandFailed,
		ErrorKindDecode,
		ErrorKindUnknown,
	} {
		if kind == "" {
			t.Fatalf("empty error kind")
		}
	}
}

func TestCanonicalFixturesExposeTypedFields(t *testing.T) {
	search := readFixture[SearchResponse](t, "search.results.json")
	if search.ContractVersion != APIVersion || search.Operation != OperationSearch || search.Backend.Kind != BackendKindLocal {
		t.Fatalf("unexpected search envelope: %+v", search.Envelope)
	}
	if len(search.Search.Results) != 1 || search.Search.Results[0].WhyMatched[0] != "text" {
		t.Fatalf("unexpected typed search results: %+v", search.Search.Results)
	}
	if search.Search.Results[0].ResultType != "event" || search.Search.Results[0].Citations[0].TargetType != "event" {
		t.Fatalf("unexpected typed result/citation type: %+v", search.Search.Results[0])
	}
	if search.Search.Pagination == nil || search.Search.Pagination.Limit != 20 {
		t.Fatalf("unexpected pagination: %+v", search.Search.Pagination)
	}
	if search.Search.Truncation == nil || search.Search.Truncation.Truncated {
		t.Fatalf("unexpected truncation: %+v", search.Search.Truncation)
	}

	session := readFixture[ShowSessionResponse](t, "show-session.transcript.json")
	if session.Session.Session == nil || session.Session.Session.ProviderSessionID != "codex-fixture-session" {
		t.Fatalf("unexpected typed session: %+v", session.Session.Session)
	}

	location := readFixture[LocateEventResponse](t, "locate-event.location.json")
	if location.Location.Resume == nil || location.Location.Resume.Cursor != "line:2" {
		t.Fatalf("unexpected typed resume location: %+v", location.Location.Resume)
	}

	errorEnvelope := readFixture[ErrorResponse](t, "error.not-supported.json")
	if errorEnvelope.Error.Code != ErrorKindHostedNotImplemented || errorEnvelope.Backend.Kind != BackendKindHosted {
		t.Fatalf("unexpected error envelope: %+v", errorEnvelope)
	}
}

func TestContractFixturesIfPresent(t *testing.T) {
	fixtureRoot := filepath.Clean("../../contracts/agent-history-v1/fixtures")
	entries, err := os.ReadDir(fixtureRoot)
	if errors.Is(err, os.ErrNotExist) {
		t.Skip("agent-history-v1 fixtures are not present yet")
	}
	if err != nil {
		t.Fatalf("read fixture root: %v", err)
	}

	seen := false
	for _, entry := range entries {
		if entry.IsDir() || filepath.Ext(entry.Name()) != ".json" {
			continue
		}
		seen = true
		path := filepath.Join(fixtureRoot, entry.Name())
		data, err := os.ReadFile(path)
		if err != nil {
			t.Fatalf("read fixture %s: %v", path, err)
		}
		var envelope struct {
			Operation string          `json:"operation"`
			Response  json.RawMessage `json:"response"`
		}
		if err := json.Unmarshal(data, &envelope); err == nil && len(envelope.Response) > 0 {
			assertFixtureDecodes(t, path, envelope.Operation, envelope.Response)
			continue
		}
		assertFixtureDecodes(t, path, operationFromFilename(entry.Name()), data)
	}
	if !seen {
		t.Skip("agent-history-v1 fixture directory is present but empty")
	}
}

func assertFixtureDecodes(t *testing.T, path, operation string, data []byte) {
	t.Helper()
	var err error
	switch operation {
	case "status":
		var value StatusResponse
		err = json.Unmarshal(data, &value)
	case "init", "setup":
		var value InitResponse
		err = json.Unmarshal(data, &value)
	case "sources":
		var value SourcesResponse
		err = json.Unmarshal(data, &value)
	case "import", "sync":
		var value ImportResponse
		err = json.Unmarshal(data, &value)
	case "search":
		var value SearchResponse
		err = json.Unmarshal(data, &value)
	case "show_event", "showEvent":
		var value ShowEventResponse
		err = json.Unmarshal(data, &value)
	case "show_session", "showSession":
		var value ShowSessionResponse
		err = json.Unmarshal(data, &value)
	case "locate_event", "locateEvent":
		var value LocateEventResponse
		err = json.Unmarshal(data, &value)
	case "locate_session", "locateSession":
		var value LocateSessionResponse
		err = json.Unmarshal(data, &value)
	case "error":
		var value ErrorResponse
		err = json.Unmarshal(data, &value)
	default:
		var value map[string]any
		err = json.Unmarshal(data, &value)
	}
	if err != nil {
		t.Fatalf("decode fixture %s as %s: %v", path, operation, err)
	}
}

func readFixture[T any](t *testing.T, name string) T {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("../../contracts/agent-history-v1/fixtures", name))
	if errors.Is(err, os.ErrNotExist) {
		t.Skip("agent-history-v1 fixtures are not present yet")
	}
	if err != nil {
		t.Fatalf("read fixture %s: %v", name, err)
	}
	var value T
	if err := json.Unmarshal(data, &value); err != nil {
		t.Fatalf("decode fixture %s: %v", name, err)
	}
	return value
}

func operationFromFilename(name string) string {
	base := name[:len(name)-len(filepath.Ext(name))]
	if prefix, _, ok := strings.Cut(base, "."); ok {
		base = prefix
	}
	switch base {
	case "setup":
		return "init"
	case "show-event":
		return "showEvent"
	case "show-session":
		return "showSession"
	case "locate-event":
		return "locateEvent"
	case "locate-session":
		return "locateSession"
	default:
		return base
	}
}

type fakeTransport struct {
	response string
	err      error
}

func (f fakeTransport) Do(context.Context, Operation) ([]byte, error) {
	if f.err != nil {
		return nil, f.err
	}
	return []byte(f.response), nil
}

type recordingTransport struct {
	response string
	op       Operation
}

func (r *recordingTransport) Do(_ context.Context, op Operation) ([]byte, error) {
	r.op = op
	return []byte(r.response), nil
}

type fakeRunner struct {
	result commandResult
}

func (f fakeRunner) Run(context.Context, string, []string, []string) commandResult {
	return f.result
}

type recordingRunner struct {
	result commandResult
	path   string
	args   []string
	env    []string
}

func (r *recordingRunner) Run(_ context.Context, path string, args []string, env []string) commandResult {
	r.path = path
	r.args = append([]string(nil), args...)
	r.env = append([]string(nil), env...)
	return r.result
}

func contains(values []string, want string) bool {
	for _, value := range values {
		if value == want {
			return true
		}
	}
	return false
}
