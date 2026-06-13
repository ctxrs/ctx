CREATE TABLE IF NOT EXISTS session_compaction_seeds (
  session_id TEXT PRIMARY KEY NOT NULL,
  seed_text TEXT NOT NULL,
  reason TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);
