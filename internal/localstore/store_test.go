package localstore

import (
	"context"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/ctxrs/ctx/internal/capture"
)

func TestAtomicVisibility(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t, ctx)

	source := upsertTestSource(t, ctx, store, "codex:session")
	gen1 := beginGeneration(t, ctx, store, source.Key)
	appendToGeneration(t, ctx, store, source.Key, gen1.ID, "ident-a", 0, "", 100, "tail-a", []Event{
		testEvent("a-1", "visible old sqlite text"),
	})
	if err := store.ActivateGeneration(ctx, gen1.ID); err != nil {
		t.Fatal(err)
	}

	gen2 := beginGeneration(t, ctx, store, source.Key)
	appendToGeneration(t, ctx, store, source.Key, gen2.ID, "ident-a", 0, "", 80, "tail-b", []Event{
		testEvent("b-1", "hidden replacement text"),
	})
	assertSearchCount(t, ctx, store, "hidden", 0)
	assertSearchCount(t, ctx, store, "visible", 1)

	if err := store.ActivateGeneration(ctx, gen2.ID); err != nil {
		t.Fatal(err)
	}
	assertSearchCount(t, ctx, store, "hidden", 1)
	assertSearchCount(t, ctx, store, "visible", 0)
}

func TestAppendIdempotency(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t, ctx)

	source := upsertTestSource(t, ctx, store, "codex:append")
	gen := beginGeneration(t, ctx, store, source.Key)
	if err := store.ActivateGeneration(ctx, gen.ID); err != nil {
		t.Fatal(err)
	}

	req := AppendRequest{
		SourceKey:        source.Key,
		SourceIdentity:   "ident-a",
		PreviousSize:     0,
		PreviousTailHash: "",
		NewSize:          64,
		NewTailHash:      "tail-64",
		PageStartOffset:  0,
		PageEndOffset:    64,
		Events: []Event{
			testEvent("evt-1", "bounded page replay sqlite"),
			testEvent("evt-2", "another bounded page row"),
		},
	}
	first, err := store.AppendEvents(ctx, req)
	if err != nil {
		t.Fatal(err)
	}
	if first.InsertedEvents != 2 || first.Replayed {
		t.Fatalf("first append = %+v", first)
	}
	second, err := store.AppendEvents(ctx, req)
	if err != nil {
		t.Fatal(err)
	}
	if second.InsertedEvents != 0 || !second.Replayed {
		t.Fatalf("second append = %+v", second)
	}
	assertSearchCount(t, ctx, store, "bounded", 2)
	status, err := store.Status(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if status.EventCount != 2 || status.ActiveEventCount != 2 {
		t.Fatalf("status after replay = %+v", status)
	}
}

func TestReplacementGenerationFlip(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t, ctx)

	source := upsertTestSource(t, ctx, store, "claude:replace")
	oldGen, err := store.BeginGeneration(ctx, source.Key, GenerationOptions{
		Kind:           GenerationReplace,
		SourceIdentity: "identity-old",
	})
	if err != nil {
		t.Fatal(err)
	}
	appendToGeneration(t, ctx, store, source.Key, oldGen.ID, "identity-old", 0, "", 10, "old-tail", []Event{
		testEvent("old", "old generation text"),
	})
	if err := store.ActivateGeneration(ctx, oldGen.ID); err != nil {
		t.Fatal(err)
	}

	newGen, err := store.BeginGeneration(ctx, source.Key, GenerationOptions{
		Kind:           GenerationReplace,
		SourceIdentity: "identity-new",
	})
	if err != nil {
		t.Fatal(err)
	}
	appendToGeneration(t, ctx, store, source.Key, newGen.ID, "identity-new", 0, "", 20, "new-tail", []Event{
		testEvent("new", "new generation text"),
	})
	if err := store.ActivateGeneration(ctx, newGen.ID); err != nil {
		t.Fatal(err)
	}

	assertSearchCount(t, ctx, store, "new", 1)
	assertSearchCount(t, ctx, store, "old", 0)
	status, err := store.Status(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if status.ActiveGenerationCount != 1 || status.StaleGenerationCount != 1 {
		t.Fatalf("status after replacement = %+v", status)
	}
}

func TestStaleInactiveCleanupMarkerStatus(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t, ctx)

	source := upsertTestSource(t, ctx, store, "codex:stale")
	if _, err := store.BeginGeneration(ctx, source.Key, GenerationOptions{Kind: GenerationReplace, SourceIdentity: "stale-ident"}); err != nil {
		t.Fatal(err)
	}
	marked, err := store.MarkInactiveGenerationsStale(ctx, time.Now().Add(time.Hour))
	if err != nil {
		t.Fatal(err)
	}
	if marked != 1 {
		t.Fatalf("marked = %d, want 1", marked)
	}
	status, err := store.Status(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if status.InactiveGenerationCount != 0 || status.StaleGenerationCount != 1 {
		t.Fatalf("status = %+v", status)
	}
}

