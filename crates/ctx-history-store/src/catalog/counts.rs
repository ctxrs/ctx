impl Store {
    pub fn catalog_session_count(&self) -> Result<usize> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count as usize)
            .map_err(StoreError::from)
    }

    pub fn catalog_session_counts(&self) -> Result<CatalogCounts> {
        let total = self.conn.query_row(
            "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let indexed = self
            .conn
            .query_row(catalog_indexed_count_sql().as_str(), [], |row| {
                row.get::<_, i64>(0)
            })? as usize;
        let stale = self.conn.query_row(
            "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale != 0",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let pending = self.conn.query_row(
            format!(
                "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0 AND {}",
                catalog_pending_import_condition_sql("catalog_sessions")
            )
            .as_str(),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let failed = self.conn.query_row(
                "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0 AND indexed_status = 'failed'",
                [],
                |row| row.get::<_, i64>(0),
            )? as usize;
        let completed_with_rejections = self.conn.query_row(
            "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0 AND indexed_status = 'completed_with_rejections'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let rejected = self.conn.query_row(
            "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0 AND indexed_status = 'rejected'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        Ok(CatalogCounts {
            total,
            indexed,
            stale,
            pending,
            failed,
            completed_with_rejections,
            rejected,
        })
    }

    pub fn source_import_file_counts(&self) -> Result<SourceImportFileCounts> {
        let total = self.conn.query_row(
            "SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let indexed = self.conn.query_row(
            r#"
            SELECT COUNT(*)
            FROM source_import_files
            WHERE is_stale = 0
              AND indexed_status IN ('indexed', 'completed_with_rejections')
              AND indexed_file_size_bytes = file_size_bytes
              AND indexed_file_modified_at_ms = file_modified_at_ms
              AND indexed_import_revision = import_revision
            "#,
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let stale = self.conn.query_row(
            "SELECT COUNT(*) FROM source_import_files WHERE is_stale != 0",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let pending = self.conn.query_row(
            format!(
                "SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND {}",
                source_import_file_pending_condition_sql("source_import_files")
            )
            .as_str(),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let failed = self.conn.query_row(
            "SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND indexed_status = 'failed'",
            [],
            |row| row.get::<_, i64>(0),
            )? as usize;
        let completed_with_rejections = self.conn.query_row(
            "SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND indexed_status = 'completed_with_rejections'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let rejected = self.conn.query_row(
            "SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND indexed_status = 'rejected'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        Ok(SourceImportFileCounts {
            total,
            indexed,
            stale,
            pending,
            failed,
            completed_with_rejections,
            rejected,
        })
    }

    pub fn indexed_history_item_count(&self) -> Result<usize> {
        Ok(self.indexed_history_counts()?.items())
    }

    pub fn indexed_history_counts(&self) -> Result<IndexedHistoryCounts> {
        let sessions: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        let events: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
        Ok(IndexedHistoryCounts {
            sessions: sessions as usize,
            events: events as usize,
        })
    }
}
