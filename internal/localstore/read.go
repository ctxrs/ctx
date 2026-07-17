package localstore

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"strconv"
	"strings"
)

const defaultSQLMaxRows = 1000

type StableSession struct {
	CtxSessionID      string
	Provider          string
	ProviderSessionID string
	SourcePath        string
	StartedAtMS       int64
	EndedAtMS         int64
}

type StableEvent struct {
	CtxEventID        string
	CtxSessionID      string
	Provider          string
	ProviderSessionID string
	EventSeq          int64
	EventType         string
	Role              string
	OccurredAtMS      int64
	PayloadJSON       string
	SourcePath        string
	SourceEventID     string
	Text              string
}

type StableTranscript struct {
	Session StableSession
	Events  []StableEvent
}

type StableEventWindow struct {
	Event  StableEvent
	Events []StableEvent
}

type StableLocation struct {
	CtxSessionID      string
	CtxEventID        string
	Provider          string
	ProviderSessionID string
	SourcePath        string
	SourceEventID     string
	ResumeCursor      string
}

type StableSource struct {
	SourceKey         string
	Provider          string
	SourceFormat      string
	SourcePath        string
	IndexedStatus     string
	IndexedEventCount int
}

type SQLRows struct {
	Columns   []string
	Rows      [][]string
	Truncated bool
}

func (s *Store) CodexSessionTreeIDs(ctx context.Context, activeID string) (map[string]bool, error) {
	activeID = strings.TrimSpace(activeID)
	if activeID == "" {
		return map[string]bool{}, nil
	}
	rows, err := s.db.QueryContext(ctx, `
		SELECT DISTINCT provider_session_id, parent_provider_session_id, root_provider_session_id
		FROM events e
		JOIN sources src ON src.id = e.source_id AND src.active_generation_id = e.generation_id
		JOIN source_generations gen ON gen.id = e.generation_id AND gen.state = 'active'
		WHERE src.provider = 'codex'
	`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	type relation struct {
		session string
		parent  string
		root    string
	}
	var relations []relation
	for rows.Next() {
		var item relation
		if err := rows.Scan(&item.session, &item.parent, &item.root); err != nil {
			return nil, err
		}
		relations = append(relations, item)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}

	ids := map[string]bool{activeID: true}
	changed := true
	for changed {
		changed = false
		for _, item := range relations {
			if ids[item.session] || ids[item.parent] || ids[item.root] {
				for _, id := range []string{item.session, item.parent, item.root} {
					if id != "" && !ids[id] {
						ids[id] = true
						changed = true
					}
				}
			}
		}
	}
	return ids, nil
}

func (s *Store) ReadSession(ctx context.Context, id string) (StableTranscript, error) {
	session, err := s.readSessionRow(ctx, id)
	if err != nil {
		return StableTranscript{}, err
	}
	events, err := s.readSessionEvents(ctx, session.CtxSessionID)
	if err != nil {
		return StableTranscript{}, err
	}
	return StableTranscript{Session: session, Events: events}, nil
}

func (s *Store) ReadEvent(ctx context.Context, id string, before, after int) (StableEventWindow, error) {
	event, err := s.readEventRow(ctx, id)
	if err != nil {
		return StableEventWindow{}, err
	}
	events, err := s.readSessionEvents(ctx, event.CtxSessionID)
	if err != nil {
		return StableEventWindow{}, err
	}
	index := -1
	for i, candidate := range events {
		if candidate.CtxEventID == event.CtxEventID {
			index = i
			break
		}
	}
	if index < 0 {
		return StableEventWindow{}, ErrNotFound
	}
	start := index - before
	if start < 0 {
		start = 0
	}
	end := index + after + 1
	if end > len(events) {
		end = len(events)
	}
	return StableEventWindow{Event: event, Events: events[start:end]}, nil
}

func (s *Store) LocateSession(ctx context.Context, id string) (StableLocation, error) {
	session, err := s.readSessionRow(ctx, id)
	if err != nil {
		return StableLocation{}, err
	}
	return StableLocation{
		CtxSessionID:      session.CtxSessionID,
		Provider:          session.Provider,
		ProviderSessionID: session.ProviderSessionID,
		SourcePath:        session.SourcePath,
		ResumeCursor:      "session:" + session.CtxSessionID,
	}, nil
}

func (s *Store) LocateEvent(ctx context.Context, id string) (StableLocation, error) {
	event, err := s.readEventRow(ctx, id)
	if err != nil {
		return StableLocation{}, err
	}
	return StableLocation{
		CtxSessionID:      event.CtxSessionID,
		CtxEventID:        event.CtxEventID,
		Provider:          event.Provider,
		ProviderSessionID: event.ProviderSessionID,
		SourcePath:        event.SourcePath,
		SourceEventID:     event.SourceEventID,
		ResumeCursor:      event.CtxEventID,
	}, nil
}

func (s *Store) ListStableSources(ctx context.Context) ([]StableSource, error) {
	rows, err := s.db.QueryContext(ctx, `
		SELECT source_key, provider, source_format, source_path, indexed_status, indexed_event_count
		FROM ctx_sources
		ORDER BY provider, source_path, source_key
	`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var sources []StableSource
	for rows.Next() {
		var source StableSource
		if err := rows.Scan(&source.SourceKey, &source.Provider, &source.SourceFormat, &source.SourcePath, &source.IndexedStatus, &source.IndexedEventCount); err != nil {
			return nil, err
		}
		sources = append(sources, source)
	}
	return sources, rows.Err()
}

func (s *Store) QueryReadOnlySQL(ctx context.Context, text string, maxRows int) (SQLRows, error) {
	text = strings.TrimSpace(text)
	if text == "" {
		return SQLRows{}, fmt.Errorf("sql statement is required")
	}
	if maxRows <= 0 {
		maxRows = defaultSQLMaxRows
	}
	if hasMultipleSQLStatements(text) {
		return SQLRows{}, fmt.Errorf("only one SQL statement is allowed")
	}

	conn, err := s.db.Conn(ctx)
	if err != nil {
		return SQLRows{}, err
	}
	defer conn.Close()
	if _, err := conn.ExecContext(ctx, `PRAGMA query_only = ON`); err != nil {
		return SQLRows{}, err
	}
	defer func() {
		_, _ = conn.ExecContext(context.Background(), `PRAGMA query_only = OFF`)
	}()

	rows, err := conn.QueryContext(ctx, text)
	if err != nil {
		return SQLRows{}, err
	}
	defer rows.Close()

	columns, err := rows.Columns()
	if err != nil {
		return SQLRows{}, err
	}
	result := SQLRows{Columns: columns}
	for rows.Next() {
		if len(result.Rows) >= maxRows {
			result.Truncated = true
			break
		}
		values := make([]any, len(columns))
		scan := make([]any, len(columns))
		for i := range values {
			scan[i] = &values[i]
		}
		if err := rows.Scan(scan...); err != nil {
			return SQLRows{}, err
		}
		row := make([]string, len(columns))
		for i, value := range values {
			row[i] = sqlValueString(value)
		}
		result.Rows = append(result.Rows, row)
	}
	return result, rows.Err()
}

func (s *Store) readSessionRow(ctx context.Context, id string) (StableSession, error) {
	id = strings.TrimSpace(id)
	if id == "" {
		return StableSession{}, ErrNotFound
	}
	row := s.db.QueryRowContext(ctx, `
		SELECT ctx_session_id, provider, provider_session_id, source_path,
			COALESCE(started_at_ms, 0), COALESCE(ended_at_ms, 0)
		FROM ctx_sessions
		WHERE ctx_session_id = ? OR provider_session_id = ?
		ORDER BY started_at_ms DESC, ctx_session_id
		LIMIT 1
	`, id, id)
	var session StableSession
	if err := row.Scan(&session.CtxSessionID, &session.Provider, &session.ProviderSessionID,
		&session.SourcePath, &session.StartedAtMS, &session.EndedAtMS); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return StableSession{}, ErrNotFound
		}
		return StableSession{}, err
	}
	return session, nil
}

