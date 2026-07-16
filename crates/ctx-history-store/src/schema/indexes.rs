pub(crate) const INDEXES_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS idx_capture_sources_external_session_id ON capture_sources(provider, external_session_id);
CREATE INDEX IF NOT EXISTS idx_capture_sources_provider_source_identity ON capture_sources(provider, source_format, source_identity);
CREATE INDEX IF NOT EXISTS idx_capture_sources_provider_material_owner ON capture_sources(provider, source_format, source_root, raw_source_path, external_session_id, id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_provider_file_publications_owner ON provider_file_publications(owner_id);
CREATE INDEX IF NOT EXISTS idx_provider_file_publications_fence ON provider_file_publications(mutation_started, provider, material_source_format, material_source_root, source_path);

CREATE INDEX IF NOT EXISTS idx_catalog_sessions_provider_external_session_id ON catalog_sessions(provider, external_session_id);
CREATE INDEX IF NOT EXISTS idx_catalog_sessions_provider_source_root_stale ON catalog_sessions(provider, source_root, is_stale);
CREATE INDEX IF NOT EXISTS idx_catalog_sessions_provider_source_root_import ON catalog_sessions(provider, source_root, is_stale, indexed_status);
CREATE INDEX IF NOT EXISTS idx_catalog_sessions_started_at ON catalog_sessions(session_started_at_ms);
CREATE INDEX IF NOT EXISTS idx_catalog_sessions_cwd ON catalog_sessions(cwd);
CREATE INDEX IF NOT EXISTS idx_source_import_files_provider_source_root_import ON source_import_files(provider, source_root, is_stale, indexed_status);
CREATE INDEX IF NOT EXISTS idx_source_import_files_provider_source_root_stale ON source_import_files(provider, source_root, is_stale);
CREATE INDEX IF NOT EXISTS idx_sessions_provider_external_session_id ON sessions(provider, external_session_id);

CREATE INDEX IF NOT EXISTS idx_history_records_primary_vcs_workspace_id ON history_records(primary_vcs_workspace_id);
CREATE INDEX IF NOT EXISTS idx_history_records_source_id ON history_records(source_id);
CREATE INDEX IF NOT EXISTS idx_history_records_last_activity_at_ms ON history_records(last_activity_at_ms);
CREATE INDEX IF NOT EXISTS idx_history_records_created_at ON history_records(created_at DESC);

CREATE INDEX IF NOT EXISTS idx_sessions_history_record_id ON sessions(history_record_id);
CREATE INDEX IF NOT EXISTS idx_sessions_parent_session_id ON sessions(parent_session_id);
CREATE INDEX IF NOT EXISTS idx_sessions_root_session_id ON sessions(root_session_id);
CREATE INDEX IF NOT EXISTS idx_sessions_capture_source_id ON sessions(capture_source_id);
CREATE INDEX IF NOT EXISTS idx_sessions_transcript_blob_id ON sessions(transcript_blob_id);
CREATE INDEX IF NOT EXISTS idx_session_aliases_session_id ON session_aliases(session_id);

CREATE INDEX IF NOT EXISTS idx_session_edges_from_session_id ON session_edges(from_session_id);
CREATE INDEX IF NOT EXISTS idx_session_edges_to_session_id ON session_edges(to_session_id);
CREATE INDEX IF NOT EXISTS idx_session_edges_source_id ON session_edges(source_id);

CREATE INDEX IF NOT EXISTS idx_runs_history_record_started_at_ms ON runs(history_record_id, started_at_ms);
CREATE INDEX IF NOT EXISTS idx_runs_history_record_id ON runs(history_record_id);
CREATE INDEX IF NOT EXISTS idx_runs_session_id ON runs(session_id);
CREATE INDEX IF NOT EXISTS idx_runs_input_blob_id ON runs(input_blob_id);
CREATE INDEX IF NOT EXISTS idx_runs_output_blob_id ON runs(output_blob_id);
CREATE INDEX IF NOT EXISTS idx_runs_source_id ON runs(source_id);

CREATE INDEX IF NOT EXISTS idx_events_history_record_occurred_at_ms ON events(history_record_id, occurred_at_ms);
CREATE INDEX IF NOT EXISTS idx_events_session_occurred_at_ms ON events(session_id, occurred_at_ms);
CREATE INDEX IF NOT EXISTS idx_events_history_record_id ON events(history_record_id);
CREATE INDEX IF NOT EXISTS idx_events_session_id ON events(session_id);
CREATE INDEX IF NOT EXISTS idx_events_run_id ON events(run_id);
CREATE INDEX IF NOT EXISTS idx_events_role_occurred_seq ON events(event_type, role, occurred_at_ms DESC, seq DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_events_run_role_occurred_seq ON events(run_id, event_type, role, occurred_at_ms DESC, seq DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_events_session_run_role_occurred_seq ON events(session_id, run_id, event_type, role, occurred_at_ms DESC, seq DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_events_capture_source_id ON events(capture_source_id);
CREATE INDEX IF NOT EXISTS idx_events_payload_blob_id ON events(payload_blob_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_events_dedupe_key ON events(dedupe_key) WHERE dedupe_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_event_aliases_event_id ON event_aliases(event_id);

CREATE INDEX IF NOT EXISTS idx_vcs_workspaces_kind_repo_fingerprint ON vcs_workspaces(kind, repo_fingerprint);
CREATE INDEX IF NOT EXISTS idx_vcs_workspaces_source_id ON vcs_workspaces(source_id);

CREATE INDEX IF NOT EXISTS idx_vcs_changes_vcs_workspace_id ON vcs_changes(vcs_workspace_id);
CREATE INDEX IF NOT EXISTS idx_vcs_changes_source_id ON vcs_changes(source_id);

CREATE INDEX IF NOT EXISTS idx_history_record_links_history_record_id ON history_record_links(history_record_id);
CREATE INDEX IF NOT EXISTS idx_history_record_links_source_id ON history_record_links(source_id);

CREATE INDEX IF NOT EXISTS idx_artifacts_source_id ON artifacts(source_id);

CREATE INDEX IF NOT EXISTS idx_summaries_history_record_id ON summaries(history_record_id);
CREATE INDEX IF NOT EXISTS idx_summaries_session_id ON summaries(session_id);
CREATE INDEX IF NOT EXISTS idx_summaries_source_id ON summaries(source_id);

CREATE INDEX IF NOT EXISTS idx_files_touched_history_record_id ON files_touched(history_record_id);
CREATE INDEX IF NOT EXISTS idx_files_touched_run_id ON files_touched(run_id);
CREATE INDEX IF NOT EXISTS idx_files_touched_event_id ON files_touched(event_id);
CREATE INDEX IF NOT EXISTS idx_files_touched_vcs_workspace_id ON files_touched(vcs_workspace_id);
CREATE INDEX IF NOT EXISTS idx_files_touched_source_id ON files_touched(source_id);
CREATE INDEX IF NOT EXISTS idx_files_touched_path ON files_touched(path);
CREATE INDEX IF NOT EXISTS idx_files_touched_old_path ON files_touched(old_path);

CREATE INDEX IF NOT EXISTS idx_history_record_tags_tag_id ON history_record_tags(tag_id);
CREATE INDEX IF NOT EXISTS idx_history_record_tags_source_id ON history_record_tags(source_id);

CREATE INDEX IF NOT EXISTS idx_record_edges_from_record_id ON record_edges(from_record_id);
CREATE INDEX IF NOT EXISTS idx_record_edges_to_record_id ON record_edges(to_record_id);
CREATE INDEX IF NOT EXISTS idx_record_edges_source_id ON record_edges(source_id);

CREATE INDEX IF NOT EXISTS idx_sync_outbox_sync_state_updated_at_ms ON sync_outbox(sync_state, updated_at_ms);
CREATE INDEX IF NOT EXISTS idx_local_workspaces_device_id ON local_workspaces(device_id);
CREATE INDEX IF NOT EXISTS idx_local_workspaces_vcs_workspace_id ON local_workspaces(vcs_workspace_id);
CREATE INDEX IF NOT EXISTS idx_audit_log_source_id ON audit_log(source_id);

CREATE INDEX IF NOT EXISTS idx_reconcile_history_record_links_source_id ON history_record_links(source_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_files_touched_source_id ON files_touched(source_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_files_touched_event_id ON files_touched(event_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_files_touched_run_id ON files_touched(run_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_session_edges_source_id ON session_edges(source_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_session_edges_from_session_id ON session_edges(from_session_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_session_edges_to_session_id ON session_edges(to_session_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_summaries_source_id ON summaries(source_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_events_capture_source_id ON events(capture_source_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_events_session_id ON events(session_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_events_run_id ON events(run_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_runs_source_id ON runs(source_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_runs_session_id ON runs(session_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_sessions_capture_source_id ON sessions(capture_source_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_vcs_changes_source_id ON vcs_changes(source_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_artifacts_source_id ON artifacts(source_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_record_edges_source_id ON record_edges(source_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_history_records_source_id ON history_records(source_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_vcs_workspaces_source_id ON vcs_workspaces(source_id, id);
CREATE INDEX IF NOT EXISTS idx_reconcile_audit_log_source_id ON audit_log(source_id, id);
"#;

pub(crate) const PENDING_WORK_INDEXES_SQL: &str = r#"
-- Fresh stores have no legacy rows to classify. Real upgrades retain v51's
-- incomplete ledger until bounded maintenance advances it.
UPDATE import_pending_reason_repairs
SET completed = 1
WHERE completed = 0
  AND cursor_provider IS NULL
  AND cursor_source_root IS NULL
  AND cursor_source_path IS NULL
  AND (
    (inventory_family = 'catalog_sessions'
      AND NOT EXISTS (SELECT 1 FROM catalog_sessions))
    OR
    (inventory_family = 'source_import_files'
      AND NOT EXISTS (SELECT 1 FROM source_import_files))
  );

DROP INDEX IF EXISTS idx_catalog_sessions_pending_reason;
DROP INDEX IF EXISTS idx_source_import_files_pending_reason;
DROP INDEX IF EXISTS idx_catalog_sessions_pending_fresh;
DROP INDEX IF EXISTS idx_catalog_sessions_pending_recovery;
DROP INDEX IF EXISTS idx_source_import_files_pending_fresh;
DROP INDEX IF EXISTS idx_source_import_files_pending_recovery;

CREATE INDEX IF NOT EXISTS idx_catalog_sessions_pending_fresh_attempt
ON catalog_sessions(provider, source_root, indexed_at_ms, source_path)
WHERE is_stale = 0
  AND pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append');
CREATE INDEX IF NOT EXISTS idx_catalog_sessions_pending_recovery_attempt
ON catalog_sessions(provider, source_root, indexed_at_ms, source_path)
WHERE is_stale = 0
  AND pending_reason IN (
    'recovery_retry', 'recovery_replacement', 'parser_revision',
    'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'
  );

CREATE INDEX IF NOT EXISTS idx_source_import_files_pending_fresh_attempt
ON source_import_files(provider, source_root, indexed_at_ms, source_path)
WHERE is_stale = 0
  AND pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append');
CREATE INDEX IF NOT EXISTS idx_source_import_files_pending_recovery_attempt
ON source_import_files(provider, source_root, indexed_at_ms, source_path)
WHERE is_stale = 0
  AND pending_reason IN (
    'recovery_retry', 'recovery_replacement', 'parser_revision',
    'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'
  );
"#;
