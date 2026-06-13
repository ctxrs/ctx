CREATE TABLE IF NOT EXISTS event_log_checkpoints (
  id INTEGER PRIMARY KEY NOT NULL,
  checkpoint_seq INTEGER NOT NULL,
  payload_json TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
