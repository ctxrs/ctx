impl SemanticVectorStore {
    fn open(path: &Path) -> Result<Self> {
        let _ = register_sqlite_vec_auto_extension();
        if let Some(parent) = path.parent() {
            create_private_dir_all(parent)?;
        }
        if !path.exists() {
            drop(
                private_create_new_file(path)
                    .with_context(|| format!("create semantic vector store {}", path.display()))?,
            );
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open semantic vector store {}", path.display()))?;
        conn.busy_timeout(StdDuration::from_millis(SEMANTIC_VECTOR_BUSY_TIMEOUT_MS))?;
        conn.execute_batch("PRAGMA secure_delete = ON;")?;
        let mut store = Self { conn };
        store.ensure_schema()?;
        secure_semantic_vector_permissions(path)?;
        Ok(store)
    }

    fn open_read_only(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let _ = register_sqlite_vec_auto_extension();
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open semantic vector store read-only {}", path.display()))?;
        conn.busy_timeout(StdDuration::from_millis(SEMANTIC_VECTOR_BUSY_TIMEOUT_MS))?;
        let store = Self { conn };
        store.ensure_readable_schema()?;
        Ok(Some(store))
    }

    fn ensure_readable_schema(&self) -> Result<()> {
        let user_version = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap_or(0);
        if user_version > 5 {
            return Err(anyhow!(
                "semantic vector store schema version {user_version} is newer than this ctx supports"
            ));
        }
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Err(anyhow!(
                "semantic vector store is missing event_embedding_chunks"
            ));
        }
        if !sqlite_table_has_columns(
            &self.conn,
            "event_embedding_chunks",
            &[
                "event_id",
                "model_key",
                "source_text_sha256",
                "start_char",
                "end_char",
                "dimensions",
                "embedding_f32",
            ],
        )? {
            return Err(anyhow!(
                "semantic vector store event_embedding_chunks schema is incomplete"
            ));
        }
        Ok(())
    }

    fn ensure_schema(&mut self) -> Result<()> {
        let user_version = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap_or(0);
        if user_version > 5 {
            return Err(anyhow!(
                "semantic vector store schema version {user_version} is newer than this ctx supports"
            ));
        }
        let mut compact_after_schema = false;
        if sqlite_table_exists(&self.conn, "event_embedding_chunks")?
            && !sqlite_table_has_columns(
                &self.conn,
                "event_embedding_chunks",
                &[
                    "event_id",
                    "model_key",
                    "history_record_id",
                    "session_id",
                    "event_seq",
                    "chunk_index",
                    "chunk_count",
                    "source_text_sha256",
                    "chunk_text_sha256",
                    "chunk_text",
                    "start_char",
                    "end_char",
                    "dimensions",
                    "embedding_f32",
                    "embedded_at_ms",
                ],
            )?
        {
            self.conn.execute("DROP TABLE event_embedding_chunks", [])?;
            compact_after_schema = true;
        }
        self.conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS embedding_models (
                model_key TEXT PRIMARY KEY,
                backend TEXT NOT NULL,
                model_id TEXT NOT NULL,
                dimensions INTEGER NOT NULL,
                distance TEXT NOT NULL,
                normalized INTEGER NOT NULL,
                created_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS event_embeddings (
                event_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                history_record_id TEXT,
                session_id TEXT,
                event_seq INTEGER NOT NULL,
                text_sha256 TEXT NOT NULL,
                preview_text TEXT NOT NULL DEFAULT '',
                dimensions INTEGER NOT NULL,
                embedding_f32 BLOB NOT NULL,
                embedded_at_ms INTEGER NOT NULL,
                PRIMARY KEY (event_id, model_key)
            );
            CREATE INDEX IF NOT EXISTS idx_event_embeddings_model_seq
                ON event_embeddings(model_key, event_seq);
            CREATE INDEX IF NOT EXISTS idx_event_embeddings_model_session
                ON event_embeddings(model_key, session_id);
            CREATE TABLE IF NOT EXISTS event_embedding_chunks (
                event_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                history_record_id TEXT,
                session_id TEXT,
                event_seq INTEGER NOT NULL,
                chunk_index INTEGER NOT NULL,
                chunk_count INTEGER NOT NULL,
                source_text_sha256 TEXT NOT NULL,
                chunk_text_sha256 TEXT NOT NULL,
                chunk_text TEXT NOT NULL DEFAULT '',
                start_char INTEGER NOT NULL,
                end_char INTEGER NOT NULL,
                dimensions INTEGER NOT NULL,
                embedding_f32 BLOB NOT NULL,
                embedded_at_ms INTEGER NOT NULL,
                PRIMARY KEY (event_id, model_key, chunk_index)
            );
            CREATE INDEX IF NOT EXISTS idx_event_embedding_chunks_model_seq
                ON event_embedding_chunks(model_key, event_seq);
            CREATE INDEX IF NOT EXISTS idx_event_embedding_chunks_model_session
                ON event_embedding_chunks(model_key, session_id);
            CREATE INDEX IF NOT EXISTS idx_event_embedding_chunks_model_event
                ON event_embedding_chunks(model_key, event_id);
            CREATE TABLE IF NOT EXISTS semantic_index_stats (
                model_key TEXT PRIMARY KEY,
                embedded_items INTEGER NOT NULL,
                embedded_chunks INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS semantic_dirty_events (
                event_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                queued_at_ms INTEGER NOT NULL,
                priority_seq INTEGER,
                reason TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (event_id, model_key)
            );
            CREATE INDEX IF NOT EXISTS idx_semantic_dirty_events_model_priority
                ON semantic_dirty_events(model_key, priority_seq, queued_at_ms);
            CREATE TABLE IF NOT EXISTS semantic_maintenance_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            PRAGMA user_version = 5;
            "#,
        )?;
        if !sqlite_column_exists(&self.conn, "event_embeddings", "preview_text")? {
            self.conn.execute(
                "ALTER TABLE event_embeddings ADD COLUMN preview_text TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        let foreign_vec0_rows = if sqlite_table_exists(&self.conn, "event_embedding_vec0_meta")?
            && sqlite_column_exists(&self.conn, "event_embedding_vec0_meta", "model_key")?
        {
            self.conn.query_row(
                "SELECT COUNT(*) FROM event_embedding_vec0_meta WHERE model_key != ?1",
                [semantic_model_key()],
                |row| row.get::<_, i64>(0),
            )?
        } else {
            0
        };
        if foreign_vec0_rows > 0 {
            self.drop_sqlite_vec0_schema()?;
        }
        let deleted_legacy_embeddings = self.conn.execute("DELETE FROM event_embeddings", [])?;
        let scrubbed_chunk_text = self.conn.execute(
            "UPDATE event_embedding_chunks SET chunk_text = '' WHERE chunk_text != ''",
            [],
        )?;
        self.conn.execute(
            r#"
            INSERT OR IGNORE INTO embedding_models
                (model_key, backend, model_id, dimensions, distance, normalized, created_at_ms)
            VALUES (?1, ?2, ?3, ?4, 'cosine', 1, ?5)
            "#,
            params![
                semantic_model_key(),
                SEMANTIC_BACKEND,
                SEMANTIC_MODEL_ID,
                SEMANTIC_DIMENSIONS as i64,
                utc_now().timestamp_millis()
            ],
        )?;
        if compact_after_schema || deleted_legacy_embeddings > 0 || scrubbed_chunk_text > 0 {
            self.compact_after_plaintext_scrub()?;
        }
        self.ensure_sqlite_vec0_schema()?;
        Ok(())
    }

    fn compact_after_plaintext_scrub(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            PRAGMA wal_checkpoint(TRUNCATE);
            VACUUM;
            "#,
        )?;
        Ok(())
    }

    fn sqlite_vec0_runtime_available(&self) -> bool {
        if !register_sqlite_vec_auto_extension() {
            return false;
        }
        self.conn
            .query_row("SELECT vec_version()", [], |row| row.get::<_, String>(0))
            .is_ok()
    }

    fn ensure_sqlite_vec0_schema(&mut self) -> Result<()> {
        if !self.sqlite_vec0_runtime_available() {
            return Ok(());
        }
        if !self.sqlite_vec0_schema_compatible()? {
            self.drop_sqlite_vec0_schema()?;
        }
        self.create_sqlite_vec0_schema()?;
        self.sync_sqlite_vec0_from_chunks_if_needed()
    }

    fn sqlite_vec0_schema_compatible(&self) -> Result<bool> {
        let meta_exists = sqlite_table_exists(&self.conn, "event_embedding_vec0_meta")?;
        let vec_exists = sqlite_table_exists(&self.conn, "event_embedding_vec0")?;
        if !meta_exists && !vec_exists {
            return Ok(true);
        }
        if meta_exists != vec_exists {
            return Ok(false);
        }
        if !sqlite_table_has_columns(
            &self.conn,
            "event_embedding_vec0_meta",
            &[
                "rowid",
                "event_id",
                "model_key",
                "history_record_id",
                "session_id",
                "event_seq",
                "chunk_index",
                "source_text_sha256",
                "start_char",
                "end_char",
            ],
        )? {
            return Ok(false);
        }
        let Some(sql) = sqlite_table_sql(&self.conn, "event_embedding_vec0")? else {
            return Ok(false);
        };
        let sql = sql.to_ascii_lowercase();
        Ok(sql.contains("using vec0")
            && sql.contains(&format!("embedding float[{SEMANTIC_DIMENSIONS}]")))
    }

    fn create_sqlite_vec0_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS event_embedding_vec0_meta (
                rowid INTEGER PRIMARY KEY,
                event_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                history_record_id TEXT,
                session_id TEXT,
                event_seq INTEGER NOT NULL,
                chunk_index INTEGER NOT NULL,
                source_text_sha256 TEXT NOT NULL,
                start_char INTEGER NOT NULL,
                end_char INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_event_embedding_vec0_meta_model_event
                ON event_embedding_vec0_meta(model_key, event_id);
            CREATE INDEX IF NOT EXISTS idx_event_embedding_vec0_meta_model_seq
                ON event_embedding_vec0_meta(model_key, event_seq);
            "#,
        )?;
        self.conn.execute_batch(&format!(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS event_embedding_vec0
            USING vec0(embedding float[{SEMANTIC_DIMENSIONS}] distance_metric=cosine);
            "#
        ))?;
        Ok(())
    }

    fn sqlite_vec0_mismatch_count(&self) -> Result<usize> {
        if !self.sqlite_vec0_runtime_available()
            || !sqlite_table_exists(&self.conn, "event_embedding_vec0")?
            || !sqlite_table_exists(&self.conn, "event_embedding_vec0_meta")?
        {
            return Ok(0);
        }
        let missing_or_stale_meta = self
            .conn
            .query_row(
                r#"
	                SELECT COUNT(*)
	                FROM event_embedding_chunks AS c
	                LEFT JOIN event_embedding_vec0_meta AS m
	                  ON m.rowid = c.rowid
	                 AND m.model_key = c.model_key
	                WHERE c.model_key = ?1
	                  AND c.dimensions = ?2
	                  AND (
	                        m.rowid IS NULL
	                     OR m.event_id != c.event_id
	                     OR COALESCE(m.history_record_id, '') != COALESCE(c.history_record_id, '')
	                     OR COALESCE(m.session_id, '') != COALESCE(c.session_id, '')
	                     OR m.event_seq != c.event_seq
	                     OR m.chunk_index != c.chunk_index
	                     OR m.source_text_sha256 != c.source_text_sha256
	                     OR m.start_char != c.start_char
	                     OR m.end_char != c.end_char
	                  )
	                "#,
                params![semantic_model_key(), SEMANTIC_DIMENSIONS as i64],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as usize;
        let orphan_meta = self
            .conn
            .query_row(
                r#"
	                SELECT COUNT(*)
	                FROM event_embedding_vec0_meta AS m
	                LEFT JOIN event_embedding_chunks AS c
	                  ON c.rowid = m.rowid
	                 AND c.model_key = m.model_key
	                 AND c.dimensions = ?2
	                WHERE m.model_key = ?1
	                  AND c.rowid IS NULL
	                "#,
                params![semantic_model_key(), SEMANTIC_DIMENSIONS as i64],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as usize;
        let missing_or_stale_vector = self
            .conn
            .query_row(
                r#"
	                SELECT COUNT(*)
	                FROM event_embedding_chunks AS c
	                LEFT JOIN event_embedding_vec0 AS v
	                  ON v.rowid = c.rowid
	                WHERE c.model_key = ?1
	                  AND c.dimensions = ?2
	                  AND (
	                        v.rowid IS NULL
	                     OR v.embedding != c.embedding_f32
	                  )
	                "#,
                params![semantic_model_key(), SEMANTIC_DIMENSIONS as i64],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as usize;
        Ok(missing_or_stale_meta
            .saturating_add(orphan_meta)
            .saturating_add(missing_or_stale_vector))
    }

    fn drop_sqlite_vec0_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            DROP TABLE IF EXISTS event_embedding_vec0;
            DROP TABLE IF EXISTS event_embedding_vec0_meta;
            "#,
        )?;
        Ok(())
    }

    fn sqlite_vec0_counts(&self) -> Result<Option<(usize, usize, usize)>> {
        if !self.sqlite_vec0_runtime_available()
            || !sqlite_table_exists(&self.conn, "event_embedding_vec0")?
            || !sqlite_table_exists(&self.conn, "event_embedding_vec0_meta")?
        {
            return Ok(None);
        }
        let canonical_chunks = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_embedding_chunks WHERE model_key = ?1 AND dimensions = ?2",
                params![semantic_model_key(), SEMANTIC_DIMENSIONS as i64],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as usize;
        let meta_rows = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_embedding_vec0_meta WHERE model_key = ?1",
                params![semantic_model_key()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as usize;
        let vec_rows = self
            .conn
            .query_row("SELECT COUNT(*) FROM event_embedding_vec0", [], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?
            .unwrap_or(0)
            .max(0) as usize;
        Ok(Some((canonical_chunks, meta_rows, vec_rows)))
    }

    #[cfg(all(test, ctx_sqlite_vec))]
    fn sqlite_vec0_ready(&self) -> Result<bool> {
        let Some((canonical_chunks, meta_rows, vec_rows)) = self.sqlite_vec0_counts()? else {
            return Ok(false);
        };
        if canonical_chunks == 0 || meta_rows != canonical_chunks || vec_rows != canonical_chunks {
            return Ok(false);
        }
        Ok(self.sqlite_vec0_mismatch_count()? == 0)
    }

    fn sqlite_vec0_search_ready(&self) -> Result<bool> {
        let Some((canonical_chunks, meta_rows, vec_rows)) = self.sqlite_vec0_counts()? else {
            return Ok(false);
        };
        Ok(canonical_chunks > 0 && meta_rows == canonical_chunks && vec_rows == canonical_chunks)
    }

    fn sync_sqlite_vec0_from_chunks_if_needed(&mut self) -> Result<()> {
        let Some((canonical_chunks, meta_rows, vec_rows)) = self.sqlite_vec0_counts()? else {
            return Ok(());
        };
        if meta_rows == canonical_chunks
            && vec_rows == canonical_chunks
            && self.sqlite_vec0_mismatch_count()? == 0
        {
            return Ok(());
        }
        self.rebuild_sqlite_vec0_from_chunks()
    }

    fn rebuild_sqlite_vec0_from_chunks(&mut self) -> Result<()> {
        if !self.sqlite_vec0_runtime_available() {
            return Ok(());
        }
        self.drop_sqlite_vec0_schema()?;
        self.create_sqlite_vec0_schema()?;
        let tx = self.conn.transaction()?;
        {
            let mut rows = tx.prepare(
                r#"
	                SELECT rowid, event_id, history_record_id, session_id, event_seq, chunk_index,
	                       source_text_sha256, start_char, end_char, embedding_f32
                FROM event_embedding_chunks
                WHERE model_key = ?1
                  AND dimensions = ?2
                ORDER BY event_seq DESC, chunk_index ASC
                "#,
            )?;
            let mut meta_stmt = tx.prepare(
                r#"
                INSERT INTO event_embedding_vec0_meta
	                    (rowid, event_id, model_key, history_record_id, session_id, event_seq,
	                     chunk_index, source_text_sha256, start_char, end_char)
	                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
            )?;
            let mut vec_stmt =
                tx.prepare("INSERT INTO event_embedding_vec0(rowid, embedding) VALUES (?1, ?2)")?;
            let mut rows = rows.query(params![semantic_model_key(), SEMANTIC_DIMENSIONS as i64])?;
            while let Some(row) = rows.next()? {
                let rowid: i64 = row.get(0)?;
                let event_id: String = row.get(1)?;
                let history_record_id: Option<String> = row.get(2)?;
                let session_id: Option<String> = row.get(3)?;
                let event_seq: i64 = row.get(4)?;
                let chunk_index: i64 = row.get(5)?;
                let source_text_sha256: String = row.get(6)?;
                let start_char: i64 = row.get(7)?;
                let end_char: i64 = row.get(8)?;
                let embedding: Vec<u8> = row.get(9)?;
                meta_stmt.execute(params![
                    rowid,
                    event_id,
                    semantic_model_key(),
                    history_record_id,
                    session_id,
                    event_seq,
                    chunk_index,
                    source_text_sha256,
                    start_char,
                    end_char,
                ])?;
                vec_stmt.execute(params![rowid, embedding])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

}
