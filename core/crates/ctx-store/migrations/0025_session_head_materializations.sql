CREATE TABLE IF NOT EXISTS session_head_materializations (
  session_id TEXT NOT NULL,
  head_kind TEXT NOT NULL,
  head_rev INTEGER NOT NULL,
  last_event_seq INTEGER NOT NULL,
  turns_json TEXT NOT NULL,
  tool_summaries_json TEXT NOT NULL,
  events_json TEXT NOT NULL,
  messages_json TEXT NOT NULL,
  has_more_turns INTEGER NOT NULL,
  head_window_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (session_id, head_kind),
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_session_head_materializations_session_id
  ON session_head_materializations(session_id);
