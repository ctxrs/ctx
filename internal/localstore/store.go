package localstore

import (
	"context"
	"database/sql"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"

	_ "modernc.org/sqlite"
)

type Store struct {
	db *sql.DB
}

func Open(ctx context.Context, path string) (*Store, error) {
	return open(ctx, path, openOptions{write: true})
}

func OpenReadOnly(ctx context.Context, path string) (*Store, error) {
	opts := openOptions{readOnly: true}
	if sidecarsAbsent(path) {
		opts.immutable = true
	}
	return open(ctx, path, opts)
}

type openOptions struct {
	write     bool
	readOnly  bool
	immutable bool
}

func open(ctx context.Context, path string, opts openOptions) (*Store, error) {
	if opts.write {
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			return nil, err
		}
	}
	dsn := sqliteDSN(path, opts)
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, err
	}
	db.SetMaxOpenConns(1)
	if err := db.PingContext(ctx); err != nil {
		_ = db.Close()
		return nil, err
	}
	if _, err := db.ExecContext(ctx, `PRAGMA busy_timeout = 5000`); err != nil {
		_ = db.Close()
		return nil, err
	}
	if opts.readOnly {
		if _, err := db.ExecContext(ctx, `PRAGMA query_only = ON`); err != nil {
			_ = db.Close()
			return nil, err
		}
	} else {
		for _, stmt := range []string{
			`PRAGMA foreign_keys = ON`,
			`PRAGMA journal_mode = WAL`,
			`PRAGMA synchronous = NORMAL`,
		} {
			if _, err := db.ExecContext(ctx, stmt); err != nil {
				_ = db.Close()
				return nil, err
			}
		}
	}
	return &Store{db: db}, nil
}

func (s *Store) Close() error {
	if s == nil || s.db == nil {
		return nil
	}
	return s.db.Close()
}

func (s *Store) Initialize(ctx context.Context) error {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer rollback(tx)

	for _, stmt := range schemaStatements {
		if _, err := tx.ExecContext(ctx, stmt); err != nil {
			return err
		}
	}
	if _, err := tx.ExecContext(ctx, `
		INSERT INTO meta(key, value) VALUES('schema_version', ?)
		ON CONFLICT(key) DO UPDATE SET value = excluded.value
	`, fmt.Sprint(SchemaVersion)); err != nil {
		return err
	}
	return tx.Commit()
}

func (s *Store) UpsertSource(ctx context.Context, desc SourceDescriptor) (Source, error) {
	if strings.TrimSpace(desc.Key) == "" {
		return Source{}, fmt.Errorf("source key is required")
	}
	now := nowUTC()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return Source{}, err
	}
	defer rollback(tx)

	if _, err := tx.ExecContext(ctx, `
		INSERT INTO sources(source_key, provider, source_format, uri, identity_token, metadata_json, created_at, updated_at)
		VALUES(?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(source_key) DO UPDATE SET
			provider = excluded.provider,
			source_format = excluded.source_format,
			uri = excluded.uri,
			identity_token = CASE
				WHEN sources.active_generation_id IS NULL THEN excluded.identity_token
				ELSE sources.identity_token
			END,
			metadata_json = excluded.metadata_json,
			updated_at = excluded.updated_at
	`, desc.Key, desc.Provider, desc.Format, desc.URI, desc.Identity, desc.MetadataJSON, formatTime(now), formatTime(now)); err != nil {
		return Source{}, err
	}
	source, err := scanSource(ctx, tx, desc.Key)
	if err != nil {
		return Source{}, err
	}
	return source, tx.Commit()
}

