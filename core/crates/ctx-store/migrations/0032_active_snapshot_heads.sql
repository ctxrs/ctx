CREATE TABLE IF NOT EXISTS session_active_snapshot_heads (
  session_id TEXT PRIMARY KEY NOT NULL,
  last_event_seq INTEGER NOT NULL,
  turns_json TEXT NOT NULL,
  tool_summaries_json TEXT NOT NULL,
  messages_json TEXT NOT NULL,
  has_more_turns INTEGER NOT NULL,
  head_window_json TEXT NOT NULL,
  summary_checkpoint_json TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_session_active_snapshot_heads_session_id
  ON session_active_snapshot_heads(session_id);

INSERT OR IGNORE INTO session_active_snapshot_heads (
  session_id,
  last_event_seq,
  turns_json,
  tool_summaries_json,
  messages_json,
  has_more_turns,
  head_window_json,
  summary_checkpoint_json,
  created_at,
  updated_at
)
SELECT
  session_id,
  last_event_seq,
  turns_json,
  tool_summaries_json,
  messages_json,
  has_more_turns,
  head_window_json,
  NULL,
  CURRENT_TIMESTAMP,
  CURRENT_TIMESTAMP
FROM session_head_materializations
WHERE head_kind = 'active';
