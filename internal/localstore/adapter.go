package localstore

import (
	"context"
	"errors"
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
	_, err := a.store().SaveCapturedBatch(ctx, batch)
	return err
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
