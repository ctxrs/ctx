CREATE TABLE IF NOT EXISTS workspace_active_snapshot_state (
  workspace_id TEXT PRIMARY KEY NOT NULL,
  snapshot_rev INTEGER NOT NULL DEFAULT 0,
  archived_rev INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS workspace_active_task_summaries (
  task_id TEXT PRIMARY KEY NOT NULL,
  workspace_id TEXT NOT NULL,
  sort_at TEXT NOT NULL,
  summary_json TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_workspace_active_task_summaries_workspace_id_sort_at
  ON workspace_active_task_summaries (workspace_id, sort_at DESC, task_id DESC);
