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
		store, err := localstore.Open(ctx, dbPath)
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
	jsonOut := hasFlag(args, "--json")
	mode := search.ModeLexical
	if value, ok, err := flagValueAny(args, "--mode", "--backend"); err != nil {
		return commandError("search", CodeUsage, err.Error(), nil)
	} else if ok {
		mode = search.Mode(value)
	}
	if mode == search.ModeSemantic || mode == search.ModeHybrid {
		return commandError("search", CodeUnavailable, fmt.Sprintf("%s search is unavailable in the Go edge runtime", mode), search.ErrSemanticUnavailable)
	}
	if mode != "" && mode != search.ModeLexical {
		return commandError("search", CodeUsage, fmt.Sprintf("unsupported search mode %q", mode), nil)
	}
	limit := 10
	if value, ok, err := flagValue(args, "--limit"); err != nil {
		return commandError("search", CodeUsage, err.Error(), nil)
	} else if ok {
		parsed, err := strconv.Atoi(value)
		if err != nil || parsed <= 0 || parsed > 100 {
			return commandError("search", CodeUsage, "--limit must be an integer from 1 to 100", nil)
		}
		limit = parsed
	}
	provider, hasProvider, err := flagValue(args, "--provider")
	if err != nil {
		return commandError("search", CodeUsage, err.Error(), nil)
	}
	if hasProvider && !supportedProvider(provider) {
		return commandError("search", CodeUnavailable, fmt.Sprintf("unsupported provider %q in the Go edge runtime", provider), nil)
	}
	terms, err := repeatedFlagValues(args, "--term")
	if err != nil {
		return commandError("search", CodeUsage, err.Error(), nil)
	}
	query := strings.TrimSpace(strings.Join(positionalArgs(args), " "))
	searchTerms := make([]string, 0, len(terms)+1)
	if query != "" {
		searchTerms = append(searchTerms, query)
	}
	for _, term := range terms {
		term = strings.TrimSpace(term)
		if term == "" {
			return commandError("search", CodeUsage, "--term must not be empty", nil)
		}
		searchTerms = append(searchTerms, term)
	}
	if len(searchTerms) == 0 {
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
	hits, err := lexicalSearch(ctx, store, searchTerms, provider, hasProvider, !hasFlag(args, "--include-current-session"), limit)
	if err != nil {
		return commandError("search", CodeStorage, "run lexical search", err)
	}
	elapsed := time.Since(start)
	if jsonOut {
		results := make([]map[string]any, 0, len(hits))
		for i, hit := range hits {
			results = append(results, searchHitJSON(i+1, hit))
		}
		return writeJSON(a.out, map[string]any{
			"schema_version": schemaVersion,
			"query":          strings.Join(searchTerms, " OR "),
			"filters": map[string]any{
				"provider":                optionalString(provider, hasProvider),
				"include_current_session": hasFlag(args, "--include-current-session"),
			},
			"freshness": map[string]any{
				"mode":   "off",
				"status": "not_refreshed",
			},
			"mode":       mode,
			"elapsed_ms": elapsed.Milliseconds(),
			"limit":      limit,
			"results":    results,
			"pagination": map[string]any{"truncated": len(hits) == limit},
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
			count, err := saveBatch(ctx, store, batch)
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

func saveBatch(ctx context.Context, store *localstore.Store, batch capture.CapturedBatch) (int, error) {
	metadata, _ := json.Marshal(map[string]any{
		"batch_id":       batch.ID,
		"source_path":    batch.Source.Path,
		"source_root":    batch.Source.RootURI,
		"content_hash":   batch.ContentHash,
		"privacy_policy": batch.Privacy.PolicyID,
	})
	source, err := store.UpsertSource(ctx, localstore.SourceDescriptor{
		Key:          string(batch.Provider) + ":" + batch.Source.ID,
		Provider:     string(batch.Provider),
		Format:       batch.SourceFormat,
		URI:          firstNonEmpty(batch.Source.URI, capture.SourceURI(batch.Source.Path)),
		Identity:     batch.Revision.Identity,
		MetadataJSON: string(metadata),
	})
	if err != nil {
		return 0, err
	}
	gen, err := store.BeginGeneration(ctx, source.Key, localstore.GenerationOptions{
		Kind:           localstore.GenerationReplace,
		SourceIdentity: batch.Revision.Identity,
	})
	if err != nil {
		return 0, err
	}
	total := 0
	prevSize := int64(0)
	prevTail := ""
	for start := 0; start < len(batch.Records); start += localstore.MaxAppendEvents {
		end := start + localstore.MaxAppendEvents
		if end > len(batch.Records) {
			end = len(batch.Records)
		}
		records := batch.Records[start:end]
		events := make([]localstore.Event, 0, len(records))
		for _, record := range records {
			events = append(events, mapRecordEvent(batch, record))
		}
		newSize := records[len(records)-1].ByteEnd
		newTail := records[len(records)-1].ContentHash
		if end == len(batch.Records) {
			newTail = batch.ContentHash
		}
		result, err := store.AppendEvents(ctx, localstore.AppendRequest{
			SourceKey:        source.Key,
			GenerationID:     gen.ID,
			SourceIdentity:   batch.Revision.Identity,
			PreviousSize:     prevSize,
			PreviousTailHash: prevTail,
			NewSize:          newSize,
			NewTailHash:      newTail,
			PageStartOffset:  records[0].ByteStart,
			PageEndOffset:    records[len(records)-1].ByteEnd,
			Events:           events,
		})
		if err != nil {
			return total, err
		}
		total += result.InsertedEvents
		prevSize = newSize
		prevTail = newTail
	}
	if err := store.ActivateGeneration(ctx, gen.ID); err != nil {
		return total, err
	}
	return total, nil
}

func mapRecordEvent(batch capture.CapturedBatch, record capture.ProviderRecord) localstore.Event {
	projection := extractRecordProjection(batch, record)
	metadata, _ := json.Marshal(map[string]any{
		"batch_id":      batch.ID,
		"record_hash":   record.ContentHash,
		"native_id":     record.NativeID,
		"malformed":     record.Malformed,
		"parse_error":   record.ParseError,
		"source_path":   batch.Source.Path,
		"source_format": batch.SourceFormat,
	})
	return localstore.Event{
		SourceEventID:      sourceEventID(record),
		ProviderSessionID:  projection.SessionID,
		ProviderEventIndex: record.Ordinal,
		Role:               projection.Role,
		Type:               projection.Type,
		OccurredAt:         projection.OccurredAt,
		Text:               projection.Text,
		MetadataJSON:       string(metadata),
		SourceOffset:       record.ByteStart,
		SourceEndOffset:    record.ByteEnd,
	}
}

type recordProjection struct {
	SessionID  string
	Role       string
	Type       string
	OccurredAt time.Time
	Text       string
}

func extractRecordProjection(batch capture.CapturedBatch, record capture.ProviderRecord) recordProjection {
	projection := recordProjection{
		SessionID: firstNonEmpty(batch.Source.NativeID, batch.Source.ID),
		Role:      firstNonEmpty(record.Hints["role"], "unknown"),
		Type:      firstNonEmpty(record.Kind, "record"),
		Text:      strings.TrimSpace(string(record.Raw)),
	}
	var value map[string]any
	if err := json.Unmarshal(record.Raw, &value); err != nil {
		return projection
	}
	projection.OccurredAt = parseJSONTime(value["timestamp"])
	switch batch.Provider {
	case capture.ProviderCodex:
		applyCodexProjection(&projection, value)
	case capture.ProviderPi:
		applyPiProjection(&projection, value)
	}
	if projection.Text == "" {
		encoded, _ := json.Marshal(value)
		projection.Text = string(encoded)
	}
	return projection
}

func applyCodexProjection(projection *recordProjection, value map[string]any) {
	if sessionID, _ := value["session_id"].(string); sessionID != "" {
		projection.SessionID = sessionID
	}
	payload, _ := value["payload"].(map[string]any)
	switch value["type"] {
	case "session_meta":
		if id, _ := payload["id"].(string); id != "" {
			projection.SessionID = id
		}
		projection.Role = "system"
		projection.Text = "session metadata " + compactJSON(payload)
	case "response_item":
		itemType, _ := payload["type"].(string)
		projection.Type = firstNonEmpty("response_item:"+itemType, projection.Type)
		if role, _ := payload["role"].(string); role != "" {
			projection.Role = role
		} else {
			projection.Role = "assistant"
		}
		projection.Text = codexPayloadText(payload)
	case "event_msg":
		projection.Role = "system"
		projection.Text = compactJSON(payload)
	default:
		if text, _ := value["text"].(string); text != "" {
			projection.Text = text
			projection.Role = "user"
			projection.Type = "history"
		}
	}
}

func applyPiProjection(projection *recordProjection, value map[string]any) {
	if id, _ := value["id"].(string); value["type"] == "session" && id != "" {
		projection.SessionID = id
		projection.Role = "system"
		projection.Text = "session metadata " + compactJSON(value)
		return
	}
	message, _ := value["message"].(map[string]any)
	if role, _ := message["role"].(string); role != "" {
		projection.Role = role
	}
	if text := textFromAny(message["content"]); text != "" {
		projection.Text = text
	}
}

func codexPayloadText(payload map[string]any) string {
	switch payload["type"] {
	case "message":
		return textFromAny(payload["content"])
	case "function_call":
		return strings.TrimSpace(fmt.Sprintf("%v %v", payload["name"], payload["arguments"]))
	case "function_call_output":
		return fmt.Sprint(payload["output"])
	default:
		return compactJSON(payload)
	}
}

func textFromAny(value any) string {
	switch typed := value.(type) {
	case string:
		return typed
	case []any:
		var parts []string
		for _, item := range typed {
			if object, ok := item.(map[string]any); ok {
				if text, _ := object["text"].(string); text != "" {
					parts = append(parts, text)
				} else {
					parts = append(parts, compactJSON(object))
				}
			} else {
				parts = append(parts, fmt.Sprint(item))
			}
		}
		return strings.Join(parts, "\n")
	case map[string]any:
		return compactJSON(typed)
	default:
		return ""
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

func lexicalSearch(ctx context.Context, store *localstore.Store, terms []string, provider string, hasProvider bool, excludeActive bool, limit int) ([]localstore.SearchHit, error) {
	type keyedHit struct {
		hit localstore.SearchHit
		key string
	}
	seen := map[string]bool{}
	var merged []keyedHit
	activeID := os.Getenv("CODEX_THREAD_ID")
	for _, term := range terms {
		hits, err := store.SearchLexical(ctx, term, limit*20)
		if err != nil {
			return nil, err
		}
		for _, hit := range hits {
			if hasProvider && hit.Provider != provider {
				continue
			}
			if excludeActive && activeID != "" && hit.Provider == "codex" && hit.ProviderSessionID == activeID {
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
	if len(merged) > limit {
		merged = merged[:limit]
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
	return "event:" + strconv.FormatInt(hit.EventID, 10)
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
	store, err := localstore.Open(ctx, dbPath)
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

func sourceEventID(record capture.ProviderRecord) string {
	if record.NativeID != "" {
		return fmt.Sprintf("%012d:%s", record.Ordinal, record.NativeID)
	}
	return fmt.Sprintf("%012d:%s", record.Ordinal, record.ContentHash)
}

func compactJSON(value any) string {
	encoded, err := json.Marshal(value)
	if err != nil {
		return fmt.Sprint(value)
	}
	return string(encoded)
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

func parseJSONTime(value any) time.Time {
	switch typed := value.(type) {
	case string:
		for _, layout := range []string{time.RFC3339Nano, time.RFC3339} {
			if parsed, err := time.Parse(layout, typed); err == nil {
				return parsed.UTC()
			}
		}
	case float64:
		if typed > 1_000_000_000_000 {
			return time.UnixMilli(int64(typed)).UTC()
		}
		return time.Unix(int64(typed), 0).UTC()
	}
	return time.Time{}
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
