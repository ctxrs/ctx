CREATE INDEX IF NOT EXISTS idx_tasks_workspace_active_created_id
    ON tasks (workspace_id, created_at DESC, id DESC)
    WHERE archived_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_sessions_task_created_id
    ON sessions (task_id, created_at ASC, id ASC);
