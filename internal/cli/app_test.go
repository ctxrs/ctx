package cli

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/ctxrs/ctx/internal/localstore"
	"github.com/ctxrs/ctx/internal/search"
)

func TestRootHelpListsPublicCommands(t *testing.T) {
	var out, stderr bytes.Buffer
	app := NewApp(&out, &stderr, Dependencies{})

	if err := app.Run(context.Background(), []string{"--help"}); err != nil {
		t.Fatalf("Run returned error: %v", err)
	}

	got := out.String()
	for _, name := range []string{
		"setup", "status", "index", "sources", "import", "search", "show", "locate",
		"sql", "docs", "integrations", "daemon", "doctor", "mcp", "upgrade", "version",
	} {
		if !strings.Contains(got, name) {
			t.Fatalf("root help missing %q\nhelp:\n%s", name, got)
		}
	}
	if stderr.Len() != 0 {
		t.Fatalf("expected no stderr, got %q", stderr.String())
	}
}

func TestVersionCommand(t *testing.T) {
	var out bytes.Buffer
	app := NewApp(&out, &bytes.Buffer{}, Dependencies{Version: "v0.0.0-test"})

	if err := app.Run(context.Background(), []string{"version"}); err != nil {
		t.Fatalf("Run returned error: %v", err)
	}
	if got, want := out.String(), "ctx v0.0.0-test\n"; got != want {
		t.Fatalf("version output = %q, want %q", got, want)
	}
}

func TestVersionHelp(t *testing.T) {
	var out bytes.Buffer
	app := NewApp(&out, &bytes.Buffer{}, Dependencies{})

	if err := app.Run(context.Background(), []string{"help", "version"}); err != nil {
		t.Fatalf("Run returned error: %v", err)
	}
	if got := out.String(); !strings.Contains(got, "ctx version") {
		t.Fatalf("version help missing usage, got %q", got)
	}
}

func TestCommandHelp(t *testing.T) {
	var out bytes.Buffer
	app := NewApp(&out, &bytes.Buffer{}, Dependencies{})

	if err := app.Run(context.Background(), []string{"search", "--help"}); err != nil {
		t.Fatalf("Run returned error: %v", err)
	}

	got := out.String()
	for _, want := range []string{"Usage:", "ctx search", "lexical|semantic|hybrid", "must not silently fall back"} {
		if !strings.Contains(got, want) {
			t.Fatalf("search help missing %q\nhelp:\n%s", want, got)
		}
	}
}

func TestUnknownCommandReturnsUsageError(t *testing.T) {
	app := NewApp(&bytes.Buffer{}, &bytes.Buffer{}, Dependencies{})

	err := app.Run(context.Background(), []string{"cloud"})
	var cliErr *Error
	if !errors.As(err, &cliErr) {
		t.Fatalf("error type = %T, want *Error", err)
	}
	if cliErr.Code != CodeUsage || cliErr.Command != "cloud" {
		t.Fatalf("error = %#v, want usage error for cloud", cliErr)
	}
	if ExitCode(err) != 2 {
		t.Fatalf("ExitCode = %d, want 2", ExitCode(err))
	}
	if !strings.Contains(err.Error(), "unknown command") {
		t.Fatalf("error should be user-readable, got %q", err.Error())
	}
}

func TestUnfinishedCommandsReturnTypedErrors(t *testing.T) {
	for _, name := range []string{"index", "docs", "integrations", "daemon", "doctor", "mcp", "upgrade"} {
		t.Run(name, func(t *testing.T) {
			app := NewApp(&bytes.Buffer{}, &bytes.Buffer{}, Dependencies{DataRoot: t.TempDir()})
			err := app.Run(context.Background(), []string{name})
			var cliErr *Error
			if !errors.As(err, &cliErr) {
				t.Fatalf("error type = %T, want *Error", err)
			}
			if cliErr.Code != CodeUnimplemented || cliErr.Command != name {
				t.Fatalf("error = %#v, want unimplemented error for %s", cliErr, name)
			}
			if !strings.Contains(err.Error(), "not implemented") {
				t.Fatalf("error should explain implementation status, got %q", err.Error())
			}
		})
	}
}

