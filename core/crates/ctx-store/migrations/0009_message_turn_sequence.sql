ALTER TABLE messages ADD COLUMN turn_sequence INTEGER;
CREATE INDEX IF NOT EXISTS idx_messages_session_turn ON messages(session_id, turn_id, turn_sequence, created_at);

CREATE TABLE IF NOT EXISTS session_turns_new (
  turn_id TEXT PRIMARY KEY NOT NULL,
  session_id TEXT NOT NULL,
  run_id TEXT,
  user_message_id TEXT,
  status TEXT NOT NULL,
  start_seq INTEGER,
  end_seq INTEGER,
  started_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  assistant_partial TEXT,
  thought_partial TEXT,
  metrics_json TEXT,
  tool_total INTEGER NOT NULL DEFAULT 0,
  tool_pending INTEGER NOT NULL DEFAULT 0,
  tool_running INTEGER NOT NULL DEFAULT 0,
  tool_completed INTEGER NOT NULL DEFAULT 0,
  tool_failed INTEGER NOT NULL DEFAULT 0,
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

INSERT INTO session_turns_new (
  turn_id,
  session_id,
  run_id,
  user_message_id,
  status,
  start_seq,
  end_seq,
  started_at,
  updated_at,
  assistant_partial,
  thought_partial,
  metrics_json,
  tool_total,
  tool_pending,
  tool_running,
  tool_completed,
  tool_failed
)
SELECT
  turn_id,
  session_id,
  run_id,
  user_message_id,
  status,
  start_seq,
  end_seq,
  started_at,
  updated_at,
  assistant_partial,
  thought_partial,
  metrics_json,
  tool_total,
  tool_pending,
  tool_running,
  tool_completed,
  tool_failed
FROM session_turns;

DROP TABLE session_turns;
ALTER TABLE session_turns_new RENAME TO session_turns;

CREATE INDEX IF NOT EXISTS idx_session_turns_session_id ON session_turns(session_id);
CREATE INDEX IF NOT EXISTS idx_session_turns_session_id_start_seq ON session_turns(session_id, start_seq);
CREATE INDEX IF NOT EXISTS idx_session_turns_session_id_started_at ON session_turns(session_id, started_at);
