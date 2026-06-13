ALTER TABLE session_snapshot_summaries
  ADD COLUMN projection_rev INTEGER NOT NULL DEFAULT 0;

UPDATE session_snapshot_summaries
SET projection_rev = COALESCE(last_event_seq, 0)
WHERE projection_rev = 0;
