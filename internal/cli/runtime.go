package cli

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/ctxrs/ctx/internal/capture"
	"github.com/ctxrs/ctx/internal/localstore"
	"github.com/ctxrs/ctx/internal/provider/codex"
	"github.com/ctxrs/ctx/internal/provider/pi"
	"github.com/ctxrs/ctx/internal/search"
)

const schemaVersion = 1

type sourceRow struct {
	Provider     string `json:"provider"`
	DisplayName  string `json:"display_name"`
	SourceFormat string `json:"source_format"`
	Path         string `json:"path"`
	Exists       bool   `json:"exists"`
	Status       string `json:"status"`
	Importable   bool   `json:"importable"`
	Message      string `json:"message,omitempty"`
}

type importSummary struct {
	Sources      int `json:"sources"`
	Imported     int `json:"imported"`
	Skipped      int `json:"skipped"`
	Events       int `json:"events"`
	Errors       int `json:"errors"`
	Batches      int `json:"batches"`
	Generations  int `json:"generations"`
	ActiveEvents int `json:"active_events"`
}

func (a *App) runStatus(ctx context.Context, args []string) error {
	jsonOut := hasFlag(args, "--json")
	root, err := a.dataRoot()
	if err != nil {
		return err
	}
	dbPath := databasePath(root)
	status := localstore.Status{}
	if _, err := os.Stat(dbPath); err == nil {
		store, err := localstore.OpenReadOnly(ctx, dbPath)
		if err != nil {
			return commandError("status", CodeStorage, "open local store", err)
		}
		defer store.Close()
		status, err = store.Status(ctx)
		if err != nil {
			return commandError("status", CodeStorage, "read local store status", err)
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return commandError("status", CodeStorage, "check local store", err)
	}

	payload := map[string]any{
		"schema_version":       schemaVersion,
		"initialized":          status.Initialized,
		"data_root":            root,
		"database_path":        dbPath,
		"store_schema_version": status.SchemaVersion,
		"sources":              status.SourceCount,
		"active_generations":   status.ActiveGenerationCount,
		"inactive_generations": status.InactiveGenerationCount,
		"stale_generations":    status.StaleGenerationCount,
		"events":               status.EventCount,
		"active_events":        status.ActiveEventCount,
		"pending_jobs":         status.PendingJobCount,
		"running_jobs":         status.RunningJobCount,
		"failed_jobs":          status.FailedJobCount,
		"completed_jobs":       status.CompletedJobCount,
		"local_only":           true,
		"read_only":            true,
		"semantic_search":      "unavailable",
		"daemon":               "not_started",
		"cloud_sync":           "disabled",
	}
	if jsonOut {
		return writeJSON(a.out, payload)
	}
	if !status.Initialized {
		fmt.Fprintf(a.out, "ctx is not set up\nData root: %s\nDatabase: %s\n", root, dbPath)
		return nil
	}
	fmt.Fprintf(a.out, "ctx local index ready\nData root: %s\nSources: %d\nActive events: %d\nSemantic search: unavailable\n", root, status.SourceCount, status.ActiveEventCount)
	return nil
}

func (a *App) runSources(ctx context.Context, args []string) error {
	_ = ctx
	jsonOut := hasFlag(args, "--json")
	showMissing := hasFlag(args, "--show-missing")
	showAll := hasFlag(args, "--all")
	provider, hasProvider, err := flagValue(args, "--provider")
	if err != nil {
		return commandError("sources", CodeUsage, err.Error(), nil)
	}
	rows, err := discoverSources(provider, hasProvider, showMissing, showAll)
	if err != nil {
		return commandError("sources", CodeStorage, "discover sources", err)
	}
	if jsonOut {
		return writeJSON(a.out, map[string]any{
			"schema_version": schemaVersion,
			"generated_at":   time.Now().UTC().Format(time.RFC3339),
			"sources":        rows,
			"local_only":     true,
			"read_only":      true,
		})
	}
	if len(rows) == 0 {
		fmt.Fprintln(a.out, "No discovered P0 sources. Use --show-missing to show default paths.")
		return nil
	}
	for _, row := range rows {
		state := "missing"
		if row.Exists {
			state = "found"
		}
		if !row.Importable {
			state = row.Status
		}
		fmt.Fprintf(a.out, "%-6s %-28s %-18s %s\n", row.Provider, row.SourceFormat, state, row.Path)
	}
	return nil
}

func (a *App) runSetup(ctx context.Context, args []string) error {
	jsonOut := hasFlag(args, "--json")
	catalogOnly := hasFlag(args, "--catalog-only")
	root, err := a.dataRoot()
	if err != nil {
		return err
	}
	store, err := localstore.Open(ctx, databasePath(root))
	if err != nil {
		return commandError("setup", CodeStorage, "open local store", err)
	}
	defer store.Close()
	if err := store.Initialize(ctx); err != nil {
		return commandError("setup", CodeStorage, "initialize local store", err)
	}
	rows, err := discoverSources("", false, false, false)
	if err != nil {
		return commandError("setup", CodeStorage, "discover sources", err)
	}
	summary := importSummary{Sources: len(rows)}
	if !catalogOnly {
		summary, err = importRows(ctx, store, rows)
		if err != nil {
			return commandError("setup", CodeStorage, "import discovered sources", err)
		}
	}
	status, err := store.Status(ctx)
	if err != nil {
		return commandError("setup", CodeStorage, "read local store status", err)
	}
	summary.ActiveEvents = status.ActiveEventCount
	payload := map[string]any{
		"schema_version":   schemaVersion,
		"mode":             setupMode(catalogOnly),
		"data_root":        root,
		"database_path":    databasePath(root),
		"catalog_only":     catalogOnly,
		"no_daemon":        hasFlag(args, "--no-daemon"),
		"network_required": false,
		"repo_writes":      false,
		"sources":          rows,
		"import":           summary,
		"status":           statusPayload(status),
	}
	if jsonOut {
		return writeJSON(a.out, payload)
	}
	if catalogOnly {
		fmt.Fprintf(a.out, "ctx store initialized; cataloged %d discovered P0 sources\n", len(rows))
		return nil
	}
	fmt.Fprintf(a.out, "ctx store initialized; imported %d events from %d sources\n", summary.Events, summary.Imported)
	return nil
}

func (a *App) runImport(ctx context.Context, args []string) error {
	jsonOut := hasFlag(args, "--json")
	provider, hasProvider, err := flagValue(args, "--provider")
	if err != nil {
		return commandError("import", CodeUsage, err.Error(), nil)
	}
	path, hasPath, err := flagValue(args, "--path")
	if err != nil {
		return commandError("import", CodeUsage, err.Error(), nil)
	}
	if hasPath && !hasProvider {
		return commandError("import", CodeUsage, "--path requires --provider codex or --provider pi", nil)
	}
	if hasProvider && !supportedProvider(provider) {
		return commandError("import", CodeUnavailable, fmt.Sprintf("unsupported provider %q in the Go edge runtime", provider), nil)
	}
	root, err := a.dataRoot()
	if err != nil {
		return err
	}
	store, err := localstore.Open(ctx, databasePath(root))
	if err != nil {
		return commandError("import", CodeStorage, "open local store", err)
	}
	defer store.Close()
	if err := store.Initialize(ctx); err != nil {
		return commandError("import", CodeStorage, "initialize local store", err)
	}

	var rows []sourceRow
	if hasPath {
		rows = []sourceRow{explicitSourceRow(provider, path)}
	} else {
		rows, err = discoverSources(provider, hasProvider, false, hasFlag(args, "--all"))
		if err != nil {
			return commandError("import", CodeStorage, "discover sources", err)
		}
	}
	summary, err := importRows(ctx, store, rows)
	if err != nil {
		return commandError("import", CodeStorage, "import sources", err)
	}
	status, err := store.Status(ctx)
	if err != nil {
		return commandError("import", CodeStorage, "read local store status", err)
	}
	summary.ActiveEvents = status.ActiveEventCount
	payload := map[string]any{
		"schema_version": schemaVersion,
		"sources":        rows,
		"import":         summary,
		"resume": map[string]any{
			"mode":   "deterministic_rescan",
			"cursor": nil,
		},
	}
	if jsonOut {
		return writeJSON(a.out, payload)
	}
	fmt.Fprintf(a.out, "Imported %d events from %d sources\n", summary.Events, summary.Imported)
	return nil
}

func (a *App) runSearch(ctx context.Context, args []string) error {
	opts, err := parseSearchArgs(args)
	if err != nil {
		return commandError("search", CodeUsage, err.Error(), nil)
	}
	if opts.Mode == search.ModeSemantic || opts.Mode == search.ModeHybrid {
		return commandError("search", CodeUnavailable, fmt.Sprintf("%s search is unavailable in the Go edge runtime", opts.Mode), search.ErrSemanticUnavailable)
	}
	if opts.Mode != "" && opts.Mode != search.ModeLexical {
		return commandError("search", CodeUsage, fmt.Sprintf("unsupported search mode %q", opts.Mode), nil)
	}
	if opts.HasProvider && !supportedProvider(opts.Provider) {
		return commandError("search", CodeUnavailable, fmt.Sprintf("unsupported provider %q in the Go edge runtime", opts.Provider), nil)
	}
	if opts.Refresh != "" && !refreshDisabled(opts.Refresh) {
		return commandError("search", CodeUnavailable, "--refresh is not available in the Go edge runtime; pass --refresh off", nil)
	}
	if opts.SemanticWeight != "" && !zeroFloatString(opts.SemanticWeight) {
		return commandError("search", CodeUnavailable, "--semantic-weight requires semantic search, which is unavailable in the Go edge runtime", nil)
	}
	if len(opts.Terms) == 0 {
		return commandError("search", CodeUsage, "provide a query or --term", nil)
	}
	root, err := a.dataRoot()
	if err != nil {
		return err
	}
	store, err := openExistingStore(ctx, root, "search")
	if err != nil {
		return err
	}
	defer store.Close()

	start := time.Now()
	hits, err := lexicalSearch(ctx, store, opts)
	if err != nil {
		return commandError("search", CodeStorage, "run lexical search", err)
	}
	elapsed := time.Since(start)
	if opts.JSON {
		results := make([]map[string]any, 0, len(hits))
		for i, hit := range hits {
			results = append(results, searchHitJSON(i+1, hit))
		}
		return writeJSON(a.out, map[string]any{
			"schema_version": schemaVersion,
			"query":          strings.Join(opts.Terms, " OR "),
			"filters": map[string]any{
				"provider":                optionalString(opts.Provider, opts.HasProvider),
				"session":                 optionalString(opts.SessionID, opts.SessionID != ""),
				"file":                    optionalString(opts.File, opts.File != ""),
				"workspace":               optionalString(opts.Workspace, opts.Workspace != ""),
				"event_type":              optionalString(opts.EventType, opts.EventType != ""),
				"include_current_session": opts.IncludeCurrentSession,
			},
			"freshness": map[string]any{
				"mode":   "off",
				"status": "not_refreshed",
			},
			"mode":       opts.Mode,
			"elapsed_ms": elapsed.Milliseconds(),
			"limit":      opts.Limit,
			"results":    results,
			"pagination": map[string]any{"truncated": len(hits) == opts.Limit},
		})
	}
	if len(hits) == 0 {
		fmt.Fprintln(a.out, "No results")
		return nil
	}
	for _, hit := range hits {
		fmt.Fprintf(a.out, "%s %-6s %s\n", eventID(hit), hit.Provider, compactSnippet(hit.Text, 96))
	}
	return nil
}

func (a *App) runShow(ctx context.Context, args []string) error {
	jsonOut, err := jsonFormat(args)
	if err != nil {
		return commandError("show", CodeUsage, err.Error(), nil)
	}
	kind, id, err := readLookupArgs(args)
	if err != nil {
		return commandError("show", CodeUsage, err.Error(), nil)
	}
	root, err := a.dataRoot()
	if err != nil {
		return err
	}
	store, err := openExistingStore(ctx, root, "show")
	if err != nil {
		return err
	}
	defer store.Close()

	switch kind {
	case "session":
		transcript, err := store.ReadSession(ctx, id)
		if err != nil {
			return readCommandError("show", err)
		}
		if jsonOut {
			return writeJSON(a.out, envelope("showSession", map[string]any{
				"session": map[string]any{
					"session": stableSessionJSON(transcript.Session),
					"events":  stableEventsJSON(transcript.Events),
					"source":  sourceLocationJSON(transcript.Session.SourcePath, ""),
					"mode":    stringFlag(args, "--mode", "lite"),
					"format":  "json",
				},
			}))
		}
		fmt.Fprintf(a.out, "%s %s\n", transcript.Session.Provider, transcript.Session.CtxSessionID)
		for _, event := range transcript.Events {
			fmt.Fprintf(a.out, "%s %-10s %s\n", event.CtxEventID, event.Role, compactSnippet(event.Text, 120))
		}
		return nil
	case "event":
		before, err := intFlag(args, "--before", 0)
		if err != nil {
			return commandError("show", CodeUsage, err.Error(), nil)
		}
		after, err := intFlag(args, "--after", 0)
		if err != nil {
			return commandError("show", CodeUsage, err.Error(), nil)
		}
		if windowSize, err := intFlag(args, "--window", -1); err != nil {
			return commandError("show", CodeUsage, err.Error(), nil)
		} else if windowSize >= 0 {
			before = windowSize
			after = windowSize
		}
		window, err := store.ReadEvent(ctx, id, before, after)
		if err != nil {
			return readCommandError("show", err)
		}
		if jsonOut {
			return writeJSON(a.out, envelope("showEvent", map[string]any{
				"event": map[string]any{
					"event":  stableEventJSON(window.Event),
					"events": stableEventsJSON(window.Events),
					"source": sourceLocationJSON(window.Event.SourcePath, ""),
				},
			}))
		}
		for _, event := range window.Events {
			prefix := " "
			if event.CtxEventID == window.Event.CtxEventID {
				prefix = "*"
			}
			fmt.Fprintf(a.out, "%s %s %-10s %s\n", prefix, event.CtxEventID, event.Role, compactSnippet(event.Text, 120))
		}
		return nil
	default:
		return commandError("show", CodeUsage, "expected `session` or `event`", nil)
	}
}

func (a *App) runLocate(ctx context.Context, args []string) error {
	jsonOut, err := jsonFormat(args)
	if err != nil {
		return commandError("locate", CodeUsage, err.Error(), nil)
	}
	kind, id, err := readLookupArgs(args)
	if err != nil {
		return commandError("locate", CodeUsage, err.Error(), nil)
	}
	root, err := a.dataRoot()
	if err != nil {
		return err
	}
	store, err := openExistingStore(ctx, root, "locate")
	if err != nil {
		return err
	}
	defer store.Close()

	var location localstore.StableLocation
	var operation string
	switch kind {
	case "session":
		location, err = store.LocateSession(ctx, id)
		operation = "locateSession"
	case "event":
		location, err = store.LocateEvent(ctx, id)
		operation = "locateEvent"
	default:
		return commandError("locate", CodeUsage, "expected `session` or `event`", nil)
	}
	if err != nil {
		return readCommandError("locate", err)
	}
	if jsonOut {
		return writeJSON(a.out, envelope(operation, map[string]any{
			"location": stableLocationJSON(location),
		}))
	}
	fmt.Fprintf(a.out, "%s\t%s\t%s\n", location.Provider, firstNonEmpty(location.CtxEventID, location.CtxSessionID), location.SourcePath)
	return nil
}

func (a *App) runSQL(ctx context.Context, args []string) error {
	jsonOut, err := jsonFormat(args)
	if err != nil {
		return commandError("sql", CodeUsage, err.Error(), nil)
	}
	statement := strings.TrimSpace(strings.Join(positionalArgs(args), " "))
	if statement == "" {
		return commandError("sql", CodeUsage, "SQL statement is required", nil)
	}
	root, err := a.dataRoot()
	if err != nil {
		return err
	}
	store, err := openExistingStore(ctx, root, "sql")
	if err != nil {
		return err
	}
	defer store.Close()

	result, err := store.QueryReadOnlySQL(ctx, statement, 0)
	if err != nil {
		return commandError("sql", CodeStorage, "run read-only SQL", err)
	}
	if jsonOut {
		return writeJSON(a.out, envelope("sql", map[string]any{
			"sql": map[string]any{
				"columns":   result.Columns,
				"rows":      sqlRowsJSON(result),
				"truncated": result.Truncated,
			},
		}))
	}
	fmt.Fprintln(a.out, strings.Join(result.Columns, "\t"))
	for _, row := range result.Rows {
		fmt.Fprintln(a.out, strings.Join(row, "\t"))
	}
	return nil
}

func readLookupArgs(args []string) (string, string, error) {
	positionals := positionalArgs(args)
	if len(positionals) < 1 {
		return "", "", fmt.Errorf("expected `session` or `event`")
	}
	kind := positionals[0]
	id := ""
	if len(positionals) > 1 {
		id = positionals[1]
	}
	if kind == "session" && id == "" {
		if value, ok, err := flagValue(args, "--provider-session"); err != nil {
			return "", "", err
		} else if ok {
			id = value
		}
	}
	if id == "" {
		return "", "", fmt.Errorf("%s id is required", kind)
	}
	return kind, id, nil
}

func readCommandError(command string, err error) *Error {
	if errors.Is(err, localstore.ErrNotFound) {
		return commandError(command, CodeUsage, "not found", err)
	}
	return commandError(command, CodeStorage, "read local store", err)
}

func jsonFormat(args []string) (bool, error) {
	if hasFlag(args, "--json") {
		return true, nil
	}
	value, ok, err := flagValue(args, "--format")
	if err != nil {
		return false, err
	}
	if !ok {
		return false, nil
	}
	switch value {
	case "json":
		return true, nil
	case "text", "table":
		return false, nil
	default:
		return false, fmt.Errorf("unsupported format %q", value)
	}
}

func intFlag(args []string, name string, fallback int) (int, error) {
	value, ok, err := flagValue(args, name)
	if err != nil {
		return 0, err
	}
	if !ok {
		return fallback, nil
	}
	parsed, err := strconv.Atoi(value)
	if err != nil || parsed < 0 || parsed > 100 {
		return 0, fmt.Errorf("%s must be an integer from 0 to 100", name)
	}
	return parsed, nil
}

func stringFlag(args []string, name, fallback string) string {
	value, ok, err := flagValue(args, name)
	if err != nil || !ok {
		return fallback
	}
	return value
}

func envelope(operation string, payload map[string]any) map[string]any {
	result := map[string]any{
		"contractVersion": "agent-history-v1",
		"schemaVersion":   1,
		"operation":       operation,
		"backend": map[string]any{
			"kind": "local",
		},
	}
	for key, value := range payload {
		result[key] = value
	}
	return result
}

func stableSessionJSON(session localstore.StableSession) map[string]any {
	return map[string]any{
		"ctxSessionId":      session.CtxSessionID,
		"provider":          session.Provider,
		"providerSessionId": session.ProviderSessionID,
		"title":             session.ProviderSessionID,
		"startedAt":         jsonTimeMillis(session.StartedAtMS),
		"updatedAt":         jsonTimeMillis(session.EndedAtMS),
		"sourcePath":        session.SourcePath,
	}
}

func stableEventsJSON(events []localstore.StableEvent) []map[string]any {
	result := make([]map[string]any, 0, len(events))
	for _, event := range events {
		result = append(result, stableEventJSON(event))
	}
	return result
}

func stableEventJSON(event localstore.StableEvent) map[string]any {
	return map[string]any{
		"ctxEventId":   event.CtxEventID,
		"ctxSessionId": event.CtxSessionID,
		"sequence":     event.EventSeq,
		"eventType":    event.EventType,
		"role":         event.Role,
		"occurredAt":   jsonTimeMillis(event.OccurredAtMS),
		"source":       event.Provider,
		"cursor":       event.CtxEventID,
		"text":         event.Text,
		"preview":      compactSnippet(event.Text, 220),
		"citations": []map[string]any{{
			"ctx_event_id":        event.CtxEventID,
			"ctx_session_id":      event.CtxSessionID,
			"provider":            event.Provider,
			"provider_session_id": event.ProviderSessionID,
			"source_path":         event.SourcePath,
		}},
	}
}

func stableLocationJSON(location localstore.StableLocation) map[string]any {
	return map[string]any{
		"ctxSessionId":      location.CtxSessionID,
		"ctxEventId":        location.CtxEventID,
		"provider":          location.Provider,
		"providerSessionId": location.ProviderSessionID,
		"source":            sourceLocationJSON(location.SourcePath, location.ResumeCursor),
		"resume": map[string]any{
			"cursor": location.ResumeCursor,
			"path":   location.SourcePath,
		},
	}
}

func sourceLocationJSON(path, cursor string) map[string]any {
	return map[string]any{
		"path":   path,
		"cursor": cursor,
	}
}

func sqlRowsJSON(result localstore.SQLRows) []map[string]string {
	rows := make([]map[string]string, 0, len(result.Rows))
	for _, values := range result.Rows {
		row := make(map[string]string, len(result.Columns))
		for i, column := range result.Columns {
			if i < len(values) {
				row[column] = values[i]
			}
		}
		rows = append(rows, row)
	}
	return rows
}

func jsonTimeMillis(value int64) any {
	if value == 0 {
		return nil
	}
	return time.UnixMilli(value).UTC().Format(time.RFC3339Nano)
}

func importRows(ctx context.Context, store *localstore.Store, rows []sourceRow) (importSummary, error) {
	summary := importSummary{Sources: len(rows)}
	for _, row := range rows {
		if !row.Importable || !row.Exists {
			summary.Skipped++
			continue
		}
		batches, err := captureSource(ctx, row)
		if err != nil {
			summary.Errors++
			return summary, err
		}
		if len(batches) == 0 {
			summary.Skipped++
			continue
		}
		importedSource := false
		for _, batch := range batches {
			count, err := store.SaveCapturedBatch(ctx, batch)
			if err != nil {
				summary.Errors++
				return summary, err
			}
			summary.Events += count
			summary.Batches++
			summary.Generations++
			importedSource = true
		}
		if importedSource {
			summary.Imported++
		}
	}
	return summary, nil
}

func captureSource(ctx context.Context, row sourceRow) ([]capture.CapturedBatch, error) {
	switch row.Provider {
	case "codex":
		adapter := codex.NewAdapter()
		if info, err := os.Stat(row.Path); err == nil && info.IsDir() {
			return adapter.CaptureTree(ctx, row.Path, capture.CaptureOptions{SourceRootURI: capture.SourceRootURI(row.Path)})
		}
		batch, err := adapter.CaptureFile(ctx, row.Path, capture.CaptureOptions{})
		if err != nil || batch == nil {
			return nil, err
		}
		return []capture.CapturedBatch{*batch}, nil
	case "pi":
		paths, err := jsonlSourceFiles(row.Path)
		if err != nil {
			return nil, err
		}
		adapter := pi.NewAdapter()
		batches := make([]capture.CapturedBatch, 0, len(paths))
		rootURI := capture.SourceRootURI(row.Path)
		for _, path := range paths {
			batch, err := adapter.CaptureFile(ctx, path, capture.CaptureOptions{SourceRootURI: rootURI})
			if err != nil {
				return nil, err
			}
			if batch != nil {
				batches = append(batches, *batch)
			}
		}
		return batches, nil
	default:
		return nil, fmt.Errorf("unsupported provider %q", row.Provider)
	}
}

func discoverSources(provider string, hasProvider bool, showMissing bool, showAll bool) ([]sourceRow, error) {
	if hasProvider && !supportedProvider(provider) {
		return []sourceRow{unsupportedSourceRow(provider)}, nil
	}
	var rows []sourceRow
	for _, candidate := range defaultSourceCandidates() {
		if hasProvider && candidate.Provider != provider {
			continue
		}
		info, err := os.Stat(candidate.Path)
		exists := err == nil
		if err != nil && !errors.Is(err, os.ErrNotExist) {
			return nil, err
		}
		row := candidate
		row.Exists = exists
		row.Importable = exists && info != nil
		row.Status = "missing"
		if exists {
			row.Status = "discovered"
		}
		if exists || showMissing {
			rows = append(rows, row)
		}
	}
	if showAll && !hasProvider {
		for _, provider := range []string{"claude", "cursor", "copilot-cli", "opencode"} {
			rows = append(rows, unsupportedSourceRow(provider))
		}
	}
	return rows, nil
}

func defaultSourceCandidates() []sourceRow {
	codexHome := envOrHome("CODEX_HOME", ".codex")
	return []sourceRow{
		{Provider: "codex", DisplayName: "Codex", SourceFormat: "codex_session_jsonl_tree", Path: filepath.Join(codexHome, "sessions")},
		{Provider: "codex", DisplayName: "Codex", SourceFormat: "codex_history_jsonl", Path: filepath.Join(codexHome, "history.jsonl")},
		{Provider: "pi", DisplayName: "Pi", SourceFormat: "pi_session_jsonl", Path: filepath.Join(homeDir(), ".pi", "agent", "sessions")},
		{Provider: "pi", DisplayName: "Pi", SourceFormat: "pi_session_jsonl", Path: filepath.Join(homeDir(), ".omp", "agent", "sessions")},
	}
}

func unsupportedSourceRow(provider string) sourceRow {
	return sourceRow{
		Provider:     provider,
		DisplayName:  provider,
		SourceFormat: "unsupported_go_edge",
		Status:       "unsupported",
		Importable:   false,
		Message:      "provider is outside the P0 Go edge runtime; codex and pi are available",
	}
}

func explicitSourceRow(provider, path string) sourceRow {
	info, err := os.Stat(path)
	return sourceRow{
		Provider:     provider,
		DisplayName:  provider,
		SourceFormat: provider + "_session_jsonl",
		Path:         path,
		Exists:       err == nil,
		Importable:   err == nil && info != nil,
		Status:       mapBool(err == nil, "discovered", "missing"),
	}
}

type searchOptions struct {
	JSON                  bool
	Mode                  search.Mode
	Limit                 int
	Provider              string
	HasProvider           bool
	Terms                 []string
	SessionID             string
	File                  string
	Workspace             string
	Since                 *time.Time
	EventType             string
	Refresh               string
	SemanticWeight        string
	IncludeSubagents      bool
	PrimaryOnly           bool
	Events                bool
	IncludeCurrentSession bool
}

func parseSearchArgs(args []string) (searchOptions, error) {
	opts := searchOptions{
		Mode:  search.ModeLexical,
		Limit: 10,
	}
	var queryParts []string
	for i := 0; i < len(args); i++ {
		arg := args[i]
		switch {
		case arg == "--json":
			opts.JSON = true
		case arg == "--include-subagents":
			opts.IncludeSubagents = true
		case arg == "--primary-only":
			opts.PrimaryOnly = true
		case arg == "--events":
			opts.Events = true
		case arg == "--include-current-session":
			opts.IncludeCurrentSession = true
		case arg == "--mode" || strings.HasPrefix(arg, "--mode="):
			value, next, err := searchFlagValue(args, i, "--mode")
			if err != nil {
				return opts, err
			}
			opts.Mode = search.Mode(value)
			i = next
		case arg == "--backend" || strings.HasPrefix(arg, "--backend="):
			value, next, err := searchFlagValue(args, i, "--backend")
			if err != nil {
				return opts, err
			}
			opts.Mode = search.Mode(value)
			i = next
		case arg == "--limit" || strings.HasPrefix(arg, "--limit="):
			value, next, err := searchFlagValue(args, i, "--limit")
			if err != nil {
				return opts, err
			}
			parsed, err := strconv.Atoi(value)
			if err != nil || parsed <= 0 || parsed > 100 {
				return opts, fmt.Errorf("--limit must be an integer from 1 to 100")
			}
			opts.Limit = parsed
			i = next
		case arg == "--provider" || strings.HasPrefix(arg, "--provider="):
			value, next, err := searchFlagValue(args, i, "--provider")
			if err != nil {
				return opts, err
			}
			opts.Provider = value
			opts.HasProvider = true
			i = next
		case arg == "--term" || strings.HasPrefix(arg, "--term="):
			value, next, err := searchFlagValue(args, i, "--term")
			if err != nil {
				return opts, err
			}
			value = strings.TrimSpace(value)
			if value == "" {
				return opts, fmt.Errorf("--term must not be empty")
			}
			opts.Terms = append(opts.Terms, value)
			i = next
		case arg == "--session" || strings.HasPrefix(arg, "--session="):
			value, next, err := searchFlagValue(args, i, "--session")
			if err != nil {
				return opts, err
			}
			opts.SessionID = value
			i = next
		case arg == "--file" || strings.HasPrefix(arg, "--file="):
			value, next, err := searchFlagValue(args, i, "--file")
			if err != nil {
				return opts, err
			}
			opts.File = value
			i = next
		case arg == "--workspace" || strings.HasPrefix(arg, "--workspace="):
			value, next, err := searchFlagValue(args, i, "--workspace")
			if err != nil {
				return opts, err
			}
			opts.Workspace = value
			i = next
		case arg == "--since" || strings.HasPrefix(arg, "--since="):
			value, next, err := searchFlagValue(args, i, "--since")
			if err != nil {
				return opts, err
			}
			since, err := parseSince(value)
			if err != nil {
				return opts, err
			}
			opts.Since = &since
			i = next
		case arg == "--event-type" || strings.HasPrefix(arg, "--event-type="):
			value, next, err := searchFlagValue(args, i, "--event-type")
			if err != nil {
				return opts, err
			}
			opts.EventType = value
			i = next
		case arg == "--refresh" || strings.HasPrefix(arg, "--refresh="):
			value, next, err := optionalSearchFlagValue(args, i, "--refresh", "auto")
			if err != nil {
				return opts, err
			}
			opts.Refresh = value
			i = next
		case arg == "--semantic-weight" || strings.HasPrefix(arg, "--semantic-weight="):
			value, next, err := searchFlagValue(args, i, "--semantic-weight")
			if err != nil {
				return opts, err
			}
			opts.SemanticWeight = value
			i = next
		case strings.HasPrefix(arg, "-"):
			return opts, fmt.Errorf("unsupported search flag %s", arg)
		default:
			queryParts = append(queryParts, arg)
		}
	}
	if query := strings.TrimSpace(strings.Join(queryParts, " ")); query != "" {
		opts.Terms = append([]string{query}, opts.Terms...)
	}
	return opts, nil
}

func searchFlagValue(args []string, index int, name string) (string, int, error) {
	prefix := name + "="
	arg := args[index]
	if strings.HasPrefix(arg, prefix) {
		value := strings.TrimPrefix(arg, prefix)
		if value == "" {
			return "", index, fmt.Errorf("missing value for %s", name)
		}
		return value, index, nil
	}
	if index+1 >= len(args) || args[index+1] == "" || strings.HasPrefix(args[index+1], "-") {
		return "", index, fmt.Errorf("missing value for %s", name)
	}
	return args[index+1], index + 1, nil
}

func optionalSearchFlagValue(args []string, index int, name, fallback string) (string, int, error) {
	prefix := name + "="
	arg := args[index]
	if strings.HasPrefix(arg, prefix) {
		value := strings.TrimPrefix(arg, prefix)
		if value == "" {
			return "", index, fmt.Errorf("missing value for %s", name)
		}
		return value, index, nil
	}
	if index+1 >= len(args) || args[index+1] == "" || strings.HasPrefix(args[index+1], "-") {
		return fallback, index, nil
	}
	return args[index+1], index + 1, nil
}

func parseSince(value string) (time.Time, error) {
	value = strings.TrimSpace(value)
	for _, layout := range []string{time.RFC3339Nano, time.RFC3339, "2006-01-02"} {
		if parsed, err := time.Parse(layout, value); err == nil {
			return parsed.UTC(), nil
		}
	}
	if strings.HasSuffix(value, "d") || strings.HasSuffix(value, "w") {
		multiplier := 24 * time.Hour
		number := strings.TrimSuffix(value, "d")
		if strings.HasSuffix(value, "w") {
			multiplier = 7 * 24 * time.Hour
			number = strings.TrimSuffix(value, "w")
		}
		count, err := strconv.Atoi(number)
		if err == nil && count >= 0 {
			return time.Now().UTC().Add(-time.Duration(count) * multiplier), nil
		}
	}
	if duration, err := time.ParseDuration(value); err == nil {
		return time.Now().UTC().Add(-duration), nil
	}
	return time.Time{}, fmt.Errorf("--since must be RFC3339, YYYY-MM-DD, or a duration like 24h/7d")
}

func refreshDisabled(value string) bool {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "", "off", "false", "0", "never", "none":
		return true
	default:
		return false
	}
}

