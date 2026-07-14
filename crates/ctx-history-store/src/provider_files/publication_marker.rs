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
                SELECT replacement_id, staging_id, mutation_started
                FROM provider_file_publications
                WHERE owner_id = ?1
                "#,
                params![&scope.owner_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()?;
        let changed = if let Some((publication_id, staging_id, mutation_started)) = prior {
            scope.tracks_prior_material |= mutation_started;
            scope.scope_id = Uuid::parse_str(&publication_id)?;
            scope.staging_id = staging_id;
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
                    preparation_complete = CASE WHEN ?2 = 'incremental' THEN 1 ELSE 0 END,
                    preparation_cursor = NULL, cleanup_phase = 0,
                    cleanup_source_cursor = NULL, cleanup_entity_cursor = NULL,
                    removed_artifacts = CASE WHEN ?15 THEN removed_artifacts ELSE 0 END,
                    removed_summaries = CASE WHEN ?15 THEN removed_summaries ELSE 0 END,
                    removed_history_record_links = CASE WHEN ?15 THEN removed_history_record_links ELSE 0 END,
                    removed_history_records = CASE WHEN ?15 THEN removed_history_records ELSE 0 END,
                    removed_history_record_tags = CASE WHEN ?15 THEN removed_history_record_tags ELSE 0 END,
                    removed_record_edges = CASE WHEN ?15 THEN removed_record_edges ELSE 0 END,
                    removed_audit_log_entries = CASE WHEN ?15 THEN removed_audit_log_entries ELSE 0 END,
                    removed_vcs_workspaces = CASE WHEN ?15 THEN removed_vcs_workspaces ELSE 0 END,
                    removed_vcs_changes = CASE WHEN ?15 THEN removed_vcs_changes ELSE 0 END,
                    removed_events = CASE WHEN ?15 THEN removed_events ELSE 0 END,
                    removed_runs = CASE WHEN ?15 THEN removed_runs ELSE 0 END,
                    removed_files_touched = CASE WHEN ?15 THEN removed_files_touched ELSE 0 END,
                    removed_session_edges = CASE WHEN ?15 THEN removed_session_edges ELSE 0 END,
                    tombstoned_sessions = CASE WHEN ?15 THEN tombstoned_sessions ELSE 0 END,
                    started_at_ms = ?16, updated_at_ms = ?16
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
                     import_revision, metadata_json, mutation_started,
                     preparation_complete, preparation_cursor, cleanup_phase,
                     cleanup_source_cursor, cleanup_entity_cursor,
                     removed_artifacts, removed_summaries, removed_history_record_links,
                     removed_history_records, removed_history_record_tags, removed_record_edges,
                     removed_audit_log_entries,
                     removed_vcs_workspaces, removed_vcs_changes, removed_events, removed_runs,
                     removed_files_touched, removed_session_edges, tombstoned_sessions,
                     started_at_ms, updated_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                        ?13, ?14, ?15, ?16, 0,
                        CASE WHEN ?3 = 'incremental' THEN 1 ELSE 0 END, NULL,
                        0, NULL, NULL, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                        ?17, ?17)
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
                SELECT mutation_started, preparation_complete, preparation_cursor,
                       cleanup_phase, cleanup_source_cursor, cleanup_entity_cursor,
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
                        mutation_started: row.get(0)?,
                        preparation_complete: row.get(1)?,
                        preparation_cursor: row.get(2)?,
                        cleanup_phase: row.get(3)?,
                        source_cursor: row.get(4)?,
                        entity_cursor: row.get(5)?,
                        counts: ProviderFileReconciliationCounts {
                            artifacts: nonnegative_i64_to_usize(row.get(6)?)?,
                            summaries: nonnegative_i64_to_usize(row.get(7)?)?,
                            history_record_links: nonnegative_i64_to_usize(row.get(8)?)?,
                            history_records: nonnegative_i64_to_usize(row.get(9)?)?,
                            history_record_tags: nonnegative_i64_to_usize(row.get(10)?)?,
                            record_edges: nonnegative_i64_to_usize(row.get(11)?)?,
                            audit_log_entries: nonnegative_i64_to_usize(row.get(12)?)?,
                            vcs_workspaces: nonnegative_i64_to_usize(row.get(13)?)?,
                            vcs_changes: nonnegative_i64_to_usize(row.get(14)?)?,
                            events: nonnegative_i64_to_usize(row.get(15)?)?,
                            runs: nonnegative_i64_to_usize(row.get(16)?)?,
                            files_touched: nonnegative_i64_to_usize(row.get(17)?)?,
                            session_edges: nonnegative_i64_to_usize(row.get(18)?)?,
                            sessions_tombstoned: nonnegative_i64_to_usize(row.get(19)?)?,
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
                tombstoned_sessions = ?21, updated_at_ms = ?22
            WHERE replacement_id = ?1
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

    fn attach_provider_file_publication_staging(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<()> {
        let owner_id = provider_file_owner_lock_name(
            self.store_identity.digest(),
            scope.provider,
            &scope.material_source_format,
            &scope.material_source_root,
            &scope.source_path,
        );
        let staging_dir = self.store_identity.private_root().join(format!(
            "{STAGING_DIR_PREFIX}-{owner_id}-{}",
            scope.staging_id
        ));
        create_or_validate_private_lock_dir(&staging_dir)?;
        #[cfg(test)]
        let staging_dir_mode = staging_directory_mode(&staging_dir)?;
        let staging_path = staging_dir.join("seen.sqlite");
        let file = match create_private_staging_file(&staging_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                open_existing_private_staging_file(&staging_path)?
            }
            Err(error) => return Err(error.into()),
        };
        drop(file);
        #[cfg(test)]
        let staging_file_mode = staging_file_mode(&staging_path)?;

        let attach_result = (|| -> Result<()> {
            self.conn.execute(
                &format!("ATTACH DATABASE ?1 AS {STAGING_SCHEMA}"),
                params![staging_path.to_string_lossy().as_ref()],
            )?;
            self.conn.execute_batch(&format!(
                r#"
                PRAGMA {STAGING_SCHEMA}.page_size = 4096;
                PRAGMA {STAGING_SCHEMA}.cache_size = -8192;
                PRAGMA {STAGING_SCHEMA}.journal_mode = OFF;
                PRAGMA {STAGING_SCHEMA}.synchronous = OFF;
                CREATE TABLE IF NOT EXISTS {STAGING_SCHEMA}.scope (
                    scope_id TEXT PRIMARY KEY NOT NULL,
                    provider TEXT NOT NULL,
                    material_source_format TEXT NOT NULL,
                    material_source_root TEXT NOT NULL,
                    source_path TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS {STAGING_SCHEMA}.seen (
                    entity_kind TEXT NOT NULL,
                    entity_id TEXT NOT NULL,
                    PRIMARY KEY (entity_kind, entity_id)
                );
                CREATE TABLE IF NOT EXISTS {STAGING_SCHEMA}.prior_sources (id TEXT PRIMARY KEY NOT NULL);
                CREATE TABLE IF NOT EXISTS {STAGING_SCHEMA}.batch (
                    source_id TEXT NOT NULL,
                    entity_id TEXT NOT NULL,
                    PRIMARY KEY (source_id, entity_id)
                );
                DELETE FROM {STAGING_SCHEMA}.scope;
                DELETE FROM {STAGING_SCHEMA}.seen;
                DELETE FROM {STAGING_SCHEMA}.prior_sources;
                DELETE FROM {STAGING_SCHEMA}.batch;
                "#
            ))?;
            self.conn.execute(
                &format!(
                    "INSERT INTO {STAGING_SCHEMA}.scope
                     (scope_id, provider, material_source_format, material_source_root, source_path)
                     VALUES (?1, ?2, ?3, ?4, ?5)"
                ),
                params![
                    scope.scope_id.to_string(),
                    scope.provider.as_str(),
                    &scope.material_source_format,
                    &scope.material_source_root,
                    &scope.source_path,
                ],
            )?;
            Ok(())
        })();
        if let Err(error) = attach_result {
            let _ = self
                .conn
                .execute_batch(&format!("DETACH DATABASE {STAGING_SCHEMA}"));
            return Err(error);
        }
        #[cfg(unix)]
        fs::remove_file(&staging_path)?;
        let mut active = self.provider_file_publication.borrow_mut();
        let active = active
            .as_mut()
            .filter(|active| active.scope_id == scope.scope_id)
            .ok_or(StoreError::InvalidProviderFilePublicationScope)?;
        active.attached = true;
        active.staging_dir_path = Some(staging_dir);
        #[cfg(unix)]
        {
            active.staging_path = None;
        }
        #[cfg(not(unix))]
        {
            active.staging_path = Some(staging_path);
        }
        #[cfg(test)]
        {
            active.staging_file_mode = staging_file_mode;
            active.staging_dir_mode = staging_dir_mode;
        }
        Ok(())
    }
}
