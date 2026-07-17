package localstore

import (
	"context"
	"database/sql"
	"errors"
	"regexp"
	"strings"
	"time"
)

type queryer interface {
	QueryRowContext(context.Context, string, ...any) *sql.Row
}

func rollback(tx *sql.Tx) {
	_ = tx.Rollback()
}

func nowUTC() time.Time {
	return time.Now().UTC().Round(time.Microsecond)
}

func formatTime(t time.Time) string {
	return t.UTC().Format(time.RFC3339Nano)
}

func formatOptionalTime(t time.Time) any {
	if t.IsZero() {
		return nil
	}
	return formatTime(t)
}

func parseTime(value string) time.Time {
	if value == "" {
		return time.Time{}
	}
	t, _ := time.Parse(time.RFC3339Nano, value)
	return t
}

func nullableTime(value sql.NullString) time.Time {
	if !value.Valid {
		return time.Time{}
	}
	return parseTime(value.String)
}

func scanSource(ctx context.Context, q queryer, key string) (Source, error) {
	return scanSourceRow(q.QueryRowContext(ctx, `
		SELECT id, source_key, provider, source_format, uri, identity_token, metadata_json,
			size_bytes, tail_hash, COALESCE(active_generation_id, 0), created_at, updated_at
		FROM sources
		WHERE source_key = ?
	`, key))
}

func scanSourceByID(ctx context.Context, q queryer, id int64) (Source, error) {
	return scanSourceRow(q.QueryRowContext(ctx, `
		SELECT id, source_key, provider, source_format, uri, identity_token, metadata_json,
			size_bytes, tail_hash, COALESCE(active_generation_id, 0), created_at, updated_at
		FROM sources
		WHERE id = ?
	`, id))
}

func scanSourceRow(row *sql.Row) (Source, error) {
	var source Source
	var created, updated string
	if err := row.Scan(&source.ID, &source.Key, &source.Provider, &source.Format, &source.URI,
		&source.Identity, &source.MetadataJSON, &source.SizeBytes, &source.TailHash,
		&source.ActiveGenerationID, &created, &updated); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return Source{}, ErrNotFound
		}
		return Source{}, err
	}
	source.CreatedAt = parseTime(created)
	source.UpdatedAt = parseTime(updated)
	return source, nil
}

func scanGenerationByID(ctx context.Context, q queryer, id int64) (Generation, error) {
	var gen Generation
	var activated, stale, cleanup sql.NullString
	var created string
	err := q.QueryRowContext(ctx, `
		SELECT gen.id, gen.source_id, src.source_key, gen.kind, gen.state,
			gen.source_identity, gen.base_size_bytes, gen.base_tail_hash,
			gen.high_water_bytes, gen.tail_hash, gen.created_at, gen.activated_at,
			gen.stale_at, gen.cleanup_marked_at
		FROM source_generations gen
		JOIN sources src ON src.id = gen.source_id
		WHERE gen.id = ?
	`, id).Scan(&gen.ID, &gen.SourceID, &gen.SourceKey, &gen.Kind, &gen.State,
		&gen.SourceIdentity, &gen.BaseSizeBytes, &gen.BaseTailHash, &gen.HighWaterBytes,
		&gen.TailHash, &created, &activated, &stale, &cleanup)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return Generation{}, ErrNotFound
		}
		return Generation{}, err
	}
	gen.CreatedAt = parseTime(created)
	gen.ActivatedAt = nullableTime(activated)
	gen.StaleAt = nullableTime(stale)
	gen.CleanupMarkedAt = nullableTime(cleanup)
	return gen, nil
}

func appendTarget(ctx context.Context, tx *sql.Tx, req AppendRequest) (Source, Generation, bool, error) {
	if req.SourceKey == "" {
		return Source{}, Generation{}, false, ErrInvalidAppend
	}
	source, err := scanSource(ctx, tx, req.SourceKey)
	if err != nil {
		return Source{}, Generation{}, false, err
	}
	if req.GenerationID != 0 {
		gen, err := scanGenerationByID(ctx, tx, req.GenerationID)
		if err != nil {
			return Source{}, Generation{}, false, err
		}
		if gen.SourceID != source.ID {
			return Source{}, Generation{}, false, ErrInvalidAppend
		}
		if gen.State != GenerationBuilding {
			return Source{}, Generation{}, false, ErrInvalidAppend
		}
		return source, gen, false, nil
	}
	if source.ActiveGenerationID == 0 {
		return Source{}, Generation{}, false, ErrNotFound
	}
	gen, err := scanGenerationByID(ctx, tx, source.ActiveGenerationID)
	if err != nil {
		return Source{}, Generation{}, false, err
	}
	return source, gen, true, nil
}

func scalarInt(ctx context.Context, db *sql.DB, query string, args ...any) (int, error) {
	var value int
	if err := db.QueryRowContext(ctx, query, args...).Scan(&value); err != nil {
		return 0, err
	}
	return value, nil
}

func tableExists(ctx context.Context, db *sql.DB, name string) (bool, error) {
	var count int
	if err := db.QueryRowContext(ctx, `
		SELECT count(*)
		FROM sqlite_master
		WHERE type IN ('table', 'view') AND name = ?
	`, name).Scan(&count); err != nil {
		return false, err
	}
	return count > 0, nil
}

func errorsIsNoRows(err error) bool {
	return errors.Is(err, sql.ErrNoRows)
}

func maxInt64(a, b int64) int64 {
	if a > b {
		return a
	}
	return b
}

var ftsTokenPattern = regexp.MustCompile(`[\pL\pN_./:-]+`)

func ftsQuery(query string) string {
	parts := ftsTokenPattern.FindAllString(strings.ToLower(query), -1)
	if len(parts) == 0 {
		return ""
	}
	terms := make([]string, 0, len(parts))
	for _, part := range parts {
		part = strings.Trim(part, `"`)
		if part == "" {
			continue
		}
		terms = append(terms, `"`+strings.ReplaceAll(part, `"`, `""`)+`"`)
	}
	return strings.Join(terms, " AND ")
}
