CREATE TABLE IF NOT EXISTS run_archive_ingest_sequence (
  id INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
  next_seq INTEGER NOT NULL
);

INSERT OR IGNORE INTO run_archive_ingest_sequence (id, next_seq)
VALUES (1, 1);

CREATE TABLE IF NOT EXISTS run_audit_event_ingest_sequences (
  audit_event_id TEXT PRIMARY KEY NOT NULL,
  run_id TEXT,
  ingest_seq INTEGER NOT NULL UNIQUE,
  created_at TEXT NOT NULL,
  FOREIGN KEY (audit_event_id) REFERENCES run_audit_events(id) ON DELETE CASCADE,
  FOREIGN KEY (run_id) REFERENCES runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_run_audit_event_ingest_sequences_run_seq
  ON run_audit_event_ingest_sequences(run_id, ingest_seq ASC)
  WHERE run_id IS NOT NULL;

INSERT OR IGNORE INTO run_audit_event_ingest_sequences (
  audit_event_id,
  run_id,
  ingest_seq,
  created_at
)
SELECT
  id,
  run_id,
  ROW_NUMBER() OVER (ORDER BY created_at ASC, id ASC),
  created_at
FROM run_audit_events;

UPDATE run_archive_ingest_sequence
SET next_seq = (
  SELECT COALESCE(MAX(ingest_seq), 0) + 1
  FROM run_audit_event_ingest_sequences
)
WHERE id = 1;

CREATE TABLE IF NOT EXISTS run_archive_ingest_cursors (
  run_id TEXT PRIMARY KEY NOT NULL,
  workspace_id TEXT NOT NULL,
  org_id TEXT,
  archive_visibility TEXT NOT NULL,
  retention_policy_key TEXT,
  retention_legal_hold_key TEXT,
  last_session_event_seq INTEGER NOT NULL DEFAULT 0,
  last_audit_event_seq INTEGER NOT NULL DEFAULT 0,
  last_batch_id TEXT,
  last_synced_at TEXT,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (run_id) REFERENCES runs(id) ON DELETE CASCADE,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_run_archive_ingest_cursors_org_updated
  ON run_archive_ingest_cursors(org_id, updated_at DESC)
  WHERE org_id IS NOT NULL;
