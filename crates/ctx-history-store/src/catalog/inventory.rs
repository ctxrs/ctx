const CATALOG_ACTIVE_PATH_INVENTORY_PAGE_SQL: &str = "SELECT rowid, source_path \
     FROM catalog_sessions INDEXED BY idx_catalog_sessions_provider_source_root_stale \
     WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0 AND rowid > ?3 \
     ORDER BY rowid LIMIT ?4";

const CATALOG_UNPUBLISHED_DELETE_PAGE_SQL: &str = r#"
    SELECT rowid,
        length(source_path) + length(provider) + length(source_format)
        + length(source_root) + COALESCE(length(external_session_id), 0)
        + COALESCE(length(parent_external_session_id), 0)
        + length(agent_type) + COALESCE(length(role_hint), 0)
        + COALESCE(length(external_agent_id), 0) + COALESCE(length(cwd), 0)
        + length(metadata_json) + 256 AS estimated_bytes
    FROM catalog_sessions INDEXED BY idx_catalog_sessions_provider_source_root_stale
    WHERE provider = ?1 AND source_root = ?2
    ORDER BY is_stale, rowid
    LIMIT ?3
"#;

impl Store {
    const INVENTORY_PATH_PAGE_LIMIT: usize = 64;

    pub fn allocate_catalog_inventory_generation(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<u64> {
        self.allocate_import_inventory_generation(provider, source_root, "catalog_sessions")
    }

    pub fn allocate_source_import_inventory_generation(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<u64> {
        self.allocate_import_inventory_generation(provider, source_root, "source_import_files")
    }

    pub fn current_source_import_inventory_generation(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Option<u64>> {
        self.current_import_inventory_generation(provider, source_root, "source_import_files")
    }

    pub fn current_catalog_inventory_generation(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Option<u64>> {
        self.current_import_inventory_generation(provider, source_root, "catalog_sessions")
    }

    #[doc(hidden)]
    pub fn list_catalog_observation_states_for_paths(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        source_paths: &[String],
    ) -> Result<Vec<CatalogObservationState>> {
        if source_paths.is_empty() {
            return Ok(Vec::new());
        }
        if source_paths.len() > Self::INVENTORY_PATH_PAGE_LIMIT {
            return Err(StoreError::Sql(rusqlite::Error::InvalidQuery));
        }
        let placeholders = vec!["?"; source_paths.len()].join(", ");
        let sql = format!(
            "SELECT source_path, source_format, file_size_bytes, file_modified_at_ms, \
                    import_revision, is_stale, \
                    json_extract(metadata_json, '$.file_observation_token_v1') \
             FROM catalog_sessions \
             WHERE provider = ?1 AND source_root = ?2 \
               AND source_path IN ({placeholders}) \
             ORDER BY source_path"
        );
        let parameters = std::iter::once(provider.as_str())
            .chain(std::iter::once(source_root))
            .chain(source_paths.iter().map(String::as_str));
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(parameters), |row| {
            Ok(CatalogObservationState {
                source_path: row.get(0)?,
                source_format: row.get(1)?,
                file_size_bytes: nonnegative_i64_to_u64(row.get(2)?)?,
                file_modified_at_ms: row.get(3)?,
                import_revision: nonnegative_i64_to_u32(row.get(4)?)?,
                is_stale: row.get(5)?,
                observation_token: row.get(6)?,
            })
        })?;
        collect_rows(rows)
    }

    #[doc(hidden)]
    pub fn list_catalog_inventory_paths_page(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        after_rowid: Option<i64>,
        limit: usize,
    ) -> Result<Vec<(i64, String)>> {
        let limit = limit.clamp(1, Self::INVENTORY_PATH_PAGE_LIMIT);
        let mut statement = self.conn.prepare(CATALOG_ACTIVE_PATH_INVENTORY_PAGE_SQL)?;
        let paths = collect_rows(statement.query_map(
            params![
                provider.as_str(),
                source_root,
                after_rowid.unwrap_or(0),
                capped_i64(limit as u64)
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)?;
        Ok(paths)
    }

    #[doc(hidden)]
    pub fn mark_catalog_inventory_paths_stale(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        paths: &[String],
        cataloged_at_ms: i64,
        inventory_generation: u64,
    ) -> Result<usize> {
        if paths.is_empty() {
            return Ok(0);
        }
        if paths.len() > Self::INVENTORY_PATH_PAGE_LIMIT {
            return Err(StoreError::Sql(rusqlite::Error::InvalidQuery));
        }
        let placeholders = vec!["?"; paths.len()].join(", ");
        let sql = format!(
            "UPDATE catalog_sessions SET is_stale = 1, cataloged_at_ms = ?3 \
             WHERE provider = ?1 AND source_root = ?2 \
               AND EXISTS (\
                   SELECT 1 FROM import_inventory_generations AS inventory \
                   WHERE inventory.provider = ?1 AND inventory.source_root = ?2 \
                     AND inventory.inventory_family = 'catalog_sessions' \
                     AND inventory.current_generation = ?4\
               ) \
               AND source_path IN ({placeholders})"
        );
        let mut parameters: Vec<rusqlite::types::Value> = vec![
            provider.as_str().to_owned().into(),
            source_root.to_owned().into(),
            cataloged_at_ms.into(),
            capped_i64(inventory_generation).into(),
        ];
        parameters.extend(paths.iter().cloned().map(Into::into));
        self.conn
            .execute(&sql, rusqlite::params_from_iter(parameters))
            .map_err(StoreError::from)
    }

    fn current_import_inventory_generation(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_family: &str,
    ) -> Result<Option<u64>> {
        self.conn
            .query_row(
                r#"
                SELECT current_generation
                FROM import_inventory_generations
                WHERE provider = ?1
                  AND source_root = ?2
                  AND inventory_family = ?3
                "#,
                params![provider.as_str(), source_root, inventory_family],
                |row| nonnegative_i64_to_u64(row.get(0)?),
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn catalog_inventory_generation_is_current(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_generation: u64,
    ) -> Result<bool> {
        let effective_publication =
            crate::provider_files::effective_provider_file_publication_predicate("publication");
        self.conn
            .query_row(
                &format!(
                    r#"
                    SELECT current_generation = ?4
                       AND NOT EXISTS (
                            SELECT 1
                            FROM provider_file_publications AS publication
                            WHERE publication.provider = ?1
                              AND publication.inventory_family = ?3
                              AND publication.inventory_source_root = ?2
                              AND ({effective_publication})
                       )
                    FROM import_inventory_generations
                    WHERE provider = ?1 AND source_root = ?2 AND inventory_family = ?3
                    "#
                ),
                params![
                    provider.as_str(),
                    source_root,
                    "catalog_sessions",
                    capped_i64(inventory_generation),
                ],
                |row| row.get(0),
            )
            .optional()
            .map(|current| current.unwrap_or(false))
            .map_err(StoreError::from)
    }

    #[doc(hidden)]
    pub fn catalog_inventory_generation_is_unpublished(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_generation: u64,
    ) -> Result<bool> {
        self.conn
            .query_row(
                r#"
                SELECT current_generation = ?4 AND completed_generation = 0
                FROM import_inventory_generations
                WHERE provider = ?1
                  AND source_root = ?2
                  AND inventory_family = ?3
                "#,
                params![
                    provider.as_str(),
                    source_root,
                    "catalog_sessions",
                    capped_i64(inventory_generation),
                ],
                |row| row.get(0),
            )
            .optional()
            .map(|unpublished| unpublished.unwrap_or(false))
            .map_err(StoreError::from)
    }

    #[doc(hidden)]
    pub fn delete_unpublished_catalog_sessions_batch(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_generation: u64,
        limit: usize,
    ) -> Result<Option<(usize, u64)>> {
        self.delete_unpublished_catalog_sessions_batch_paced(
            provider,
            source_root,
            inventory_generation,
            limit,
            |_| {},
        )
    }

    #[doc(hidden)]
    pub fn delete_unpublished_catalog_sessions_batch_paced(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_generation: u64,
        limit: usize,
        pace: impl Fn(u64),
    ) -> Result<Option<(usize, u64)>> {
        let limit = limit.clamp(1, Self::INVENTORY_PATH_PAGE_LIMIT);
        with_immediate_transaction(&self.conn, || {
            if !self.catalog_inventory_generation_is_unpublished(
                provider,
                source_root,
                inventory_generation,
            )? {
                return Ok(None);
            }
            let mut statement = self.conn.prepare(CATALOG_UNPUBLISHED_DELETE_PAGE_SQL)?;
            let page = collect_rows(statement.query_map(
                params![provider.as_str(), source_root, capped_i64(limit as u64),],
                |row| Ok((row.get::<_, i64>(0)?, nonnegative_i64_to_u64(row.get(1)?)?)),
            )?)?;
            let rows = page.len();
            let bytes = page
                .iter()
                .fold(0_u64, |total, (_, bytes)| total.saturating_add(*bytes));
            pace(bytes);
            if page.is_empty() {
                return Ok(Some((0, 0)));
            }
            let placeholders = (1..=page.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!("DELETE FROM catalog_sessions WHERE rowid IN ({placeholders})");
            let deleted = self.conn.execute(
                &sql,
                rusqlite::params_from_iter(page.into_iter().map(|(rowid, _)| rowid)),
            )?;
            debug_assert_eq!(deleted, rows);
            Ok(Some((deleted, bytes)))
        })
    }

    #[doc(hidden)]
    pub fn catalog_inventory_generation_is_complete(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_generation: u64,
    ) -> Result<bool> {
        let effective_publication =
            crate::provider_files::effective_provider_file_publication_predicate("publication");
        self.conn
            .query_row(
                &format!(
                    "SELECT current_generation = ?4 AND completed_generation = ?4\n\
                         AND NOT EXISTS (\n\
                            SELECT 1 FROM provider_file_publications AS publication\n\
                            WHERE publication.provider = ?1\n\
                              AND publication.inventory_source_root = ?2\n\
                              AND publication.inventory_family = ?3\n\
                              AND ({effective_publication})\n\
                         )\n\
                 FROM import_inventory_generations\n\
                 WHERE provider = ?1 AND source_root = ?2 AND inventory_family = ?3"
                ),
                params![
                    provider.as_str(),
                    source_root,
                    "catalog_sessions",
                    capped_i64(inventory_generation)
                ],
                |row| row.get(0),
            )
            .optional()
            .map(|complete| complete.unwrap_or(false))
            .map_err(StoreError::from)
    }

    #[doc(hidden)]
    pub fn source_import_inventory_generation_is_complete(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_generation: u64,
    ) -> Result<bool> {
        let effective_publication =
            crate::provider_files::effective_provider_file_publication_predicate("publication");
        self.conn
            .query_row(
                &format!(
                    "SELECT current_generation = ?4 AND completed_generation = ?4\n\
                         AND NOT EXISTS (\n\
                            SELECT 1 FROM provider_file_publications AS publication\n\
                            WHERE publication.provider = ?1\n\
                              AND publication.inventory_source_root = ?2\n\
                              AND publication.inventory_family = ?3\n\
                              AND ({effective_publication})\n\
                         )\n\
                     FROM import_inventory_generations\n\
                     WHERE provider = ?1 AND source_root = ?2 AND inventory_family = ?3"
                ),
                params![
                    provider.as_str(),
                    source_root,
                    "source_import_files",
                    capped_i64(inventory_generation)
                ],
                |row| row.get(0),
            )
            .optional()
            .map(|complete| complete.unwrap_or(false))
            .map_err(StoreError::from)
    }

    pub fn catalog_inventory_generation_is_complete_without_pending(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_generation: u64,
    ) -> Result<bool> {
        let visible = crate::provider_files::catalog_material_visible_predicate("catalog_sessions");
        let effective_publication =
            crate::provider_files::effective_provider_file_publication_predicate("publication");
        self.conn
            .query_row(
                format!(
                    "SELECT current_generation = ?4\n\
                            AND completed_generation = ?4\n\
                            AND NOT EXISTS (\n\
                                SELECT 1 FROM catalog_sessions\n\
                                WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0\n\
                                  AND {visible}\n\
                                  AND {}\n\
                            )\n\
                            AND NOT EXISTS (\n\
                                SELECT 1 FROM provider_file_publications AS publication\n\
                                WHERE publication.provider = ?1\n\
                                  AND publication.inventory_source_root = ?2\n\
                                  AND publication.inventory_family = ?3\n\
                                  AND ({effective_publication})\n\
                            )\n\
                     FROM import_inventory_generations\n\
                     WHERE provider = ?1 AND source_root = ?2 AND inventory_family = ?3",
                    catalog_pending_import_condition_sql("catalog_sessions")
                )
                .as_str(),
                params![
                    provider.as_str(),
                    source_root,
                    "catalog_sessions",
                    capped_i64(inventory_generation)
                ],
                |row| row.get(0),
            )
            .optional()
            .map(|complete| complete.unwrap_or(false))
            .map_err(StoreError::from)
    }

    pub fn complete_catalog_inventory_generation(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_generation: u64,
    ) -> Result<bool> {
        let effective_publication =
            crate::provider_files::effective_provider_file_publication_predicate("publication");
        let changed = self.conn.execute(
            &format!(
                "UPDATE import_inventory_generations\n\
             SET completed_generation = ?4\n\
             WHERE provider = ?1 AND source_root = ?2 AND inventory_family = ?3\n\
               AND current_generation = ?4\n\
               AND NOT EXISTS (\n\
                    SELECT 1 FROM provider_file_publications AS publication\n\
                    WHERE publication.provider = ?1\n\
                      AND publication.inventory_source_root = ?2\n\
                      AND publication.inventory_family = ?3\n\
                      AND ({effective_publication})\n\
               )"
            ),
            params![
                provider.as_str(),
                source_root,
                "catalog_sessions",
                capped_i64(inventory_generation)
            ],
        )?;
        Ok(changed == 1)
    }

    #[doc(hidden)]
    pub fn complete_source_import_inventory_generation(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_generation: u64,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE import_inventory_generations SET completed_generation = ?4 \
             WHERE provider = ?1 AND source_root = ?2 \
               AND inventory_family = ?3 AND current_generation = ?4",
            params![
                provider.as_str(),
                source_root,
                "source_import_files",
                capped_i64(inventory_generation),
            ],
        )?;
        Ok(changed == 1)
    }

    fn allocate_import_inventory_generation(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_family: &str,
    ) -> Result<u64> {
        let generation = self.conn.query_row(
            r#"
            INSERT INTO import_inventory_generations
                (provider, source_root, inventory_family, current_generation, completed_generation)
            VALUES (?1, ?2, ?3, 1, 0)
            ON CONFLICT(provider, source_root, inventory_family) DO UPDATE SET
                current_generation = current_generation + 1,
                completed_generation = 0
            RETURNING current_generation
            "#,
            params![provider.as_str(), source_root, inventory_family],
            |row| nonnegative_i64_to_u64(row.get(0)?),
        )?;
        Ok(generation)
    }

    pub fn mark_catalog_source_stale(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        cataloged_at_ms: i64,
    ) -> Result<usize> {
        let changed = self.conn.execute(
            r#"
                UPDATE catalog_sessions
                SET is_stale = 1, cataloged_at_ms = ?3
                WHERE provider = ?1 AND source_root = ?2
                "#,
            params![provider.as_str(), source_root, cataloged_at_ms],
        )?;
        Ok(changed)
    }

    fn classify_catalog_pending_reason(
        &self,
        session: &CatalogSession,
    ) -> Result<Option<ImportPendingReason>> {
        let prior = self
            .conn
            .query_row(
                r#"
                SELECT provider, source_format, source_root, file_size_bytes,
                       file_modified_at_ms, import_revision, is_stale,
                       indexed_file_size_bytes, indexed_file_modified_at_ms,
                       indexed_status, indexed_import_revision, pending_reason, metadata_json
                FROM catalog_sessions
                WHERE source_path = ?1
                "#,
                params![&session.source_path],
                |row| {
                    Ok(CatalogPendingState {
                        provider: parse_text_enum(row.get(0)?)?,
                        source_format: row.get(1)?,
                        source_root: row.get(2)?,
                        file_size_bytes: nonnegative_i64_to_u64(row.get(3)?)?,
                        file_modified_at_ms: row.get(4)?,
                        import_revision: nonnegative_i64_to_u32(row.get(5)?)?,
                        is_stale: row.get(6)?,
                        indexed_file_size_bytes: row
                            .get::<_, Option<i64>>(7)?
                            .map(nonnegative_i64_to_u64)
                            .transpose()?,
                        indexed_file_modified_at_ms: row.get(8)?,
                        indexed_status: parse_text_enum(row.get(9)?)?,
                        indexed_import_revision: row
                            .get::<_, Option<i64>>(10)?
                            .map(nonnegative_i64_to_u32)
                            .transpose()?,
                        pending_reason: row
                            .get::<_, Option<String>>(11)?
                            .map(parse_text_enum)
                            .transpose()?,
                        metadata_json: row.get(12)?,
                    })
                },
            )
            .optional()?;
        let Some(prior) = prior else {
            return Ok(Some(ImportPendingReason::FreshNew));
        };
        if self.provider_file_publication_was_abandoned(
            session.provider,
            "catalog_sessions",
            &prior.source_format,
            &prior.source_root,
            &session.source_path,
        )? {
            return Ok(Some(ImportPendingReason::AbandonedPublication));
        }
        let same_identity = prior.provider == session.provider
            && prior.source_format == session.source_format
            && prior.source_root == session.source_root;
        let same_fingerprint = same_identity
            && prior.file_size_bytes == session.file_size_bytes
            && prior.file_modified_at_ms == session.file_modified_at_ms
            && prior.import_revision == session.import_revision
            && catalog_observation_metadata_matches(&prior.metadata_json, &session.metadata)
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
                && prior.file_size_bytes == session.file_size_bytes
                && prior.file_modified_at_ms == session.file_modified_at_ms
                && catalog_observation_metadata_matches(&prior.metadata_json, &session.metadata)
                && prior.import_revision != session.import_revision
                && !prior.is_stale;
            if parser_revision_only {
                return Ok(Some(ImportPendingReason::ParserRevision));
            }
            let grew_in_place = same_identity
                && prior.import_revision == session.import_revision
                && !prior.is_stale
                && session.file_size_bytes > prior.file_size_bytes;
            if grew_in_place
                && matches!(
                    prior.pending_reason,
                    Some(ImportPendingReason::FreshAppend | ImportPendingReason::RecoveryRetry)
                )
                && self.catalog_incremental_material_is_supported(&prior, session)?
            {
                return Ok(prior.pending_reason);
            }
            if self.catalog_observation_is_append(&prior, session)? {
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
                let indexed_matches = prior.indexed_file_size_bytes
                    == Some(session.file_size_bytes)
                    && prior.indexed_file_modified_at_ms == Some(session.file_modified_at_ms);
                if prior.indexed_import_revision != Some(session.import_revision) {
                    Ok(Some(ImportPendingReason::ParserRevision))
                } else if !indexed_matches {
                    Ok(Some(
                        prior.pending_reason.unwrap_or(ImportPendingReason::Legacy),
                    ))
                } else if !self.catalog_session_material_exists(session)? {
                    Ok(Some(ImportPendingReason::MissingMaterial))
                } else {
                    Ok(None)
                }
            }
            CatalogIndexedStatus::Rejected => Ok(None),
        }
    }

    fn catalog_observation_is_append(
        &self,
        prior: &CatalogPendingState,
        session: &CatalogSession,
    ) -> Result<bool> {
        if prior.provider != session.provider
            || prior.source_format != session.source_format
            || prior.source_root != session.source_root
            || prior.import_revision != session.import_revision
            || prior.is_stale
            || session.file_size_bytes <= prior.file_size_bytes
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
            session.provider,
            &session.source_format,
            &session.source_root,
            &session.source_path,
            session.import_revision,
            prior.file_size_bytes,
        )? && self.catalog_session_material_exists(session)?)
    }

    fn catalog_incremental_material_is_supported(
        &self,
        prior: &CatalogPendingState,
        session: &CatalogSession,
    ) -> Result<bool> {
        Ok(self.provider_file_checkpoint_matches_prior_observation(
            session.provider,
            &session.source_format,
            &session.source_root,
            &session.source_path,
            session.import_revision,
            prior.file_size_bytes,
        )? && self.catalog_session_material_exists(session)?)
    }

    fn catalog_session_material_exists(&self, session: &CatalogSession) -> Result<bool> {
        let Some(external_session_id) = session.external_session_id.as_deref() else {
            return Ok(false);
        };
        let material_source_format =
            expected_material_source_format(session.provider, &session.source_format);
        let owner =
            crate::provider_files::material_owner_predicate("source", "?1", "?3", "?5", "?4");
        self.conn
            .query_row(
                &format!(
                    r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM sessions AS material_session
                    JOIN capture_sources AS source
                      ON source.id = material_session.capture_source_id
                    WHERE material_session.provider = ?1
                      AND material_session.external_session_id = ?2
                      AND ({owner})
                      AND source.external_session_id = ?2
                    LIMIT 1
                )
                "#
                ),
                params![
                    session.provider.as_str(),
                    external_session_id,
                    material_source_format,
                    &session.source_path,
                    &session.source_root,
                ],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn upsert_catalog_sessions(
        &self,
        inventory_generation: u64,
        sessions: &[CatalogSession],
    ) -> Result<usize> {
        let mut stmt = self.conn.prepare(
                r#"
                INSERT INTO catalog_sessions
                (
                    source_path, provider, source_format, source_root,
                    external_session_id, parent_external_session_id, agent_type, role_hint,
                    external_agent_id, cwd, session_started_at_ms, file_size_bytes,
                    file_modified_at_ms, import_revision, cataloged_at_ms, is_stale,
                    pending_reason, metadata_json
                )
                SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 0, ?16, ?17
                WHERE EXISTS (
                    SELECT 1
                    FROM import_inventory_generations AS inventory
                    WHERE inventory.provider = ?2
                      AND inventory.source_root = ?4
                      AND inventory.inventory_family = 'catalog_sessions'
                      AND inventory.current_generation = ?18
                )
                ON CONFLICT(source_path) DO UPDATE SET
                    provider = excluded.provider,
                    source_format = excluded.source_format,
                    source_root = excluded.source_root,
                    external_session_id = excluded.external_session_id,
                    parent_external_session_id = excluded.parent_external_session_id,
                    agent_type = excluded.agent_type,
                    role_hint = excluded.role_hint,
                    external_agent_id = excluded.external_agent_id,
                    cwd = excluded.cwd,
                    session_started_at_ms = excluded.session_started_at_ms,
                    file_size_bytes = excluded.file_size_bytes,
                    file_modified_at_ms = excluded.file_modified_at_ms,
                    import_revision = excluded.import_revision,
                    cataloged_at_ms = excluded.cataloged_at_ms,
                    is_stale = 0,
                    indexed_at_ms = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND json_extract(catalog_sessions.metadata_json, '$.file_observation_token_v1')
                             IS json_extract(excluded.metadata_json, '$.file_observation_token_v1')
                        THEN catalog_sessions.indexed_at_ms
                        ELSE NULL
                    END,
                    indexed_file_size_bytes = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND json_extract(catalog_sessions.metadata_json, '$.file_observation_token_v1')
                             IS json_extract(excluded.metadata_json, '$.file_observation_token_v1')
                        THEN catalog_sessions.indexed_file_size_bytes
                        ELSE NULL
                    END,
                    indexed_file_modified_at_ms = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND json_extract(catalog_sessions.metadata_json, '$.file_observation_token_v1')
                             IS json_extract(excluded.metadata_json, '$.file_observation_token_v1')
                        THEN catalog_sessions.indexed_file_modified_at_ms
                        ELSE NULL
                    END,
                    indexed_status = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND json_extract(catalog_sessions.metadata_json, '$.file_observation_token_v1')
                             IS json_extract(excluded.metadata_json, '$.file_observation_token_v1')
                        THEN catalog_sessions.indexed_status
                        WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                         AND catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.indexed_status = 'completed_with_rejections'
                         AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                         AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                         AND catalog_sessions.indexed_import_revision = catalog_sessions.import_revision
                        THEN catalog_sessions.indexed_status
                        ELSE 'pending'
                    END,
                    indexed_error = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND json_extract(catalog_sessions.metadata_json, '$.file_observation_token_v1')
                             IS json_extract(excluded.metadata_json, '$.file_observation_token_v1')
                        THEN catalog_sessions.indexed_error
                        WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                         AND catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.indexed_status = 'completed_with_rejections'
                         AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                         AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                         AND catalog_sessions.indexed_import_revision = catalog_sessions.import_revision
                        THEN catalog_sessions.indexed_error
                        ELSE NULL
                    END,
                    indexed_event_count = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND json_extract(catalog_sessions.metadata_json, '$.file_observation_token_v1')
                             IS json_extract(excluded.metadata_json, '$.file_observation_token_v1')
                        THEN catalog_sessions.indexed_event_count
                        ELSE NULL
                    END,
                    indexed_import_revision = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND json_extract(catalog_sessions.metadata_json, '$.file_observation_token_v1')
                             IS json_extract(excluded.metadata_json, '$.file_observation_token_v1')
                        THEN catalog_sessions.indexed_import_revision
                        ELSE NULL
                    END,
                    last_imported_at_ms = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND json_extract(catalog_sessions.metadata_json, '$.file_observation_token_v1')
                             IS json_extract(excluded.metadata_json, '$.file_observation_token_v1')
                        THEN catalog_sessions.last_imported_at_ms
                        WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                         AND catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.indexed_status IN ('indexed', 'completed_with_rejections')
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                         AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                         AND catalog_sessions.indexed_import_revision = catalog_sessions.import_revision
                         AND catalog_sessions.last_imported_file_size_bytes > 0
                         AND catalog_sessions.last_imported_file_size_bytes <= catalog_sessions.file_size_bytes
                        THEN catalog_sessions.last_imported_at_ms
                        ELSE NULL
                    END,
                    last_imported_file_size_bytes = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND json_extract(catalog_sessions.metadata_json, '$.file_observation_token_v1')
                             IS json_extract(excluded.metadata_json, '$.file_observation_token_v1')
                        THEN catalog_sessions.last_imported_file_size_bytes
                        WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                         AND catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.indexed_status IN ('indexed', 'completed_with_rejections')
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                         AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                         AND catalog_sessions.indexed_import_revision = catalog_sessions.import_revision
                         AND catalog_sessions.last_imported_file_size_bytes > 0
                         AND catalog_sessions.last_imported_file_size_bytes <= catalog_sessions.file_size_bytes
                        THEN catalog_sessions.last_imported_file_size_bytes
                        ELSE NULL
                    END,
                    last_imported_file_modified_at_ms = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND json_extract(catalog_sessions.metadata_json, '$.file_observation_token_v1')
                             IS json_extract(excluded.metadata_json, '$.file_observation_token_v1')
                        THEN catalog_sessions.last_imported_file_modified_at_ms
                        WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                         AND catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.indexed_status IN ('indexed', 'completed_with_rejections')
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                         AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                         AND catalog_sessions.indexed_import_revision = catalog_sessions.import_revision
                         AND catalog_sessions.last_imported_file_size_bytes > 0
                         AND catalog_sessions.last_imported_file_size_bytes <= catalog_sessions.file_size_bytes
                        THEN catalog_sessions.last_imported_file_modified_at_ms
                        ELSE NULL
                    END,
                    last_imported_file_sha256 = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND json_extract(catalog_sessions.metadata_json, '$.file_observation_token_v1')
                             IS json_extract(excluded.metadata_json, '$.file_observation_token_v1')
                        THEN catalog_sessions.last_imported_file_sha256
                        WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                         AND catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.indexed_status IN ('indexed', 'completed_with_rejections')
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                         AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                         AND catalog_sessions.indexed_import_revision = catalog_sessions.import_revision
                         AND catalog_sessions.last_imported_file_size_bytes > 0
                         AND catalog_sessions.last_imported_file_size_bytes <= catalog_sessions.file_size_bytes
                        THEN catalog_sessions.last_imported_file_sha256
                        ELSE NULL
                    END,
                    last_imported_event_count = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND json_extract(catalog_sessions.metadata_json, '$.file_observation_token_v1')
                             IS json_extract(excluded.metadata_json, '$.file_observation_token_v1')
                        THEN catalog_sessions.last_imported_event_count
                        WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                         AND catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.indexed_status IN ('indexed', 'completed_with_rejections')
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                         AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                         AND catalog_sessions.indexed_import_revision = catalog_sessions.import_revision
                         AND catalog_sessions.last_imported_file_size_bytes > 0
                         AND catalog_sessions.last_imported_file_size_bytes <= catalog_sessions.file_size_bytes
                        THEN catalog_sessions.last_imported_event_count
                        ELSE NULL
                    END,
                    pending_reason = excluded.pending_reason,
                    metadata_json = excluded.metadata_json
                WHERE EXISTS (
                    SELECT 1
                    FROM import_inventory_generations AS inventory
                    WHERE inventory.provider = excluded.provider
                      AND inventory.source_root = excluded.source_root
                      AND inventory.inventory_family = 'catalog_sessions'
                      AND inventory.current_generation = ?18
                )
                  AND (
                       catalog_sessions.provider IS NOT excluded.provider
                    OR catalog_sessions.source_format IS NOT excluded.source_format
                    OR catalog_sessions.source_root IS NOT excluded.source_root
                    OR catalog_sessions.external_session_id IS NOT excluded.external_session_id
                    OR catalog_sessions.parent_external_session_id IS NOT excluded.parent_external_session_id
                    OR catalog_sessions.agent_type IS NOT excluded.agent_type
                    OR catalog_sessions.role_hint IS NOT excluded.role_hint
                    OR catalog_sessions.external_agent_id IS NOT excluded.external_agent_id
                    OR catalog_sessions.cwd IS NOT excluded.cwd
                    OR catalog_sessions.session_started_at_ms IS NOT excluded.session_started_at_ms
                    OR catalog_sessions.file_size_bytes != excluded.file_size_bytes
                    OR catalog_sessions.file_modified_at_ms != excluded.file_modified_at_ms
                    OR catalog_sessions.import_revision != excluded.import_revision
                    OR catalog_sessions.is_stale != 0
                    OR catalog_sessions.pending_reason IS NOT excluded.pending_reason
                    OR catalog_sessions.metadata_json IS NOT excluded.metadata_json
                  )
                "#,
            )?;
        let mut changed = 0;
        for session in sessions {
            let pending_reason = self.classify_catalog_pending_reason(session)?;
            changed += stmt.execute(params![
                session.source_path.as_str(),
                session.provider.as_str(),
                session.source_format.as_str(),
                session.source_root.as_str(),
                session.external_session_id.as_deref(),
                session.parent_external_session_id.as_deref(),
                session.agent_type.as_str(),
                session.role_hint.as_deref(),
                session.external_agent_id.as_deref(),
                session.cwd.as_deref(),
                session.session_started_at_ms,
                capped_i64(session.file_size_bytes),
                session.file_modified_at_ms,
                i64::from(session.import_revision),
                session.cataloged_at_ms,
                pending_reason.map(ImportPendingReason::as_str),
                serde_json::to_string(&session.metadata)?,
                capped_i64(inventory_generation),
            ])?;
        }
        Ok(changed)
    }

    pub fn list_catalog_sessions_for_source(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Vec<CatalogSession>> {
        let visible = crate::provider_files::catalog_material_visible_predicate("catalog_sessions");
        let mut stmt = self.conn.prepare(
            format!(
                "{} WHERE provider = ?1 AND source_root = ?2 AND {visible}",
                catalog_session_select_sql("")
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(
            params![provider.as_str(), source_root],
            catalog_session_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn catalog_source_stale_session_count(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<usize> {
        let visible = crate::provider_files::catalog_material_visible_predicate("catalog_sessions");
        self.conn
            .query_row(
                &format!(
                    r#"
                    SELECT COUNT(*)
                    FROM catalog_sessions
                    WHERE provider = ?1
                      AND source_root = ?2
                      AND is_stale != 0
                      AND {visible}
                    "#
                ),
                params![provider.as_str(), source_root],
                |row| row.get::<_, usize>(0),
            )
            .map_err(Into::into)
    }
}
