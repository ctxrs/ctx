package localstore

import (
	"context"
	"path/filepath"
	"testing"
	"time"
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