func TestBasicFTSSearch(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t, ctx)

	source := upsertTestSource(t, ctx, store, "gemini:fts")
	gen := beginGeneration(t, ctx, store, source.Key)
	appendToGeneration(t, ctx, store, source.Key, gen.ID, "ident-a", 0, "", 10, "tail", []Event{
		testEvent("one", "alpha sqlite search term"),
		testEvent("two", "beta unrelated term"),
	})
	if err := store.ActivateGeneration(ctx, gen.ID); err != nil {
		t.Fatal(err)
	}

	hits, err := store.SearchLexical(ctx, "sqlite", 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(hits) != 1 || hits[0].SourceKey != source.Key || hits[0].Text != "alpha sqlite search term" {
		t.Fatalf("hits = %+v", hits)
	}
}

func TestSaveCapturedBatchOwnsRecordProjection(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t, ctx)

	batch, err := capture.NewCapturedBatch(capture.BatchInput{
		Provider:       capture.ProviderCodex,
		SourceFormat:   "codex_session_jsonl",
		SourceID:       "source-save-batch",
		SourcePath:     filepath.Join(t.TempDir(), "session.jsonl"),
		NativeSourceID: "projected-session",
		Revision: capture.SourceRevision{
			ID:          "rev-save-batch",
			Identity:    "identity-save-batch",
			ContentHash: "tail-save-batch",
		},
		Records: []capture.ProviderRecord{
			{
				Ordinal:     0,
				ByteStart:   0,
				ByteEnd:     94,
				Raw:         []byte(`{"timestamp":"2026-07-17T12:00:00Z","type":"session_meta","payload":{"id":"projected-session"}}`),
				ContentHash: "record-hash-0",
			},
			{
				Ordinal:     1,
				ByteStart:   95,
				ByteEnd:     249,
				Raw:         []byte(`{"timestamp":"2026-07-17T12:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"shared localstore ingestion needle"}]}}`),
				ContentHash: "record-hash-1",
			},
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	count, err := store.SaveCapturedBatch(ctx, batch)
	if err != nil {
		t.Fatal(err)
	}
	if count != 2 {
		t.Fatalf("inserted events = %d, want 2", count)
	}
	hits, err := store.SearchLexical(ctx, "needle", 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(hits) != 1 || hits[0].ProviderSessionID != "projected-session" || hits[0].Text != "shared localstore ingestion needle" {
		t.Fatalf("hits = %+v", hits)
	}
}

func TestReadAPIsUseStableViews(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t, ctx)

	source := upsertTestSource(t, ctx, store, "codex:read")
	gen := beginGeneration(t, ctx, store, source.Key)
	appendToGeneration(t, ctx, store, source.Key, gen.ID, "ident-a", 0, "", 200, "tail", []Event{
		{
			SourceEventID:      "source-1",
			ProviderSessionID:  "session-read",
			ProviderEventIndex: 1,
			Role:               "user",
			Type:               "message",
			OccurredAt:         time.Date(2026, 7, 17, 12, 0, 0, 0, time.UTC),
			Text:               "first read api event",
		},
		{
			SourceEventID:      "source-2",
			ProviderSessionID:  "session-read",
			ProviderEventIndex: 2,
			Role:               "assistant",
			Type:               "message",
			OccurredAt:         time.Date(2026, 7, 17, 12, 0, 1, 0, time.UTC),
			Text:               "second read api event",
		},
	})
	if err := store.ActivateGeneration(ctx, gen.ID); err != nil {
		t.Fatal(err)
	}

	transcript, err := store.ReadSession(ctx, "session-read")
	if err != nil {
		t.Fatal(err)
	}
	if transcript.Session.CtxSessionID != "codex:read#session:session-read" || len(transcript.Events) != 2 {
		t.Fatalf("transcript = %+v", transcript)
	}
	window, err := store.ReadEvent(ctx, "source-2", 1, 0)
	if err != nil {
		t.Fatal(err)
	}
	if window.Event.CtxEventID != "codex:read#event:source-2" || len(window.Events) != 2 || window.Events[0].SourceEventID != "source-1" {
		t.Fatalf("window = %+v", window)
	}
	location, err := store.LocateEvent(ctx, "codex:read#event:source-2")
	if err != nil {
		t.Fatal(err)
	}
	if location.CtxSessionID != transcript.Session.CtxSessionID || location.SourcePath == "" {
		t.Fatalf("location = %+v", location)
	}

	sqlRows, err := store.QueryReadOnlySQL(ctx, `
		SELECT ctx_session_id, provider, provider_session_id
		FROM ctx_sessions
		WHERE provider_session_id = 'session-read'
	`, 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(sqlRows.Rows) != 1 || sqlRows.Rows[0][0] != transcript.Session.CtxSessionID {
		t.Fatalf("sql rows = %+v", sqlRows)
	}
}

func TestReadOnlySQLRejectsWritesAndMultipleStatements(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t, ctx)

	if _, err := store.QueryReadOnlySQL(ctx, "INSERT INTO meta(key, value) VALUES('x', 'y')", 0); err == nil {
		t.Fatal("write statement succeeded, want read-only error")
	}
	if _, err := store.QueryReadOnlySQL(ctx, "SELECT 1; SELECT 2", 0); err == nil {
		t.Fatal("multiple statements succeeded, want error")
	}
	rows, err := store.QueryReadOnlySQL(ctx, "SELECT 1 AS one", 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(rows.Columns) != 1 || rows.Columns[0] != "one" || len(rows.Rows) != 1 || rows.Rows[0][0] != "1" {
		t.Fatalf("rows = %+v", rows)
	}
}

func TestOpenReadOnlyDoesNotCreateWALSidecars(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "work.sqlite")
	store, err := Open(ctx, dbPath)
	if err != nil {
		t.Fatal(err)
	}
	if err := store.Initialize(ctx); err != nil {
		t.Fatal(err)
	}
	source := upsertTestSource(t, ctx, store, "codex:readonly")
	gen := beginGeneration(t, ctx, store, source.Key)
	appendToGeneration(t, ctx, store, source.Key, gen.ID, "ident-a", 0, "", 100, "tail-readonly", []Event{
		testEvent("readonly-1", "readonly sidecar search needle"),
	})
	if err := store.ActivateGeneration(ctx, gen.ID); err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}
	for _, suffix := range []string{"-wal", "-shm"} {
		if err := os.Remove(dbPath + suffix); err != nil && !os.IsNotExist(err) {
			t.Fatal(err)
		}
	}

	readOnly, err := OpenReadOnly(ctx, dbPath)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := readOnly.QueryReadOnlySQL(ctx, "SELECT count(*) AS events FROM ctx_events", 0); err != nil {
		t.Fatal(err)
	}
	hits, err := readOnly.SearchLexical(ctx, "sidecar", 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(hits) != 1 {
		t.Fatalf("read-only search hits = %+v, want one", hits)
	}
	if err := readOnly.Close(); err != nil {
		t.Fatal(err)
	}
	for _, suffix := range []string{"-wal", "-shm"} {
		if _, err := os.Stat(dbPath + suffix); !os.IsNotExist(err) {
			t.Fatalf("read-only open created %s sidecar, stat err = %v", suffix, err)
		}
	}
}

func newTestStore(t *testing.T, ctx context.Context) *Store {
	t.Helper()
	store, err := Open(ctx, filepath.Join(t.TempDir(), "work.sqlite"))
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

func upsertTestSource(t *testing.T, ctx context.Context, store *Store, key string) Source {
	t.Helper()
	source, err := store.UpsertSource(ctx, SourceDescriptor{
		Key:      key,
		Provider: "codex",
		Format:   "jsonl",
		URI:      "file:///tmp/" + key,
		Identity: "ident-a",
	})
	if err != nil {
		t.Fatal(err)
	}
	return source
}

func beginGeneration(t *testing.T, ctx context.Context, store *Store, key string) Generation {
	t.Helper()
	gen, err := store.BeginGeneration(ctx, key, GenerationOptions{
		Kind:           GenerationReplace,
		SourceIdentity: "ident-a",
	})
	if err != nil {
		t.Fatal(err)
	}
	return gen
}

func appendToGeneration(t *testing.T, ctx context.Context, store *Store, key string, generationID int64, identity string, prevSize int64, prevTail string, newSize int64, newTail string, events []Event) {
	t.Helper()
	_, err := store.AppendEvents(ctx, AppendRequest{
		SourceKey:        key,
		GenerationID:     generationID,
		SourceIdentity:   identity,
		PreviousSize:     prevSize,
		PreviousTailHash: prevTail,
		NewSize:          newSize,
		NewTailHash:      newTail,
		PageStartOffset:  prevSize,
		PageEndOffset:    newSize,
		Events:           events,
	})
	if err != nil {
		t.Fatal(err)
	}
}

func testEvent(id string, text string) Event {
	return Event{
		SourceEventID:      id,
		ProviderSessionID:  "session-1",
		ProviderEventIndex: 1,
		Role:               "assistant",
		Type:               "message",
		OccurredAt:         time.Date(2026, 7, 17, 12, 0, 0, 0, time.UTC),
		Text:               text,
	}
}

func assertSearchCount(t *testing.T, ctx context.Context, store *Store, query string, want int) {
	t.Helper()
	hits, err := store.SearchLexical(ctx, query, 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(hits) != want {
		t.Fatalf("search %q returned %d hits, want %d: %+v", query, len(hits), want, hits)
	}
}
