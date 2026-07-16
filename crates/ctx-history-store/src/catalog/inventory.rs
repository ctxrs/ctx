impl Store {
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

    fn allocate_import_inventory_generation(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_family: &str,
    ) -> Result<u64> {
        let generation = self.conn.query_row(
            r#"
            INSERT INTO import_inventory_generations
                (provider, source_root, inventory_family, current_generation)
            VALUES (?1, ?2, ?3, 1)
            ON CONFLICT(provider, source_root, inventory_family) DO UPDATE SET
                current_generation = current_generation + 1
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
                    file_modified_at_ms, import_revision, cataloged_at_ms, is_stale, metadata_json
                )
                SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 0, ?16
                WHERE EXISTS (
                    SELECT 1
                    FROM import_inventory_generations AS inventory
                    WHERE inventory.provider = ?2
                      AND inventory.source_root = ?4
                      AND inventory.inventory_family = 'catalog_sessions'
                      AND inventory.current_generation = ?17
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
                    metadata_json = excluded.metadata_json
                WHERE EXISTS (
                    SELECT 1
                    FROM import_inventory_generations AS inventory
                    WHERE inventory.provider = excluded.provider
                      AND inventory.source_root = excluded.source_root
                      AND inventory.inventory_family = 'catalog_sessions'
                      AND inventory.current_generation = ?17
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
                    OR catalog_sessions.metadata_json IS NOT excluded.metadata_json
                  )
                "#,
            )?;
        let mut changed = 0;
        for session in sessions {
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
        let mut stmt = self.conn.prepare(
            format!(
                "{} WHERE provider = ?1 AND source_root = ?2",
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
        self.conn
            .query_row(
                r#"
                    SELECT COUNT(*)
                    FROM catalog_sessions
                    WHERE provider = ?1
                      AND source_root = ?2
                      AND is_stale != 0
                    "#,
                params![provider.as_str(), source_root],
                |row| row.get::<_, usize>(0),
            )
            .map_err(Into::into)
    }

    pub fn mark_catalog_source_missing_paths_stale(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        current_paths: &[String],
        cataloged_at_ms: i64,
        inventory_generation: u64,
    ) -> Result<usize> {
        self.conn.execute(
                "CREATE TEMP TABLE IF NOT EXISTS temp_catalog_current_paths(source_path TEXT PRIMARY KEY)",
                [],
            )?;
        self.conn
            .execute("DELETE FROM temp_catalog_current_paths", [])?;
        {
            let mut stmt = self.conn.prepare(
                "INSERT OR IGNORE INTO temp_catalog_current_paths(source_path) VALUES (?1)",
            )?;
            for path in current_paths {
                stmt.execute(params![path.as_str()])?;
            }
        }
        let changed = self.conn.execute(
            r#"
                UPDATE catalog_sessions
                SET is_stale = 1, cataloged_at_ms = ?3
                WHERE provider = ?1
                  AND source_root = ?2
                  AND is_stale = 0
                  AND EXISTS (
                      SELECT 1
                      FROM import_inventory_generations AS inventory
                      WHERE inventory.provider = ?1
                        AND inventory.source_root = ?2
                        AND inventory.inventory_family = 'catalog_sessions'
                        AND inventory.current_generation = ?4
                  )
                  AND NOT EXISTS (
                      SELECT 1
                      FROM temp_catalog_current_paths current
                      WHERE current.source_path = catalog_sessions.source_path
                  )
                "#,
            params![
                provider.as_str(),
                source_root,
                cataloged_at_ms,
                capped_i64(inventory_generation)
            ],
        )?;
        self.conn
            .execute("DELETE FROM temp_catalog_current_paths", [])?;
        Ok(changed)
    }
}
