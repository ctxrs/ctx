ALTER TABLE sandbox_bindings
  ADD COLUMN sandbox_instance_id TEXT NOT NULL DEFAULT '';

UPDATE sandbox_bindings
SET sandbox_instance_id = workspace_id
WHERE sandbox_instance_id = '';
