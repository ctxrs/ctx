ALTER TABLE session_active_snapshot_heads
ADD COLUMN head_rev INTEGER NOT NULL DEFAULT 0;

UPDATE session_active_snapshot_heads
SET head_rev = COALESCE(
  (
    SELECT projection_rev
    FROM session_snapshot_summaries ss
    WHERE ss.session_id = session_active_snapshot_heads.session_id
  ),
  last_event_seq
);
