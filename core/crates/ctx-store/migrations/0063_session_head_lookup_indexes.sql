CREATE INDEX IF NOT EXISTS idx_messages_session_turn_created
  ON messages (session_id, turn_id, created_at ASC, turn_sequence ASC);

CREATE INDEX IF NOT EXISTS idx_session_turn_tools_session_turn_created
  ON session_turn_tools (session_id, turn_id, created_at ASC);
