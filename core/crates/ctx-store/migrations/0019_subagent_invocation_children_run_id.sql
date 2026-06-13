ALTER TABLE subagent_invocation_children
  ADD COLUMN run_id TEXT;

CREATE INDEX IF NOT EXISTS idx_subagent_invocation_children_run_id
  ON subagent_invocation_children(run_id);
