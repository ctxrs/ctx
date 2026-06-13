CREATE TABLE IF NOT EXISTS sandbox_bindings (
  worktree_id TEXT PRIMARY KEY NOT NULL,
  workspace_id TEXT NOT NULL,
  runtime_family TEXT NOT NULL,
  profile TEXT NOT NULL DEFAULT 'standard',
  live_workspace_root TEXT NOT NULL,
  live_worktree_root TEXT NOT NULL,
  container_name TEXT,
  host_projection_root TEXT,
  created_at TEXT NOT NULL,
  FOREIGN KEY (worktree_id) REFERENCES worktrees(id) ON DELETE CASCADE,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_sandbox_bindings_workspace_id
  ON sandbox_bindings(workspace_id);