func zeroFloatString(value string) bool {
	parsed, err := strconv.ParseFloat(value, 64)
	return err == nil && parsed == 0
}

func matchesPathFilter(path, filter string) bool {
	path = filepath.Clean(path)
	filter = filepath.Clean(filter)
	return path == filter || strings.Contains(path, filter)
}

func excludesActiveCodexTree(hit localstore.SearchHit, activeTree map[string]bool) bool {
	if hit.Provider != "codex" || len(activeTree) == 0 {
		return false
	}
	return activeTree[hit.ProviderSessionID] || activeTree[hit.ParentSessionID] || activeTree[hit.RootSessionID]
}

func lexicalSearch(ctx context.Context, store *localstore.Store, opts searchOptions) ([]localstore.SearchHit, error) {
	type keyedHit struct {
		hit localstore.SearchHit
		key string
	}
	seen := map[string]bool{}
	var merged []keyedHit
	activeID := os.Getenv("CODEX_THREAD_ID")
	activeTree := map[string]bool{}
	if activeID != "" && !opts.IncludeCurrentSession && opts.SessionID == "" {
		ids, err := store.CodexSessionTreeIDs(ctx, activeID)
		if err != nil {
			return nil, err
		}
		activeTree = ids
	}
	for _, term := range opts.Terms {
		hits, err := store.SearchLexical(ctx, term, opts.Limit*20)
		if err != nil {
			return nil, err
		}
		for _, hit := range hits {
			if opts.HasProvider && hit.Provider != opts.Provider {
				continue
			}
			if opts.SessionID != "" && hit.ProviderSessionID != opts.SessionID && sessionID(hit) != opts.SessionID {
				continue
			}
			if opts.File != "" && !matchesPathFilter(hit.SourcePath, opts.File) {
				continue
			}
			if opts.Workspace != "" && !matchesPathFilter(hit.SourcePath, opts.Workspace) {
				continue
			}
			if opts.Since != nil && (hit.OccurredAt.IsZero() || hit.OccurredAt.Before(*opts.Since)) {
				continue
			}
			if opts.EventType != "" && hit.Type != opts.EventType {
				continue
			}
			if opts.PrimaryOnly && hit.ParentSessionID != "" {
				continue
			}
			if excludesActiveCodexTree(hit, activeTree) {
				continue
			}
			key := eventID(hit)
			if seen[key] {
				continue
			}
			seen[key] = true
			merged = append(merged, keyedHit{hit: hit, key: key})
		}
	}
	sort.SliceStable(merged, func(i, j int) bool {
		if merged[i].hit.Rank != merged[j].hit.Rank {
			return merged[i].hit.Rank < merged[j].hit.Rank
		}
		if !merged[i].hit.OccurredAt.Equal(merged[j].hit.OccurredAt) {
			return merged[i].hit.OccurredAt.After(merged[j].hit.OccurredAt)
		}
		if merged[i].hit.Provider != merged[j].hit.Provider {
			return merged[i].hit.Provider < merged[j].hit.Provider
		}
		return merged[i].key < merged[j].key
	})
	if len(merged) > opts.Limit {
		merged = merged[:opts.Limit]
	}
	results := make([]localstore.SearchHit, len(merged))
	for i, item := range merged {
		results[i] = item.hit
	}
	return results, nil
}

