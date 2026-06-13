ALTER TABLE workspace_attachments ADD COLUMN status TEXT NOT NULL DEFAULT 'ready';
ALTER TABLE workspace_attachments ADD COLUMN last_sync_at TEXT;
ALTER TABLE workspace_attachments ADD COLUMN error_message TEXT;
