package localstore

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"time"

	"github.com/ctxrs/ctx/internal/capture"
	historystore "github.com/ctxrs/ctx/internal/store"
)

type Adapter struct {
	Store    *Store
	DataRoot string
}

var _ historystore.Store = (*Adapter)(nil)

func NewAdapter(store *Store, dataRoot string) *Adapter {
	return &Adapter{Store: store, DataRoot: dataRoot}
}

func (a *Adapter) Setup(ctx context.Context, _ historystore.SetupOptions) error {
	return a.store().Initialize(ctx)
}

func (a *Adapter) Status(ctx context.Context) (historystore.Status, error) {
	status, err := a.store().Status(ctx)
	if err != nil {
		return historystore.Status{}, err
	}
	return historystore.Status{
		Ready:           status.Initialized,
		DataRoot:        a.DataRoot,
		SchemaVersion:   status.SchemaVersion,
		Sources:         status.SourceCount,
		IndexedSessions: status.ActiveGenerationCount,
		IndexedEvents:   status.ActiveEventCount,
	}, nil
}

func (a *Adapter) ListSources(ctx context.Context, filter historystore.SourceFilter) ([]historystore.Source, error) {
	sources, err := a.store().ListStableSources(ctx)
	if err != nil {
		return nil, err
	}
	result := make([]historystore.Source, 0, len(sources))
	for _, source := range sources {
		if filter.Provider != "" && source.Provider != filter.Provider {
			continue
		}
		result = append(result, historystore.Source{
			ID:        source.SourceKey,
			Provider:  source.Provider,
			Path:      source.SourcePath,
			Available: source.IndexedStatus == "indexed",
		})
	}
	return result, nil
}

func (a *Adapter) SaveBatch(ctx context.Context, batch capture.CapturedBatch) error {
	if len(batch.Records) == 0 {
		return capture.ErrNoRecords
	}
	source, err := a.store().UpsertSource(ctx, SourceDescriptor{
		Key:          string(batch.Provider) + ":" + batch.Source.ID,
		Provider:     string(batch.Provider),
		Format:       batch.SourceFormat,
		URI:          batch.Source.URI,
		Identity:     batch.Revision.Identity,
		MetadataJSON: batchMetadataJSON(batch),
	})
	if err != nil {
		return err
	}
	gen, err := a.store().BeginGeneration(ctx, source.Key, GenerationOptions{
		Kind:           GenerationReplace,
		SourceIdentity: batch.Revision.Identity,
		BaseSizeBytes:  batch.Revision.SizeBytes,
		BaseTailHash:   batch.Revision.ContentHash,
	})
	if err != nil {
		return err
	}
	events := make([]Event, 0, len(batch.Records))
	for _, record := range batch.Records {
		events = append(events, Event{
			SourceEventID:      sourceEventID(record),
			ProviderSessionID:  providerSessionID(batch, record),
			ProviderEventIndex: record.Ordinal,
			Role:               record.Hints["role"],
			Type:               record.Kind,
			OccurredAt:         recordOccurredAt(record),
			Text:               strings.TrimRight(string(record.Raw), "\r\n"),
			MetadataJSON:       recordMetadataJSON(record),
			SourceOffset:       record.ByteStart,
			SourceEndOffset:    record.ByteEnd,
		})
	}
	if _, err := a.store().AppendEvents(ctx, AppendRequest{
		SourceKey:        source.Key,
		GenerationID:     gen.ID,
		SourceIdentity:   batch.Revision.Identity,
		PreviousSize:     batch.Revision.SizeBytes,
		PreviousTailHash: batch.Revision.ContentHash,
		NewSize:          batch.Checkpoint.NextByte,
		NewTailHash:      batch.Checkpoint.ContentHash,
		PageStartOffset:  batch.Range.StartByte,
		PageEndOffset:    batch.Range.EndByte,
		Events:           events,
	}); err != nil {
		return err
	}
	return a.store().ActivateGeneration(ctx, gen.ID)
}

func (a *Adapter) ShowSession(ctx context.Context, id string) (historystore.Transcript, error) {
	transcript, err := a.store().ReadSession(ctx, id)
	if err != nil {
		return historystore.Transcript{}, mapStoreError(err)
	}
	return transcriptFromStable(transcript), nil
}

func (a *Adapter) ShowEvent(ctx context.Context, id string, opts historystore.EventWindowOptions) (historystore.Transcript, error) {
	window, err := a.store().ReadEvent(ctx, id, opts.Before, opts.After)
	if err != nil {
		return historystore.Transcript{}, mapStoreError(err)
	}
	return transcriptFromEventWindow(window), nil
}

