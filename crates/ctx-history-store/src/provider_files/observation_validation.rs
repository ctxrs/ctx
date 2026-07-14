impl Store {
    fn ensure_provider_file_observation_is_current(
        &self,
        provider: CaptureProvider,
        observation: ProviderFileInventoryObservation<'_>,
    ) -> Result<()> {
        self.ensure_inventory_generation_is_current(provider, observation)?;
        let matches = match observation {
            ProviderFileInventoryObservation::Catalog {
                source_format,
                update,
            } => self.conn.query_row(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM catalog_sessions
                    WHERE provider = ?1 AND source_format = ?2 AND source_root = ?3
                      AND source_path = ?4 AND is_stale = 0
                      AND file_size_bytes = ?5 AND file_modified_at_ms = ?6
                      AND import_revision = ?7
                )
                "#,
                params![
                    provider.as_str(),
                    source_format,
                    update.source_root,
                    update.source_path,
                    capped_i64(update.file_size_bytes),
                    update.file_modified_at_ms,
                    i64::from(update.import_revision),
                ],
                |row| row.get::<_, bool>(0),
            )?,
            ProviderFileInventoryObservation::SourceImport {
                source_format,
                update,
            } => self.conn.query_row(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM source_import_files
                    WHERE provider = ?1 AND source_format = ?2 AND source_root = ?3
                      AND source_path = ?4 AND is_stale = 0
                      AND file_size_bytes = ?5 AND file_modified_at_ms = ?6
                      AND import_revision = ?7 AND metadata_json IS ?8
                )
                "#,
                params![
                    provider.as_str(),
                    source_format,
                    update.source_root,
                    update.source_path,
                    capped_i64(update.file_size_bytes),
                    update.file_modified_at_ms,
                    i64::from(update.import_revision),
                    serde_json::to_string(update.metadata)?,
                ],
                |row| row.get::<_, bool>(0),
            )?,
        };
        if !matches {
            return Err(provider_file_observation_changed(provider, observation));
        }
        Ok(())
    }

    fn ensure_inventory_generation_is_current(
        &self,
        provider: CaptureProvider,
        observation: ProviderFileInventoryObservation<'_>,
    ) -> Result<()> {
        let generation_is_current = self
            .conn
            .query_row(
                r#"
                SELECT current_generation = ?4
                FROM import_inventory_generations
                WHERE provider = ?1 AND source_root = ?2 AND inventory_family = ?3
                "#,
                params![
                    provider.as_str(),
                    observation.source_root(),
                    observation.inventory_family(),
                    capped_i64(observation.inventory_generation()),
                ],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false);
        if !generation_is_current {
            return Err(StoreError::ImportInventorySuperseded {
                provider: provider.as_str().to_owned(),
                inventory_family: observation.inventory_family(),
                expected_generation: observation.inventory_generation(),
            });
        }
        Ok(())
    }

    fn ensure_scope_observation_is_current(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<()> {
        let generation_is_current = self
            .conn
            .query_row(
                r#"
                SELECT current_generation = ?4
                FROM import_inventory_generations
                WHERE provider = ?1 AND source_root = ?2 AND inventory_family = ?3
                "#,
                params![
                    scope.provider.as_str(),
                    &scope.inventory_source_root,
                    scope.inventory_family,
                    capped_i64(scope.inventory_generation),
                ],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false);
        if !generation_is_current {
            return Err(StoreError::ImportInventorySuperseded {
                provider: scope.provider.as_str().to_owned(),
                inventory_family: scope.inventory_family,
                expected_generation: scope.inventory_generation,
            });
        }
        let matches: bool = if scope.inventory_family == CATALOG_INVENTORY_FAMILY {
            self.conn.query_row(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM catalog_sessions
                    WHERE provider = ?1 AND source_format = ?2 AND source_root = ?3
                      AND source_path = ?4 AND is_stale = 0 AND file_size_bytes = ?5
                      AND file_modified_at_ms = ?6 AND import_revision = ?7
                )
                "#,
                params![
                    scope.provider.as_str(),
                    &scope.inventory_source_format,
                    &scope.inventory_source_root,
                    &scope.source_path,
                    capped_i64(scope.file_size_bytes),
                    scope.file_modified_at_ms,
                    i64::from(scope.import_revision),
                ],
                |row| row.get(0),
            )?
        } else {
            self.conn.query_row(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM source_import_files
                    WHERE provider = ?1 AND source_format = ?2 AND source_root = ?3
                      AND source_path = ?4 AND is_stale = 0 AND file_size_bytes = ?5
                      AND file_modified_at_ms = ?6 AND import_revision = ?7
                      AND metadata_json IS ?8
                )
                "#,
                params![
                    scope.provider.as_str(),
                    &scope.inventory_source_format,
                    &scope.inventory_source_root,
                    &scope.source_path,
                    capped_i64(scope.file_size_bytes),
                    scope.file_modified_at_ms,
                    i64::from(scope.import_revision),
                    &scope.metadata_json,
                ],
                |row| row.get(0),
            )?
        };
        if !matches {
            return Err(StoreError::ProviderFileObservationChanged {
                provider: scope.provider.as_str().to_owned(),
                owner_id: opaque_provider_file_owner_id(
                    scope.provider,
                    &scope.material_source_format,
                    &scope.material_source_root,
                    &scope.source_path,
                ),
            });
        }
        Ok(())
    }

    fn ensure_scope_observation_allows_progress(
        &self,
        scope: &ProviderFilePublicationScope,
        marker: &ReplacementMarker,
    ) -> Result<()> {
        if !scope.retires_observation {
            return self.ensure_scope_observation_is_current(scope);
        }
        if !marker.mutation_started
            || self.provider_file_owner_has_current_observation(
                scope.provider,
                &scope.inventory_source_root,
                &scope.source_path,
            )?
        {
            return Err(StoreError::ProviderFileObservationChanged {
                provider: scope.provider.as_str().to_owned(),
                owner_id: scope.owner_id.clone(),
            });
        }
        Ok(())
    }

    fn validate_replacement_marker(
        &self,
        scope: &ProviderFilePublicationScope,
        marker: &ReplacementMarker,
    ) -> Result<()> {
        if scope.kind != ProviderFilePublicationKind::Replacement
            || marker.publication_kind != ProviderFilePublicationKind::Replacement
        {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        Ok(())
    }

    fn provider_file_owner_has_current_observation(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        source_path: &str,
    ) -> Result<bool> {
        self.conn
            .query_row(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM catalog_sessions
                    WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3
                      AND is_stale = 0
                    UNION ALL
                    SELECT 1 FROM source_import_files
                    WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3
                      AND is_stale = 0
                    LIMIT 1
                )
                "#,
                params![provider.as_str(), source_root, source_path],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    fn load_durable_provider_file_publication(
        &self,
        provider: CaptureProvider,
        material_source_format: &str,
        material_source_root: &str,
        source_path: &str,
        owner_id: &str,
    ) -> Result<Option<DurableProviderFilePublication>> {
        self.conn
            .query_row(
                r#"
                SELECT replacement_id, staging_id, inventory_family,
                       inventory_source_format, inventory_source_root, source_path,
                       inventory_generation, file_size_bytes, file_modified_at_ms,
                       import_revision, metadata_json, mutation_started
                FROM provider_file_publications
                WHERE owner_id = ?1 AND provider = ?2 AND material_source_format = ?3
                  AND material_source_root = ?4 AND source_path = ?5
                "#,
                params![
                    owner_id,
                    provider.as_str(),
                    material_source_format,
                    material_source_root,
                    source_path,
                ],
                |row| {
                    Ok(DurableProviderFilePublication {
                        scope_id: parse_uuid_text(row.get(0)?)?,
                        staging_id: row.get(1)?,
                        inventory_family: parse_provider_file_inventory_family_sql(
                            &row.get::<_, String>(2)?,
                        )?,
                        inventory_source_format: row.get(3)?,
                        inventory_source_root: row.get(4)?,
                        source_path: row.get(5)?,
                        inventory_generation: nonnegative_i64_to_u64(row.get(6)?)?,
                        file_size_bytes: nonnegative_i64_to_u64(row.get(7)?)?,
                        file_modified_at_ms: row.get(8)?,
                        import_revision: nonnegative_i64_to_u32(row.get(9)?)?,
                        metadata_json: row.get(10)?,
                        mutation_started: row.get(11)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    fn advance_provider_file_checkpoint(&self, checkpoint: &ProviderFileCheckpoint) -> Result<()> {
        if let Some(existing) = self.provider_file_checkpoint(checkpoint.key())? {
            let compatible = existing.import_revision == checkpoint.import_revision
                && existing.checkpoint_version == checkpoint.checkpoint_version
                && existing.stable_file_identity == checkpoint.stable_file_identity
                && existing.head_sha256 == checkpoint.head_sha256
                && existing.committed_byte_offset <= checkpoint.committed_byte_offset
                && existing.committed_complete_line_count
                    <= checkpoint.committed_complete_line_count
                && (existing.committed_byte_offset != checkpoint.committed_byte_offset
                    || (existing.committed_complete_line_count
                        == checkpoint.committed_complete_line_count
                        && existing.boundary_sha256 == checkpoint.boundary_sha256));
            if !compatible {
                return Err(StoreError::ProviderFileCheckpointRequiresReplacement {
                    provider: checkpoint.provider.as_str().to_owned(),
                    owner_id: opaque_provider_file_owner_id(
                        checkpoint.provider,
                        &checkpoint.source_format,
                        &checkpoint.source_root,
                        &checkpoint.source_path,
                    ),
                });
            }
        }
        self.write_provider_file_checkpoint(checkpoint)
    }

    fn replace_provider_file_checkpoint(
        &self,
        outcome: ProviderFileImportOutcome<'_>,
        checkpoint: Option<&ProviderFileCheckpoint>,
    ) -> Result<()> {
        let observation = outcome.observation;
        self.conn.execute(
            r#"
            DELETE FROM provider_file_checkpoints
            WHERE provider = ?1 AND source_format = ?2 AND source_root = ?3 AND source_path = ?4
            "#,
            params![
                outcome.provider.as_str(),
                observation.source_format(),
                observation.source_root(),
                observation.source_path(),
            ],
        )?;
        if let Some(checkpoint) = checkpoint {
            self.write_provider_file_checkpoint(checkpoint)?;
        }
        Ok(())
    }

    fn delete_provider_file_checkpoint_for_scope(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            DELETE FROM provider_file_checkpoints
            WHERE provider = ?1 AND source_format = ?2 AND source_root = ?3 AND source_path = ?4
            "#,
            params![
                scope.provider.as_str(),
                &scope.inventory_source_format,
                &scope.inventory_source_root,
                &scope.source_path,
            ],
        )?;
        Ok(())
    }

    fn retire_stale_provider_file_observation(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<()> {
        let table = match scope.inventory_family {
            CATALOG_INVENTORY_FAMILY => "catalog_sessions",
            SOURCE_IMPORT_INVENTORY_FAMILY => "source_import_files",
            _ => return Err(StoreError::InvalidProviderFilePublicationScope),
        };
        self.conn.execute(
            &format!(
                "DELETE FROM {table} WHERE provider = ?1 AND source_root = ?2 \
                 AND source_path = ?3 AND is_stale != 0"
            ),
            params![
                scope.provider.as_str(),
                &scope.inventory_source_root,
                &scope.source_path,
            ],
        )?;
        Ok(())
    }

    fn write_provider_file_checkpoint(&self, checkpoint: &ProviderFileCheckpoint) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO provider_file_checkpoints
                (provider, source_format, source_root, source_path, import_revision,
                 checkpoint_version,
                 stable_file_identity, committed_byte_offset, committed_complete_line_count,
                 head_sha256, boundary_sha256, resume_state, updated_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ON CONFLICT(provider, source_format, source_root, source_path) DO UPDATE SET
                import_revision = excluded.import_revision,
                checkpoint_version = excluded.checkpoint_version,
                stable_file_identity = excluded.stable_file_identity,
                committed_byte_offset = excluded.committed_byte_offset,
                committed_complete_line_count = excluded.committed_complete_line_count,
                head_sha256 = excluded.head_sha256,
                boundary_sha256 = excluded.boundary_sha256,
                resume_state = excluded.resume_state,
                updated_at_ms = excluded.updated_at_ms
            "#,
            params![
                checkpoint.provider.as_str(),
                &checkpoint.source_format,
                &checkpoint.source_root,
                &checkpoint.source_path,
                i64::from(checkpoint.import_revision),
                i64::from(checkpoint.checkpoint_version),
                &checkpoint.stable_file_identity,
                capped_i64(checkpoint.committed_byte_offset),
                capped_i64(checkpoint.committed_complete_line_count),
                &checkpoint.head_sha256,
                &checkpoint.boundary_sha256,
                &checkpoint.resume_state,
                checkpoint.updated_at_ms,
            ],
        )?;
        Ok(())
    }
}
