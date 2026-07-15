impl Store {
    fn publish_provider_file_publication_marker(
        &self,
        scope: &mut ProviderFilePublicationScope,
        created_at_ms: i64,
    ) -> Result<()> {
        let prior = self
            .conn
            .query_row(
                r#"
                SELECT replacement_id, staging_id, publication_kind, mutation_started,
                       inventory_family = ?2
                       AND inventory_source_format = ?3
                       AND inventory_source_root = ?4
                       AND source_path = ?5
                       AND material_source_format = ?6
                       AND material_source_root = ?7
                       AND file_size_bytes = ?8
                       AND file_modified_at_ms = ?9
                       AND import_revision = ?10
                       AND metadata_json IS ?11,
                       tracks_prior_material, staging_initialized
                FROM provider_file_publications
                WHERE owner_id = ?1
                "#,
                params![
                    &scope.owner_id,
                    scope.inventory_family,
                    &scope.inventory_source_format,
                    &scope.inventory_source_root,
                    &scope.source_path,
                    &scope.material_source_format,
                    &scope.material_source_root,
                    capped_i64(scope.file_size_bytes),
                    scope.file_modified_at_ms,
                    i64::from(scope.import_revision),
                    &scope.metadata_json,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, bool>(3)?,
                        row.get::<_, bool>(4)?,
                        row.get::<_, bool>(5)?,
                        row.get::<_, bool>(6)?,
                    ))
                },
            )
            .optional()?;
        let changed = if let Some((
            publication_id,
            staging_id,
            prior_kind,
            mutation_started,
            same_observation,
            tracks_prior_material,
            staging_initialized,
        )) = prior
        {
            let prior_kind = parse_provider_file_publication_kind(&prior_kind)?;
            scope.kind = match (prior_kind, mutation_started) {
                (_, true) => ProviderFilePublicationKind::Replacement,
                (_, false) => scope.kind,
            };
            if mutation_started {
                scope.tracks_prior_material = tracks_prior_material;
            }
            scope.scope_id = Uuid::parse_str(&publication_id)?;
            scope.staging_id = staging_id;
            let reuse_staging_state = same_observation && prior_kind == scope.kind;
            let reuse_staging_state = reuse_staging_state && staging_initialized;
            scope.reuse_staging_state = reuse_staging_state;
            self.conn.execute(
                r#"
                UPDATE provider_file_publications
                SET publication_kind = ?2, inventory_family = ?4,
                    inventory_source_format = ?5, inventory_source_root = ?6,
                    source_path = ?7, material_source_format = ?8,
                    material_source_root = ?9,
                    inventory_generation = ?10, file_size_bytes = ?11,
                    file_modified_at_ms = ?12, import_revision = ?13,
                    metadata_json = ?14, mutation_started = ?15,
                    tracks_prior_material = ?16,
                    staging_initialized = CASE
                        WHEN ?17 THEN staging_initialized ELSE 0
                    END,
                    preparation_complete = CASE
                        WHEN ?17 THEN preparation_complete
                        WHEN ?2 = 'incremental' THEN 1 ELSE 0
                    END,
                    preparation_cursor = CASE WHEN ?17 THEN preparation_cursor ELSE NULL END,
                    cleanup_phase = CASE WHEN ?17 THEN cleanup_phase ELSE 0 END,
                    cleanup_source_cursor = CASE WHEN ?17 THEN cleanup_source_cursor ELSE NULL END,
                    cleanup_entity_cursor = CASE WHEN ?17 THEN cleanup_entity_cursor ELSE NULL END,
                    removed_artifacts = CASE WHEN ?17 THEN removed_artifacts ELSE 0 END,
                    removed_summaries = CASE WHEN ?17 THEN removed_summaries ELSE 0 END,
                    removed_history_record_links = CASE WHEN ?17 THEN removed_history_record_links ELSE 0 END,
                    removed_history_records = CASE WHEN ?17 THEN removed_history_records ELSE 0 END,
                    removed_history_record_tags = CASE WHEN ?17 THEN removed_history_record_tags ELSE 0 END,
                    removed_record_edges = CASE WHEN ?17 THEN removed_record_edges ELSE 0 END,
                    removed_audit_log_entries = CASE WHEN ?17 THEN removed_audit_log_entries ELSE 0 END,
                    removed_vcs_workspaces = CASE WHEN ?17 THEN removed_vcs_workspaces ELSE 0 END,
                    removed_vcs_changes = CASE WHEN ?17 THEN removed_vcs_changes ELSE 0 END,
                    removed_events = CASE WHEN ?17 THEN removed_events ELSE 0 END,
                    removed_runs = CASE WHEN ?17 THEN removed_runs ELSE 0 END,
                    removed_files_touched = CASE WHEN ?17 THEN removed_files_touched ELSE 0 END,
                    removed_session_edges = CASE WHEN ?17 THEN removed_session_edges ELSE 0 END,
                    tombstoned_sessions = CASE WHEN ?17 THEN tombstoned_sessions ELSE 0 END,
                    completion_payload_json = CASE
                        WHEN ?17 THEN completion_payload_json ELSE NULL
                    END,
                    started_at_ms = ?18,
                    updated_at_ms = MAX(updated_at_ms + 1, ?18)
                WHERE owner_id = ?3 AND replacement_id = ?1
                "#,
                params![
                    scope.scope_id.to_string(),
                    scope.kind.as_str(),
                    &scope.owner_id,
                    scope.inventory_family,
                    &scope.inventory_source_format,
                    &scope.inventory_source_root,
                    &scope.source_path,
                    &scope.material_source_format,
                    &scope.material_source_root,
                    capped_i64(scope.inventory_generation),
                    capped_i64(scope.file_size_bytes),
                    scope.file_modified_at_ms,
                    i64::from(scope.import_revision),
                    &scope.metadata_json,
                    mutation_started,
                    scope.tracks_prior_material,
                    reuse_staging_state,
                    created_at_ms,
                ],
            )?
        } else {
            self.conn.execute(
                r#"
                INSERT INTO provider_file_publications
                    (replacement_id, owner_id, publication_kind, staging_id, provider,
                     inventory_family, inventory_source_format, inventory_source_root,
                     source_path, material_source_format, material_source_root,
                     inventory_generation, file_size_bytes, file_modified_at_ms,
                     import_revision, metadata_json, mutation_started, tracks_prior_material,
                     staging_initialized,
                     preparation_complete, preparation_cursor, cleanup_phase,
                     cleanup_source_cursor, cleanup_entity_cursor,
                     removed_artifacts, removed_summaries, removed_history_record_links,
                     removed_history_records, removed_history_record_tags, removed_record_edges,
                     removed_audit_log_entries,
                     removed_vcs_workspaces, removed_vcs_changes, removed_events, removed_runs,
                     removed_files_touched, removed_session_edges, tombstoned_sessions,
                     started_at_ms, updated_at_ms, completion_payload_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                        ?13, ?14, ?15, ?16, 0, ?17, 0,
                        CASE WHEN ?3 = 'incremental' THEN 1 ELSE 0 END, NULL,
                        0, NULL, NULL, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                        ?18, ?18, NULL)
                "#,
                params![
                    scope.scope_id.to_string(),
                    &scope.owner_id,
                    scope.kind.as_str(),
                    &scope.staging_id,
                    scope.provider.as_str(),
                    scope.inventory_family,
                    &scope.inventory_source_format,
                    &scope.inventory_source_root,
                    &scope.source_path,
                    &scope.material_source_format,
                    &scope.material_source_root,
                    capped_i64(scope.inventory_generation),
                    capped_i64(scope.file_size_bytes),
                    scope.file_modified_at_ms,
                    i64::from(scope.import_revision),
                    &scope.metadata_json,
                    scope.tracks_prior_material,
                    created_at_ms,
                ],
            )?
        };
        if changed != 1 {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        if !scope.tracks_prior_material {
            let changed = self.conn.execute(
                "UPDATE provider_file_publications SET preparation_complete = 1 WHERE replacement_id = ?1",
                params![scope.scope_id.to_string()],
            )?;
            if changed != 1 {
                return Err(StoreError::InvalidProviderFilePublicationScope);
            }
        }
        Ok(())
    }

    fn load_replacement_marker(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<ReplacementMarker> {
        self.conn
            .query_row(
                r#"
                SELECT publication_kind, mutation_started, preparation_complete, preparation_cursor,
                       cleanup_phase, cleanup_source_cursor, cleanup_entity_cursor,
                       completion_payload_json,
                       removed_artifacts, removed_summaries, removed_history_record_links,
                       removed_history_records, removed_history_record_tags, removed_record_edges,
                       removed_audit_log_entries, removed_vcs_workspaces, removed_vcs_changes,
                       removed_events, removed_runs, removed_files_touched, removed_session_edges,
                       tombstoned_sessions
                FROM provider_file_publications
                WHERE replacement_id = ?1 AND provider = ?2
                  AND inventory_family = ?3 AND inventory_source_format = ?4
                  AND inventory_source_root = ?5 AND source_path = ?6
                  AND material_source_format = ?7 AND material_source_root = ?8
                  AND inventory_generation = ?9 AND file_size_bytes = ?10
                  AND file_modified_at_ms = ?11 AND import_revision = ?12
                  AND metadata_json IS ?13
                "#,
                params![
                    scope.scope_id.to_string(),
                    scope.provider.as_str(),
                    scope.inventory_family,
                    &scope.inventory_source_format,
                    &scope.inventory_source_root,
                    &scope.source_path,
                    &scope.material_source_format,
                    &scope.material_source_root,
                    capped_i64(scope.inventory_generation),
                    capped_i64(scope.file_size_bytes),
                    scope.file_modified_at_ms,
                    i64::from(scope.import_revision),
                    &scope.metadata_json,
                ],
                |row| {
                    Ok(ReplacementMarker {
                        publication_kind: parse_provider_file_publication_kind_sql(
                            &row.get::<_, String>(0)?,
                        )?,
                        mutation_started: row.get(1)?,
                        preparation_complete: row.get(2)?,
                        preparation_cursor: row.get(3)?,
                        cleanup_phase: row.get(4)?,
                        source_cursor: row.get(5)?,
                        entity_cursor: row.get(6)?,
                        completion_payload_json: row.get(7)?,
                        counts: ProviderFileReconciliationCounts {
                            artifacts: nonnegative_i64_to_usize(row.get(8)?)?,
                            summaries: nonnegative_i64_to_usize(row.get(9)?)?,
                            history_record_links: nonnegative_i64_to_usize(row.get(10)?)?,
                            history_records: nonnegative_i64_to_usize(row.get(11)?)?,
                            history_record_tags: nonnegative_i64_to_usize(row.get(12)?)?,
                            record_edges: nonnegative_i64_to_usize(row.get(13)?)?,
                            audit_log_entries: nonnegative_i64_to_usize(row.get(14)?)?,
                            vcs_workspaces: nonnegative_i64_to_usize(row.get(15)?)?,
                            vcs_changes: nonnegative_i64_to_usize(row.get(16)?)?,
                            events: nonnegative_i64_to_usize(row.get(17)?)?,
                            runs: nonnegative_i64_to_usize(row.get(18)?)?,
                            files_touched: nonnegative_i64_to_usize(row.get(19)?)?,
                            session_edges: nonnegative_i64_to_usize(row.get(20)?)?,
                            sessions_tombstoned: nonnegative_i64_to_usize(row.get(21)?)?,
                        },
                    })
                },
            )
            .optional()?
            .ok_or(StoreError::InvalidProviderFilePublicationScope)
    }

    fn update_replacement_marker(
        &self,
        scope: &ProviderFilePublicationScope,
        marker: &ReplacementMarker,
    ) -> Result<()> {
        let changed = self.conn.execute(
            r#"
            UPDATE provider_file_publications
            SET mutation_started = ?2, preparation_complete = ?3, preparation_cursor = ?4,
                cleanup_phase = ?5, cleanup_source_cursor = ?6, cleanup_entity_cursor = ?7,
                removed_artifacts = ?8, removed_summaries = ?9,
                removed_history_record_links = ?10, removed_history_records = ?11,
                removed_history_record_tags = ?12, removed_record_edges = ?13,
                removed_audit_log_entries = ?14, removed_vcs_workspaces = ?15,
                removed_vcs_changes = ?16, removed_events = ?17, removed_runs = ?18,
                removed_files_touched = ?19, removed_session_edges = ?20,
                tombstoned_sessions = ?21, updated_at_ms = MAX(updated_at_ms, ?22)
            WHERE replacement_id = ?1 AND publication_kind = ?23
            "#,
            params![
                scope.scope_id.to_string(),
                marker.mutation_started,
                marker.preparation_complete,
                &marker.preparation_cursor,
                marker.cleanup_phase,
                &marker.source_cursor,
                &marker.entity_cursor,
                capped_i64(marker.counts.artifacts as u64),
                capped_i64(marker.counts.summaries as u64),
                capped_i64(marker.counts.history_record_links as u64),
                capped_i64(marker.counts.history_records as u64),
                capped_i64(marker.counts.history_record_tags as u64),
                capped_i64(marker.counts.record_edges as u64),
                capped_i64(marker.counts.audit_log_entries as u64),
                capped_i64(marker.counts.vcs_workspaces as u64),
                capped_i64(marker.counts.vcs_changes as u64),
                capped_i64(marker.counts.events as u64),
                capped_i64(marker.counts.runs as u64),
                capped_i64(marker.counts.files_touched as u64),
                capped_i64(marker.counts.session_edges as u64),
                capped_i64(marker.counts.sessions_tombstoned as u64),
                scope.file_modified_at_ms,
                scope.kind.as_str(),
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        Ok(())
    }

    fn acquire_provider_file_owner_lock(
        &self,
        provider: CaptureProvider,
        material_source_format: &str,
        material_source_root: &str,
        source_path: &str,
    ) -> Result<(File, PathBuf)> {
        let lock_dir = self.store_identity.private_root();
        create_or_validate_private_lock_dir(&lock_dir)?;
        let owner_id = provider_file_owner_lock_name(
            self.store_identity.digest(),
            provider,
            material_source_format,
            material_source_root,
            source_path,
        );
        let lock_path = lock_dir.join(format!("{owner_id}.lock"));
        let lock = open_private_owner_lock_file(&lock_path)?;
        match lock.try_lock_exclusive() {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(StoreError::ProviderFileReplacementBusy {
                    provider: provider.as_str().to_owned(),
                    owner_id,
                });
            }
            Err(error) => return Err(error.into()),
        }
        validate_open_private_owner_lock_file(&lock, &lock_path)?;
        Ok((lock, lock_path))
    }

    fn advance_provider_file_publication_attempt(
        &self,
        scope: &ProviderFilePublicationScope,
        attempted_at_ms: i64,
    ) -> Result<()> {
        self.ensure_active_provider_file_publication(scope)?;
        let changed = self.conn.execute(
            r#"
            UPDATE provider_file_publications
            SET updated_at_ms = MAX(updated_at_ms + 1, ?2)
            WHERE replacement_id = ?1 AND mutation_started != 0
            "#,
            params![scope.scope_id.to_string(), attempted_at_ms],
        )?;
        if changed != 1 {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        Ok(())
    }

    fn attach_provider_file_publication_staging(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<()> {
        self.with_atomic_provider_file_update(|| {
            let marker = self.load_replacement_marker(scope)?;
            let replacement_id = scope.scope_id.to_string();
            let staged_state_count: usize = self.conn.query_row(
                &format!(
                    "SELECT (SELECT COUNT(*) FROM {STAGING_SEEN_TABLE} WHERE replacement_id = ?1) + \
                            (SELECT COUNT(*) FROM {STAGING_PRIOR_SOURCES_TABLE} WHERE replacement_id = ?1)"
                ),
                params![&replacement_id],
                |row| nonnegative_i64_to_usize(row.get(0)?),
            )?;
            let progress_without_state = staged_state_count == 0
                && (marker.mutation_started
                    || marker.preparation_cursor.is_some()
                    || marker.cleanup_phase != CLEANUP_PHASE_LINKS
                    || marker.source_cursor.is_some()
                    || marker.entity_cursor.is_some()
                    || (scope.tracks_prior_material
                        && marker.completion_payload_json.is_some())
                    || marker.counts != ProviderFileReconciliationCounts::default());
            if !scope.reuse_staging_state || progress_without_state {
                self.reset_provider_file_publication_staging(scope)?;
            } else {
                self.conn.execute(
                    &format!("DELETE FROM {STAGING_BATCH_TABLE} WHERE replacement_id = ?1"),
                    params![&replacement_id],
                )?;
                let changed = self.conn.execute(
                    "UPDATE provider_file_publications SET staging_initialized = 1 \
                     WHERE replacement_id = ?1",
                    params![&replacement_id],
                )?;
                if changed != 1 {
                    return Err(StoreError::InvalidProviderFilePublicationScope);
                }
            }
            Ok(())
        })?;
        let mut active = self.provider_file_publication.borrow_mut();
        let active = active
            .as_mut()
            .filter(|active| active.scope_id == scope.scope_id)
            .ok_or(StoreError::InvalidProviderFilePublicationScope)?;
        active.attached = true;
        Ok(())
    }

    fn reset_provider_file_publication_staging(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<()> {
        let replacement_id = scope.scope_id.to_string();
        for table in [
            STAGING_BATCH_TABLE,
            STAGING_SEEN_TABLE,
            STAGING_PRIOR_SOURCES_TABLE,
        ] {
            self.conn.execute(
                &format!("DELETE FROM {table} WHERE replacement_id = ?1"),
                params![&replacement_id],
            )?;
        }
        let changed = self.conn.execute(
            r#"
            UPDATE provider_file_publications
            SET staging_initialized = 1,
                preparation_complete = CASE WHEN ?2 THEN 0 ELSE 1 END,
                preparation_cursor = NULL,
                cleanup_phase = 0,
                cleanup_source_cursor = NULL,
                cleanup_entity_cursor = NULL,
                removed_artifacts = 0,
                removed_summaries = 0,
                removed_history_record_links = 0,
                removed_history_records = 0,
                removed_history_record_tags = 0,
                removed_record_edges = 0,
                removed_audit_log_entries = 0,
                removed_vcs_workspaces = 0,
                removed_vcs_changes = 0,
                removed_events = 0,
                removed_runs = 0,
                removed_files_touched = 0,
                removed_session_edges = 0,
                tombstoned_sessions = 0,
                completion_payload_json = NULL
             WHERE replacement_id = ?1
            "#,
            params![&replacement_id, scope.tracks_prior_material],
        )?;
        if changed != 1 {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        Ok(())
    }
}
