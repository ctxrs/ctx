CREATE TABLE IF NOT EXISTS session_turns (
  turn_id TEXT PRIMARY KEY NOT NULL,
  session_id TEXT NOT NULL,
  run_id TEXT,
  user_message_id TEXT,
  assistant_message_id TEXT,
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

CREATE INDEX IF NOT EXISTS idx_session_turns_session_id ON session_turns(session_id);
CREATE INDEX IF NOT EXISTS idx_session_turns_session_id_start_seq ON session_turns(session_id, start_seq);
CREATE INDEX IF NOT EXISTS idx_session_turns_session_id_started_at ON session_turns(session_id, started_at);

CREATE TABLE IF NOT EXISTS session_turn_tools (
  session_id TEXT NOT NULL,
  tool_call_id TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  tool_kind TEXT,
  title TEXT,
  status TEXT,
  input_json TEXT,
  output_text TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (session_id, tool_call_id),
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE,
  FOREIGN KEY (turn_id) REFERENCES session_turns(turn_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_session_turn_tools_turn_id ON session_turn_tools(turn_id);
CREATE INDEX IF NOT EXISTS idx_session_turn_tools_session_id ON session_turn_tools(session_id);
