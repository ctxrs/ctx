UPDATE sessions
SET title = 'subagent-' || lower(hex(randomblob(8)))
WHERE relationship = 'sub_agent'
  AND EXISTS (
    SELECT 1
    FROM sessions s2
    WHERE s2.task_id = sessions.task_id
      AND s2.title = sessions.title
      AND s2.relationship = 'sub_agent'
    GROUP BY s2.task_id, s2.title
    HAVING COUNT(*) > 1
  );

CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_task_title_subagent_unique
    ON sessions (task_id, title)
    WHERE relationship = 'sub_agent';
