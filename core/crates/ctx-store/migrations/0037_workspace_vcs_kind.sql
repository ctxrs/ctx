ALTER TABLE workspaces ADD COLUMN vcs_kind TEXT;

UPDATE workspaces SET vcs_kind = 'git' WHERE vcs_kind IS NULL;