func searchHitJSON(rank int, hit localstore.SearchHit) map[string]any {
	return map[string]any{
		"rank":                rank,
		"score":               hit.Rank,
		"ctx_event_id":        eventID(hit),
		"ctx_session_id":      sessionID(hit),
		"provider":            hit.Provider,
		"provider_session_id": hit.ProviderSessionID,
		"event_seq":           hit.ProviderEventIndex,
		"result_scope":        "event",
		"role":                hit.Role,
		"event_type":          hit.Type,
		"timestamp":           formatJSONTime(hit.OccurredAt),
		"title":               hit.ProviderSessionID,
		"snippet":             compactSnippet(hit.Text, 220),
		"why_matched":         []string{"lexical_fts"},
		"citations": []map[string]any{{
			"ctx_event_id":        eventID(hit),
			"ctx_session_id":      sessionID(hit),
			"provider":            hit.Provider,
			"provider_session_id": hit.ProviderSessionID,
			"source_id":           hit.SourceKey,
		}},
		"suggested_next_commands": []string{
			"ctx locate event " + eventID(hit),
		},
	}
}

func eventID(hit localstore.SearchHit) string {
	if hit.SourceEventID == "" {
		return hit.SourceKey + "#event:" + strconv.FormatInt(hit.EventID, 10)
	}
	return hit.SourceKey + "#event:" + hit.SourceEventID
}

