CREATE TABLE IF NOT EXISTS workspace_task_index (
    task_id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS workspace_task_index_workspace_id_idx
    ON workspace_task_index (workspace_id);

CREATE TABLE IF NOT EXISTS workspace_session_index (
    session_id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS workspace_session_index_workspace_id_idx
    ON workspace_session_index (workspace_id);

CREATE TABLE IF NOT EXISTS workspace_worktree_index (
    worktree_id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS workspace_worktree_index_workspace_id_idx
    ON workspace_worktree_index (workspace_id);
