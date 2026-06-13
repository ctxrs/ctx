CREATE TABLE IF NOT EXISTS session_summary_checkpoints (
  session_id TEXT PRIMARY KEY NOT NULL,
  checkpoint_id TEXT NOT NULL,
  summary TEXT NOT NULL,
  last_turn_id TEXT,
  last_event_seq INTEGER,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);