func TestCommandInventoryIsStable(t *testing.T) {
	got := commandList()
	want := "setup, status, index, sources, import, search, show, locate, sql, docs, integrations, daemon, doctor, mcp, upgrade"
	if got != want {
		t.Fatalf("commandList() = %q, want %q", got, want)
	}
}

func TestJSONFlagDoesNotPrintErrorPayloadToStdout(t *testing.T) {
	var out, stderr bytes.Buffer
	app := NewApp(&out, &stderr, Dependencies{DataRoot: t.TempDir()})

	err := app.Run(context.Background(), []string{"search", "--mode", "semantic", "--json", "anything"})
	if err == nil {
		t.Fatal("Run returned nil, want unavailable error")
	}
	if out.Len() != 0 {
		t.Fatalf("--json error wrote stdout %q, want no stdout", out.String())
	}
}

func TestStatusJSONMissingStoreIsReadOnly(t *testing.T) {
	dataRoot := t.TempDir()
	var out, stderr bytes.Buffer
	app := NewApp(&out, &stderr, Dependencies{DataRoot: dataRoot})

	if err := app.Run(context.Background(), []string{"status", "--json"}); err != nil {
		t.Fatalf("Run returned error: %v", err)
	}
	if stderr.Len() != 0 {
		t.Fatalf("expected no stderr, got %q", stderr.String())
	}
	var payload map[string]any
	if err := json.Unmarshal(out.Bytes(), &payload); err != nil {
		t.Fatalf("status JSON did not parse: %v\n%s", err, out.String())
	}
	if payload["initialized"] != false || payload["read_only"] != true || payload["local_only"] != true {
		t.Fatalf("unexpected status payload: %#v", payload)
	}
	if _, err := os.Stat(filepath.Join(dataRoot, "work.sqlite")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("status created or touched database, stat err = %v", err)
	}
}

func TestSourcesJSONShowsP0AndUnsupportedProvider(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("CODEX_HOME", filepath.Join(home, ".codex"))
	var out bytes.Buffer
	app := NewApp(&out, &bytes.Buffer{}, Dependencies{DataRoot: t.TempDir()})

	if err := app.Run(context.Background(), []string{"sources", "--show-missing", "--all", "--json"}); err != nil {
		t.Fatalf("Run returned error: %v", err)
	}
	var payload struct {
		Sources []sourceRow `json:"sources"`
	}
	if err := json.Unmarshal(out.Bytes(), &payload); err != nil {
		t.Fatalf("sources JSON did not parse: %v\n%s", err, out.String())
	}
	foundCodex := false
	foundPi := false
	foundUnsupported := false
	for _, row := range payload.Sources {
		if row.Provider == "codex" && strings.Contains(row.Path, ".codex") {
			foundCodex = true
		}
		if row.Provider == "pi" && strings.Contains(row.Path, ".pi") {
			foundPi = true
		}
		if row.Provider == "claude" && row.Status == "unsupported" && !row.Importable {
			foundUnsupported = true
		}
	}
	if !foundCodex || !foundPi || !foundUnsupported {
		t.Fatalf("missing expected source rows: %+v", payload.Sources)
	}
}

func TestSetupImportsDiscoveredCodexAndSearches(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("CODEX_HOME", filepath.Join(home, ".codex"))
	dataRoot := t.TempDir()
	sessionDir := filepath.Join(home, ".codex", "sessions", "2026", "07", "17")
	if err := os.MkdirAll(sessionDir, 0o755); err != nil {
		t.Fatal(err)
	}
	writeFile(t, filepath.Join(sessionDir, "session.jsonl"), strings.Join([]string{
		`{"timestamp":"2026-07-17T12:00:00Z","type":"session_meta","payload":{"id":"codex-test-session"}}`,
		`{"timestamp":"2026-07-17T12:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"alpha milestone setup import needle"}]}}`,
		`{"timestamp":"2026-07-17T12:00:02Z","type":"event_msg","payload":{"message":"finished tiny smoke"}}`,
	}, "\n")+"\n")

	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	var setupOut bytes.Buffer
	app := NewApp(&setupOut, &bytes.Buffer{}, Dependencies{DataRoot: dataRoot})
	if err := app.Run(ctx, []string{"setup", "--json", "--no-daemon"}); err != nil {
		t.Fatalf("setup returned error: %v\n%s", err, setupOut.String())
	}
	var setupPayload map[string]any
	if err := json.Unmarshal(setupOut.Bytes(), &setupPayload); err != nil {
		t.Fatalf("setup JSON did not parse: %v\n%s", err, setupOut.String())
	}
	if setupPayload["network_required"] != false || setupPayload["repo_writes"] != false {
		t.Fatalf("setup payload should be local-only: %#v", setupPayload)
	}
	if size := fileSize(t, filepath.Join(dataRoot, "work.sqlite")); size > 2*1024*1024 {
		t.Fatalf("tiny setup database size = %d bytes, want <= 2 MiB", size)
	}

	var searchOut bytes.Buffer
	app = NewApp(&searchOut, &bytes.Buffer{}, Dependencies{DataRoot: dataRoot})
	if err := app.Run(context.Background(), []string{"search", "--json", "--provider", "codex", "needle"}); err != nil {
		t.Fatalf("search returned error: %v", err)
	}
	assertSearchJSONResult(t, searchOut.Bytes(), "codex")
}

