CREATE TABLE IF NOT EXISTS worktree_vcs_snapshot_cache (
  worktree_id TEXT PRIMARY KEY NOT NULL,
  workspace_id TEXT NOT NULL,
  vcs_kind TEXT,
  snapshot_json TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_worktree_vcs_snapshot_cache_workspace_id
  ON worktree_vcs_snapshot_cache(workspace_id, updated_at DESC);