func (s *Store) BeginGeneration(ctx context.Context, sourceKey string, opts GenerationOptions) (Generation, error) {
	if opts.Kind == "" {
		opts.Kind = GenerationReplace
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return Generation{}, err
	}
	defer rollback(tx)

	source, err := scanSource(ctx, tx, sourceKey)
	if err != nil {
		return Generation{}, err
	}
	identity := opts.SourceIdentity
	if identity == "" {
		identity = source.Identity
	}
	now := nowUTC()
	result, err := tx.ExecContext(ctx, `
		INSERT INTO source_generations(
			source_id, kind, state, source_identity, base_size_bytes, base_tail_hash,
			high_water_bytes, tail_hash, created_at
		)
		VALUES(?, ?, 'building', ?, ?, ?, ?, ?, ?)
	`, source.ID, string(opts.Kind), identity, opts.BaseSizeBytes, opts.BaseTailHash, opts.BaseSizeBytes, opts.BaseTailHash, formatTime(now))
	if err != nil {
		return Generation{}, err
	}
	id, err := result.LastInsertId()
	if err != nil {
		return Generation{}, err
	}
	gen, err := scanGenerationByID(ctx, tx, id)
	if err != nil {
		return Generation{}, err
	}
	return gen, tx.Commit()
}

func (s *Store) AppendEvents(ctx context.Context, req AppendRequest) (AppendResult, error) {
	if req.NewSize < req.PreviousSize || req.PageEndOffset < req.PageStartOffset {
		return AppendResult{}, ErrInvalidAppend
	}
	if len(req.Events) > MaxAppendEvents || req.PageEndOffset-req.PageStartOffset > MaxAppendBytes {
		return AppendResult{}, ErrInvalidAppend
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return AppendResult{}, err
	}
	defer rollback(tx)

	source, gen, activeAppend, err := appendTarget(ctx, tx, req)
	if err != nil {
		return AppendResult{}, err
	}
	if req.SourceIdentity != "" && req.SourceIdentity != gen.SourceIdentity {
		return AppendResult{}, ErrCheckpointMismatch
	}

	replayed := false
	if activeAppend {
		switch {
		case source.Identity != gen.SourceIdentity:
			return AppendResult{}, ErrCheckpointMismatch
		case source.SizeBytes == req.PreviousSize && source.TailHash == req.PreviousTailHash:
		case source.SizeBytes == req.NewSize && source.TailHash == req.NewTailHash:
			replayed = true
		default:
			return AppendResult{}, ErrCheckpointMismatch
		}
	} else {
		switch {
		case gen.HighWaterBytes == req.PreviousSize && gen.TailHash == req.PreviousTailHash:
		case gen.HighWaterBytes == req.NewSize && gen.TailHash == req.NewTailHash:
			replayed = true
		default:
			return AppendResult{}, ErrCheckpointMismatch
		}
	}

	inserted := 0
	now := nowUTC()
	for _, event := range req.Events {
		if strings.TrimSpace(event.SourceEventID) == "" {
			return AppendResult{}, fmt.Errorf("source event id is required")
		}
		result, err := tx.ExecContext(ctx, `
			INSERT OR IGNORE INTO events(
				generation_id, source_id, source_event_id, provider_session_id,
				parent_provider_session_id, root_provider_session_id, provider_event_index,
				role, event_type, occurred_at, text, metadata_json, source_offset,
				source_end_offset, created_at
			)
			VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		`, gen.ID, gen.SourceID, event.SourceEventID, event.ProviderSessionID,
			event.ParentSessionID, event.RootSessionID, event.ProviderEventIndex,
			event.Role, event.Type, formatOptionalTime(event.OccurredAt), event.Text,
			event.MetadataJSON, event.SourceOffset, event.SourceEndOffset, formatTime(now))
		if err != nil {
			return AppendResult{}, err
		}
		rows, err := result.RowsAffected()
		if err != nil {
			return AppendResult{}, err
		}
		inserted += int(rows)
	}

	nextHighWater := maxInt64(gen.HighWaterBytes, req.NewSize)
	nextTail := gen.TailHash
	if req.NewTailHash != "" {
		nextTail = req.NewTailHash
	}
	if _, err := tx.ExecContext(ctx, `
		UPDATE source_generations
		SET high_water_bytes = ?, tail_hash = ?
		WHERE id = ?
	`, nextHighWater, nextTail, gen.ID); err != nil {
		return AppendResult{}, err
	}
	if activeAppend && !replayed {
		if _, err := tx.ExecContext(ctx, `
			UPDATE sources
			SET size_bytes = ?, tail_hash = ?, updated_at = ?
			WHERE id = ?
		`, req.NewSize, req.NewTailHash, formatTime(now), source.ID); err != nil {
			return AppendResult{}, err
		}
	}
	if _, err := tx.ExecContext(ctx, `
		INSERT OR IGNORE INTO source_revisions(
			source_id, generation_id, source_identity, previous_size_bytes, previous_tail_hash,
			size_bytes, tail_hash, page_start_offset, page_end_offset, created_at
		)
		VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
	`, source.ID, gen.ID, gen.SourceIdentity, req.PreviousSize, req.PreviousTailHash,
		req.NewSize, req.NewTailHash, req.PageStartOffset, req.PageEndOffset, formatTime(now)); err != nil {
		return AppendResult{}, err
	}

	return AppendResult{InsertedEvents: inserted, Replayed: replayed, HighWaterBytes: nextHighWater}, tx.Commit()
}

