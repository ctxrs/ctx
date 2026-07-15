impl Store {
    fn classify_source_import_pending_reason(
        &self,
        file: &SourceImportFile,
    ) -> Result<Option<ImportPendingReason>> {
        let metadata_json = serde_json::to_string(&file.metadata)?;
        let prior = self
            .conn
            .query_row(
                r#"
                SELECT source_format, file_size_bytes, file_modified_at_ms,
                       import_revision, is_stale,
                       indexed_file_size_bytes, indexed_file_modified_at_ms,
                       indexed_status, indexed_import_revision, pending_reason, metadata_json
                FROM source_import_files
                WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3
                "#,
                params![file.provider.as_str(), &file.source_root, &file.source_path],
                |row| {
                    Ok(SourceImportPendingState {
                        source_format: row.get(0)?,
                        file_size_bytes: nonnegative_i64_to_u64(row.get(1)?)?,
                        file_modified_at_ms: row.get(2)?,
                        import_revision: nonnegative_i64_to_u32(row.get(3)?)?,
                        is_stale: row.get(4)?,
                        indexed_file_size_bytes: row
                            .get::<_, Option<i64>>(5)?
                            .map(nonnegative_i64_to_u64)
                            .transpose()?,
                        indexed_file_modified_at_ms: row.get(6)?,
                        indexed_status: parse_text_enum(row.get(7)?)?,
                        indexed_import_revision: row
                            .get::<_, Option<i64>>(8)?
                            .map(nonnegative_i64_to_u32)
                            .transpose()?,
                        pending_reason: row
                            .get::<_, Option<String>>(9)?
                            .map(parse_text_enum)
                            .transpose()?,
                        metadata_json: row.get(10)?,
                    })
                },
            )
            .optional()?;
        let Some(prior) = prior else {
            return Ok(Some(ImportPendingReason::FreshNew));
        };
        if self.provider_file_publication_was_abandoned(
            file.provider,
            "source_import_files",
            &prior.source_format,
            &file.source_root,
            &file.source_path,
        )? {
            return Ok(Some(ImportPendingReason::AbandonedPublication));
        }
        let same_identity = prior.source_format == file.source_format;
        let same_fingerprint = same_identity
            && prior.file_size_bytes == file.file_size_bytes
            && prior.file_modified_at_ms == file.file_modified_at_ms
            && prior.import_revision == file.import_revision
            && prior.metadata_json == metadata_json
            && !prior.is_stale;
        if same_fingerprint && prior.pending_reason == Some(ImportPendingReason::ExplicitRescan) {
            return Ok(prior.pending_reason);
        }
        if !same_fingerprint {
            if let Some(reason) = prior
                .pending_reason
                .filter(|reason| reason.requires_replacement())
            {
                return Ok(Some(reason));
            }
            let parser_revision_only = same_identity
                && prior.file_size_bytes == file.file_size_bytes
                && prior.file_modified_at_ms == file.file_modified_at_ms
                && prior.metadata_json == metadata_json
                && prior.import_revision != file.import_revision
                && !prior.is_stale;
            if parser_revision_only {
                return Ok(Some(ImportPendingReason::ParserRevision));
            }
            let grew_in_place = same_identity
                && prior.import_revision == file.import_revision
                && prior.metadata_json == metadata_json
                && !prior.is_stale
                && file.file_size_bytes > prior.file_size_bytes;
            if grew_in_place
                && matches!(
                    prior.pending_reason,
                    Some(ImportPendingReason::FreshAppend | ImportPendingReason::RecoveryRetry)
                )
                && self.source_import_incremental_material_is_supported(&prior, file)?
            {
                return Ok(prior.pending_reason);
            }
            if self.source_import_observation_is_append(&prior, file)? {
                return Ok(Some(ImportPendingReason::FreshAppend));
            }
            return Ok(Some(ImportPendingReason::FreshChanged));
        }
        match prior.indexed_status {
            CatalogIndexedStatus::Failed => Ok(Some(ImportPendingReason::retry_after_failure(
                prior.pending_reason,
            ))),
            CatalogIndexedStatus::Pending => Ok(Some(
                prior.pending_reason.unwrap_or(ImportPendingReason::Legacy),
            )),
            CatalogIndexedStatus::Indexed | CatalogIndexedStatus::CompletedWithRejections => {
                let indexed_matches = prior.indexed_file_size_bytes == Some(file.file_size_bytes)
                    && prior.indexed_file_modified_at_ms == Some(file.file_modified_at_ms);
                if prior.indexed_import_revision != Some(file.import_revision) {
                    Ok(Some(ImportPendingReason::ParserRevision))
                } else if !indexed_matches {
                    Ok(Some(
                        prior.pending_reason.unwrap_or(ImportPendingReason::Legacy),
                    ))
                } else if !self.source_import_material_exists(file)? {
                    Ok(Some(ImportPendingReason::MissingMaterial))
                } else {
                    Ok(None)
                }
            }
            CatalogIndexedStatus::Rejected => Ok(None),
        }
    }

    fn source_import_observation_is_append(
        &self,
        prior: &SourceImportPendingState,
        file: &SourceImportFile,
    ) -> Result<bool> {
        if prior.source_format != file.source_format
            || prior.import_revision != file.import_revision
            || prior.is_stale
            || file.file_size_bytes <= prior.file_size_bytes
            || !matches!(
                prior.indexed_status,
                CatalogIndexedStatus::Indexed | CatalogIndexedStatus::CompletedWithRejections
            )
            || prior.indexed_file_size_bytes != Some(prior.file_size_bytes)
            || prior.indexed_file_modified_at_ms != Some(prior.file_modified_at_ms)
            || prior.indexed_import_revision != Some(prior.import_revision)
        {
            return Ok(false);
        }
        Ok(self.provider_file_checkpoint_matches_prior_observation(
            file.provider,
            &file.source_format,
            &file.source_root,
            &file.source_path,
            file.import_revision,
            prior.file_size_bytes,
        )? && self.source_import_material_exists(file)?)
    }

    fn source_import_incremental_material_is_supported(
        &self,
        prior: &SourceImportPendingState,
        file: &SourceImportFile,
    ) -> Result<bool> {
        Ok(self.provider_file_checkpoint_matches_prior_observation(
            file.provider,
            &file.source_format,
            &file.source_root,
            &file.source_path,
            file.import_revision,
            prior.file_size_bytes,
        )? && self.source_import_material_exists(file)?)
    }

    fn source_import_material_exists(&self, file: &SourceImportFile) -> Result<bool> {
        let metadata_json = serde_json::to_string(&file.metadata)?;
        let material_source_format =
            expected_material_source_format(file.provider, &file.source_format);
        self.conn
            .query_row(
                r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM capture_sources AS source
                    WHERE source.provider = ?1
                      AND source.source_format = ?2
                      AND (
                          (
                              json_extract(?5, '$.inventory_unit') = 'source_root'
                              AND source.source_root = ?3
                          )
                          OR (
                              json_extract(?5, '$.inventory_unit') IS NOT 'source_root'
                              AND source.raw_source_path = ?4
                              AND (
                                  source.source_root = ?3
                                  OR source.source_root = source.raw_source_path
                                  OR source.source_root IS NULL
                              )
                          )
                      )
                    LIMIT 1
                )
                "#,
                params![
                    file.provider.as_str(),
                    material_source_format,
                    &file.source_root,
                    &file.source_path,
                    metadata_json
                ],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    fn provider_file_checkpoint_matches_prior_observation(
        &self,
        provider: CaptureProvider,
        source_format: &str,
        source_root: &str,
        source_path: &str,
        import_revision: u32,
        prior_size_bytes: u64,
    ) -> Result<bool> {
        self.conn
            .query_row(
                r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM provider_file_checkpoints
                    WHERE provider = ?1 AND source_format = ?2
                      AND source_root = ?3 AND source_path = ?4
                      AND import_revision = ?5
                      AND committed_byte_offset <= ?6
                )
                "#,
                params![
                    provider.as_str(),
                    source_format,
                    source_root,
                    source_path,
                    i64::from(import_revision),
                    capped_i64(prior_size_bytes)
                ],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    fn provider_file_publication_was_abandoned(
        &self,
        provider: CaptureProvider,
        inventory_family: &str,
        source_format: &str,
        source_root: &str,
        source_path: &str,
    ) -> Result<bool> {
        self.conn
            .query_row(
                r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM provider_file_publications
                    WHERE provider = ?1 AND inventory_family = ?2
                      AND inventory_source_format = ?3
                      AND inventory_source_root = ?4 AND source_path = ?5
                      AND mutation_started = 1
                )
                "#,
                params![
                    provider.as_str(),
                    inventory_family,
                    source_format,
                    source_root,
                    source_path
                ],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }
}