func sessionID(hit localstore.SearchHit) string {
	if hit.ProviderSessionID == "" {
		return hit.SourceKey + "#session:" + hit.SourceKey
	}
	return hit.SourceKey + "#session:" + hit.ProviderSessionID
}

func openExistingStore(ctx context.Context, dataRoot, command string) (*localstore.Store, error) {
	dbPath := databasePath(dataRoot)
	if _, err := os.Stat(dbPath); err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, commandError(command, CodeStorage, "local store is not initialized; run `ctx setup` or `ctx import` first", nil)
		}
		return nil, commandError(command, CodeStorage, "check local store", err)
	}
	store, err := localstore.OpenReadOnly(ctx, dbPath)
	if err != nil {
		return nil, commandError(command, CodeStorage, "open local store", err)
	}
	return store, nil
}

func (a *App) dataRoot() (string, error) {
	if a.deps.DataRoot != "" {
		return filepath.Abs(a.deps.DataRoot)
	}
	if root := os.Getenv("CTX_DATA_ROOT"); root != "" {
		return filepath.Abs(root)
	}
	return filepath.Abs(filepath.Join(homeDir(), ".ctx"))
}

func databasePath(dataRoot string) string {
	return filepath.Join(dataRoot, "work.sqlite")
}

func jsonlSourceFiles(path string) ([]string, error) {
	info, err := os.Stat(path)
	if err != nil {
		return nil, err
	}
	if !info.IsDir() {
		return []string{path}, nil
	}
	var paths []string
	err = filepath.WalkDir(path, func(path string, entry os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if entry.IsDir() {
			return nil
		}
		if filepath.Ext(path) == ".jsonl" {
			paths = append(paths, path)
		}
		return nil
	})
	sort.Strings(paths)
	return paths, err
}

