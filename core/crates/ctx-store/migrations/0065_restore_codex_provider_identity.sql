DELETE FROM provider_session_bindings
WHERE provider_id = 'codex-crp'
  AND EXISTS (
    SELECT 1
    FROM provider_session_bindings AS existing
    WHERE existing.provider_id = 'codex'
      AND existing.provider_account_scope = provider_session_bindings.provider_account_scope
      AND existing.provider_session_ref = provider_session_bindings.provider_session_ref
  );

UPDATE provider_session_bindings
SET provider_id = 'codex',
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE provider_id = 'codex-crp';

UPDATE sessions
SET provider_id = 'codex',
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE provider_id = 'codex-crp';
