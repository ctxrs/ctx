impl Store {
    pub fn catalog_session_count(&self) -> Result<usize> {
        let visible = crate::provider_files::catalog_material_visible_predicate("catalog_sessions");
        self.conn
            .query_row(
                &format!("SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0 AND {visible}"),
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count as usize)
            .map_err(StoreError::from)
    }

    pub fn catalog_session_counts(&self) -> Result<CatalogCounts> {
        let visible = crate::provider_files::catalog_material_visible_predicate("catalog_sessions");
        let total = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0 AND {visible}"),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let indexed = self
            .conn
            .query_row(catalog_indexed_count_sql().as_str(), [], |row| {
                row.get::<_, i64>(0)
            })? as usize;
        let stale = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM catalog_sessions WHERE is_stale != 0 AND {visible}"),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let pending = self.conn.query_row(
            format!(
                "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0 AND {visible} AND {}",
                catalog_pending_import_condition_sql("catalog_sessions"),
            )
            .as_str(),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let failed = self.conn.query_row(
                &format!("SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0 AND indexed_status = 'failed' AND {visible}"),
                [],
                |row| row.get::<_, i64>(0),
            )? as usize;
        let completed_with_rejections = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0 AND indexed_status = 'completed_with_rejections' AND {visible}"),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let rejected = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0 AND indexed_status = 'rejected' AND {visible}"),
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
        let visible = crate::provider_files::source_import_file_material_visible_predicate(
            "source_import_files",
        );
        let total = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND {visible}"),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let indexed = self.conn.query_row(
            &format!(
                r#"
            SELECT COUNT(*)
            FROM source_import_files
            WHERE is_stale = 0
              AND indexed_status IN ('indexed', 'completed_with_rejections')
              AND indexed_file_size_bytes = file_size_bytes
              AND indexed_file_modified_at_ms = file_modified_at_ms
              AND indexed_import_revision = import_revision
              AND {visible}
              AND {}
            "#,
                source_import_material_exists_sql("source_import_files")
            ),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let stale = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM source_import_files WHERE is_stale != 0 AND {visible}"),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let pending = self.conn.query_row(
            format!(
                "SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND {visible} AND {}",
                source_import_file_pending_condition_sql("source_import_files"),
            )
            .as_str(),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let failed = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND indexed_status = 'failed' AND {visible}"),
            [],
            |row| row.get::<_, i64>(0),
            )? as usize;
        let completed_with_rejections = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND indexed_status = 'completed_with_rejections' AND {visible}"),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let rejected = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND indexed_status = 'rejected' AND {visible}"),
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
        let session_visible = crate::provider_files::session_material_visible_predicate("sessions");
        let event_visible = crate::provider_files::event_material_visible_predicate("events");
        let sessions: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM sessions WHERE {session_visible}"),
            [],
            |row| row.get(0),
        )?;
        let events: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM events WHERE {event_visible}"),
            [],
            |row| row.get(0),
        )?;
        Ok(IndexedHistoryCounts {
            sessions: sessions as usize,
            events: events as usize,
        })
    }
}
