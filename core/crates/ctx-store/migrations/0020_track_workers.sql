CREATE TABLE IF NOT EXISTS track_workers (
  track_id TEXT PRIMARY KEY NOT NULL,
  worker_id TEXT NOT NULL,
  gateway_url TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_track_workers_track_id ON track_workers(track_id);
