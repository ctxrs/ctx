PRAGMA foreign_keys = OFF;

CREATE TABLE IF NOT EXISTS tasks_new (
  id TEXT PRIMARY KEY NOT NULL,
  workspace_id TEXT NOT NULL,
  title TEXT NOT NULL,
  description TEXT,
  status TEXT NOT NULL,
  exec_plan_id TEXT,
  primary_session_id TEXT,
  primary_worktree_id TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  archived_at TEXT,
  assistant_seen_at TEXT,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

INSERT INTO tasks_new (
  id,
  workspace_id,
  title,
  description,
  status,
  exec_plan_id,
  primary_session_id,
  primary_worktree_id,
  created_at,
  updated_at,
  archived_at,
  assistant_seen_at
)
SELECT
  id,
  workspace_id,
  title,
  description,
  status,
  exec_plan_id,
  NULL,
  NULL,
  created_at,
  updated_at,
  archived_at,
  assistant_seen_at
FROM tasks;

UPDATE tasks_new
SET primary_session_id = (
    SELECT s.id
    FROM sessions s
    WHERE s.task_id = tasks_new.id
      AND (s.relationship IS NULL OR s.relationship != 'sub_agent')
    ORDER BY
      CASE s.status
        WHEN 'active' THEN 0
        ELSE 1
      END,
      s.updated_at DESC
    LIMIT 1
  ),
  primary_worktree_id = COALESCE(
    (
      SELECT s.worktree_id
      FROM sessions s
      WHERE s.task_id = tasks_new.id
        AND (s.relationship IS NULL OR s.relationship != 'sub_agent')
      ORDER BY
        CASE s.status
          WHEN 'active' THEN 0
          ELSE 1
        END,
        s.updated_at DESC
      LIMIT 1
    ),
    (
      SELECT tr.worktree_id
      FROM tracks tr
      WHERE tr.task_id = tasks_new.id
      ORDER BY tr.created_at ASC
      LIMIT 1
    )
  );

CREATE TABLE IF NOT EXISTS sessions_new (
  id TEXT PRIMARY KEY NOT NULL,
  task_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  worktree_id TEXT NOT NULL,
  parent_session_id TEXT,
  relationship TEXT,
  provider_id TEXT NOT NULL,
  model_id TEXT NOT NULL,
  agent_role TEXT NOT NULL,
  status TEXT NOT NULL,
  provider_session_ref TEXT,
  title TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (task_id) REFERENCES tasks_new(id) ON DELETE CASCADE,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
  FOREIGN KEY (worktree_id) REFERENCES worktrees(id) ON DELETE CASCADE
);

INSERT INTO sessions_new (
  id,
  task_id,
  workspace_id,
  worktree_id,
  parent_session_id,
  relationship,
  provider_id,
  model_id,
  agent_role,
  status,
  provider_session_ref,
  title,
  created_at,
  updated_at
)
SELECT
  id,
  task_id,
  workspace_id,
  worktree_id,
  parent_session_id,
  relationship,
  provider_id,
  model_id,
  agent_role,
  status,
  provider_session_ref,
  title,
  created_at,
  updated_at
FROM sessions;

CREATE TABLE IF NOT EXISTS messages_new (
  id TEXT PRIMARY KEY NOT NULL,
  session_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  run_id TEXT,
  turn_id TEXT,
  turn_sequence INTEGER,
  role TEXT NOT NULL,
  content TEXT NOT NULL,
  attachments_json TEXT,
  delivery TEXT NOT NULL,
  delivered_at TEXT,
  created_at TEXT NOT NULL,
  FOREIGN KEY (session_id) REFERENCES sessions_new(id) ON DELETE CASCADE
);

INSERT INTO messages_new (
  id,
  session_id,
  task_id,
  run_id,
  turn_id,
  turn_sequence,
  role,
  content,
  attachments_json,
  delivery,
  delivered_at,
  created_at
)
SELECT
  id,
  session_id,
  task_id,
  run_id,
  turn_id,
  turn_sequence,
  role,
  content,
  attachments_json,
  delivery,
  delivered_at,
  created_at
FROM messages;

CREATE TABLE IF NOT EXISTS artifacts_new (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  worktree_id TEXT NOT NULL,
  position INTEGER NOT NULL,
  name TEXT,
  absolute_path TEXT NOT NULL,
  mime_type TEXT NOT NULL,
  bytes INTEGER NOT NULL,
  created_at TEXT NOT NULL
);

INSERT INTO artifacts_new (
  id,
  session_id,
  task_id,
  workspace_id,
  worktree_id,
  position,
  name,
  absolute_path,
  mime_type,
  bytes,
  created_at
)
SELECT
  id,
  session_id,
  task_id,
  workspace_id,
  worktree_id,
  position,
  name,
  absolute_path,
  mime_type,
  bytes,
  created_at
FROM artifacts;

CREATE TABLE IF NOT EXISTS worktree_attachment_mounts (
  worktree_id TEXT NOT NULL,
  attachment_id TEXT NOT NULL,
  mount_abs_path TEXT NOT NULL,
  materialized_id TEXT NOT NULL,
  status TEXT NOT NULL,
  last_sync_at TEXT,
  error_message TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (worktree_id, attachment_id),
  FOREIGN KEY (worktree_id) REFERENCES worktrees(id) ON DELETE CASCADE,
  FOREIGN KEY (attachment_id) REFERENCES workspace_attachments(id) ON DELETE CASCADE
);

INSERT INTO worktree_attachment_mounts (
  worktree_id,
  attachment_id,
  mount_abs_path,
  materialized_id,
  status,
  last_sync_at,
  error_message,
  created_at,
  updated_at
)
SELECT
  worktree_id,
  attachment_id,
  mount_abs_path,
  materialized_id,
  status,
  last_sync_at,
  error_message,
  created_at,
  updated_at
FROM (
  SELECT
    tr.worktree_id AS worktree_id,
    tam.attachment_id AS attachment_id,
    tam.mount_abs_path AS mount_abs_path,
    tam.materialized_id AS materialized_id,
    tam.status AS status,
    tam.last_sync_at AS last_sync_at,
    tam.error_message AS error_message,
    tam.created_at AS created_at,
    tam.updated_at AS updated_at,
    ROW_NUMBER() OVER (
      PARTITION BY tr.worktree_id, tam.attachment_id
      ORDER BY tam.updated_at DESC, tam.rowid DESC
    ) AS rn
  FROM track_attachment_mounts tam
  JOIN tracks tr ON tr.id = tam.track_id
)
WHERE rn = 1;

DROP TABLE IF EXISTS track_attachment_mounts;
DROP TABLE IF EXISTS track_workers;
DROP TABLE IF EXISTS tracks;
DROP TABLE IF EXISTS sessions;
DROP TABLE IF EXISTS messages;
DROP TABLE IF EXISTS artifacts;
DROP TABLE IF EXISTS tasks;

ALTER TABLE tasks_new RENAME TO tasks;
ALTER TABLE sessions_new RENAME TO sessions;
ALTER TABLE messages_new RENAME TO messages;
ALTER TABLE artifacts_new RENAME TO artifacts;

CREATE INDEX IF NOT EXISTS idx_tasks_workspace_id ON tasks(workspace_id);
CREATE INDEX IF NOT EXISTS idx_sessions_task_id ON sessions(task_id);
CREATE INDEX IF NOT EXISTS idx_messages_session_id ON messages(session_id);
CREATE INDEX IF NOT EXISTS idx_session_events_session_id ON session_events(session_id);
CREATE INDEX IF NOT EXISTS idx_artifacts_session_id ON artifacts(session_id);
CREATE INDEX IF NOT EXISTS idx_worktree_attachment_mounts_worktree_id ON worktree_attachment_mounts(worktree_id);

PRAGMA foreign_keys = ON;
