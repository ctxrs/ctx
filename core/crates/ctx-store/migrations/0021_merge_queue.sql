PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS merge_queue_entries (
  id TEXT PRIMARY KEY NOT NULL,
  workspace_id TEXT NOT NULL,
  worktree_id TEXT,
  session_id TEXT,
  target_branch TEXT NOT NULL,
  message TEXT,
  patch_source TEXT NOT NULL,
  base_commit_sha TEXT,
  head_commit_sha TEXT,
  patch_path TEXT NOT NULL,
  patch_size INTEGER NOT NULL,
  status TEXT NOT NULL,
  result_commit_sha TEXT,
  error_message TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
  FOREIGN KEY (worktree_id) REFERENCES worktrees(id) ON DELETE SET NULL,
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS merge_queue_runs (
  id TEXT PRIMARY KEY NOT NULL,
  entry_id TEXT NOT NULL,
  status TEXT NOT NULL,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  exit_code INTEGER,
  log_path TEXT,
  error_message TEXT,
  result_commit_sha TEXT,
  FOREIGN KEY (entry_id) REFERENCES merge_queue_entries(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_merge_queue_entries_workspace_id
  ON merge_queue_entries(workspace_id);
CREATE INDEX IF NOT EXISTS idx_merge_queue_entries_status
  ON merge_queue_entries(status);
CREATE INDEX IF NOT EXISTS idx_merge_queue_runs_entry_id
  ON merge_queue_runs(entry_id);
