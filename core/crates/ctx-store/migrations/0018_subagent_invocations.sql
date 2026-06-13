CREATE TABLE IF NOT EXISTS subagent_invocations (
  id TEXT PRIMARY KEY NOT NULL,
  tool_call_id TEXT NOT NULL,
  parent_session_id TEXT NOT NULL,
  parent_turn_id TEXT,
  requested_count INTEGER NOT NULL,
  request_json TEXT,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (parent_session_id) REFERENCES sessions(id) ON DELETE CASCADE,
  FOREIGN KEY (parent_turn_id) REFERENCES session_turns(turn_id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_subagent_invocations_parent_session_id
  ON subagent_invocations(parent_session_id);
CREATE INDEX IF NOT EXISTS idx_subagent_invocations_parent_turn_id
  ON subagent_invocations(parent_turn_id);
CREATE INDEX IF NOT EXISTS idx_subagent_invocations_tool_call_id
  ON subagent_invocations(tool_call_id);

CREATE TABLE IF NOT EXISTS subagent_invocation_children (
  invocation_id TEXT NOT NULL,
  child_session_id TEXT NOT NULL,
  position INTEGER NOT NULL,
  status TEXT NOT NULL,
  label TEXT,
  harness TEXT,
  model TEXT,
  reasoning_effort TEXT,
  prompt_length INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (invocation_id, child_session_id),
  FOREIGN KEY (invocation_id) REFERENCES subagent_invocations(id) ON DELETE CASCADE,
  FOREIGN KEY (child_session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_subagent_invocation_children_invocation_id
  ON subagent_invocation_children(invocation_id);
CREATE INDEX IF NOT EXISTS idx_subagent_invocation_children_child_session_id
  ON subagent_invocation_children(child_session_id);
