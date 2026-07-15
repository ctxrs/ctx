INSERT INTO catalog_sessions (
    source_path, provider, source_format, source_root, external_session_id,
    agent_type, file_size_bytes, file_modified_at_ms, import_revision,
    cataloged_at_ms, indexed_at_ms, indexed_file_size_bytes,
    indexed_file_modified_at_ms, indexed_status, indexed_error,
    indexed_import_revision
) VALUES (
    '/fixture/codex/failed.jsonl', 'codex', 'codex_session_jsonl',
    '/fixture/codex', 'failed-codex', 'primary', 128, 1000, 1,
    1000, 1001, 128, 1000, 'failed', 'legacy retry', 1
);

INSERT INTO source_import_files (
    provider, source_format, source_root, source_path, file_size_bytes,
    file_modified_at_ms, import_revision, observed_at_ms, indexed_at_ms,
    indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status,
    indexed_error, indexed_import_revision, metadata_json
) VALUES (
    'pi', 'pi_session_jsonl', '/fixture/pi', '/fixture/pi/failed.jsonl',
    96, 2000, 1, 2000, 2001, 96, 2000, 'failed', 'legacy retry', 1, '{}'
), (
    'tabnine', 'tabnine_cli_recording_jsonl', '/fixture/tabnine',
    '/fixture/tabnine/revision.jsonl', 64, 3000, 2, 3000, 3001,
    64, 3000, 'indexed', NULL, 1, '{}'
), (
    'qwen_code', 'qwen_code_json', '/fixture/qwen',
    '/fixture/qwen/missing.json', 80, 4000, 1, 4000, 4001,
    80, 4000, 'indexed', NULL, 1, '{"inventory_unit":"logical_import_unit"}'
), (
    'hermes', 'hermes_state_sqlite', '/fixture/hermes',
    '/fixture/hermes/state.db', 72, 5000, 1, 5000, 5001,
    72, 5000, 'indexed', NULL, 1, '{"inventory_unit":"source_root"}'
);

INSERT INTO capture_sources (
    id, kind, provider, machine_id, raw_source_path, source_format,
    source_root, started_at_ms, fidelity
) VALUES (
    'qwen-sibling', 'provider_import', 'qwen_code', 'fixture-machine',
    '/fixture/qwen/sibling.json', 'qwen_code_json', '/fixture/qwen', 4001, 'imported'
), (
    'hermes-root', 'provider_import', 'hermes', 'fixture-machine',
    '/fixture/hermes/sibling.db', 'hermes_state_sqlite', '/fixture/hermes', 5001, 'imported'
);
