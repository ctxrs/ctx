package ctxagenthistory

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"testing"
)

func TestStatusDecodesAgentHistoryV1(t *testing.T) {
	client := NewClient(WithTransport(fakeTransport{
		response: `{
			"schema_version": 1,
			"data_root": "/tmp/ctx",
			"config_path": "/tmp/ctx/config.toml",
			"indexed_items": 7,
			"indexed_sessions": 3,
			"indexed_events": 4,
			"indexed_sources": 2,
			"lexical": {"status": "ready", "generation_id": "gen-7"},
			"refresh": {"status": "ready", "generation_id": "gen-7"},
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
	if !status.Status.Initialized || status.Status.IndexedItems != 7 ||
		status.Status.IndexedSessions != 3 || status.Status.IndexedEvents != 4 ||
		status.Status.Lexical["generationId"] != "gen-7" || !status.Status.LocalOnly {
		t.Fatalf("unexpected status: %+v", status)
	}
}

func TestInitAcceptsMaximumExactCrossSDKStatusCounters(t *testing.T) {
	client := NewClient(WithTransport(fakeTransport{
		response: `{
			"schema_version": 2,
			"initialized": true,
			"data_root": "/tmp/ctx",
			"mode": "ready",
			"indexed_items": 9007199254740991,
			"indexed_sessions": 9007199254740991,
			"indexed_events": 9007199254740991,
			"indexed_sources": 9007199254740991,
			"lexical": {"status": "ready", "generation_id": "gen-64"},
			"refresh": {"status": "ready"}
		}`,
	}))

	initialized, err := client.Init(context.Background(), InitOptions{})
	if err != nil {
		t.Fatalf("Init returned error: %v", err)
	}
	if initialized.Status.IndexedItems != MaxSafeStatusCounter ||
		initialized.Status.IndexedSessions != MaxSafeStatusCounter ||
		initialized.Status.IndexedEvents != MaxSafeStatusCounter ||
		initialized.Status.IndexedSources != MaxSafeStatusCounter {
		t.Fatalf("status counters did not preserve the exact maximum: %+v", initialized.Status)
	}
}

func TestStatusRejectsCountersOutsideExactCrossSDKDomain(t *testing.T) {
	for _, rejected := range []string{"9007199254740993", "18446744073709551615"} {
		t.Run(rejected, func(t *testing.T) {
			client := NewClient(WithTransport(fakeTransport{
				response: `{"initialized":true,"indexed_items":` + rejected + `}`,
			}))

			if _, err := client.Status(context.Background()); !IsErrorKind(err, ErrorKindDecode) {
				t.Fatalf("Status error = %v, want decode_error", err)
			}
		})
	}
}

func TestSearchBuildsAgentHistoryV1Operation(t *testing.T) {
	transport := &recordingTransport{response: `{
		"schema_version": 1,
		"query": "panic",
		"filters": {},
		"freshness": {"mode": "off", "status": "skipped", "source_count": 0, "totals": {}},
		"generated_at": "2026-01-01T00:00:00Z",
		"results": [{"result_scope": "event"}],
		"result_window": {"limit": 1, "returned": 1, "more_available": true},
		"truncation": {}
	}`}
	client := NewClient(WithTransport(transport))
	semanticWeight := 0.35

	response, err := client.Search(context.Background(), SearchOptions{
		Query:                 "panic",
		Terms:                 []string{"sqlite", "retry"},
		Limit:                 5,
		Backend:               "hybrid",
		SemanticWeight:        &semanticWeight,
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
	if response.Search.ResultWindow == nil ||
		response.Search.ResultWindow.Limit != 1 ||
		response.Search.ResultWindow.Returned != 1 ||
		!response.Search.ResultWindow.MoreAvailable {
		t.Fatalf("unexpected result window: %+v", response.Search.ResultWindow)
	}
	if response.Search.Pagination == nil ||
		response.Search.Pagination.Limit != 1 ||
		!response.Search.Pagination.HasMore ||
		response.Search.Pagination.NextCursor != "" {
		t.Fatalf("unexpected compatibility pagination: %+v", response.Search.Pagination)
	}

	want := []string{
		"search", "panic", "--format=json", "--limit", "5",
		"--term", "sqlite", "--term", "retry",
		"--backend", "hybrid",
		"--semantic-weight", "0.35",
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

func TestSearchContentScopeValuesAreClosed(t *testing.T) {
	got := []SearchContentScope{
		SearchContentScopeAll,
		SearchContentScopeTranscript,
		SearchContentScopeCalls,
		SearchContentScopeOutputs,
	}
	want := []SearchContentScope{"all", "transcript", "calls", "outputs"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("content scope values mismatch\nwant: %#v\n got: %#v", want, got)
	}

	transport := &recordingTransport{response: `{"schema_version":1,"results":[]}`}
	client := NewClient(WithTransport(transport))
	if _, err := client.Search(context.Background(), SearchOptions{
		Query:        "agent history",
		ContentScope: SearchContentScope("messages"),
	}); !IsErrorKind(err, ErrorKindInvalidArgument) {
		t.Fatalf("Search error = %v, want invalid_request", err)
	}
	if transport.op.Args != nil {
		t.Fatalf("Search invoked transport for an invalid content scope: %#v", transport.op.Args)
	}
}

func TestSearchForwardsContentScopeOnce(t *testing.T) {
	transport := &recordingTransport{response: `{"schema_version":1,"results":[]}`}
	client := NewClient(WithTransport(transport))

	if _, err := client.Search(context.Background(), SearchOptions{
		Query:        "agent history",
		ContentScope: SearchContentScopeCalls,
	}); err != nil {
		t.Fatalf("Search returned error: %v", err)
	}

	want := []string{"search", "agent history", "--format=json", "--content-scope", "calls"}
	if !reflect.DeepEqual(transport.op.Args, want) {
		t.Fatalf("args mismatch\nwant: %#v\n got: %#v", want, transport.op.Args)
	}
	count := 0
	for _, arg := range transport.op.Args {
		if arg == "--content-scope" {
			count++
		}
	}
	if count != 1 {
		t.Fatalf("--content-scope count = %d, want 1 in %#v", count, transport.op.Args)
	}
}

func TestSearchRejectsContentScopeEventTypeConflictBeforeTransport(t *testing.T) {
	transport := &recordingTransport{response: `{"schema_version":1,"results":[]}`}
	client := NewClient(WithTransport(transport))

	if _, err := client.Search(context.Background(), SearchOptions{
		Query:        "agent history",
		EventType:    "message",
		ContentScope: SearchContentScopeAll,
	}); !IsErrorKind(err, ErrorKindInvalidArgument) {
		t.Fatalf("Search error = %v, want invalid_request", err)
	}
	if transport.op.Args != nil {
		t.Fatalf("Search invoked transport for conflicting filters: %#v", transport.op.Args)
	}
}

func TestSearchCamelizesRetrievalJSON(t *testing.T) {
	client := NewClient(WithTransport(fakeTransport{response: `{
		"schema_version": 1,
		"query": "agent history",
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
			"result_scope": "event",
			"provider": "codex",
			"provider_session_id": "codex-resume-uuid",
			"source_format": "codex_session_jsonl",
			"rank": 1,
			"retrieval_score": 0.98
		}]
	}`}))

	response, err := client.Search(context.Background(), SearchOptions{Query: "agent history"})
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
	hit := response.Search.Results[0]
	if hit.Rank != 1 || hit.RetrievalScore == nil || *hit.RetrievalScore != 0.98 {
		t.Fatalf("search hit rank fields were not decoded: %#v", hit)
	}
	if hit.Provider != "codex" || hit.ProviderSessionID != "codex-resume-uuid" || hit.SourceFormat != "codex_session_jsonl" {
		t.Fatalf("search hit Core identity fields were not decoded: %#v", hit)
	}
	diagnostics, ok := retrieval["diagnostics"].(map[string]any)
	if !ok || diagnostics["queryEmbedMs"] != float64(2) {
		t.Fatalf("retrieval diagnostics were not camelized: %#v", retrieval)
	}
}

func TestSearchRequiresQueryTermOrFileBeforeTransport(t *testing.T) {
	transport := &recordingTransport{response: `{"schema_version":1,"results":[]}`}
	client := NewClient(WithTransport(transport))

	for name, opts := range map[string]SearchOptions{
		"empty":        {},
		"filters only": {Refresh: "off", Limit: 5},
		"blank query":  {Query: "   "},
		"blank terms":  {Terms: []string{"", "   "}},
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

func TestSourcesAndImportPreserveLegitimateNestedSourceSemantics(t *testing.T) {
	for _, test := range []struct {
		name      string
		operation string
		raw       string
		path      []string
	}{
		{
			name:      "sources acquisition",
			operation: "sources",
			raw:       `{"sources":[{"provider":"codex","path":"/configured/root","status":"available","importable":true,"acquisition":{"source":"local_scan","cursor":"opaque-checkpoint"}}]}`,
			path:      []string{"sources", "0", "acquisition"},
		},
		{
			name:      "import source",
			operation: "import",
			raw:       `{"resume":false,"totals":{},"sources":[{"source":{"source":"provider","cursor":"provider-checkpoint"}}]}`,
			path:      []string{"import", "sources", "0", "source"},
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			payload, err := normalizePayload(Operation{Name: test.operation}, []byte(test.raw))
			if err != nil {
				t.Fatalf("normalize payload: %v", err)
			}
			var envelope map[string]any
			if err := json.Unmarshal(payload, &envelope); err != nil {
				t.Fatalf("decode normalized payload: %v", err)
			}
			var current any = envelope
			for _, part := range test.path {
				if index, err := strconv.Atoi(part); err == nil {
					current = current.([]any)[index]
				} else {
					current = current.(map[string]any)[part]
				}
			}
			semantic := current.(map[string]any)
			if semantic["source"] == nil || semantic["cursor"] == nil {
				t.Fatalf("legitimate source semantics were erased: %#v", semantic)
			}
		})
	}
}

func TestShowEventValidatesRequiredEventID(t *testing.T) {
	client := NewClient(WithTransport(fakeTransport{response: `{}`}))
	if _, err := client.ShowEvent(context.Background(), ShowEventOptions{}); !IsErrorKind(err, ErrorKindInvalidArgument) {
		t.Fatalf("ShowEvent error kind mismatch: %v", err)
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

func TestLocalCLIAdapterCommandFailureIsStructured(t *testing.T) {
	adapter := NewLocalCLIAdapter(WithCLIPath("ctx"))
	adapter.runner = fakeRunner{
		result: commandResult{
			Stderr:   []byte("no importable provider history sources found\n"),
			ExitCode: 1,
			Err:      errors.New("exit status 1"),
		},
	}

	_, err := adapter.Do(context.Background(), Operation{Name: "import", Args: []string{"import", "--format=json"}})
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

	_, err := adapter.Do(context.Background(), Operation{Name: "status", Args: []string{"status", "--format=json"}})
	if !IsErrorKind(err, ErrorKindTimeout) {
		t.Fatalf("expected timeout error, got %v", err)
	}
}

func TestLocalCLIAdapterStrictlyDecodesSpawnedStdoutUTF8(t *testing.T) {
	prefix := []byte(`{"event":{"mcp_tool_call":{"server":"`)
	suffix := []byte(`","tool":"tool"}},"events":[]}`)
	invalid := append(append(append([]byte(nil), prefix...), 0xff), suffix...)
	valid := []byte(`{"event":{"mcp_tool_call":{"server":"�","tool":"tool"}},"events":[]}`)

	for _, test := range []struct {
		name    string
		payload []byte
		valid   bool
	}{
		{name: "invalid byte", payload: invalid},
		{name: "encoded replacement character", payload: valid, valid: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			adapter := NewLocalCLIAdapter(
				WithCLIPath(os.Args[0]),
				WithEnv([]string{spawnedStdoutHexEnvironment + "=" + hex.EncodeToString(test.payload)}),
			)
			stdout, err := adapter.Do(context.Background(), Operation{
				Name: "showEvent",
				Args: []string{"-test.run=^TestSpawnedStdoutHelper$"},
			})
			if test.valid {
				if err != nil || !strings.Contains(string(stdout), "�") {
					t.Fatalf("valid encoded U+FFFD was not preserved: stdout=%q err=%v", stdout, err)
				}
				return
			}
			if !IsErrorKind(err, ErrorKindDecode) {
				t.Fatalf("invalid UTF-8 should be a decode error, got %v", err)
			}
		})
	}
}

const spawnedStdoutHexEnvironment = "CTX_AGENT_HISTORY_GO_TEST_STDOUT_HEX"

func TestSpawnedStdoutHelper(t *testing.T) {
	rawHex := os.Getenv(spawnedStdoutHexEnvironment)
	if rawHex == "" {
		return
	}
	payload, err := hex.DecodeString(rawHex)
	if err != nil {
		os.Exit(2)
	}
	_, _ = os.Stdout.Write(payload)
	os.Exit(0)
}

func TestLocalCLIAdapterAddsDataRootEnvironment(t *testing.T) {
	runner := &recordingRunner{result: commandResult{Stdout: []byte(`{"schema_version":1}`)}}
	adapter := NewLocalCLIAdapter(WithCLIPath("ctx"), WithDataRoot("/tmp/ctx-data"))
	adapter.runner = runner

	_, err := adapter.Do(context.Background(), Operation{Name: "status", Args: []string{"status", "--format=json"}})
	if err != nil {
		t.Fatalf("Do returned error: %v", err)
	}
	if !contains(runner.env, "CTX_DATA_ROOT=/tmp/ctx-data") {
		t.Fatalf("CTX_DATA_ROOT missing from env: %#v", runner.env)
	}
}

func TestLocalCLIAdapterForcesAnalyticsOffAfterAmbientAndUserEnvironment(t *testing.T) {
	t.Setenv(analyticsEnabledEnvironment, "true")
	runner := &recordingRunner{result: commandResult{Stdout: []byte(`{"schema_version":1}`)}}
	adapter := NewLocalCLIAdapter(
		WithCLIPath("ctx"),
		WithEnv([]string{analyticsEnabledEnvironment + "=true"}),
	)
	adapter.runner = runner

	_, err := adapter.Do(context.Background(), Operation{Name: "status", Args: []string{"status", "--format=json"}})
	if err != nil {
		t.Fatalf("Do returned error: %v", err)
	}
	if got := os.Getenv(analyticsEnabledEnvironment); got != "true" {
		t.Fatalf("ambient analytics value = %q, want true", got)
	}
	if !contains(runner.env, analyticsEnabledEnvironment+"=true") {
		t.Fatalf("user analytics override missing from env: %#v", runner.env)
	}
	effective := append(os.Environ(), runner.env...)
	if got, ok := environmentValue(effective, analyticsEnabledEnvironment); !ok || got != "false" {
		t.Fatalf("effective analytics value = %q, %v; want false, true (env: %#v)", got, ok, runner.env)
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
	if search.Search.Results[0].Rank != 1 || search.Search.Results[0].RetrievalScore == nil || *search.Search.Results[0].RetrievalScore != 0.98 {
		t.Fatalf("unexpected typed rank fields: %+v", search.Search.Results[0])
	}
	if search.Search.Results[0].ResultType != "event" || search.Search.Results[0].Citations[0].TargetType != "event" {
		t.Fatalf("unexpected typed result/citation type: %+v", search.Search.Results[0])
	}
	if search.Search.Results[0].ProviderSessionID != "codex-fixture-session" || search.Search.Results[0].SourceFormat != "codex_session_jsonl" {
		t.Fatalf("unexpected typed search identity: %+v", search.Search.Results[0])
	}
	if search.Search.Pagination == nil || search.Search.Pagination.Limit != 20 || search.Search.Pagination.HasMore {
		t.Fatalf("unexpected pagination: %+v", search.Search.Pagination)
	}
	if search.Search.ResultWindow == nil ||
		search.Search.ResultWindow.Limit != 20 ||
		search.Search.ResultWindow.Returned != 1 ||
		search.Search.ResultWindow.MoreAvailable {
		t.Fatalf("unexpected result window: %+v", search.Search.ResultWindow)
	}
	if search.Search.Truncation == nil || search.Search.Truncation.Truncated {
		t.Fatalf("unexpected truncation: %+v", search.Search.Truncation)
	}

	session := readFixture[ShowSessionResponse](t, "show-session.transcript.json")
	if session.Session.Session == nil ||
		session.Session.Session.ProviderSessionID != "codex-fixture-session" ||
		session.Session.Session.SourceFormat != "codex_session_jsonl" {
		t.Fatalf("unexpected typed session: %+v", session.Session.Session)
	}

	event := readFixture[ShowEventResponse](t, "show-event.window.json")
	if event.Event.Event == nil ||
		event.Event.Event.Provider != "codex" ||
		event.Event.Event.ProviderSessionID != "codex-fixture-session" ||
		event.Event.Event.SourceFormat != "codex_session_jsonl" ||
		event.Event.Event.Content == nil ||
		!event.Event.Event.Content.Complete ||
		event.Event.Event.Content.PolicyStatus != CoreContentPolicyStatusSelected {
		t.Fatalf("unexpected typed event: %+v", event.Event.Event)
	}
	var structured Object
	if err := json.Unmarshal(event.Event.Event.StructuredContent, &structured); err != nil {
		t.Fatalf("decode typed structured content: %v", err)
	}
	payload, ok := structured["payload"].(map[string]any)
	if !ok || payload["ok"] != true || payload["count"] != float64(2) {
		t.Fatalf("unexpected structured content: %#v", structured)
	}
	if got := string(event.Event.Events[0].StructuredContent); got != "null" {
		t.Fatalf("nullable structured content was not preserved: %q", got)
	}
	if event.Event.Event.MCPToolCall != nil {
		t.Fatalf("legacy event unexpectedly gained MCP metadata: %+v", event.Event.Event.MCPToolCall)
	}
	legacyJSON, err := json.Marshal(event.Event.Event)
	if err != nil {
		t.Fatalf("encode legacy event: %v", err)
	}
	var legacyObject Object
	if err := json.Unmarshal(legacyJSON, &legacyObject); err != nil {
		t.Fatalf("decode encoded legacy event: %v", err)
	}
	if _, exists := legacyObject["mcpToolCall"]; exists {
		t.Fatalf("absent MCP metadata was serialized: %#v", legacyObject)
	}

	mcpEvent := readFixture[ShowEventResponse](t, "show-event.mcp-tool-call.json")
	call := mcpEvent.Event.Event.MCPToolCall
	if call == nil || call.Server != "mcp-サーバー-🦀" || call.Tool != "検索/工具/🛠️" {
		t.Fatalf("unexpected typed MCP tool call: %+v", call)
	}
	encodedCall, err := json.Marshal(call)
	if err != nil {
		t.Fatalf("encode MCP tool call: %v", err)
	}
	var roundTrip Object
	if err := json.Unmarshal(encodedCall, &roundTrip); err != nil {
		t.Fatalf("decode encoded MCP tool call: %v", err)
	}
	if roundTrip["server"] != "mcp-サーバー-🦀" || roundTrip["tool"] != "検索/工具/🛠️" {
		t.Fatalf("MCP Unicode did not round trip: %#v", roundTrip)
	}
	if _, exists := roundTrip["futureLabel"]; exists {
		t.Fatalf("Go typed DTO unexpectedly retained an unknown MCP field: %#v", roundTrip)
	}
	exchange := mcpEvent.Event.Event.MCPExchange
	if exchange == nil || exchange.ProviderCallID != "native-call-呼び出し-🦀" ||
		exchange.Invocation == nil || exchange.Response == nil {
		t.Fatalf("unexpected typed MCP exchange: %+v", exchange)
	}
	if exchange.Response.DurationNS == nil || *exchange.Response.DurationNS != MaxSafeInteger {
		t.Fatalf("MCP duration did not preserve safe maximum: %+v", exchange.Response.DurationNS)
	}
	var arguments map[string]any
	if err := json.Unmarshal(exchange.Invocation.Arguments.Value, &arguments); err != nil {
		t.Fatalf("decode captured arguments: %v", err)
	}
	if _, exists := arguments["snake_key"]; !exists {
		t.Fatalf("captured JSON key was rewritten: %#v", arguments)
	}
	if _, exists := arguments["snakeKey"]; exists {
		t.Fatalf("captured JSON gained camelized key: %#v", arguments)
	}
	if observed := mcpEvent.Event.Events[2].MCPExchange.Response.Text.ObservedEncodedBytes; observed == nil || *observed != MaxSafeInteger {
		t.Fatalf("observed encoded bytes did not preserve safe maximum: %v", observed)
	}
	if mcpEvent.Event.Events[3].MCPExchange != nil {
		t.Fatal("absent MCP exchange was materialized")
	}
	var outerWithAddition Event
	if err := json.Unmarshal([]byte(`{"mcpToolCall":{"server":"server","tool":"tool"},"futureEventField":true}`), &outerWithAddition); err != nil {
		t.Fatalf("outer Event addition was not accepted: %v", err)
	}
	exact := `{"mcpToolCall":{"server":" ","tool":"` + strings.Repeat("🦀", 16_384) + `"}}`
	if err := json.Unmarshal([]byte(exact), &outerWithAddition); err != nil {
		t.Fatalf("exact 64 KiB MCP component was rejected: %v", err)
	}

	for name, invalid := range map[string]string{
		"missing server":  `{"mcpToolCall":{"tool":"only-tool"}}`,
		"missing tool":    `{"mcpToolCall":{"server":"only-server"}}`,
		"unknown member":  `{"mcpToolCall":{"server":"server","tool":"tool","future":true}}`,
		"empty component": `{"mcpToolCall":{"server":"","tool":"tool"}}`,
		"oversized":       `{"mcpToolCall":{"server":"server","tool":"` + strings.Repeat("a", 64*1024+1) + `"}}`,
		"non-string":      `{"mcpToolCall":{"server":"server","tool":7}}`,
		"bad surrogate":   `{"mcpToolCall":{"server":"server","tool":"\ud800"}}`,
		"explicit null":   `{"mcpToolCall":null}`,
	} {
		t.Run(name, func(t *testing.T) {
			var value Event
			if err := json.Unmarshal([]byte(invalid), &value); err == nil {
				t.Fatalf("invalid MCP tool call decoded: %+v", value.MCPToolCall)
			}
		})
	}

	errorEnvelope := readFixture[ErrorResponse](t, "error.not-supported.json")
	if errorEnvelope.Error.Code != ErrorKindHostedNotImplemented || errorEnvelope.Backend.Kind != BackendKindHosted {
		t.Fatalf("unexpected error envelope: %+v", errorEnvelope)
	}
}

func TestRawMCPToolCallDuplicateMembersAreRejected(t *testing.T) {
	fixtureRoot := filepath.Clean("../../contracts/agent-history-v1/fixtures/adversarial")
	for _, name := range []string{
		"duplicate-event-mcp-tool-call-snake.json",
		"duplicate-event-mcp-tool-call-camel.json",
		"duplicate-mcp-tool-call-server.json",
		"duplicate-mcp-tool-call-tool.json",
		"duplicate-event-mcp-exchange-snake.json",
		"duplicate-mcp-exchange-captured-value.json",
		"invalid-mcp-exchange-explicit-null.json",
		"invalid-mcp-exchange-outer-alias-collision.json",
		"invalid-mcp-exchange-unknown-field.json",
		"invalid-mcp-exchange-normalized-body-missing-event-text.json",
		"invalid-mcp-exchange-normalized-body-empty-event-text.json",
		"invalid-mcp-exchange-unsafe-duration-ns.json",
		"invalid-mcp-exchange-unsafe-observed-encoded-bytes.json",
		"invalid-mcp-tool-call-transformed-server.json",
		"invalid-mcp-tool-call-transformed-tool.json",
		"invalid-mcp-tool-call-transformed-collision.json",
		"invalid-mcp-tool-call-outer-alias-collision.json",
		"invalid-mcp-tool-call-outer-mixed-case.json",
		"invalid-mcp-tool-call-outer-repeated-separator.json",
		"invalid-mcp-tool-call-outer-trailing-separator.json",
		"invalid-mcp-tool-call-outer-camel-snake.json",
	} {
		data, err := os.ReadFile(filepath.Join(fixtureRoot, name))
		if err != nil {
			t.Fatalf("read adversarial fixture %s: %v", name, err)
		}
		t.Run(name, func(t *testing.T) {
			client := NewClient(WithTransport(fakeTransport{response: string(data)}))
			if _, err := client.ShowEvent(context.Background(), ShowEventOptions{ID: "event-1"}); err == nil {
				t.Fatal("duplicate JSON object member was accepted")
			}
		})
	}

	data, err := os.ReadFile(filepath.Join(fixtureRoot, "valid-repeated-string-contents.json"))
	if err != nil {
		t.Fatalf("read repeated-string fixture: %v", err)
	}
	client := NewClient(WithTransport(fakeTransport{response: string(data)}))
	response, err := client.ShowEvent(context.Background(), ShowEventOptions{ID: "event-1"})
	if err != nil {
		t.Fatalf("repeated string contents were rejected: %v", err)
	}
	call := response.Event.Event.MCPToolCall
	if call == nil || call.Server != "server server" || call.Tool != "tool tool" {
		t.Fatalf("unexpected repeated-string MCP call: %+v", call)
	}

	data, err = os.ReadFile(filepath.Join(fixtureRoot, "valid-mcp-tool-call-outer-aliases.json"))
	if err != nil {
		t.Fatalf("read outer-alias fixture: %v", err)
	}
	client = NewClient(WithTransport(fakeTransport{response: string(data)}))
	response, err = client.ShowEvent(context.Background(), ShowEventOptions{ID: "event-1"})
	if err != nil {
		t.Fatalf("valid outer aliases were rejected: %v", err)
	}
	if response.Event.Event == nil || len(response.Event.Events) != 1 ||
		response.Event.Event.MCPToolCall == nil || response.Event.Events[0].MCPToolCall == nil {
		t.Fatal("valid outer aliases did not normalize to typed MCP calls")
	}
	if response.Event.Event.MCPToolCall.Server != "snake-server" ||
		response.Event.Events[0].MCPToolCall.Server != "camel-server" {
		t.Fatalf("unexpected outer-alias calls: %+v %+v", response.Event.Event.MCPToolCall, response.Event.Events[0].MCPToolCall)
	}
}

func TestMCPUnmarshalJSONReceiverReuseClearsAbsentFields(t *testing.T) {
	unmarshal := func(t *testing.T, input string, target any) {
		t.Helper()
		if err := json.Unmarshal([]byte(input), target); err != nil {
			t.Fatalf("unmarshal %s: %v", input, err)
		}
	}
	assertValue := func(t *testing.T, got, want any) {
		t.Helper()
		if !reflect.DeepEqual(got, want) {
			t.Fatalf("decoded value = %#v, want %#v", got, want)
		}
	}
	assertJSON := func(t *testing.T, value any, want string) {
		t.Helper()
		encoded, err := json.Marshal(value)
		if err != nil {
			t.Fatalf("marshal reused receiver: %v", err)
		}
		if got := string(encoded); got != want {
			t.Fatalf("reused receiver JSON = %s, want %s", got, want)
		}
	}

	t.Run("event", func(t *testing.T) {
		var value Event
		unmarshal(t, `{
			"text":"body",
			"mcpToolCall":{"server":"server","tool":"tool"},
			"mcpExchange":{
				"providerCallId":"call-populated",
				"response":{
					"status":"succeeded",
					"text":{"captureStatus":"normalized_body"},
					"payload":{"captureStatus":"absent"}
				}
			}
		}`, &value)
		unmarshal(t, `{"text":"plain"}`, &value)

		assertValue(t, value, Event{Text: "plain"})
		assertJSON(t, value, `{"text":"plain"}`)
	})

	t.Run("exchange", func(t *testing.T) {
		var value MCPExchange
		unmarshal(t, `{
			"providerCallId":"call-populated",
			"invocation":{
				"server":"server",
				"tool":"tool",
				"arguments":{"captureStatus":"present","value":{"snake_key":true}}
			},
			"response":{
				"status":"failed",
				"failureKind":"tool_reported",
				"durationNs":17,
				"text":{"captureStatus":"omitted","reason":"size_limit","observedEncodedBytes":19},
				"payload":{"captureStatus":"present","value":{"result":true}}
			}
		}`, &value)
		unmarshal(t, `{
			"providerCallId":"call-response-only",
			"response":{
				"status":"succeeded",
				"text":{"captureStatus":"absent"},
				"payload":{"captureStatus":"unavailable"}
			}
		}`, &value)

		wantResponseOnly := MCPExchange{
			ProviderCallID: "call-response-only",
			Response: &MCPResponse{
				Status:  MCPResponseStatusSucceeded,
				Text:    MCPTextCapture{CaptureStatus: MCPJSONCaptureStatusAbsent},
				Payload: MCPJSONCapture{CaptureStatus: MCPJSONCaptureStatusUnavailable},
			},
		}
		assertValue(t, value, wantResponseOnly)
		assertJSON(t, value, `{"providerCallId":"call-response-only","response":{"status":"succeeded","text":{"captureStatus":"absent"},"payload":{"captureStatus":"unavailable"}}}`)

		unmarshal(t, `{
			"providerCallId":"call-invocation-only",
			"invocation":{
				"server":"next-server",
				"tool":"next-tool",
				"arguments":{"captureStatus":"absent"}
			}
		}`, &value)
		wantInvocationOnly := MCPExchange{
			ProviderCallID: "call-invocation-only",
			Invocation: &MCPInvocation{
				Server:    "next-server",
				Tool:      "next-tool",
				Arguments: MCPJSONCapture{CaptureStatus: MCPJSONCaptureStatusAbsent},
			},
		}
		assertValue(t, value, wantInvocationOnly)
		assertJSON(t, value, `{"providerCallId":"call-invocation-only","invocation":{"server":"next-server","tool":"next-tool","arguments":{"captureStatus":"absent"}}}`)
	})

	t.Run("invocation", func(t *testing.T) {
		var value MCPInvocation
		unmarshal(t, `{
			"server":"server",
			"tool":"tool",
			"arguments":{"captureStatus":"present","value":{"camelKey":2,"snake_key":[1,2]}}
		}`, &value)
		if got, want := string(value.Arguments.Value), `{"camelKey":2,"snake_key":[1,2]}`; got != want {
			t.Fatalf("opaque argument JSON = %s, want %s", got, want)
		}
		unmarshal(t, `{
			"server":"sparse-server",
			"tool":"sparse-tool",
			"arguments":{"captureStatus":"absent"}
		}`, &value)

		want := MCPInvocation{
			Server:    "sparse-server",
			Tool:      "sparse-tool",
			Arguments: MCPJSONCapture{CaptureStatus: MCPJSONCaptureStatusAbsent},
		}
		assertValue(t, value, want)
		assertJSON(t, value, `{"server":"sparse-server","tool":"sparse-tool","arguments":{"captureStatus":"absent"}}`)
	})

	t.Run("response", func(t *testing.T) {
		var value MCPResponse
		unmarshal(t, `{
			"status":"failed",
			"failureKind":"invocation",
			"durationNs":23,
			"text":{"captureStatus":"omitted","reason":"size_limit","observedEncodedBytes":29},
			"payload":{"captureStatus":"omitted","reason":"size_limit","observedEncodedBytes":31}
		}`, &value)
		unmarshal(t, `{
			"status":"succeeded",
			"text":{"captureStatus":"absent"},
			"payload":{"captureStatus":"unavailable"}
		}`, &value)

		want := MCPResponse{
			Status:  MCPResponseStatusSucceeded,
			Text:    MCPTextCapture{CaptureStatus: MCPJSONCaptureStatusAbsent},
			Payload: MCPJSONCapture{CaptureStatus: MCPJSONCaptureStatusUnavailable},
		}
		assertValue(t, value, want)
		assertJSON(t, value, `{"status":"succeeded","text":{"captureStatus":"absent"},"payload":{"captureStatus":"unavailable"}}`)
	})

	t.Run("JSON capture", func(t *testing.T) {
		var value MCPJSONCapture
		unmarshal(t, `{"captureStatus":"present","value":{"camelKey":2,"snake_key":[1,2]}}`, &value)
		if got, want := string(value.Value), `{"camelKey":2,"snake_key":[1,2]}`; got != want {
			t.Fatalf("opaque captured JSON = %s, want %s", got, want)
		}
		unmarshal(t, `{"captureStatus":"absent"}`, &value)
		want := MCPJSONCapture{CaptureStatus: MCPJSONCaptureStatusAbsent}
		assertValue(t, value, want)
		assertJSON(t, value, `{"captureStatus":"absent"}`)

		unmarshal(t, `{"captureStatus":"omitted","reason":"size_limit","observedEncodedBytes":37}`, &value)
		unmarshal(t, `{"captureStatus":"unavailable"}`, &value)
		want = MCPJSONCapture{CaptureStatus: MCPJSONCaptureStatusUnavailable}
		assertValue(t, value, want)
		assertJSON(t, value, `{"captureStatus":"unavailable"}`)
	})

	t.Run("text capture", func(t *testing.T) {
		var value MCPTextCapture
		unmarshal(t, `{"captureStatus":"omitted","reason":"size_limit","observedEncodedBytes":41}`, &value)
		unmarshal(t, `{"captureStatus":"normalized_body"}`, &value)

		want := MCPTextCapture{CaptureStatus: MCPTextCaptureStatusNormalizedBody}
		assertValue(t, value, want)
		assertJSON(t, value, `{"captureStatus":"normalized_body"}`)
	})

	t.Run("tool call", func(t *testing.T) {
		var value MCPToolCall
		unmarshal(t, `{"server":"first-server","tool":"first-tool"}`, &value)
		unmarshal(t, `{"server":"next-server","tool":"next-tool"}`, &value)

		want := MCPToolCall{Server: "next-server", Tool: "next-tool"}
		assertValue(t, value, want)
		assertJSON(t, value, `{"server":"next-server","tool":"next-tool"}`)
	})
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

func environmentValue(values []string, name string) (string, bool) {
	value := ""
	found := false
	for _, entry := range values {
		key, current, ok := strings.Cut(entry, "=")
		if ok && key == name {
			value = current
			found = true
		}
	}
	return value, found
}
