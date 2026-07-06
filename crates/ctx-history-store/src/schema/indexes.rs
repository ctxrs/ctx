pub(crate) const INDEXES_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS idx_capture_sources_external_session_id ON capture_sources(provider, external_session_id);
CREATE INDEX IF NOT EXISTS idx_capture_sources_provider_source_identity ON capture_sources(provider, source_format, source_identity);

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

CREATE INDEX IF NOT EXISTS idx_session_edges_from_session_id ON session_edges(from_session_id);
CREATE INDEX IF NOT EXISTS idx_session_edges_to_session_id ON session_edges(to_session_id);
CREATE INDEX IF NOT EXISTS idx_session_edges_source_id ON session_edges(source_id);

CREATE INDEX IF NOT EXISTS idx_runs_history_record_started_at_ms ON runs(history_record_id, started_at_ms);
CREATE INDEX IF NOT EXISTS idx_runs_history_record_id ON runs(history_record_id);
CREATE INDEX IF NOT EXISTS idx_runs_session_id ON runs(session_id);
CREATE INDEX IF NOT EXISTS idx_runs_input_blob_id ON runs(input_blob_id);
CREATE INDEX IF NOT EXISTS idx_runs_output_blob_id ON runs(output_blob_id);
CREATE INDEX IF NOT EXISTS idx_runs_source_id ON runs(source_id);

CREATE INDEX IF NOT EXISTS idx_events_seq ON events(seq);
CREATE INDEX IF NOT EXISTS idx_events_history_record_occurred_at_ms ON events(history_record_id, occurred_at_ms);
CREATE INDEX IF NOT EXISTS idx_events_session_occurred_at_ms ON events(session_id, occurred_at_ms);
CREATE INDEX IF NOT EXISTS idx_events_history_record_id ON events(history_record_id);
CREATE INDEX IF NOT EXISTS idx_events_session_id ON events(session_id);
CREATE INDEX IF NOT EXISTS idx_events_run_id ON events(run_id);
CREATE INDEX IF NOT EXISTS idx_events_capture_source_id ON events(capture_source_id);
CREATE INDEX IF NOT EXISTS idx_events_payload_blob_id ON events(payload_blob_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_events_dedupe_key ON events(dedupe_key) WHERE dedupe_key IS NOT NULL;

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
"#;

// `safe_preview_text` is legacy schema naming. It stores local searchable
// preview text and must not be interpreted as share-safe redaction.
