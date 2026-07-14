const SEMANTIC_SCHEMA_CLEANUP_STAGE_KEY: &str = "schema_cleanup_stage";
const SEMANTIC_SCHEMA_STATS_CURSOR_KEY: &str = "schema_stats_cursor";
const SEMANTIC_SCHEMA_STATS_ITEMS_KEY: &str = "schema_stats_items";
const SEMANTIC_SCHEMA_STATS_CHUNKS_KEY: &str = "schema_stats_chunks";
const SEMANTIC_VEC0_REBUILD_STAGE_KEY: &str = "vec0_rebuild_stage";
const SEMANTIC_VEC0_REBUILD_CURSOR_KEY: &str = "vec0_rebuild_cursor";
const SEMANTIC_VEC0_REBUILD_COMPLETE_KEY: &str = "vec0_rebuild_complete_v1";
const SEMANTIC_VEC0_ACTIVE_GENERATION_KEY: &str = "vec0_active_generation";
const SEMANTIC_VEC0_REBUILD_GENERATION_KEY: &str = "vec0_rebuild_generation";
const SEMANTIC_VEC0_REBUILD_ROWS_KEY: &str = "vec0_rebuild_rows";
const SEMANTIC_VEC0_VALIDATION_STAGE_KEY: &str = "vec0_validation_stage";
const SEMANTIC_VEC0_VALIDATION_CURSOR_KEY: &str = "vec0_validation_cursor";
const SEMANTIC_VEC0_VALIDATION_TARGET_REVISION_KEY: &str = "vec0_validation_target_revision";
const SEMANTIC_VEC0_VALIDATION_TARGET_GENERATION_KEY: &str =
    "vec0_validation_target_generation";
const SEMANTIC_VEC0_VALIDATED_REVISION_KEY: &str = "vec0_validated_revision";
const SEMANTIC_VEC0_VALIDATED_GENERATION_KEY: &str = "vec0_validated_generation";
const SEMANTIC_MAINTENANCE_BATCH_ROWS: usize = 32;
const SEMANTIC_VEC0_ESTIMATED_BYTES_PER_CHUNK: u64 = 3 * 1024;

fn sqlite_vec0_generation_tables(generation: i64) -> Option<(&'static str, &'static str)> {
    match generation {
        1 => Some(("event_embedding_vec0_a", "event_embedding_vec0_meta_a")),
        2 => Some(("event_embedding_vec0_b", "event_embedding_vec0_meta_b")),
        _ => None,
    }
}

fn inactive_sqlite_vec0_generation(active: Option<i64>) -> i64 {
    if active == Some(1) { 2 } else { 1 }
}

fn semantic_vec0_remaining_write_estimate(total_chunks: u64, written_chunks: u64) -> u64 {
    total_chunks
        .saturating_sub(written_chunks)
        .saturating_mul(SEMANTIC_VEC0_ESTIMATED_BYTES_PER_CHUNK)
        .max(INDEXING_WAL_DELTA_BYTES)
}

