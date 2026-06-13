ALTER TABLE sessions ADD COLUMN archived_at TEXT;

DROP INDEX IF EXISTS idx_sessions_task_title_subagent_unique;

CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_task_title_subagent_unique
    ON sessions (task_id, title)
    WHERE relationship = 'sub_agent' AND archived_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_sessions_parent_relationship_active
    ON sessions (parent_session_id, relationship, created_at)
    WHERE relationship = 'sub_agent' AND archived_at IS NULL;
