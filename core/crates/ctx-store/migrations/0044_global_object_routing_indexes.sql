CREATE TABLE IF NOT EXISTS workspace_artifact_index (
    artifact_id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS workspace_artifact_index_workspace_id_idx
    ON workspace_artifact_index (workspace_id);

CREATE TABLE IF NOT EXISTS workspace_message_index (
    message_id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS workspace_message_index_workspace_id_idx
    ON workspace_message_index (workspace_id);

CREATE TABLE IF NOT EXISTS workspace_subagent_invocation_index (
    invocation_id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS workspace_subagent_invocation_index_workspace_id_idx
    ON workspace_subagent_invocation_index (workspace_id);

CREATE TABLE IF NOT EXISTS workspace_merge_queue_entry_index (
    entry_id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS workspace_merge_queue_entry_index_workspace_id_idx
    ON workspace_merge_queue_entry_index (workspace_id);

CREATE INDEX IF NOT EXISTS workspace_merge_queue_entry_index_status_created_at_idx
    ON workspace_merge_queue_entry_index (status, created_at);
