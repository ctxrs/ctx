ALTER TABLE worktrees ADD COLUMN vcs_kind TEXT;
ALTER TABLE worktrees ADD COLUMN base_revision TEXT;
ALTER TABLE worktrees ADD COLUMN vcs_ref TEXT;

UPDATE worktrees SET vcs_kind = 'git' WHERE vcs_kind IS NULL;
UPDATE worktrees SET base_revision = base_commit_sha WHERE base_revision IS NULL;
UPDATE worktrees SET vcs_ref = git_branch WHERE vcs_ref IS NULL;
