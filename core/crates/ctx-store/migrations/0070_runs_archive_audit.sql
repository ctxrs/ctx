CREATE TABLE IF NOT EXISTS runs (
  id TEXT PRIMARY KEY NOT NULL,
  session_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  worktree_id TEXT NOT NULL,
  parent_run_id TEXT,
  account_id TEXT,
  org_id TEXT,
  run_grant_id TEXT,
  status TEXT NOT NULL,
  archive_state TEXT NOT NULL DEFAULT 'active',
  archive_visibility TEXT NOT NULL DEFAULT 'local_only',
  retention_policy_key TEXT,
  retention_legal_hold_key TEXT,
  created_at TEXT NOT NULL,
  started_at TEXT,
  completed_at TEXT,
  archived_at TEXT,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE,
  FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
  FOREIGN KEY (worktree_id) REFERENCES worktrees(id) ON DELETE CASCADE,
  FOREIGN KEY (run_grant_id) REFERENCES run_grants(id) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_runs_session_created_at
  ON runs(session_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_runs_task_created_at
  ON runs(task_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_runs_workspace_archive_state_created_at
  ON runs(workspace_id, archive_state, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_runs_org_created_at
  ON runs(org_id, created_at DESC)
  WHERE org_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_runs_run_grant_id
  ON runs(run_grant_id)
  WHERE run_grant_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS run_audit_events (
  id TEXT PRIMARY KEY NOT NULL,
  workspace_id TEXT NOT NULL,
  task_id TEXT,
  session_id TEXT,
  run_id TEXT,
  account_id TEXT,
  org_id TEXT,
  actor_kind TEXT NOT NULL,
  actor_account_id TEXT,
  actor_org_id TEXT,
  actor_membership_role TEXT,
  event_kind TEXT NOT NULL,
  archive_visibility TEXT,
  retention_policy_key TEXT,
  retention_legal_hold_key TEXT,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
  FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE,
  FOREIGN KEY (run_id) REFERENCES runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_run_audit_events_run_created_at
  ON run_audit_events(run_id, created_at ASC);

CREATE INDEX IF NOT EXISTS idx_run_audit_events_workspace_created_at
  ON run_audit_events(workspace_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_run_audit_events_org_created_at
  ON run_audit_events(org_id, created_at DESC)
  WHERE org_id IS NOT NULL;

WITH run_candidates AS (
  SELECT session_id, run_id
  FROM session_turns
  WHERE run_id IS NOT NULL

  UNION

  SELECT session_id, run_id
  FROM session_events
  WHERE run_id IS NOT NULL

  UNION

  SELECT session_id, run_id
  FROM messages
  WHERE run_id IS NOT NULL

  UNION

  SELECT child_session_id AS session_id, run_id
  FROM subagent_invocation_children
  WHERE run_id IS NOT NULL
),
run_source AS (
  SELECT
    c.run_id,
    c.session_id,
    s.task_id,
    s.workspace_id,
    s.worktree_id,
    COALESCE(
      (SELECT MIN(t.started_at)
       FROM session_turns AS t
       WHERE t.session_id = c.session_id
         AND t.run_id = c.run_id),
      (SELECT MIN(e.created_at)
       FROM session_events AS e
       WHERE e.session_id = c.session_id
         AND e.run_id = c.run_id),
      (SELECT MIN(m.created_at)
       FROM messages AS m
       WHERE m.session_id = c.session_id
         AND m.run_id = c.run_id),
      s.created_at
    ) AS created_at,
    (SELECT MIN(t.started_at)
     FROM session_turns AS t
     WHERE t.session_id = c.session_id
       AND t.run_id = c.run_id) AS started_at,
    COALESCE(
      (SELECT MAX(t.updated_at)
       FROM session_turns AS t
       WHERE t.session_id = c.session_id
         AND t.run_id = c.run_id),
      (SELECT MAX(e.created_at)
       FROM session_events AS e
       WHERE e.session_id = c.session_id
         AND e.run_id = c.run_id),
      (SELECT MAX(m.created_at)
       FROM messages AS m
       WHERE m.session_id = c.session_id
         AND m.run_id = c.run_id),
      s.updated_at
    ) AS updated_at,
    (SELECT t.status
     FROM session_turns AS t
     WHERE t.session_id = c.session_id
       AND t.run_id = c.run_id
     ORDER BY t.updated_at DESC, t.turn_id DESC
     LIMIT 1) AS latest_turn_status,
    (SELECT e.event_type
     FROM session_events AS e
     WHERE e.session_id = c.session_id
       AND e.run_id = c.run_id
     ORDER BY e.seq DESC
     LIMIT 1) AS latest_event_type
  FROM run_candidates AS c
  JOIN sessions AS s
    ON s.id = c.session_id
),
run_backfill AS (
  SELECT
    run_id,
    session_id,
    task_id,
    workspace_id,
    worktree_id,
    CASE
      WHEN latest_turn_status = 'queued' THEN 'queued'
      WHEN latest_turn_status IN ('starting', 'running') THEN 'running'
      WHEN latest_turn_status = 'failed' THEN 'failed'
      WHEN latest_turn_status = 'interrupted' THEN 'cancelled'
      WHEN latest_turn_status = 'completed' THEN 'completed'
      WHEN latest_event_type IN ('input_queued', 'turn_queued') THEN 'queued'
      WHEN latest_event_type = 'turn_started' THEN 'running'
      WHEN latest_event_type = 'error' THEN 'failed'
      WHEN latest_event_type IN ('interrupt_requested', 'turn_interrupted') THEN 'cancelled'
      WHEN latest_event_type IN ('done', 'turn_finished', 'assistant_complete') THEN 'completed'
      ELSE 'running'
    END AS status,
    created_at,
    started_at,
    CASE
      WHEN latest_turn_status IN ('completed', 'failed', 'interrupted') THEN updated_at
      WHEN latest_event_type IN ('done', 'error', 'turn_finished', 'turn_interrupted') THEN updated_at
      ELSE NULL
    END AS completed_at,
    updated_at
  FROM run_source
)
INSERT OR IGNORE INTO runs (
  id,
  session_id,
  task_id,
  workspace_id,
  worktree_id,
  status,
  archive_state,
  archive_visibility,
  created_at,
  started_at,
  completed_at,
  updated_at
)
SELECT
  run_id,
  session_id,
  task_id,
  workspace_id,
  worktree_id,
  status,
  'active',
  'local_only',
  created_at,
  started_at,
  completed_at,
  updated_at
FROM run_backfill;