func (s *Store) readEventRow(ctx context.Context, id string) (StableEvent, error) {
	id = strings.TrimSpace(id)
	if id == "" {
		return StableEvent{}, ErrNotFound
	}
	eventID := id
	if _, err := strconv.ParseInt(id, 10, 64); err == nil {
		eventID = "event:" + id
	}
	row := s.db.QueryRowContext(ctx, `
		SELECT ctx_event_id, ctx_session_id, provider, provider_session_id,
			event_seq, event_type, role, COALESCE(occurred_at_ms, 0), payload_json,
			source_path, source_event_id, text
		FROM ctx_events
		WHERE ctx_event_id = ? OR source_event_id = ?
		ORDER BY occurred_at_ms DESC, ctx_event_id
		LIMIT 1
	`, eventID, id)
	event, err := scanStableEvent(row)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return StableEvent{}, ErrNotFound
		}
		return StableEvent{}, err
	}
	return event, nil
}

func (s *Store) readSessionEvents(ctx context.Context, sessionID string) ([]StableEvent, error) {
	rows, err := s.db.QueryContext(ctx, `
		SELECT ctx_event_id, ctx_session_id, provider, provider_session_id,
			event_seq, event_type, role, COALESCE(occurred_at_ms, 0), payload_json,
			source_path, source_event_id, text
		FROM ctx_events
		WHERE ctx_session_id = ?
		ORDER BY event_seq, occurred_at_ms, ctx_event_id
	`, sessionID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var events []StableEvent
	for rows.Next() {
		event, err := scanStableEvent(rows)
		if err != nil {
			return nil, err
		}
		events = append(events, event)
	}
	return events, rows.Err()
}

type stableEventScanner interface {
	Scan(...any) error
}

func scanStableEvent(scanner stableEventScanner) (StableEvent, error) {
	var event StableEvent
	err := scanner.Scan(&event.CtxEventID, &event.CtxSessionID, &event.Provider,
		&event.ProviderSessionID, &event.EventSeq, &event.EventType, &event.Role,
		&event.OccurredAtMS, &event.PayloadJSON, &event.SourcePath, &event.SourceEventID,
		&event.Text)
	return event, err
}

func sqlValueString(value any) string {
	switch typed := value.(type) {
	case nil:
		return ""
	case []byte:
		return string(typed)
	case string:
		return typed
	case int64:
		return strconv.FormatInt(typed, 10)
	case float64:
		return strconv.FormatFloat(typed, 'f', -1, 64)
	case bool:
		if typed {
			return "true"
		}
		return "false"
	default:
		return fmt.Sprint(typed)
	}
}

func hasMultipleSQLStatements(text string) bool {
	inSingle := false
	inDouble := false
	for i := 0; i < len(text); i++ {
		switch text[i] {
		case '\'':
			if !inDouble {
				if inSingle && i+1 < len(text) && text[i+1] == '\'' {
					i++
					continue
				}
				inSingle = !inSingle
			}
		case '"':
			if !inSingle {
				inDouble = !inDouble
			}
		case ';':
			if !inSingle && !inDouble && strings.TrimSpace(text[i+1:]) != "" {
				return true
			}
		}
	}
	return false
}
