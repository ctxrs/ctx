ALTER TABLE session_active_snapshot_heads
  ADD COLUMN events_json TEXT NOT NULL DEFAULT '[]';

UPDATE session_active_snapshot_heads
SET events_json = COALESCE(
  (
    SELECT events_json
    FROM session_head_materializations
    WHERE session_head_materializations.session_id = session_active_snapshot_heads.session_id
      AND session_head_materializations.head_kind = 'active'
  ),
  '[]'
);
