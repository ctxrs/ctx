impl Store {
    pub fn catalog_sessions_have_external_path_owners(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        sessions: &[CatalogSession],
    ) -> Result<bool> {
        self.catalog_sessions_have_external_path_owners_paced(
            provider,
            source_root,
            sessions,
            |_| {},
        )
    }

    #[doc(hidden)]
    pub fn catalog_sessions_have_external_path_owners_paced(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        sessions: &[CatalogSession],
        pace: impl Fn(u64),
    ) -> Result<bool> {
        let (_, external_owner) =
            self.catalog_session_path_ownership(provider, source_root, sessions, pace)?;
        Ok(external_owner)
    }

    #[doc(hidden)]
    pub fn catalog_sessions_all_owned_by_source_paced(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        sessions: &[CatalogSession],
        pace: impl Fn(u64),
    ) -> Result<bool> {
        let (all_owned, _) =
            self.catalog_session_path_ownership(provider, source_root, sessions, pace)?;
        Ok(all_owned)
    }

    fn catalog_session_path_ownership(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        sessions: &[CatalogSession],
        pace: impl Fn(u64),
    ) -> Result<(bool, bool)> {
        const QUERY_PATHS: usize = 256;

        let mut all_owned = true;
        let mut external_owner = false;
        for sessions in sessions.chunks(QUERY_PATHS) {
            pace(sessions.iter().fold(0u64, |bytes, session| {
                bytes
                    .saturating_add(session.source_path.len() as u64)
                    .saturating_add(64)
            }));
            let placeholders = vec!["?"; sessions.len()].join(", ");
            let sql = format!(
                "SELECT\n\
                    COALESCE(SUM(CASE WHEN provider = ?1 AND source_root = ?2 THEN 1 ELSE 0 END), 0),\n\
                    COALESCE(SUM(CASE WHEN provider != ?1 OR source_root != ?2 THEN 1 ELSE 0 END), 0)\n\
                 FROM catalog_sessions\n\
                 WHERE source_path IN ({placeholders})"
            );
            let parameters = std::iter::once(provider.as_str())
                .chain(std::iter::once(source_root))
                .chain(sessions.iter().map(|session| session.source_path.as_str()));
            let (owned, external): (usize, usize) =
                self.conn
                    .query_row(&sql, rusqlite::params_from_iter(parameters), |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })?;
            all_owned &= owned == sessions.len();
            external_owner |= external > 0;
        }
        Ok((all_owned, external_owner))
    }

    pub fn mark_catalog_source_missing_paths_stale(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        current_paths: &[String],
        cataloged_at_ms: i64,
        inventory_generation: u64,
    ) -> Result<usize> {
        self.mark_catalog_source_missing_paths_stale_paced(
            provider,
            source_root,
            current_paths,
            cataloged_at_ms,
            inventory_generation,
            |_| {},
        )
    }

    #[doc(hidden)]
    pub fn mark_catalog_source_missing_paths_stale_paced(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        current_paths: &[String],
        cataloged_at_ms: i64,
        inventory_generation: u64,
        pace: impl Fn(u64),
    ) -> Result<usize> {
        if self.conn.is_autocommit() {
            return with_immediate_transaction(&self.conn, || {
                self.mark_catalog_source_missing_paths_stale_paced(
                    provider,
                    source_root,
                    current_paths,
                    cataloged_at_ms,
                    inventory_generation,
                    pace,
                )
            });
        }
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
            for paths in current_paths.chunks(64) {
                pace(paths.iter().fold(0u64, |bytes, path| {
                    bytes.saturating_add(path.len() as u64).saturating_add(64)
                }));
                for path in paths {
                    stmt.execute(params![path.as_str()])?;
                }
            }
        }
        let mut changed = 0usize;
        loop {
            let (batch_rows, batch_bytes) = self.conn.query_row(
                r#"
                SELECT COUNT(*), COALESCE(SUM(length(source_path) + 128), 0)
                FROM (
                    SELECT source_path
                    FROM catalog_sessions
                    WHERE provider = ?1
                      AND source_root = ?2
                      AND is_stale = 0
                      AND EXISTS (
                          SELECT 1
                          FROM import_inventory_generations AS inventory
                          WHERE inventory.provider = ?1
                            AND inventory.source_root = ?2
                            AND inventory.inventory_family = 'catalog_sessions'
                            AND inventory.current_generation = ?3
                      )
                      AND NOT EXISTS (
                          SELECT 1
                          FROM temp_catalog_current_paths current
                          WHERE current.source_path = catalog_sessions.source_path
                      )
                    ORDER BY source_path
                    LIMIT 64
                )
                "#,
                params![
                    provider.as_str(),
                    source_root,
                    capped_i64(inventory_generation)
                ],
                |row| {
                    Ok((
                        row.get::<_, usize>(0)?,
                        nonnegative_i64_to_u64(row.get(1)?)?,
                    ))
                },
            )?;
            if batch_rows == 0 {
                break;
            }
            pace(batch_bytes);
            let batch_changed = self.conn.execute(
                r#"
            UPDATE catalog_sessions
            SET is_stale = 1, cataloged_at_ms = ?3
            WHERE rowid IN (
                SELECT rowid
                FROM catalog_sessions
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
                ORDER BY source_path
                LIMIT 64
              )
            "#,
                params![
                    provider.as_str(),
                    source_root,
                    cataloged_at_ms,
                    capped_i64(inventory_generation)
                ],
            )?;
            changed = changed.saturating_add(batch_changed);
            if batch_changed == 0 {
                break;
            }
        }
        self.conn
            .execute("DELETE FROM temp_catalog_current_paths", [])?;
        Ok(changed)
    }
}
