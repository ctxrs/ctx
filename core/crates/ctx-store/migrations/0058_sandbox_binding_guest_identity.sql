ALTER TABLE sandbox_bindings
  ADD COLUMN guest_platform TEXT NOT NULL DEFAULT 'linux';

ALTER TABLE sandbox_bindings
  ADD COLUMN isolation_kind TEXT NOT NULL DEFAULT 'container';

ALTER TABLE sandbox_bindings
  ADD COLUMN guest_runtime TEXT NOT NULL DEFAULT 'ubuntu';