impl SemanticVectorStore {
    fn open(path: &Path) -> Result<Self> {
        let relational_path = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("work.sqlite");
        let write_admission = IndexingAdmission::acquire(
            &relational_path,
            IndexingWorkClass::Background,
        )
        .context("acquire shared semantic writer admission")?;
        let mut open_lease = write_admission
            .acquire_external_writer(path, 0, "semantic vector open")
            .context("admit semantic vector open")?;
        let existed = path.exists();
        if !existed {
            open_lease.require_growth(
                INDEXING_WAL_DELTA_BYTES,
                "semantic vector store creation",
            )?;
        }
        let _ = register_sqlite_vec_auto_extension();
        if let Some(parent) = path.parent() {
            create_private_dir_all(parent)?;
        }
        if !existed {
            drop(
                private_create_new_file(path)
                    .with_context(|| format!("create semantic vector store {}", path.display()))?,
            );
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open semantic vector store {}", path.display()))?;
        conn.busy_timeout(StdDuration::from_millis(SEMANTIC_VECTOR_BUSY_TIMEOUT_MS))?;
        conn.execute_batch("PRAGMA secure_delete = ON;")?;
        let mut store = Self {
            conn,
            path: path.to_path_buf(),
            write_admission: Some(write_admission),
        };
        if !store.writable_schema_current()? {
            let estimated = sqlite_amplifying_write_estimate(
                path,
                3,
                INDEXING_WAL_DELTA_BYTES,
            )?;
            open_lease.require_growth(estimated, "semantic vector schema maintenance")?;
            store.ensure_schema()?;
        }
        if store.sqlite_vec0_rebuild_required()? {
            let estimated = sqlite_amplifying_write_estimate(
                path,
                2,
                INDEXING_WAL_DELTA_BYTES,
            )?;
            open_lease.require_growth(estimated, "semantic vec0 schema maintenance")?;
            store.schedule_sqlite_vec0_rebuild_if_needed()?;
        }
        let repair_request = semantic_vector_repair_request_path(path);
        if repair_request.exists() && store.sqlite_vec0_runtime_available() {
            let estimated = sqlite_amplifying_write_estimate(
                path,
                2,
                INDEXING_WAL_DELTA_BYTES,
            )?;
            open_lease.require_growth(estimated, "semantic vec0 repair scheduling")?;
            if let Some(active_generation) =
                store.maintenance_state_i64(SEMANTIC_VEC0_ACTIVE_GENERATION_KEY)?
            {
                schedule_sqlite_vec0_repair_conn(&store.conn, active_generation)?;
            } else {
                store.schedule_sqlite_vec0_rebuild_if_needed()?;
            }
            fs::remove_file(&repair_request).with_context(|| {
                format!(
                    "remove consumed semantic repair request {}",
                    repair_request.display()
                )
            })?;
        }
        secure_semantic_vector_permissions(path)?;
        drop(open_lease);
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
        let store = Self {
            conn,
            path: path.to_path_buf(),
            write_admission: None,
        };
        store.ensure_readable_schema()?;
        Ok(Some(store))
    }

    fn acquire_write_lease(
        &self,
        estimated_write_bytes: u64,
        operation: &'static str,
    ) -> Result<ctx_history_store::ExternalIndexingWriterLease> {
        self.write_admission
            .as_ref()
            .ok_or_else(|| anyhow!("semantic vector store is read-only"))?
            .acquire_external_writer(&self.path, estimated_write_bytes, operation)
            .map_err(anyhow::Error::from)
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

    fn writable_schema_current(&self) -> Result<bool> {
        let user_version = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap_or(0);
        if user_version != 5 {
            return Ok(false);
        }
        for table in [
            "embedding_models",
            "event_embedding_chunks",
            "semantic_index_stats",
            "semantic_dirty_events",
            "semantic_maintenance_state",
        ] {
            if !sqlite_table_exists(&self.conn, table)? {
                return Ok(false);
            }
        }
        if !sqlite_table_has_columns(
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
        )? {
            return Ok(false);
        }
        for index in [
            "idx_event_embedding_chunks_model_seq",
            "idx_event_embedding_chunks_model_session",
            "idx_event_embedding_chunks_model_event",
            "idx_semantic_dirty_events_model_priority",
        ] {
            if !sqlite_index_exists(&self.conn, index)? {
                return Ok(false);
            }
        }
        Ok(true)
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
        let chunks_incompatible = sqlite_table_exists(&self.conn, "event_embedding_chunks")?
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
            )?;
        if chunks_incompatible {
            let legacy_table = if !sqlite_table_exists(
                &self.conn,
                "event_embedding_chunks_legacy_v5",
            )? {
                "event_embedding_chunks_legacy_v5"
            } else if !sqlite_table_exists(
                &self.conn,
                "event_embedding_chunks_legacy_v5_pending",
            )? {
                "event_embedding_chunks_legacy_v5_pending"
            } else {
                return Err(anyhow!(
                    "semantic vector migration has multiple undrained legacy chunk generations"
                ));
            };
            self.conn.execute(
                &format!("ALTER TABLE event_embedding_chunks RENAME TO {legacy_table}"),
                [],
            )?;
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
        let stats_exist = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM semantic_index_stats WHERE model_key = ?1)",
            [semantic_model_key()],
            |row| row.get::<_, bool>(0),
        )?;
        if !stats_exist && !table_has_any_rows(&self.conn, "event_embedding_chunks")? {
            self.conn.execute(
                r#"
                INSERT INTO semantic_index_stats
                    (model_key, embedded_items, embedded_chunks, updated_at_ms)
                VALUES (?1, 0, 0, ?2)
                "#,
                params![semantic_model_key(), utc_now().timestamp_millis()],
            )?;
        }
        if table_has_any_rows(&self.conn, "event_embeddings")?
            || table_has_matching_rows(
                &self.conn,
                "event_embedding_chunks",
                "chunk_text != ''",
            )?
            || table_has_any_rows(&self.conn, "event_embedding_chunks_legacy_v5")?
            || table_has_any_rows(
                &self.conn,
                "event_embedding_chunks_legacy_v5_pending",
            )?
        {
            self.set_maintenance_state_i64(SEMANTIC_SCHEMA_CLEANUP_STAGE_KEY, 1)?;
        } else if !stats_exist && table_has_any_rows(&self.conn, "event_embedding_chunks")? {
            self.set_maintenance_state_i64(SEMANTIC_SCHEMA_CLEANUP_STAGE_KEY, 4)?;
            self.set_maintenance_state_i64(SEMANTIC_SCHEMA_STATS_CURSOR_KEY, 0)?;
            self.set_maintenance_state_i64(SEMANTIC_SCHEMA_STATS_ITEMS_KEY, 0)?;
            self.set_maintenance_state_i64(SEMANTIC_SCHEMA_STATS_CHUNKS_KEY, 0)?;
        }
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

    fn sqlite_vec0_schema_compatible(&self) -> Result<bool> {
        let Some(generation) = self
            .maintenance_state_i64(SEMANTIC_VEC0_ACTIVE_GENERATION_KEY)?
        else {
            return Ok(false);
        };
        self.sqlite_vec0_generation_schema_compatible(generation)
    }

    fn sqlite_vec0_generation_schema_compatible(&self, generation: i64) -> Result<bool> {
        let Some((vec_table, meta_table)) = sqlite_vec0_generation_tables(generation) else {
            return Ok(false);
        };
        let meta_exists = sqlite_table_exists(&self.conn, meta_table)?;
        let vec_exists = sqlite_table_exists(&self.conn, vec_table)?;
        if !meta_exists && !vec_exists {
            return Ok(true);
        }
        if meta_exists != vec_exists {
            return Ok(false);
        }
        if !sqlite_table_has_columns(
            &self.conn,
            meta_table,
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
        let Some(sql) = sqlite_table_sql(&self.conn, vec_table)? else {
            return Ok(false);
        };
        let sql = sql.to_ascii_lowercase();
        Ok(sql.contains("using vec0")
            && sql.contains(&format!("embedding float[{SEMANTIC_DIMENSIONS}]")))
    }

    fn create_sqlite_vec0_generation_schema(&self, generation: i64) -> Result<()> {
        Self::create_sqlite_vec0_generation_schema_conn(&self.conn, generation)
    }

    fn create_sqlite_vec0_generation_schema_conn(
        conn: &Connection,
        generation: i64,
    ) -> Result<()> {
        let Some((vec_table, meta_table)) = sqlite_vec0_generation_tables(generation) else {
            return Err(anyhow!("invalid semantic vec0 generation {generation}"));
        };
        conn.execute_batch(&format!(
            r#"
            CREATE TABLE IF NOT EXISTS {meta_table} (
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
            CREATE INDEX IF NOT EXISTS idx_{meta_table}_model_event
                ON {meta_table}(model_key, event_id);
            CREATE INDEX IF NOT EXISTS idx_{meta_table}_model_seq
                ON {meta_table}(model_key, event_seq);
            "#
        ))?;
        conn.execute_batch(&format!(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS {vec_table}
            USING vec0(embedding float[{SEMANTIC_DIMENSIONS}] distance_metric=cosine);
            "#
        ))?;
        Ok(())
    }

    #[cfg(all(test, ctx_sqlite_vec))]
    fn sqlite_vec0_mismatch_count(&self) -> Result<usize> {
        let Some(generation) = self
            .maintenance_state_i64(SEMANTIC_VEC0_ACTIVE_GENERATION_KEY)?
        else {
            return Ok(0);
        };
        let Some((vec_table, meta_table)) = sqlite_vec0_generation_tables(generation) else {
            return Ok(0);
        };
        if !self.sqlite_vec0_runtime_available()
            || !sqlite_table_exists(&self.conn, vec_table)?
            || !sqlite_table_exists(&self.conn, meta_table)?
        {
            return Ok(0);
        }
        let missing_or_stale_meta = self
            .conn
            .query_row(
                &format!(r#"
	                SELECT COUNT(*)
	                FROM event_embedding_chunks AS c
	                LEFT JOIN {meta_table} AS m
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
	                "#),
                params![semantic_model_key(), SEMANTIC_DIMENSIONS as i64],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as usize;
        let orphan_meta = self
            .conn
            .query_row(
	                &format!(r#"
	                SELECT COUNT(*)
	                FROM {meta_table} AS m
	                LEFT JOIN event_embedding_chunks AS c
	                  ON c.rowid = m.rowid
	                 AND c.model_key = m.model_key
	                 AND c.dimensions = ?2
	                WHERE m.model_key = ?1
	                  AND c.rowid IS NULL
	                "#),
                params![semantic_model_key(), SEMANTIC_DIMENSIONS as i64],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as usize;
        let missing_or_stale_vector = self
            .conn
            .query_row(
	                &format!(r#"
	                SELECT COUNT(*)
	                FROM event_embedding_chunks AS c
	                LEFT JOIN {vec_table} AS v
	                  ON v.rowid = c.rowid
	                WHERE c.model_key = ?1
	                  AND c.dimensions = ?2
	                  AND (
	                        v.rowid IS NULL
	                     OR v.embedding != c.embedding_f32
	                  )
	                "#),
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

    #[cfg(all(test, ctx_sqlite_vec))]
    fn sqlite_vec0_counts(&self) -> Result<Option<(usize, usize, usize)>> {
        let Some(generation) = self
            .maintenance_state_i64(SEMANTIC_VEC0_ACTIVE_GENERATION_KEY)?
        else {
            return Ok(None);
        };
        let Some((vec_table, meta_table)) = sqlite_vec0_generation_tables(generation) else {
            return Ok(None);
        };
        if !self.sqlite_vec0_runtime_available()
            || !sqlite_table_exists(&self.conn, vec_table)?
            || !sqlite_table_exists(&self.conn, meta_table)?
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
                &format!("SELECT COUNT(*) FROM {meta_table} WHERE model_key = ?1"),
                params![semantic_model_key()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as usize;
        let vec_rows = self
            .conn
            .query_row(&format!("SELECT COUNT(*) FROM {vec_table}"), [], |row| {
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

    fn sqlite_vec0_search_ready(&self, relational_revision: u64) -> Result<bool> {
        let Some(generation) = self
            .maintenance_state_i64(SEMANTIC_VEC0_ACTIVE_GENERATION_KEY)?
        else {
            return Ok(false);
        };
        let Some((vec_table, meta_table)) = sqlite_vec0_generation_tables(generation) else {
            return Ok(false);
        };
        if self
            .maintenance_state_i64(SEMANTIC_VEC0_REBUILD_COMPLETE_KEY)?
            != Some(1)
            || !self.sqlite_vec0_schema_compatible()?
            || !sqlite_table_exists(&self.conn, vec_table)?
            || !sqlite_table_exists(&self.conn, meta_table)?
            || !self.sqlite_vec0_validation_is_current(relational_revision)?
        {
            return Ok(false);
        }
        table_has_any_rows(&self.conn, vec_table)
    }

    fn schedule_sqlite_vec0_rebuild_if_needed(&self) -> Result<()> {
        if !self.sqlite_vec0_runtime_available() {
            return Ok(());
        }
        let complete = self
            .maintenance_state_i64(SEMANTIC_VEC0_REBUILD_COMPLETE_KEY)?
            == Some(1);
        if complete && self.sqlite_vec0_schema_compatible()? {
            return Ok(());
        }
        let active = self.maintenance_state_i64(SEMANTIC_VEC0_ACTIVE_GENERATION_KEY)?;
        let rebuild = self
            .maintenance_state_i64(SEMANTIC_VEC0_REBUILD_GENERATION_KEY)?
            .and_then(|generation| sqlite_vec0_generation_tables(generation).map(|_| generation))
            .unwrap_or_else(|| inactive_sqlite_vec0_generation(active));
        self.create_sqlite_vec0_generation_schema(rebuild)?;
        self.set_maintenance_state_i64(SEMANTIC_VEC0_REBUILD_GENERATION_KEY, rebuild)?;
        if self
            .maintenance_state_i64(SEMANTIC_VEC0_REBUILD_STAGE_KEY)?
            .is_none()
        {
            self.set_maintenance_state_i64(SEMANTIC_VEC0_REBUILD_STAGE_KEY, 1)?;
            self.set_maintenance_state_i64(SEMANTIC_VEC0_REBUILD_CURSOR_KEY, 0)?;
        }
        Ok(())
    }

    fn sqlite_vec0_rebuild_required(&self) -> Result<bool> {
        if !self.sqlite_vec0_runtime_available() {
            return Ok(false);
        }
        Ok(self
            .maintenance_state_i64(SEMANTIC_VEC0_REBUILD_COMPLETE_KEY)?
            != Some(1)
            || !self.sqlite_vec0_schema_compatible()?)
    }

    fn maintenance_pending(&self, relational_revision: u64) -> Result<bool> {
        Ok(self
            .maintenance_state_i64(SEMANTIC_SCHEMA_CLEANUP_STAGE_KEY)?
            .is_some()
            || self
                .maintenance_state_i64(SEMANTIC_VEC0_REBUILD_STAGE_KEY)?
                .is_some()
            || (self.sqlite_vec0_runtime_available()
                && !self.sqlite_vec0_validation_is_current(relational_revision)?))
    }

    fn run_maintenance_slice(&mut self, relational_revision: u64) -> Result<bool> {
        if !self.maintenance_pending(relational_revision)? {
            return Ok(false);
        }
        let schema_cleanup = self
            .maintenance_state_i64(SEMANTIC_SCHEMA_CLEANUP_STAGE_KEY)?
            .is_some();
        let vec0_stage = self.maintenance_state_i64(SEMANTIC_VEC0_REBUILD_STAGE_KEY)?;
        let rebuild_generation = self
            .maintenance_state_i64(SEMANTIC_VEC0_REBUILD_GENERATION_KEY)?;
        let rebuild_schema_missing = if !schema_cleanup && vec0_stage == Some(1) {
            if let Some((vec_table, meta_table)) =
                rebuild_generation.and_then(sqlite_vec0_generation_tables)
            {
                !sqlite_table_exists(&self.conn, vec_table)?
                    || !sqlite_table_exists(&self.conn, meta_table)?
            } else {
                false
            }
        } else {
            false
        };
        let estimated_growth = if rebuild_schema_missing {
            sqlite_amplifying_write_estimate(
                &self.path,
                2,
                INDEXING_WAL_DELTA_BYTES,
            )?
        } else if !schema_cleanup && vec0_stage == Some(2) {
            let total = self.cached_stats()?.map_or(0, |stats| stats.embedded_chunks as u64);
            let written = self
                .maintenance_state_i64(SEMANTIC_VEC0_REBUILD_ROWS_KEY)?
                .unwrap_or(0)
                .max(0) as u64;
            semantic_vec0_remaining_write_estimate(total, written)
        } else if !schema_cleanup {
            INDEXING_WAL_DELTA_BYTES
        } else {
            0
        };
        let lease = self.acquire_write_lease(estimated_growth, "semantic vector maintenance")?;
        if rebuild_schema_missing {
            self.create_sqlite_vec0_generation_schema(
                rebuild_generation.ok_or_else(|| anyhow!("semantic vec0 repair has no target"))?,
            )?;
        }
        let tx = self.conn.transaction()?;
        if schema_cleanup {
            run_semantic_schema_cleanup_slice(&tx)?;
        } else if vec0_stage.is_some() {
            run_sqlite_vec0_rebuild_slice(&tx)?;
        } else {
            run_sqlite_vec0_validation_slice(&tx, relational_revision)?;
        }
        commit_semantic_transaction(tx, || {
            Ok(lease.revalidate_growth("semantic vector maintenance")?)
        })?;
        self.maintenance_pending(relational_revision)
    }

    fn sqlite_vec0_validation_is_current(&self, relational_revision: u64) -> Result<bool> {
        let Some(active_generation) = self
            .maintenance_state_i64(SEMANTIC_VEC0_ACTIVE_GENERATION_KEY)?
        else {
            return Ok(false);
        };
        Ok(self
            .maintenance_state_i64(SEMANTIC_VEC0_VALIDATED_REVISION_KEY)?
            == Some(relational_revision.min(i64::MAX as u64) as i64)
            && self
                .maintenance_state_i64(SEMANTIC_VEC0_VALIDATED_GENERATION_KEY)?
                == Some(active_generation)
            && self
                .maintenance_state_i64(SEMANTIC_VEC0_VALIDATION_STAGE_KEY)?
                .is_none())
    }

}

fn semantic_state_i64_conn(conn: &Connection, key: &str) -> Result<Option<i64>> {
    let key = SemanticVectorStore::maintenance_state_key(key);
    let value = conn
        .query_row(
            "SELECT value FROM semantic_maintenance_state WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(value.and_then(|value| value.parse::<i64>().ok()))
}

fn set_semantic_state_i64_conn(conn: &Connection, key: &str, value: i64) -> Result<()> {
    let key = SemanticVectorStore::maintenance_state_key(key);
    conn.execute(
        r#"
        INSERT INTO semantic_maintenance_state (key, value, updated_at_ms)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            updated_at_ms = excluded.updated_at_ms
        "#,
        params![key, value.to_string(), utc_now().timestamp_millis()],
    )?;
    Ok(())
}

fn delete_semantic_state_conn(conn: &Connection, key: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM semantic_maintenance_state WHERE key = ?1",
        [SemanticVectorStore::maintenance_state_key(key)],
    )?;
    Ok(())
}

fn run_semantic_schema_cleanup_slice(conn: &Connection) -> Result<()> {
    let stage = semantic_state_i64_conn(conn, SEMANTIC_SCHEMA_CLEANUP_STAGE_KEY)?.unwrap_or(1);
    let changed = match stage {
        1 => delete_rows_bounded(conn, "event_embeddings", None)?,
        2 => {
            if !sqlite_table_exists(conn, "event_embedding_chunks")? {
                0
            } else {
                conn.execute(
                    r#"
                    UPDATE event_embedding_chunks
                    SET chunk_text = ''
                    WHERE rowid IN (
                        SELECT rowid FROM event_embedding_chunks
                        WHERE chunk_text != ''
                        ORDER BY rowid
                        LIMIT ?1
                    )
                    "#,
                    [SEMANTIC_MAINTENANCE_BATCH_ROWS as i64],
                )?
            }
        }
        3 => {
            let deleted = delete_rows_bounded(conn, "event_embedding_chunks_legacy_v5", None)?;
            if deleted > 0 {
                deleted
            } else {
                delete_rows_bounded(conn, "event_embedding_chunks_legacy_v5_pending", None)?
            }
        }
        4 => {
            run_semantic_stats_rebuild_slice(conn)?;
            return Ok(());
        }
        _ => 0,
    };
    if changed > 0 {
        return Ok(());
    }
    match stage {
        1 | 2 => set_semantic_state_i64_conn(
            conn,
            SEMANTIC_SCHEMA_CLEANUP_STAGE_KEY,
            stage + 1,
        ),
        3 => {
            conn.execute_batch(
                "DROP TABLE IF EXISTS event_embedding_chunks_legacy_v5;
                 DROP TABLE IF EXISTS event_embedding_chunks_legacy_v5_pending;",
            )?;
            set_semantic_state_i64_conn(conn, SEMANTIC_SCHEMA_CLEANUP_STAGE_KEY, 4)?;
            set_semantic_state_i64_conn(conn, SEMANTIC_SCHEMA_STATS_CURSOR_KEY, 0)?;
            set_semantic_state_i64_conn(conn, SEMANTIC_SCHEMA_STATS_ITEMS_KEY, 0)?;
            set_semantic_state_i64_conn(conn, SEMANTIC_SCHEMA_STATS_CHUNKS_KEY, 0)
        }
        _ => delete_semantic_state_conn(conn, SEMANTIC_SCHEMA_CLEANUP_STAGE_KEY),
    }
}

fn run_semantic_stats_rebuild_slice(conn: &Connection) -> Result<()> {
    let cursor = semantic_state_i64_conn(conn, SEMANTIC_SCHEMA_STATS_CURSOR_KEY)?.unwrap_or(0);
    let row = conn
        .query_row(
            r#"
            SELECT MAX(rowid), COUNT(*), COALESCE(SUM(CASE WHEN chunk_index = 0 THEN 1 ELSE 0 END), 0)
            FROM (
                SELECT rowid, chunk_index
                FROM event_embedding_chunks
                WHERE model_key = ?1 AND rowid > ?2
                ORDER BY rowid
                LIMIT ?3
            )
            "#,
            params![
                semantic_model_key(),
                cursor,
                SEMANTIC_MAINTENANCE_BATCH_ROWS as i64,
            ],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
    let Some(last_rowid) = row.0 else {
        let items = semantic_state_i64_conn(conn, SEMANTIC_SCHEMA_STATS_ITEMS_KEY)?.unwrap_or(0);
        let chunks = semantic_state_i64_conn(conn, SEMANTIC_SCHEMA_STATS_CHUNKS_KEY)?.unwrap_or(0);
        conn.execute(
            r#"
            INSERT INTO semantic_index_stats
                (model_key, embedded_items, embedded_chunks, updated_at_ms)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(model_key) DO UPDATE SET
                embedded_items = excluded.embedded_items,
                embedded_chunks = excluded.embedded_chunks,
                updated_at_ms = excluded.updated_at_ms
            "#,
            params![
                semantic_model_key(),
                items.max(0),
                chunks.max(0),
                utc_now().timestamp_millis(),
            ],
        )?;
        for key in [
            SEMANTIC_SCHEMA_CLEANUP_STAGE_KEY,
            SEMANTIC_SCHEMA_STATS_CURSOR_KEY,
            SEMANTIC_SCHEMA_STATS_ITEMS_KEY,
            SEMANTIC_SCHEMA_STATS_CHUNKS_KEY,
        ] {
            delete_semantic_state_conn(conn, key)?;
        }
        return Ok(());
    };
    set_semantic_state_i64_conn(conn, SEMANTIC_SCHEMA_STATS_CURSOR_KEY, last_rowid)?;
    let items = semantic_state_i64_conn(conn, SEMANTIC_SCHEMA_STATS_ITEMS_KEY)?
        .unwrap_or(0)
        .saturating_add(row.2);
    let chunks = semantic_state_i64_conn(conn, SEMANTIC_SCHEMA_STATS_CHUNKS_KEY)?
        .unwrap_or(0)
        .saturating_add(row.1);
    set_semantic_state_i64_conn(conn, SEMANTIC_SCHEMA_STATS_ITEMS_KEY, items)?;
    set_semantic_state_i64_conn(conn, SEMANTIC_SCHEMA_STATS_CHUNKS_KEY, chunks)
}

fn run_sqlite_vec0_rebuild_slice(conn: &Connection) -> Result<()> {
    let Some(stage) = semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_STAGE_KEY)? else {
        return Ok(());
    };
    let rebuild_generation = semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_GENERATION_KEY)?
        .ok_or_else(|| anyhow!("semantic vec0 rebuild has no target generation"))?;
    let (rebuild_vec, rebuild_meta) = sqlite_vec0_generation_tables(rebuild_generation)
        .ok_or_else(|| anyhow!("invalid semantic vec0 rebuild generation {rebuild_generation}"))?;
    match stage {
        1 => {
            let vec_deleted = delete_rows_bounded(conn, rebuild_vec, None)?;
            let meta_deleted = if vec_deleted == 0 {
                delete_rows_bounded(conn, rebuild_meta, None)?
            } else {
                0
            };
            if vec_deleted == 0 && meta_deleted == 0 {
                set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_STAGE_KEY, 2)?;
                set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_CURSOR_KEY, 0)?;
                set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_ROWS_KEY, 0)?;
            }
        }
        2 => fill_sqlite_vec0_rebuild_slice(conn, rebuild_vec, rebuild_meta)?,
        3 => publish_sqlite_vec0_rebuild(conn, rebuild_generation)?,
        4 => cleanup_old_sqlite_vec0_slice(conn, rebuild_generation)?,
        _ => {
            delete_semantic_state_conn(conn, SEMANTIC_VEC0_REBUILD_STAGE_KEY)?;
            delete_semantic_state_conn(conn, SEMANTIC_VEC0_REBUILD_CURSOR_KEY)?;
            delete_semantic_state_conn(conn, SEMANTIC_VEC0_REBUILD_GENERATION_KEY)?;
            delete_semantic_state_conn(conn, SEMANTIC_VEC0_REBUILD_ROWS_KEY)?;
        }
    }
    Ok(())
}

fn run_sqlite_vec0_validation_slice(
    conn: &Connection,
    relational_revision: u64,
) -> Result<()> {
    let active_generation = semantic_state_i64_conn(conn, SEMANTIC_VEC0_ACTIVE_GENERATION_KEY)?
        .ok_or_else(|| anyhow!("semantic vec0 validation has no active generation"))?;
    let (active_vec, active_meta) = sqlite_vec0_generation_tables(active_generation)
        .ok_or_else(|| anyhow!("invalid semantic vec0 validation generation"))?;
    let revision = relational_revision.min(i64::MAX as u64) as i64;
    let target_revision =
        semantic_state_i64_conn(conn, SEMANTIC_VEC0_VALIDATION_TARGET_REVISION_KEY)?;
    let target_generation =
        semantic_state_i64_conn(conn, SEMANTIC_VEC0_VALIDATION_TARGET_GENERATION_KEY)?;
    if target_revision != Some(revision) || target_generation != Some(active_generation) {
        set_semantic_state_i64_conn(
            conn,
            SEMANTIC_VEC0_VALIDATION_TARGET_REVISION_KEY,
            revision,
        )?;
        set_semantic_state_i64_conn(
            conn,
            SEMANTIC_VEC0_VALIDATION_TARGET_GENERATION_KEY,
            active_generation,
        )?;
        set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_VALIDATION_STAGE_KEY, 1)?;
        set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_VALIDATION_CURSOR_KEY, 0)?;
        delete_semantic_state_conn(conn, SEMANTIC_VEC0_VALIDATED_REVISION_KEY)?;
        delete_semantic_state_conn(conn, SEMANTIC_VEC0_VALIDATED_GENERATION_KEY)?;
    }

    let stage = semantic_state_i64_conn(conn, SEMANTIC_VEC0_VALIDATION_STAGE_KEY)?.unwrap_or(1);
    let cursor = semantic_state_i64_conn(conn, SEMANTIC_VEC0_VALIDATION_CURSOR_KEY)?.unwrap_or(0);
    let (rows, mismatch) = match stage {
        1 => validate_vec0_canonical_slice(conn, active_vec, active_meta, cursor)?,
        2 => validate_vec0_meta_slice(conn, active_meta, cursor)?,
        3 => validate_vec0_vector_slice(conn, active_vec, cursor)?,
        _ => (Vec::new(), false),
    };
    if mismatch {
        schedule_sqlite_vec0_repair_conn(conn, active_generation)?;
        return Ok(());
    }
    if let Some(last_rowid) = rows.last() {
        set_semantic_state_i64_conn(
            conn,
            SEMANTIC_VEC0_VALIDATION_CURSOR_KEY,
            *last_rowid,
        )?;
        return Ok(());
    }
    if stage < 3 {
        set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_VALIDATION_STAGE_KEY, stage + 1)?;
        set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_VALIDATION_CURSOR_KEY, 0)?;
        return Ok(());
    }
    set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_VALIDATED_REVISION_KEY, revision)?;
    set_semantic_state_i64_conn(
        conn,
        SEMANTIC_VEC0_VALIDATED_GENERATION_KEY,
        active_generation,
    )?;
    for key in [
        SEMANTIC_VEC0_VALIDATION_STAGE_KEY,
        SEMANTIC_VEC0_VALIDATION_CURSOR_KEY,
        SEMANTIC_VEC0_VALIDATION_TARGET_REVISION_KEY,
        SEMANTIC_VEC0_VALIDATION_TARGET_GENERATION_KEY,
    ] {
        delete_semantic_state_conn(conn, key)?;
    }
    Ok(())
}

fn validate_vec0_canonical_slice(
    conn: &Connection,
    active_vec: &str,
    active_meta: &str,
    cursor: i64,
) -> Result<(Vec<i64>, bool)> {
    let mut stmt = conn.prepare(&format!(
        r#"
        SELECT c.rowid,
               m.rowid IS NULL
               OR m.event_id != c.event_id
               OR m.model_key != c.model_key
               OR COALESCE(m.history_record_id, '') != COALESCE(c.history_record_id, '')
               OR COALESCE(m.session_id, '') != COALESCE(c.session_id, '')
               OR m.event_seq != c.event_seq
               OR m.chunk_index != c.chunk_index
               OR m.source_text_sha256 != c.source_text_sha256
               OR m.start_char != c.start_char
               OR m.end_char != c.end_char
               OR v.rowid IS NULL
               OR v.embedding != c.embedding_f32
        FROM event_embedding_chunks AS c
        LEFT JOIN {active_meta} AS m ON m.rowid = c.rowid
        LEFT JOIN {active_vec} AS v ON v.rowid = c.rowid
        WHERE c.model_key = ?1 AND c.dimensions = ?2 AND c.rowid > ?3
        ORDER BY c.rowid
        LIMIT ?4
        "#
    ))?;
    collect_vec0_validation_rows(&mut stmt, params![
        semantic_model_key(),
        SEMANTIC_DIMENSIONS as i64,
        cursor,
        SEMANTIC_MAINTENANCE_BATCH_ROWS as i64,
    ])
}

fn validate_vec0_meta_slice(
    conn: &Connection,
    active_meta: &str,
    cursor: i64,
) -> Result<(Vec<i64>, bool)> {
    let mut stmt = conn.prepare(&format!(
        r#"
        SELECT m.rowid, c.rowid IS NULL
        FROM {active_meta} AS m
        LEFT JOIN event_embedding_chunks AS c
          ON c.rowid = m.rowid AND c.model_key = m.model_key AND c.dimensions = ?2
        WHERE m.model_key = ?1 AND m.rowid > ?3
        ORDER BY m.rowid
        LIMIT ?4
        "#
    ))?;
    collect_vec0_validation_rows(&mut stmt, params![
        semantic_model_key(),
        SEMANTIC_DIMENSIONS as i64,
        cursor,
        SEMANTIC_MAINTENANCE_BATCH_ROWS as i64,
    ])
}

fn validate_vec0_vector_slice(
    conn: &Connection,
    active_vec: &str,
    cursor: i64,
) -> Result<(Vec<i64>, bool)> {
    let mut stmt = conn.prepare(&format!(
        r#"
        SELECT v.rowid, c.rowid IS NULL
        FROM {active_vec} AS v
        LEFT JOIN event_embedding_chunks AS c
          ON c.rowid = v.rowid AND c.model_key = ?1 AND c.dimensions = ?2
        WHERE v.rowid > ?3
        ORDER BY v.rowid
        LIMIT ?4
        "#
    ))?;
    collect_vec0_validation_rows(&mut stmt, params![
        semantic_model_key(),
        SEMANTIC_DIMENSIONS as i64,
        cursor,
        SEMANTIC_MAINTENANCE_BATCH_ROWS as i64,
    ])
}

fn collect_vec0_validation_rows<P: rusqlite::Params>(
    stmt: &mut rusqlite::Statement<'_>,
    params: P,
) -> Result<(Vec<i64>, bool)> {
    let mut rows = stmt.query(params)?;
    let mut rowids = Vec::new();
    let mut mismatch = false;
    while let Some(row) = rows.next()? {
        rowids.push(row.get::<_, i64>(0)?);
        mismatch |= row.get::<_, bool>(1)?;
    }
    Ok((rowids, mismatch))
}

fn schedule_sqlite_vec0_repair_conn(conn: &Connection, active_generation: i64) -> Result<()> {
    let rebuild_generation = inactive_sqlite_vec0_generation(Some(active_generation));
    set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_COMPLETE_KEY, 0)?;
    set_semantic_state_i64_conn(
        conn,
        SEMANTIC_VEC0_REBUILD_GENERATION_KEY,
        rebuild_generation,
    )?;
    set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_STAGE_KEY, 1)?;
    set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_CURSOR_KEY, 0)?;
    set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_ROWS_KEY, 0)?;
    for key in [
        SEMANTIC_VEC0_VALIDATION_STAGE_KEY,
        SEMANTIC_VEC0_VALIDATION_CURSOR_KEY,
        SEMANTIC_VEC0_VALIDATION_TARGET_REVISION_KEY,
        SEMANTIC_VEC0_VALIDATION_TARGET_GENERATION_KEY,
        SEMANTIC_VEC0_VALIDATED_REVISION_KEY,
        SEMANTIC_VEC0_VALIDATED_GENERATION_KEY,
    ] {
        delete_semantic_state_conn(conn, key)?;
    }
    Ok(())
}

fn fill_sqlite_vec0_rebuild_slice(
    conn: &Connection,
    rebuild_vec: &str,
    rebuild_meta: &str,
) -> Result<()> {
    let cursor = semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_CURSOR_KEY)?.unwrap_or(0);
    let rows = {
        let mut stmt = conn.prepare(
            r#"
            SELECT rowid, event_id, history_record_id, session_id, event_seq, chunk_index,
                   source_text_sha256, start_char, end_char, embedding_f32
            FROM event_embedding_chunks
            WHERE model_key = ?1
              AND dimensions = ?2
              AND rowid > ?3
            ORDER BY rowid
            LIMIT ?4
            "#,
        )?;
        let rows = stmt.query_map(
            params![
                semantic_model_key(),
                SEMANTIC_DIMENSIONS as i64,
                cursor,
                SEMANTIC_MAINTENANCE_BATCH_ROWS as i64,
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Vec<u8>>(9)?,
                ))
            },
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if rows.is_empty() {
        set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_STAGE_KEY, 3)?;
        return Ok(());
    }
    let mut meta = conn.prepare(&format!(
        r#"
        INSERT OR REPLACE INTO {rebuild_meta}
            (rowid, event_id, model_key, history_record_id, session_id, event_seq,
             chunk_index, source_text_sha256, start_char, end_char)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        "#
    ))?;
    let mut vectors = conn.prepare(&format!(
        "INSERT OR REPLACE INTO {rebuild_vec}(rowid, embedding) VALUES (?1, ?2)"
    ))?;
    let rows_written = rows.len() as i64;
    let mut last_rowid = cursor;
    for row in rows {
        last_rowid = row.0;
        meta.execute(params![
            row.0,
            row.1,
            semantic_model_key(),
            row.2,
            row.3,
            row.4,
            row.5,
            row.6,
            row.7,
            row.8,
        ])?;
        vectors.execute(params![row.0, row.9])?;
    }
    set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_CURSOR_KEY, last_rowid)
        .and_then(|_| {
            let written = semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_ROWS_KEY)?
                .unwrap_or(0)
                .saturating_add(rows_written);
            set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_ROWS_KEY, written)
        })
}

