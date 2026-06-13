CREATE TABLE IF NOT EXISTS blobs (
  id TEXT PRIMARY KEY,
  sha256 TEXT NOT NULL,
  bytes INTEGER NOT NULL,
  mime_type TEXT NOT NULL,
  name TEXT,
  created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_blobs_sha256 ON blobs(sha256);
