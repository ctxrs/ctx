impl Store {
    pub fn provider_file_checkpoint(
        &self,
        key: ProviderFileCheckpointKey<'_>,
    ) -> Result<Option<ProviderFileCheckpoint>> {
        self.conn
            .query_row(
                r#"
                SELECT import_revision, checkpoint_version, stable_file_identity, committed_byte_offset,
                       committed_complete_line_count, head_sha256, boundary_sha256, resume_state,
                       updated_at_ms
                FROM provider_file_checkpoints
                WHERE provider = ?1 AND source_format = ?2 AND source_root = ?3 AND source_path = ?4
                "#,
                params![
                    key.provider.as_str(),
                    key.source_format,
                    key.source_root,
                    key.source_path
                ],
                |row| {
                    Ok(ProviderFileCheckpoint {
                        provider: key.provider,
                        source_format: key.source_format.to_owned(),
                        source_root: key.source_root.to_owned(),
                        source_path: key.source_path.to_owned(),
                        import_revision: nonnegative_i64_to_u32(row.get(0)?)?,
                        checkpoint_version: nonnegative_i64_to_u32(row.get(1)?)?,
                        stable_file_identity: row.get(2)?,
                        committed_byte_offset: nonnegative_i64_to_u64(row.get(3)?)?,
                        committed_complete_line_count: nonnegative_i64_to_u64(row.get(4)?)?,
                        head_sha256: row.get(5)?,
                        boundary_sha256: row.get(6)?,
                        resume_state: row.get(7)?,
                        updated_at_ms: row.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    #[cfg(test)]
    pub(crate) fn upsert_provider_file_checkpoint(
        &self,
        outcome: ProviderFileImportOutcome<'_>,
        checkpoint: &ProviderFileCheckpoint,
    ) -> Result<()> {
        validate_checkpoint_for_outcome(outcome, checkpoint)?;
        self.with_atomic_provider_file_update(|| {
            self.record_matching_provider_file_outcome(
                outcome,
                ProviderFileCompletionKind::AppendDelta,
                true,
            )?;
            self.advance_provider_file_checkpoint(checkpoint)
        })
    }

    /// Completes an exact inventory observation without changing or deleting
    /// its prior append checkpoint. This is for a deferred partial tail that
    /// materialized no new complete records.
    #[cfg(test)]
    pub(crate) fn complete_provider_file_observation_retaining_checkpoint(
        &self,
        outcome: ProviderFileImportOutcome<'_>,
    ) -> Result<()> {
        validate_successful_outcome(outcome)?;
        self.with_atomic_provider_file_update(|| {
            self.record_matching_provider_file_outcome(
                outcome,
                ProviderFileCompletionKind::RetainCheckpoint,
                false,
            )
        })
    }

    /// Reads a session for the importer that owns `scope`. Ordinary hydration
    /// intentionally hides this material while replacement is unpublished.
    pub fn provider_file_publication_session(
        &self,
        scope: &ProviderFilePublicationScope,
        id: Uuid,
    ) -> Result<Session> {
        self.validate_provider_file_import_read_scope(scope)?;
        self.conn
            .query_row(
                session_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                session_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    /// Reads a capture source through the active importer reservation. Public
    /// source hydration hides the owner until replacement publication.
    pub fn provider_file_publication_capture_source(
        &self,
        scope: &ProviderFilePublicationScope,
        id: Uuid,
    ) -> Result<CaptureSource> {
        self.validate_provider_file_import_read_scope(scope)?;
        self.conn
            .query_row(
                "SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, source_format, source_root, source_identity, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json FROM capture_sources WHERE id = ?1",
                params![id.to_string()],
                capture_source_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    /// Resolves an importer-owned session while the public external-session
    /// hydration paths remain fenced.
    pub fn provider_file_publication_session_by_capture_source_and_external_session(
        &self,
        scope: &ProviderFilePublicationScope,
        source_id: Uuid,
        provider: CaptureProvider,
        external_session_id: &str,
    ) -> Result<Option<Session>> {
        self.validate_provider_file_import_read_scope(scope)?;
        self.conn
            .query_row(
                session_select_sql(
                    "WHERE capture_source_id = ?1 AND provider = ?2 AND external_session_id = ?3 ORDER BY created_at_ms, id LIMIT 1",
                )
                .as_str(),
                params![source_id.to_string(), provider.as_str(), external_session_id],
                session_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    /// Reads an event for the importer that owns `scope` without publishing
    /// the replacing owner to ordinary queries.
    pub fn provider_file_publication_event(
        &self,
        scope: &ProviderFilePublicationScope,
        id: Uuid,
    ) -> Result<Event> {
        self.validate_provider_file_import_read_scope(scope)?;
        self.conn
            .query_row(
                event_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                event_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    /// Lists a session's events for the importer that owns `scope`. This is a
    /// fenced identity/hydration path, not a user-visible list operation.
    pub fn provider_file_publication_events_for_session(
        &self,
        scope: &ProviderFilePublicationScope,
        session_id: Uuid,
    ) -> Result<Vec<Event>> {
        self.validate_provider_file_import_read_scope(scope)?;
        let mut stmt = self.conn.prepare(
            event_select_sql("WHERE session_id = ?1 ORDER BY seq, occurred_at_ms").as_str(),
        )?;
        let rows = stmt.query_map(params![session_id.to_string()], event_from_row)?;
        crate::connection::collect_rows(rows)
    }
}
