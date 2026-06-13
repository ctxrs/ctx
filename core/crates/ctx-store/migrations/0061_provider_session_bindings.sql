CREATE TABLE IF NOT EXISTS provider_session_bindings (
  provider_id TEXT NOT NULL,
  provider_account_scope TEXT NOT NULL DEFAULT 'default',
  provider_session_ref TEXT NOT NULL,
  session_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  worktree_id TEXT NOT NULL,
  source TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (provider_id, provider_account_scope, provider_session_ref)
);

CREATE INDEX IF NOT EXISTS idx_provider_session_bindings_session_id
  ON provider_session_bindings(session_id);

UPDATE sessions
SET provider_session_ref = NULL,
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE provider_session_ref IS NOT NULL
  AND trim(provider_session_ref) <> ''
  AND EXISTS (
    SELECT 1
    FROM sessions AS canonical
    WHERE canonical.id <> sessions.id
      AND canonical.provider_id = sessions.provider_id
      AND canonical.provider_session_ref = sessions.provider_session_ref
      AND (
        canonical.created_at < sessions.created_at
        OR (canonical.created_at = sessions.created_at AND canonical.id < sessions.id)
      )
  );

INSERT OR IGNORE INTO provider_session_bindings (
  provider_id,
  provider_account_scope,
  provider_session_ref,
  session_id,
  workspace_id,
  task_id,
  worktree_id,
  source,
  created_at,
  updated_at
)
SELECT
  provider_id,
  'default',
  provider_session_ref,
  id,
  workspace_id,
  task_id,
  worktree_id,
  'migration',
  created_at,
  updated_at
FROM sessions
WHERE provider_session_ref IS NOT NULL
  AND trim(provider_session_ref) <> '';
