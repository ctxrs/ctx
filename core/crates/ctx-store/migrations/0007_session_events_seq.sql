CREATE TABLE IF NOT EXISTS session_events_new (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  session_id TEXT NOT NULL,
  run_id TEXT,
  turn_id TEXT,
  event_type TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

INSERT INTO session_events_new (id, session_id, run_id, turn_id, event_type, payload_json, created_at)
  SELECT id, session_id, run_id, turn_id, event_type, payload_json, created_at
  FROM session_events
  ORDER BY created_at ASC, id ASC;

DROP TABLE session_events;
ALTER TABLE session_events_new RENAME TO session_events;

CREATE INDEX IF NOT EXISTS idx_session_events_session_id ON session_events(session_id);
CREATE INDEX IF NOT EXISTS idx_session_events_session_id_seq ON session_events(session_id, seq);
