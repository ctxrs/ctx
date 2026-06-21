CREATE TABLE IF NOT EXISTS change_sets (
  id TEXT PRIMARY KEY NOT NULL,
  workspace_id TEXT NOT NULL,
  source_worktree_id TEXT,
  base_revision TEXT,
  head_revision TEXT,
  target_branch TEXT,
  record_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
  FOREIGN KEY (source_worktree_id) REFERENCES worktrees(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_change_sets_workspace_updated
  ON change_sets (workspace_id, updated_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_change_sets_source_worktree
  ON change_sets (source_worktree_id);

CREATE TABLE IF NOT EXISTS contributions (
  id TEXT PRIMARY KEY NOT NULL,
  workspace_id TEXT NOT NULL,
  change_set_id TEXT,
  subject_kind TEXT NOT NULL,
  subject_id TEXT,
  target_kind TEXT NOT NULL,
  target_id TEXT,
  record_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
  FOREIGN KEY (change_set_id) REFERENCES change_sets(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_contributions_workspace_updated
  ON contributions (workspace_id, updated_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_contributions_change_set
  ON contributions (change_set_id);

CREATE INDEX IF NOT EXISTS idx_contributions_workspace_change_set
  ON contributions (workspace_id, change_set_id);

CREATE INDEX IF NOT EXISTS idx_contributions_subject_target
  ON contributions (workspace_id, subject_kind, target_kind);

CREATE INDEX IF NOT EXISTS idx_contributions_subject_endpoint
  ON contributions (workspace_id, subject_kind, subject_id, updated_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_contributions_target_endpoint
  ON contributions (workspace_id, target_kind, target_id, updated_at DESC, id DESC);