fn publish_sqlite_vec0_rebuild(conn: &Connection, rebuild_generation: i64) -> Result<()> {
    set_semantic_state_i64_conn(
        conn,
        SEMANTIC_VEC0_ACTIVE_GENERATION_KEY,
        rebuild_generation,
    )?;
    set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_COMPLETE_KEY, 1)?;
    set_semantic_state_i64_conn(conn, SEMANTIC_VEC0_REBUILD_STAGE_KEY, 4)
}

fn cleanup_old_sqlite_vec0_slice(conn: &Connection, active_generation: i64) -> Result<()> {
    let inactive_generation = inactive_sqlite_vec0_generation(Some(active_generation));
    let (inactive_vec, inactive_meta) = sqlite_vec0_generation_tables(inactive_generation)
        .ok_or_else(|| anyhow!("invalid inactive semantic vec0 generation"))?;
    for (vec_table, meta_table, drop_tables) in [
        ("event_embedding_vec0", "event_embedding_vec0_meta", true),
        (inactive_vec, inactive_meta, false),
    ] {
        let vec_deleted = delete_rows_bounded(conn, vec_table, None)?;
        let meta_deleted = if vec_deleted == 0 {
            delete_rows_bounded(conn, meta_table, None)?
        } else {
            0
        };
        if vec_deleted > 0 || meta_deleted > 0 {
            return Ok(());
        }
        if drop_tables {
            conn.execute_batch(&format!(
                "DROP TABLE IF EXISTS {vec_table}; DROP TABLE IF EXISTS {meta_table};"
            ))?;
        }
    }
    delete_semantic_state_conn(conn, SEMANTIC_VEC0_REBUILD_STAGE_KEY)?;
    delete_semantic_state_conn(conn, SEMANTIC_VEC0_REBUILD_CURSOR_KEY)?;
    delete_semantic_state_conn(conn, SEMANTIC_VEC0_REBUILD_GENERATION_KEY)?;
    delete_semantic_state_conn(conn, SEMANTIC_VEC0_REBUILD_ROWS_KEY)?;
    Ok(())
}

fn delete_rows_bounded(conn: &Connection, table: &str, predicate: Option<&str>) -> Result<usize> {
    if !sqlite_table_exists(conn, table)? {
        return Ok(0);
    }
    let predicate = predicate
        .map(|predicate| format!("WHERE {predicate}"))
        .unwrap_or_default();
    let sql = format!(
        "DELETE FROM {table} WHERE rowid IN (SELECT rowid FROM {table} {predicate} ORDER BY rowid LIMIT ?1)"
    );
    Ok(conn.execute(&sql, [SEMANTIC_MAINTENANCE_BATCH_ROWS as i64])?)
}

fn table_has_any_rows(conn: &Connection, table: &str) -> Result<bool> {
    if !sqlite_table_exists(conn, table)? {
        return Ok(false);
    }
    table_has_matching_rows(conn, table, "1")
}

fn table_has_matching_rows(conn: &Connection, table: &str, predicate: &str) -> Result<bool> {
    if !sqlite_table_exists(conn, table)? {
        return Ok(false);
    }
    let sql = format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {predicate} LIMIT 1)");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
}