func (s *Store) ActivateGeneration(ctx context.Context, generationID int64) error {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer rollback(tx)

	gen, err := scanGenerationByID(ctx, tx, generationID)
	if err != nil {
		return err
	}
	source, err := scanSourceByID(ctx, tx, gen.SourceID)
	if err != nil {
		return err
	}
	now := nowUTC()
	if source.ActiveGenerationID != 0 && source.ActiveGenerationID != generationID {
		if _, err := tx.ExecContext(ctx, `
			UPDATE source_generations
			SET state = 'stale', stale_at = ?, cleanup_marked_at = COALESCE(cleanup_marked_at, ?)
			WHERE id = ?
		`, formatTime(now), formatTime(now), source.ActiveGenerationID); err != nil {
			return err
		}
	}
	if _, err := tx.ExecContext(ctx, `
		UPDATE source_generations
		SET state = 'active', activated_at = ?, stale_at = NULL, cleanup_marked_at = NULL
		WHERE id = ?
	`, formatTime(now), generationID); err != nil {
		return err
	}
	if _, err := tx.ExecContext(ctx, `
		UPDATE sources
		SET active_generation_id = ?, identity_token = ?, size_bytes = ?, tail_hash = ?, updated_at = ?
		WHERE id = ?
	`, generationID, gen.SourceIdentity, gen.HighWaterBytes, gen.TailHash, formatTime(now), source.ID); err != nil {
		return err
	}
	return tx.Commit()
}

func (s *Store) MarkInactiveGenerationsStale(ctx context.Context, olderThan time.Time) (int, error) {
	result, err := s.db.ExecContext(ctx, `
		UPDATE source_generations
		SET state = 'stale', stale_at = COALESCE(stale_at, ?), cleanup_marked_at = COALESCE(cleanup_marked_at, ?)
		WHERE state = 'building' AND created_at < ?
	`, formatTime(nowUTC()), formatTime(nowUTC()), formatTime(olderThan.UTC()))
	if err != nil {
		return 0, err
	}
	rows, err := result.RowsAffected()
	return int(rows), err
}