func TestReadCommandsUseImportedStore(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("CODEX_HOME", filepath.Join(home, ".codex"))
	dataRoot := t.TempDir()
	sessionDir := filepath.Join(home, ".codex", "sessions", "2026", "07", "17")
	if err := os.MkdirAll(sessionDir, 0o755); err != nil {
		t.Fatal(err)
	}
	writeFile(t, filepath.Join(sessionDir, "session.jsonl"), strings.Join([]string{
		`{"timestamp":"2026-07-17T12:00:00Z","type":"session_meta","payload":{"id":"codex-read-session"}}`,
		`{"timestamp":"2026-07-17T12:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"show locate sql needle"}]}}`,
	}, "\n")+"\n")

	app := NewApp(&bytes.Buffer{}, &bytes.Buffer{}, Dependencies{DataRoot: dataRoot})
	if err := app.Run(context.Background(), []string{"setup", "--no-daemon"}); err != nil {
		t.Fatalf("setup returned error: %v", err)
	}

	var searchOut bytes.Buffer
	app = NewApp(&searchOut, &bytes.Buffer{}, Dependencies{DataRoot: dataRoot})
	if err := app.Run(context.Background(), []string{"search", "--json", "needle"}); err != nil {
		t.Fatalf("search returned error: %v", err)
	}
	var searchPayload struct {
		Results []struct {
			CtxEventID   string `json:"ctx_event_id"`
			CtxSessionID string `json:"ctx_session_id"`
		} `json:"results"`
	}
	if err := json.Unmarshal(searchOut.Bytes(), &searchPayload); err != nil {
		t.Fatalf("search JSON did not parse: %v\n%s", err, searchOut.String())
	}
	if len(searchPayload.Results) == 0 {
		t.Fatalf("expected search result, got %s", searchOut.String())
	}

	var showOut bytes.Buffer
	app = NewApp(&showOut, &bytes.Buffer{}, Dependencies{DataRoot: dataRoot})
	if err := app.Run(context.Background(), []string{"show", "session", searchPayload.Results[0].CtxSessionID, "--mode", "lite", "--format", "json"}); err != nil {
		t.Fatalf("show session returned error: %v", err)
	}
	var showPayload map[string]any
	if err := json.Unmarshal(showOut.Bytes(), &showPayload); err != nil {
		t.Fatalf("show JSON did not parse: %v\n%s", err, showOut.String())
	}
	if showPayload["contractVersion"] != "agent-history-v1" || showPayload["operation"] != "showSession" {
		t.Fatalf("unexpected show envelope: %#v", showPayload)
	}

	var locateOut bytes.Buffer
	app = NewApp(&locateOut, &bytes.Buffer{}, Dependencies{DataRoot: dataRoot})
	if err := app.Run(context.Background(), []string{"locate", "event", searchPayload.Results[0].CtxEventID, "--format", "json"}); err != nil {
		t.Fatalf("locate event returned error: %v", err)
	}
	var locatePayload map[string]any
	if err := json.Unmarshal(locateOut.Bytes(), &locatePayload); err != nil {
		t.Fatalf("locate JSON did not parse: %v\n%s", err, locateOut.String())
	}
	if locatePayload["operation"] != "locateEvent" {
		t.Fatalf("unexpected locate envelope: %#v", locatePayload)
	}

	var sqlOut bytes.Buffer
	app = NewApp(&sqlOut, &bytes.Buffer{}, Dependencies{DataRoot: dataRoot})
	if err := app.Run(context.Background(), []string{"sql", "SELECT provider, COUNT(*) AS events FROM ctx_events GROUP BY provider", "--format", "json"}); err != nil {
		t.Fatalf("sql returned error: %v", err)
	}
	var sqlPayload map[string]any
	if err := json.Unmarshal(sqlOut.Bytes(), &sqlPayload); err != nil {
		t.Fatalf("sql JSON did not parse: %v\n%s", err, sqlOut.String())
	}
	if sqlPayload["operation"] != "sql" {
		t.Fatalf("unexpected sql envelope: %#v", sqlPayload)
	}
}