func commandError(command string, code ErrorCode, message string, err error) *Error {
	return &Error{Code: code, Command: command, Message: message, Err: err}
}

func writeJSON(w io.Writer, value any) error {
	encoder := json.NewEncoder(w)
	encoder.SetIndent("", "  ")
	return encoder.Encode(value)
}

func statusPayload(status localstore.Status) map[string]any {
	return map[string]any{
		"initialized":        status.Initialized,
		"schema_version":     status.SchemaVersion,
		"sources":            status.SourceCount,
		"active_generations": status.ActiveGenerationCount,
		"active_events":      status.ActiveEventCount,
	}
}

func setupMode(catalogOnly bool) string {
	if catalogOnly {
		return "catalog_only"
	}
	return "import"
}

func compactSnippet(value string, limit int) string {
	value = strings.Join(strings.Fields(value), " ")
	if len(value) <= limit {
		return value
	}
	if limit <= 1 {
		return value[:limit]
	}
	return value[:limit-1] + "..."
}

func formatJSONTime(value time.Time) any {
	if value.IsZero() {
		return nil
	}
	return value.UTC().Format(time.RFC3339Nano)
}

func optionalString(value string, ok bool) any {
	if !ok {
		return nil
	}
	return value
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if value != "" {
			return value
		}
	}
	return ""
}

