CREATE INDEX IF NOT EXISTS idx_session_turn_tools_session_order_desc
  ON session_turn_tools (session_id, order_seq DESC, created_at DESC, tool_call_id DESC);