func TestImportTwiceKeepsStableSearchIDsAndCitations(t *testing.T) {
	dataRoot := t.TempDir()
	path := filepath.Join(t.TempDir(), "session.jsonl")
	writeFile(t, path, strings.Join([]string{
		`{"timestamp":"2026-07-17T12:00:00Z","type":"session_meta","payload":{"id":"stable-session"}}`,
		`{"timestamp":"2026-07-17T12:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"stable citation needle"}]}}`,
	}, "\n")+"\n")

	var firstImport bytes.Buffer
	app := NewApp(&firstImport, &bytes.Buffer{}, Dependencies{DataRoot: dataRoot})
	if err := app.Run(context.Background(), []string{"import", "--provider", "codex", "--path", path, "--json"}); err != nil {
		t.Fatalf("first import returned error: %v\n%s", err, firstImport.String())
	}
	firstID, firstCitationID := searchFirstEventAndCitation(t, dataRoot, "stable")

	var secondImport bytes.Buffer
	app = NewApp(&secondImport, &bytes.Buffer{}, Dependencies{DataRoot: dataRoot})
	if err := app.Run(context.Background(), []string{"import", "--provider", "codex", "--path", path, "--json"}); err != nil {
		t.Fatalf("second import returned error: %v\n%s", err, secondImport.String())
	}
	secondID, secondCitationID := searchFirstEventAndCitation(t, dataRoot, "stable")

	if firstID != secondID || firstCitationID != secondCitationID {
		t.Fatalf("IDs changed across re-import: first=%s/%s second=%s/%s", firstID, firstCitationID, secondID, secondCitationID)
	}
	if !strings.Contains(firstID, "#event:") || strings.HasPrefix(firstID, "event:") {
		t.Fatalf("event ID is not deterministic source identity: %s", firstID)
	}
}

func TestSearchParserConsumesSDKFlagsAndFailsClosed(t *testing.T) {
	opts, err := parseSearchArgs([]string{
		"--session", "session-1",
		"--file=src/main.go",
		"--workspace", "/repo/work",
		"--since", "2026-07-01",
		"--event-type", "response_item:message",
		"--refresh", "off",
		"--semantic-weight", "0",
		"--include-subagents",
		"--primary-only",
		"--events",
		"--include-current-session",
		"--term", "needle",
		"plain query",
	})
	if err != nil {
		t.Fatal(err)
	}
	if strings.Join(opts.Terms, "|") != "plain query|needle" || opts.SessionID != "session-1" || opts.File != "src/main.go" || opts.Workspace != "/repo/work" || opts.EventType != "response_item:message" {
		t.Fatalf("SDK flags leaked into query or were not parsed: %+v", opts)
	}
	if opts.Since == nil || !opts.IncludeSubagents || !opts.PrimaryOnly || !opts.Events || !opts.IncludeCurrentSession {
		t.Fatalf("boolean/date SDK flags were not parsed: %+v", opts)
	}
	if _, err := parseSearchArgs([]string{"--definitely-unsupported", "needle"}); err == nil {
		t.Fatal("unsupported flag parsed as query text")
	}
}

