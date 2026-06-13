CREATE TABLE IF NOT EXISTS workspace_attachments (
  id TEXT PRIMARY KEY NOT NULL,
  workspace_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  source TEXT NOT NULL,
  revision TEXT,
  subpath TEXT,
  mount_relpath TEXT NOT NULL,
  mode TEXT NOT NULL,
  update_policy TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_workspace_attachments_unique
  ON workspace_attachments(workspace_id, kind, name);

CREATE TABLE IF NOT EXISTS track_attachment_mounts (
  track_id TEXT NOT NULL,
  attachment_id TEXT NOT NULL,
  mount_abs_path TEXT NOT NULL,
  materialized_id TEXT NOT NULL,
  status TEXT NOT NULL,
  last_sync_at TEXT,
  error_message TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (track_id, attachment_id),
  FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE,
  FOREIGN KEY (attachment_id) REFERENCES workspace_attachments(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_track_attachment_mounts_attachment_id
  ON track_attachment_mounts(attachment_id);