func (a *Adapter) LocateSession(ctx context.Context, id string) (historystore.Location, error) {
	location, err := a.store().LocateSession(ctx, id)
	if err != nil {
		return historystore.Location{}, mapStoreError(err)
	}
	return locationFromStable(location, location.CtxSessionID), nil
}

func (a *Adapter) LocateEvent(ctx context.Context, id string) (historystore.Location, error) {
	location, err := a.store().LocateEvent(ctx, id)
	if err != nil {
		return historystore.Location{}, mapStoreError(err)
	}
	return locationFromStable(location, location.CtxEventID), nil
}

func (a *Adapter) QuerySQL(ctx context.Context, query historystore.SQLQuery) (historystore.SQLResult, error) {
	if len(query.Args) > 0 {
		return historystore.SQLResult{}, errors.New("query parameters are not supported")
	}
	rows, err := a.store().QueryReadOnlySQL(ctx, query.Text, defaultSQLMaxRows)
	if err != nil {
		return historystore.SQLResult{}, err
	}
	return historystore.SQLResult{Columns: rows.Columns, Rows: rows.Rows}, nil
}

func (a *Adapter) Close() error {
	if a == nil || a.Store == nil {
		return nil
	}
	return a.Store.Close()
}

func (a *Adapter) store() *Store {
	if a == nil || a.Store == nil {
		panic("localstore adapter has nil store")
	}
	return a.Store
}

func transcriptFromStable(transcript StableTranscript) historystore.Transcript {
	return historystore.Transcript{
		ID:       transcript.Session.CtxSessionID,
		Title:    transcript.Session.ProviderSessionID,
		Provider: transcript.Session.Provider,
		Events:   transcriptEventsFromStable(transcript.Events),
	}
}

func transcriptFromEventWindow(window StableEventWindow) historystore.Transcript {
	return historystore.Transcript{
		ID:       window.Event.CtxSessionID,
		Title:    window.Event.ProviderSessionID,
		Provider: window.Event.Provider,
		Events:   transcriptEventsFromStable(window.Events),
	}
}

func transcriptEventsFromStable(events []StableEvent) []historystore.TranscriptEvent {
	result := make([]historystore.TranscriptEvent, 0, len(events))
	for _, event := range events {
		result = append(result, historystore.TranscriptEvent{
			ID:        event.CtxEventID,
			Role:      event.Role,
			Text:      event.Text,
			CreatedAt: time.UnixMilli(event.OccurredAtMS).UTC(),
		})
	}
	return result
}

func locationFromStable(location StableLocation, id string) historystore.Location {
	return historystore.Location{
		ID:       id,
		Provider: location.Provider,
		Path:     location.SourcePath,
	}
}

func mapStoreError(err error) error {
	if errors.Is(err, ErrNotFound) {
		return historystore.ErrNotFound
	}
	return err
}

func batchMetadataJSON(batch capture.CapturedBatch) string {
	value := map[string]any{
		"captured_batch_id": batch.ID,
		"source_path":       batch.Source.Path,
		"source_root_uri":   batch.Source.RootURI,
		"native_source_id":  batch.Source.NativeID,
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return "{}"
	}
	return string(encoded)
}

func recordMetadataJSON(record capture.ProviderRecord) string {
	value := map[string]any{
		"content_hash": record.ContentHash,
		"malformed":    record.Malformed,
		"parse_error":  record.ParseError,
		"hints":        record.Hints,
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return "{}"
	}
	return string(encoded)
}

func sourceEventID(record capture.ProviderRecord) string {
	if record.NativeID != "" {
		return record.NativeID
	}
	if record.ContentHash != "" {
		return record.ContentHash
	}
	return record.Kind
}

func providerSessionID(batch capture.CapturedBatch, record capture.ProviderRecord) string {
	if id := record.Hints["provider_session_id"]; id != "" {
		return id
	}
	if id := record.Hints["session_id"]; id != "" {
		return id
	}
	if batch.Source.NativeID != "" {
		return batch.Source.NativeID
	}
	return batch.Source.ID
}

func recordOccurredAt(record capture.ProviderRecord) time.Time {
	for _, key := range []string{"timestamp", "occurred_at", "created_at"} {
		if value := record.Hints[key]; value != "" {
			if parsed, err := time.Parse(time.RFC3339Nano, value); err == nil {
				return parsed.UTC()
			}
		}
	}
	return time.Time{}
}