func TestActiveCodexSessionTreeExclusion(t *testing.T) {
	ctx := context.Background()
	store := newCLITestStore(t, ctx)
	addSearchEvent(t, ctx, store, "codex:root", localstore.Event{
		SourceEventID:      "root-event",
		ProviderSessionID:  "root-session",
		RootSessionID:      "root-session",
		ProviderEventIndex: 1,
		Role:               "user",
		Type:               "response_item:message",
		OccurredAt:         time.Date(2026, 7, 17, 12, 0, 0, 0, time.UTC),
		Text:               "exactroot rootonly sharedneedle",
	})
	addSearchEvent(t, ctx, store, "codex:child", localstore.Event{
		SourceEventID:      "child-event",
		ProviderSessionID:  "child-session",
		ParentSessionID:    "root-session",
		RootSessionID:      "root-session",
		ProviderEventIndex: 1,
		Role:               "assistant",
		Type:               "response_item:message",
		OccurredAt:         time.Date(2026, 7, 17, 12, 0, 1, 0, time.UTC),
		Text:               "childonly sharedneedle",
	})

	t.Setenv("CODEX_THREAD_ID", "root-session")
	assertSearchCountWithOptions(t, ctx, store, searchOptions{Mode: search.ModeLexical, Limit: 10, Terms: []string{"rootonly"}}, 0)
	assertSearchCountWithOptions(t, ctx, store, searchOptions{Mode: search.ModeLexical, Limit: 10, Terms: []string{"childonly"}}, 0)
	assertSearchCountWithOptions(t, ctx, store, searchOptions{Mode: search.ModeLexical, Limit: 10, Terms: []string{"rootonly"}, SessionID: "root-session"}, 1)
	assertSearchCountWithOptions(t, ctx, store, searchOptions{Mode: search.ModeLexical, Limit: 10, Terms: []string{"sharedneedle"}, IncludeCurrentSession: true}, 2)

	t.Setenv("CODEX_THREAD_ID", "child-session")
	assertSearchCountWithOptions(t, ctx, store, searchOptions{Mode: search.ModeLexical, Limit: 10, Terms: []string{"rootonly"}}, 0)
}

func TestImportExplicitPiPathSearches(t *testing.T) {
	dataRoot := t.TempDir()
	path := filepath.Join(t.TempDir(), "pi.jsonl")
	writeFile(t, path, strings.Join([]string{
		`{"type":"session","version":3,"id":"pi-test-session","timestamp":"2026-07-17T13:00:00Z"}`,
		`{"type":"message","id":"pi-msg-1","timestamp":"2026-07-17T13:00:01Z","message":{"role":"assistant","content":"beta import path needle"}}`,
	}, "\n")+"\n")

	var importOut bytes.Buffer
	app := NewApp(&importOut, &bytes.Buffer{}, Dependencies{DataRoot: dataRoot})
	if err := app.Run(context.Background(), []string{"import", "--provider", "pi", "--path", path, "--json"}); err != nil {
		t.Fatalf("import returned error: %v\n%s", err, importOut.String())
	}
	var searchOut bytes.Buffer
	app = NewApp(&searchOut, &bytes.Buffer{}, Dependencies{DataRoot: dataRoot})
	if err := app.Run(context.Background(), []string{"search", "--json", "--term", "beta", "--provider", "pi"}); err != nil {
		t.Fatalf("search returned error: %v", err)
	}
	assertSearchJSONResult(t, searchOut.Bytes(), "pi")
}

func TestUnsupportedProviderFailsClearly(t *testing.T) {
	app := NewApp(&bytes.Buffer{}, &bytes.Buffer{}, Dependencies{DataRoot: t.TempDir()})

	err := app.Run(context.Background(), []string{"import", "--provider", "claude"})
	var cliErr *Error
	if !errors.As(err, &cliErr) {
		t.Fatalf("error type = %T, want *Error", err)
	}
	if cliErr.Code != CodeUnavailable {
		t.Fatalf("error code = %s, want %s", cliErr.Code, CodeUnavailable)
	}
	if !strings.Contains(err.Error(), "unsupported provider") || !strings.Contains(err.Error(), "codex and pi") {
		t.Fatalf("error should name supported providers, got %q", err.Error())
	}
}

func TestMissingProviderValueFailsClearly(t *testing.T) {
	app := NewApp(&bytes.Buffer{}, &bytes.Buffer{}, Dependencies{DataRoot: t.TempDir()})

	err := app.Run(context.Background(), []string{"sources", "--provider"})
	var cliErr *Error
	if !errors.As(err, &cliErr) {
		t.Fatalf("error type = %T, want *Error", err)
	}
	if cliErr.Code != CodeUsage {
		t.Fatalf("error code = %s, want %s", cliErr.Code, CodeUsage)
	}
	if !strings.Contains(err.Error(), "missing value for --provider") {
		t.Fatalf("error should name missing provider value, got %q", err.Error())
	}
}

