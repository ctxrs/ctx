impl Store {
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
            self.begin_immediate_batch()?;
            let result = self.mark_catalog_source_missing_paths_stale_paced(
                provider,
                source_root,
                current_paths,
                cataloged_at_ms,
                inventory_generation,
                pace,
            );
            return match result {
                Ok(changed) => {
                    self.commit_batch()?;
                    Ok(changed)
                }
                Err(error) => {
                    let _ = self.rollback_batch();
                    Err(error)
                }
            };
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