func (s *Store) SearchLexical(ctx context.Context, query string, limit int) ([]SearchHit, error) {
	if limit <= 0 {
		limit = 10
	}
	match := ftsQuery(query)
	if match == "" {
		return nil, nil
	}
	rows, err := s.db.QueryContext(ctx, `
		SELECT e.id, e.generation_id, src.source_key, src.provider, e.provider_session_id,
			e.parent_provider_session_id, e.root_provider_session_id, e.provider_event_index,
			e.role, e.event_type, e.occurred_at, e.text, e.source_event_id, src.uri,
			bm25(event_fts) AS rank
		FROM event_fts
		JOIN events e ON e.id = event_fts.rowid
		JOIN sources src ON src.id = e.source_id AND src.active_generation_id = e.generation_id
		JOIN source_generations gen ON gen.id = e.generation_id AND gen.state = 'active'
		WHERE event_fts MATCH ?
		ORDER BY rank, e.occurred_at DESC, e.id DESC
		LIMIT ?
	`, match, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var hits []SearchHit
	for rows.Next() {
		var hit SearchHit
		var occurred sql.NullString
		if err := rows.Scan(&hit.EventID, &hit.GenerationID, &hit.SourceKey, &hit.Provider,
			&hit.ProviderSessionID, &hit.ParentSessionID, &hit.RootSessionID,
			&hit.ProviderEventIndex, &hit.Role, &hit.Type, &occurred, &hit.Text,
			&hit.SourceEventID, &hit.SourcePath, &hit.Rank); err != nil {
			return nil, err
		}
		if occurred.Valid {
			hit.OccurredAt = parseTime(occurred.String)
		}
		hits = append(hits, hit)
	}
	return hits, rows.Err()
}

func (s *Store) Status(ctx context.Context) (Status, error) {
	var status Status
	exists, err := tableExists(ctx, s.db, "meta")
	if err != nil || !exists {
		return status, err
	}
	version, err := scalarInt(ctx, s.db, `SELECT value FROM meta WHERE key = 'schema_version'`)
	if errorsIsNoRows(err) {
		return status, nil
	}
	if err != nil {
		return status, err
	}
	status.Initialized = true
	status.SchemaVersion = version
	status.SourceCount, err = scalarInt(ctx, s.db, `SELECT count(*) FROM sources`)
	if err != nil {
		return status, err
	}
	status.ActiveGenerationCount, err = scalarInt(ctx, s.db, `SELECT count(*) FROM source_generations WHERE state = 'active'`)
	if err != nil {
		return status, err
	}
	status.InactiveGenerationCount, err = scalarInt(ctx, s.db, `SELECT count(*) FROM source_generations WHERE state = 'building'`)
	if err != nil {
		return status, err
	}
	status.StaleGenerationCount, err = scalarInt(ctx, s.db, `SELECT count(*) FROM source_generations WHERE state = 'stale'`)
	if err != nil {
		return status, err
	}
	status.EventCount, err = scalarInt(ctx, s.db, `SELECT count(*) FROM events`)
	if err != nil {
		return status, err
	}
	status.ActiveEventCount, err = scalarInt(ctx, s.db, `
		SELECT count(*)
		FROM events e
		JOIN sources src ON src.id = e.source_id AND src.active_generation_id = e.generation_id
	`)
	if err != nil {
		return status, err
	}
	status.PendingJobCount, err = scalarInt(ctx, s.db, `SELECT count(*) FROM jobs WHERE state = 'pending'`)
	if err != nil {
		return status, err
	}
	status.RunningJobCount, err = scalarInt(ctx, s.db, `SELECT count(*) FROM jobs WHERE state = 'running'`)
	if err != nil {
		return status, err
	}
	status.FailedJobCount, err = scalarInt(ctx, s.db, `SELECT count(*) FROM jobs WHERE state = 'failed'`)
	if err != nil {
		return status, err
	}
	status.CompletedJobCount, err = scalarInt(ctx, s.db, `SELECT count(*) FROM jobs WHERE state = 'completed'`)
	return status, err
}

func sqliteDSN(path string, opts openOptions) string {
	u := url.URL{Scheme: "file", Path: path}
	q := u.Query()
	if opts.readOnly {
		q.Set("mode", "ro")
		if opts.immutable {
			q.Set("immutable", "1")
		}
	}
	u.RawQuery = q.Encode()
	return u.String()
}

func sidecarsAbsent(path string) bool {
	for _, suffix := range []string{"-wal", "-shm"} {
		if _, err := os.Stat(path + suffix); err == nil || !os.IsNotExist(err) {
			return false
		}
	}
	return true
}