func writeFile(t *testing.T, path, content string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
}

func fileSize(t *testing.T, path string) int64 {
	t.Helper()
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	return info.Size()
}

func assertSearchJSONResult(t *testing.T, raw []byte, provider string) {
	t.Helper()
	var payload struct {
		Results []struct {
			Provider string `json:"provider"`
			Snippet  string `json:"snippet"`
		} `json:"results"`
	}
	if err := json.Unmarshal(raw, &payload); err != nil {
		t.Fatalf("search JSON did not parse: %v\n%s", err, string(raw))
	}
	if len(payload.Results) == 0 {
		t.Fatalf("expected at least one search result, got %s", string(raw))
	}
	if payload.Results[0].Provider != provider {
		t.Fatalf("provider = %q, want %q in %#v", payload.Results[0].Provider, provider, payload.Results)
	}
}

func searchFirstEventAndCitation(t *testing.T, dataRoot, query string) (string, string) {
	t.Helper()
	var out bytes.Buffer
	app := NewApp(&out, &bytes.Buffer{}, Dependencies{DataRoot: dataRoot})
	if err := app.Run(context.Background(), []string{"search", "--json", query}); err != nil {
		t.Fatalf("search returned error: %v", err)
	}
	var payload struct {
		Results []struct {
			CtxEventID string `json:"ctx_event_id"`
			Citations  []struct {
				CtxEventID string `json:"ctx_event_id"`
			} `json:"citations"`
		} `json:"results"`
	}
	if err := json.Unmarshal(out.Bytes(), &payload); err != nil {
		t.Fatalf("search JSON did not parse: %v\n%s", err, out.String())
	}
	if len(payload.Results) == 0 || len(payload.Results[0].Citations) == 0 {
		t.Fatalf("expected result citation, got %s", out.String())
	}
	return payload.Results[0].CtxEventID, payload.Results[0].Citations[0].CtxEventID
}

func newCLITestStore(t *testing.T, ctx context.Context) *localstore.Store {
	t.Helper()
	store, err := localstore.Open(ctx, filepath.Join(t.TempDir(), "work.sqlite"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := store.Close(); err != nil {
			t.Error(err)
		}
	})
	if err := store.Initialize(ctx); err != nil {
		t.Fatal(err)
	}
	return store
}

func addSearchEvent(t *testing.T, ctx context.Context, store *localstore.Store, sourceKey string, event localstore.Event) {
	t.Helper()
	source, err := store.UpsertSource(ctx, localstore.SourceDescriptor{
		Key:      sourceKey,
		Provider: "codex",
		Format:   "jsonl",
		URI:      "file:///tmp/" + sourceKey,
		Identity: "ident-" + sourceKey,
	})
	if err != nil {
		t.Fatal(err)
	}
	gen, err := store.BeginGeneration(ctx, source.Key, localstore.GenerationOptions{
		Kind:           localstore.GenerationReplace,
		SourceIdentity: "ident-" + sourceKey,
	})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := store.AppendEvents(ctx, localstore.AppendRequest{
		SourceKey:        source.Key,
		GenerationID:     gen.ID,
		SourceIdentity:   "ident-" + sourceKey,
		PreviousSize:     0,
		PreviousTailHash: "",
		NewSize:          100,
		NewTailHash:      "tail-" + sourceKey,
		PageStartOffset:  0,
		PageEndOffset:    100,
		Events:           []localstore.Event{event},
	}); err != nil {
		t.Fatal(err)
	}
	if err := store.ActivateGeneration(ctx, gen.ID); err != nil {
		t.Fatal(err)
	}
}

func assertSearchCountWithOptions(t *testing.T, ctx context.Context, store *localstore.Store, opts searchOptions, want int) {
	t.Helper()
	if opts.Mode == "" {
		opts.Mode = search.ModeLexical
	}
	hits, err := lexicalSearch(ctx, store, opts)
	if err != nil {
		t.Fatal(err)
	}
	if len(hits) != want {
		t.Fatalf("hit count = %d, want %d; hits=%+v opts=%+v", len(hits), want, hits, opts)
	}
}