func homeDir() string {
	if home, err := os.UserHomeDir(); err == nil && home != "" {
		return home
	}
	return "."
}

func envOrHome(envName, fallback string) string {
	if value := os.Getenv(envName); value != "" {
		return value
	}
	return filepath.Join(homeDir(), fallback)
}

func mapBool(ok bool, yes, no string) string {
	if ok {
		return yes
	}
	return no
}

func hasFlag(args []string, name string) bool {
	for _, arg := range args {
		if arg == name {
			return true
		}
	}
	return false
}

func flagValueAny(args []string, names ...string) (string, bool, error) {
	for _, name := range names {
		value, ok, err := flagValue(args, name)
		if err != nil || ok {
			return value, ok, err
		}
	}
	return "", false, nil
}

func repeatedFlagValues(args []string, name string) ([]string, error) {
	var values []string
	prefix := name + "="
	for i := 0; i < len(args); i++ {
		arg := args[i]
		if strings.HasPrefix(arg, prefix) {
			value := strings.TrimPrefix(arg, prefix)
			if value == "" {
				return nil, fmt.Errorf("missing value for %s", name)
			}
			values = append(values, value)
			continue
		}
		if arg == name {
			if i+1 >= len(args) || args[i+1] == "" || strings.HasPrefix(args[i+1], "-") {
				return nil, fmt.Errorf("missing value for %s", name)
			}
			values = append(values, args[i+1])
			i++
		}
	}
	return values, nil
}

func positionalArgs(args []string) []string {
	var positional []string
	for i := 0; i < len(args); i++ {
		arg := args[i]
		if arg == "--json" || arg == "--all" || arg == "--include-current-session" || arg == "--show-missing" || arg == "--catalog-only" || arg == "--no-daemon" {
			continue
		}
		if strings.Contains(arg, "=") && strings.HasPrefix(arg, "--") {
			continue
		}
		if arg == "--provider" || arg == "--provider-session" || arg == "--limit" || arg == "--mode" || arg == "--backend" || arg == "--term" || arg == "--path" || arg == "--format" || arg == "--before" || arg == "--after" || arg == "--window" {
			i++
			continue
		}
		if strings.HasPrefix(arg, "-") {
			continue
		}
		positional = append(positional, arg)
	}
	return positional
}
