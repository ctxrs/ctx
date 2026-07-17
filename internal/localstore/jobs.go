package localstore

import (
	"context"
	"database/sql"
	"errors"
	"time"
)

func (s *Store) EnqueueJob(ctx context.Context, job Job) (Job, error) {
	now := nowUTC()
	if job.AvailableAt.IsZero() {
		job.AvailableAt = now
	}
	_, err := s.db.ExecContext(ctx, `
		INSERT INTO jobs(job_key, job_type, state, payload_json, available_at, created_at, updated_at)
		VALUES(?, ?, 'pending', ?, ?, ?, ?)
		ON CONFLICT(job_key) DO UPDATE SET
			job_type = excluded.job_type,
			payload_json = excluded.payload_json,
			state = CASE WHEN jobs.state = 'completed' THEN jobs.state ELSE 'pending' END,
			available_at = excluded.available_at,
			updated_at = excluded.updated_at
	`, job.Key, job.Type, job.PayloadJSON, formatTime(job.AvailableAt), formatTime(now), formatTime(now))
	if err != nil {
		return Job{}, err
	}
	return s.jobByKey(ctx, job.Key)
}

func (s *Store) ClaimJob(ctx context.Context, now time.Time, lease time.Duration) (Job, bool, error) {
	if now.IsZero() {
		now = nowUTC()
	} else {
		now = now.UTC()
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return Job{}, false, err
	}
	defer rollback(tx)

	var id int64
	err = tx.QueryRowContext(ctx, `
		SELECT id
		FROM jobs
		WHERE state = 'pending' AND available_at <= ?
		ORDER BY available_at, id
		LIMIT 1
	`, formatTime(now)).Scan(&id)
	if errors.Is(err, sql.ErrNoRows) {
		return Job{}, false, tx.Commit()
	}
	if err != nil {
		return Job{}, false, err
	}
	if _, err := tx.ExecContext(ctx, `
		UPDATE jobs
		SET state = 'running', attempts = attempts + 1, leased_until = ?, updated_at = ?
		WHERE id = ?
	`, formatTime(now.Add(lease)), formatTime(now), id); err != nil {
		return Job{}, false, err
	}
	job, err := scanJobRow(tx.QueryRowContext(ctx, jobSelectSQL+` WHERE id = ?`, id))
	if err != nil {
		return Job{}, false, err
	}
	return job, true, tx.Commit()
}

func (s *Store) CompleteJob(ctx context.Context, id int64) error {
	_, err := s.db.ExecContext(ctx, `
		UPDATE jobs
		SET state = 'completed', leased_until = NULL, updated_at = ?
		WHERE id = ?
	`, formatTime(nowUTC()), id)
	return err
}

func (s *Store) FailJob(ctx context.Context, id int64, message string, retryAt time.Time) error {
	state := "failed"
	if !retryAt.IsZero() {
		state = "pending"
	}
	_, err := s.db.ExecContext(ctx, `
		UPDATE jobs
		SET state = ?, leased_until = NULL, last_error = ?, available_at = ?, updated_at = ?
		WHERE id = ?
	`, state, message, formatTime(retryAt.UTC()), formatTime(nowUTC()), id)
	return err
}

func (s *Store) jobByKey(ctx context.Context, key string) (Job, error) {
	return scanJobRow(s.db.QueryRowContext(ctx, jobSelectSQL+` WHERE job_key = ?`, key))
}

const jobSelectSQL = `
	SELECT id, job_key, job_type, state, payload_json, attempts, available_at,
		leased_until, last_error, created_at, updated_at
	FROM jobs`

func scanJobRow(row *sql.Row) (Job, error) {
	var job Job
	var available, created, updated string
	var leased sql.NullString
	if err := row.Scan(&job.ID, &job.Key, &job.Type, &job.State, &job.PayloadJSON,
		&job.Attempts, &available, &leased, &job.LastError, &created, &updated); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return Job{}, ErrNotFound
		}
		return Job{}, err
	}
	job.AvailableAt = parseTime(available)
	job.LeasedUntil = nullableTime(leased)
	job.CreatedAt = parseTime(created)
	job.UpdatedAt = parseTime(updated)
	return job, nil
}
