ALTER TABLE tasks ADD COLUMN last_activity_at TEXT;
ALTER TABLE tasks ADD COLUMN last_assistant_message_at TEXT;

CREATE TABLE IF NOT EXISTS session_snapshot_summaries (
  session_id TEXT PRIMARY KEY NOT NULL,
  last_message_at TEXT,
  last_message_preview TEXT,
  last_event_seq INTEGER,
  last_turn_status TEXT,
  last_turn_seq INTEGER,
  running_turn_count INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_session_snapshot_summaries_session_id
  ON session_snapshot_summaries(session_id);

CREATE TABLE IF NOT EXISTS session_git_status_snapshots (
  session_id TEXT PRIMARY KEY NOT NULL,
  worktree_id TEXT NOT NULL,
  summary_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_session_git_status_snapshots_worktree_id
  ON session_git_status_snapshots(worktree_id);

UPDATE tasks
SET last_activity_at = (
    SELECT MAX(m.created_at) FROM messages m WHERE m.task_id = tasks.id
  ),
  last_assistant_message_at = (
    SELECT MAX(m.created_at) FROM messages m WHERE m.task_id = tasks.id AND m.role = 'assistant'
  )
WHERE last_activity_at IS NULL OR last_assistant_message_at IS NULL;

WITH last_assistant_messages AS (
    SELECT session_id, content, created_at,
           ROW_NUMBER() OVER (
               PARTITION BY session_id
               ORDER BY created_at DESC, id DESC
           ) AS rn
    FROM messages
    WHERE role = 'assistant'
),
last_events AS (
    SELECT session_id, MAX(seq) AS last_event_seq
    FROM session_events
    GROUP BY session_id
),
last_turns AS (
    SELECT session_id, status, start_seq,
           ROW_NUMBER() OVER (
               PARTITION BY session_id
               ORDER BY COALESCE(start_seq, -1) DESC, started_at DESC, turn_id DESC
           ) AS rn
    FROM session_turns
),
running_turns AS (
    SELECT session_id, COUNT(*) AS running_count
    FROM session_turns
    WHERE status = 'running'
    GROUP BY session_id
)
INSERT OR IGNORE INTO session_snapshot_summaries (
  session_id,
  last_message_at,
  last_message_preview,
  last_event_seq,
  last_turn_status,
  last_turn_seq,
  running_turn_count,
  created_at,
  updated_at
)
SELECT
  s.id,
  lm.created_at,
  lm.content,
  le.last_event_seq,
  lt.status,
  lt.start_seq,
  COALESCE(rt.running_count, 0),
  CURRENT_TIMESTAMP,
  CURRENT_TIMESTAMP
FROM sessions s
LEFT JOIN last_assistant_messages lm
  ON lm.session_id = s.id AND lm.rn = 1
LEFT JOIN last_events le
  ON le.session_id = s.id
LEFT JOIN last_turns lt
  ON lt.session_id = s.id AND lt.rn = 1
LEFT JOIN running_turns rt
  ON rt.session_id = s.id;
