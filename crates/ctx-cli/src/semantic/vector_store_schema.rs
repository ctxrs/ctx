const CANONICAL_GENERATION_STATE_KEY: &str = "canonical_generation";
const SUMMARY_ACTIVE_SLOT_STATE_KEY: &str = "summary_active_slot";
const SUMMARY_GENERATION_STATE_KEY: &str = "summary_generation";
const STATS_BUILD_GENERATION_STATE_KEY: &str = "stats_build_generation";
const STATS_BUILD_SLOT_STATE_KEY: &str = "stats_build_slot";
const STATS_BUILD_CLEARED_STATE_KEY: &str = "stats_build_cleared";
const STATS_BUILD_CURSOR_STATE_KEY: &str = "stats_build_rowid_after";
const STATS_BUILD_ITEMS_STATE_KEY: &str = "stats_build_items";
const STATS_BUILD_CHUNKS_STATE_KEY: &str = "stats_build_chunks";
const SQLITE_VEC0_READY_STATE_KEY: &str = "sqlite_vec0_v3_ready_version";
const SQLITE_VEC0_GENERATION_STATE_KEY: &str = "sqlite_vec0_v3_generation";
const SQLITE_VEC0_ACTIVE_SLOT_STATE_KEY: &str = "sqlite_vec0_v3_active_slot";
const SQLITE_VEC0_BUILD_GENERATION_STATE_KEY: &str = "sqlite_vec0_v3_build_generation";
const SQLITE_VEC0_BUILD_SLOT_STATE_KEY: &str = "sqlite_vec0_v3_build_slot";
const SQLITE_VEC0_BUILD_CLEARED_STATE_KEY: &str = "sqlite_vec0_v3_build_cleared";
const SQLITE_VEC0_BUILD_CURSOR_STATE_KEY: &str = "sqlite_vec0_v3_build_rowid_after";
const SQLITE_VEC0_VALIDATE_GENERATION_STATE_KEY: &str = "sqlite_vec0_v3_validate_generation";
const SQLITE_VEC0_VALIDATE_PHASE_STATE_KEY: &str = "sqlite_vec0_v3_validate_phase";
const SQLITE_VEC0_VALIDATE_CURSOR_STATE_KEY: &str = "sqlite_vec0_v3_validate_rowid_after";
const MAINTENANCE_PAGE_UNITS_STATE_KEY: &str = "maintenance_page_units";
const PLAINTEXT_SANITIZED_GLOBAL_STATE_KEY: &str = "global:plaintext_sanitized_version";
const PLAINTEXT_SANITIZE_CURSOR_VERSION_GLOBAL_STATE_KEY: &str =
    "global:plaintext_sanitize_cursor_version";
const PLAINTEXT_SANITIZE_ROWID_GLOBAL_STATE_KEY: &str = "global:plaintext_sanitize_rowid_after";
const TERMINAL_MAINTENANCE_FINGERPRINT_GLOBAL_STATE_KEY: &str =
    "global:terminal_maintenance_fingerprint";
const TERMINAL_MAINTENANCE_REASON_GLOBAL_STATE_KEY: &str = "global:terminal_maintenance_reason";
const PLAINTEXT_SANITIZED_STATE_VERSION: i64 = 2;
const SQLITE_VEC0_TABLE: &str = "event_embedding_vec0_v3";
const SQLITE_VEC0_META_TABLE: &str = "event_embedding_vec0_meta_v3";
const SQLITE_VEC0_CANONICAL_INDEX: &str = "idx_event_embedding_vec0_v3_slot_canonical";
const SQLITE_VEC0_EVENT_INDEX: &str = "idx_event_embedding_vec0_v3_slot_event";
const SQLITE_VEC0_WORK_INDEX: &str = "idx_event_embedding_vec0_v3_slot_model_rowid";

#[derive(Debug, PartialEq, Eq)]
struct SqliteColumnShape {
    name: String,
    declared_type: String,
    not_null: bool,
    default_value: Option<String>,
    primary_key_position: i64,
    hidden: i64,
}

#[derive(Debug, Clone, Copy, Default)]
struct SemanticSidecarWritePacing {
    wal_bytes_before: u64,
    nominal_bytes: u64,
}

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
        let mut store = Self {
            conn,
            path: path.to_path_buf(),
        };
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
        let store = Self {
            conn,
            path: path.to_path_buf(),
        };
        store.ensure_readable_schema()?;
        Ok(Some(store))
    }

    fn ensure_readable_schema(&self) -> Result<()> {
        let user_version = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap_or(0);
        if user_version > SEMANTIC_SIDECAR_SCHEMA_VERSION {
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
                "event_seq",
                "chunk_index",
                "chunk_count",
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
        if user_version > SEMANTIC_SIDECAR_SCHEMA_VERSION {
            return Err(anyhow!(
                "semantic vector store schema version {user_version} is newer than this ctx supports"
            ));
        }
        let canonical_table_existed = sqlite_table_exists(&self.conn, "event_embedding_chunks")?;
        let summary_table_existed = sqlite_table_exists(&self.conn, "semantic_event_summary")?;
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
            CREATE TABLE IF NOT EXISTS semantic_index_stats (
                model_key TEXT PRIMARY KEY,
                embedded_items INTEGER NOT NULL,
                embedded_chunks INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                trust_version INTEGER NOT NULL DEFAULT 0,
                generation INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS semantic_event_summary (
                slot INTEGER NOT NULL,
                model_key TEXT NOT NULL,
                event_id TEXT NOT NULL,
                event_seq INTEGER NOT NULL,
                source_text_sha256 TEXT NOT NULL,
                single_source_hash INTEGER NOT NULL,
                chunk_count INTEGER NOT NULL,
                PRIMARY KEY (slot, model_key, event_id)
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
            CREATE TABLE IF NOT EXISTS semantic_maintenance_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            "#,
        )?;
        if !sqlite_column_exists(&self.conn, "event_embeddings", "preview_text")? {
            self.conn.execute(
                "ALTER TABLE event_embeddings ADD COLUMN preview_text TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        if !sqlite_column_exists(&self.conn, "semantic_index_stats", "trust_version")? {
            self.conn.execute(
                "ALTER TABLE semantic_index_stats ADD COLUMN trust_version INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !sqlite_column_exists(&self.conn, "semantic_index_stats", "generation")? {
            self.conn.execute(
                "ALTER TABLE semantic_index_stats ADD COLUMN generation INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !canonical_table_existed {
            self.create_canonical_indexes()?;
        }
        if !summary_table_existed {
            self.conn.execute(
                r#"
                CREATE INDEX idx_semantic_event_summary_prune
                ON semantic_event_summary(slot, model_key, event_seq DESC, event_id DESC)
                "#,
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
        self.conn.execute_batch(&format!(
            "PRAGMA user_version = {SEMANTIC_SIDECAR_SCHEMA_VERSION};"
        ))?;
        if !canonical_table_existed {
            self.initialize_empty_sidecar_trust()?;
        }
        Ok(())
    }

    fn create_canonical_indexes(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE INDEX idx_event_embeddings_model_seq
                ON event_embeddings(model_key, event_seq);
            CREATE INDEX idx_event_embeddings_model_session
                ON event_embeddings(model_key, session_id);
            CREATE INDEX idx_event_embedding_chunks_model_seq
                ON event_embedding_chunks(model_key, event_seq);
            CREATE INDEX idx_event_embedding_chunks_model_session
                ON event_embedding_chunks(model_key, session_id);
            CREATE INDEX idx_event_embedding_chunks_model_event
                ON event_embedding_chunks(model_key, event_id);
            CREATE INDEX idx_semantic_dirty_events_model_priority
                ON semantic_dirty_events(model_key, priority_seq, queued_at_ms);
            "#,
        )?;
        Ok(())
    }

    fn initialize_empty_sidecar_trust(&mut self) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            r#"
            INSERT INTO semantic_index_stats
                (model_key, embedded_items, embedded_chunks, updated_at_ms,
                 trust_version, generation)
            VALUES (?1, 0, 0, ?2, ?3, 1)
            ON CONFLICT(model_key) DO UPDATE SET
                embedded_items = 0,
                embedded_chunks = 0,
                updated_at_ms = excluded.updated_at_ms,
                trust_version = excluded.trust_version,
                generation = excluded.generation
            "#,
            params![
                semantic_model_key(),
                utc_now().timestamp_millis(),
                SEMANTIC_SIDECAR_TRUST_VERSION
            ],
        )?;
        Self::set_maintenance_state_i64_in_transaction(&tx, CANONICAL_GENERATION_STATE_KEY, 1)?;
        Self::set_maintenance_state_i64_in_transaction(&tx, SUMMARY_ACTIVE_SLOT_STATE_KEY, 0)?;
        Self::set_maintenance_state_i64_in_transaction(&tx, SUMMARY_GENERATION_STATE_KEY, 1)?;
        Self::set_global_maintenance_state_i64_in_transaction(
            &tx,
            PLAINTEXT_SANITIZED_GLOBAL_STATE_KEY,
            PLAINTEXT_SANITIZED_STATE_VERSION,
        )?;
        tx.commit()?;
        Ok(())
    }

    fn active_model_tuple_matches(&self) -> Result<bool> {
        Self::active_model_tuple_matches_on_connection(&self.conn)
    }

    fn active_model_tuple_matches_on_connection(conn: &Connection) -> Result<bool> {
        let tuple = conn
            .query_row(
                r#"
                SELECT backend, model_id, dimensions, distance, normalized
                FROM embedding_models
                WHERE model_key = ?1
                "#,
                [semantic_model_key()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        Ok(
            tuple.is_some_and(|(backend, model_id, dimensions, distance, normalized)| {
                backend == SEMANTIC_BACKEND
                    && model_id == SEMANTIC_MODEL_ID
                    && dimensions == SEMANTIC_DIMENSIONS as i64
                    && distance == "cosine"
                    && normalized == 1
            }),
        )
    }

    fn sqlite_vec0_runtime_available(&self) -> bool {
        if !register_sqlite_vec_auto_extension() {
            return false;
        }
        self.conn
            .query_row("SELECT vec_version()", [], |row| row.get::<_, String>(0))
            .is_ok()
    }

    fn ensure_sqlite_vec0_schema_for_maintenance(&self) -> Result<bool> {
        if !self.sqlite_vec0_runtime_available() {
            return Ok(false);
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let meta_existed = sqlite_table_exists(&tx, SQLITE_VEC0_META_TABLE)?;
        let vec_existed = sqlite_table_exists(&tx, SQLITE_VEC0_TABLE)?;
        if meta_existed && vec_existed {
            if !Self::sqlite_vec0_schema_compatible_on_connection(&tx)? {
                return Err(SemanticVectorStoreTerminal::new(
                    "semantic vec0 v3 schema is incompatible; maintenance cannot publish it",
                )
                .into());
            }
            tx.commit()?;
            return Ok(true);
        }
        if meta_existed || vec_existed {
            return Err(SemanticVectorStoreTerminal::new(
                "semantic vec0 v3 schema is partial; maintenance will not drop or rebuild it",
            )
            .into());
        }
        tx.execute_batch(&format!(
            r#"
            CREATE TABLE {SQLITE_VEC0_META_TABLE} (
                rowid INTEGER PRIMARY KEY,
                slot INTEGER NOT NULL,
                canonical_rowid INTEGER NOT NULL,
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
            CREATE UNIQUE INDEX {SQLITE_VEC0_CANONICAL_INDEX}
                ON {SQLITE_VEC0_META_TABLE}(slot, model_key, canonical_rowid);
            CREATE INDEX {SQLITE_VEC0_EVENT_INDEX}
                ON {SQLITE_VEC0_META_TABLE}(slot, model_key, event_id);
            CREATE INDEX {SQLITE_VEC0_WORK_INDEX}
                ON {SQLITE_VEC0_META_TABLE}(slot, model_key, rowid);
            CREATE VIRTUAL TABLE {SQLITE_VEC0_TABLE}
            USING vec0(
                embedding float[{SEMANTIC_DIMENSIONS}] distance_metric=cosine,
                embedding_coarse bit[{SEMANTIC_DIMENSIONS}],
                slot INTEGER PARTITION KEY,
                model_key TEXT PARTITION KEY
            );
            "#,
        ))?;
        if !Self::sqlite_vec0_schema_compatible_on_connection(&tx)? {
            return Err(SemanticVectorStoreTerminal::new(
                "semantic vec0 v3 schema failed compatibility validation",
            )
            .into());
        }
        tx.commit()?;
        Ok(true)
    }

    fn sqlite_vec0_schema_compatible(&self) -> Result<bool> {
        Self::sqlite_vec0_schema_compatible_on_connection(&self.conn)
    }

    fn sqlite_vec0_schema_compatible_on_connection(conn: &Connection) -> Result<bool> {
        let meta_columns = Self::sqlite_table_column_shapes(conn, SQLITE_VEC0_META_TABLE)?;
        let expected_meta_columns = vec![
            Self::sqlite_column_shape("rowid", "INTEGER", false, 1, 0),
            Self::sqlite_column_shape("slot", "INTEGER", true, 0, 0),
            Self::sqlite_column_shape("canonical_rowid", "INTEGER", true, 0, 0),
            Self::sqlite_column_shape("event_id", "TEXT", true, 0, 0),
            Self::sqlite_column_shape("model_key", "TEXT", true, 0, 0),
            Self::sqlite_column_shape("history_record_id", "TEXT", false, 0, 0),
            Self::sqlite_column_shape("session_id", "TEXT", false, 0, 0),
            Self::sqlite_column_shape("event_seq", "INTEGER", true, 0, 0),
            Self::sqlite_column_shape("chunk_index", "INTEGER", true, 0, 0),
            Self::sqlite_column_shape("source_text_sha256", "TEXT", true, 0, 0),
            Self::sqlite_column_shape("start_char", "INTEGER", true, 0, 0),
            Self::sqlite_column_shape("end_char", "INTEGER", true, 0, 0),
        ];
        if meta_columns != expected_meta_columns {
            return Ok(false);
        }
        if !Self::sqlite_indexes_match_exactly(
            conn,
            SQLITE_VEC0_META_TABLE,
            &[
                (
                    SQLITE_VEC0_CANONICAL_INDEX,
                    true,
                    &["slot", "model_key", "canonical_rowid"],
                ),
                (
                    SQLITE_VEC0_EVENT_INDEX,
                    false,
                    &["slot", "model_key", "event_id"],
                ),
                (
                    SQLITE_VEC0_WORK_INDEX,
                    false,
                    &["slot", "model_key", "rowid"],
                ),
            ],
        )? {
            return Ok(false);
        }
        let vec0_columns = Self::sqlite_table_column_shapes(conn, SQLITE_VEC0_TABLE)?;
        let expected_vec0_columns = vec![
            Self::sqlite_column_shape("rowid", "", false, 0, 0),
            Self::sqlite_column_shape("embedding", "", false, 0, 0),
            Self::sqlite_column_shape("embedding_coarse", "", false, 0, 0),
            Self::sqlite_column_shape("slot", "", false, 0, 0),
            Self::sqlite_column_shape("model_key", "", false, 0, 0),
            Self::sqlite_column_shape("distance", "", false, 0, 1),
            Self::sqlite_column_shape("k", "", false, 0, 1),
        ];
        if vec0_columns != expected_vec0_columns {
            return Ok(false);
        }
        let Some(sql) = sqlite_table_sql(conn, SQLITE_VEC0_TABLE)? else {
            return Ok(false);
        };
        let expected_sql = format!(
            r#"
            CREATE VIRTUAL TABLE {SQLITE_VEC0_TABLE}
            USING vec0(
                embedding float[{SEMANTIC_DIMENSIONS}] distance_metric=cosine,
                embedding_coarse bit[{SEMANTIC_DIMENSIONS}],
                slot INTEGER PARTITION KEY,
                model_key TEXT PARTITION KEY
            )
            "#
        );
        Ok(Self::normalized_schema_sql(&sql) == Self::normalized_schema_sql(&expected_sql))
    }

    fn sqlite_column_shape(
        name: &str,
        declared_type: &str,
        not_null: bool,
        primary_key_position: i64,
        hidden: i64,
    ) -> SqliteColumnShape {
        SqliteColumnShape {
            name: name.to_owned(),
            declared_type: declared_type.to_owned(),
            not_null,
            default_value: None,
            primary_key_position,
            hidden,
        }
    }

    fn sqlite_table_column_shapes(
        conn: &Connection,
        table: &str,
    ) -> Result<Vec<SqliteColumnShape>> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_xinfo({table})"))?;
        let rows = stmt.query_map([], |row| {
            Ok(SqliteColumnShape {
                name: row.get(1)?,
                declared_type: row.get(2)?,
                not_null: row.get::<_, i64>(3)? == 1,
                default_value: row.get(4)?,
                primary_key_position: row.get(5)?,
                hidden: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn normalized_schema_sql(sql: &str) -> String {
        sql.trim_end_matches(';')
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .map(|character| character.to_ascii_lowercase())
            .collect()
    }

    fn sqlite_indexes_match_exactly(
        conn: &Connection,
        table: &str,
        expected: &[(&str, bool, &[&str])],
    ) -> Result<bool> {
        let mut stmt = conn.prepare(&format!("PRAGMA index_list({table})"))?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? == 1,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)? == 1,
            ))
        })?;
        let indexes = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        if indexes.len() != expected.len()
            || indexes.iter().any(|(name, unique, origin, partial)| {
                *partial
                    || origin != "c"
                    || !expected.iter().any(|(expected_name, expected_unique, _)| {
                        name == expected_name && unique == expected_unique
                    })
            })
        {
            return Ok(false);
        }
        for (name, unique, columns) in expected {
            if !Self::sqlite_index_matches(conn, table, name, *unique, columns)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn sqlite_index_matches(
        conn: &Connection,
        table: &str,
        index: &str,
        unique: bool,
        expected_columns: &[&str],
    ) -> Result<bool> {
        let mut list = conn.prepare(&format!("PRAGMA index_list({table})"))?;
        let rows = list.query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(2)? == 1))
        })?;
        let indexes = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        if !indexes
            .iter()
            .any(|(name, is_unique)| name == index && *is_unique == unique)
        {
            return Ok(false);
        }
        let mut info = conn.prepare(&format!("PRAGMA index_info({index})"))?;
        let rows = info.query_map([], |row| row.get::<_, String>(2))?;
        let columns = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(columns
            .iter()
            .map(String::as_str)
            .eq(expected_columns.iter().copied()))
    }

    fn sqlite_vec0_search_ready(&self) -> Result<bool> {
        let Some(canonical_generation) =
            self.maintenance_state_i64(CANONICAL_GENERATION_STATE_KEY)?
        else {
            return Ok(false);
        };
        if canonical_generation <= 0
            || self.maintenance_state_i64(SQLITE_VEC0_READY_STATE_KEY)?
                != Some(SEMANTIC_SQLITE_VEC0_PROJECTION_VERSION)
            || self.maintenance_state_i64(SQLITE_VEC0_GENERATION_STATE_KEY)?
                != Some(canonical_generation)
            || !matches!(
                self.maintenance_state_i64(SQLITE_VEC0_ACTIVE_SLOT_STATE_KEY)?,
                Some(0 | 1)
            )
            || !self.active_model_tuple_matches()?
            || !self.sqlite_vec0_runtime_available()
            || !sqlite_table_exists(&self.conn, SQLITE_VEC0_TABLE)?
            || !sqlite_table_exists(&self.conn, SQLITE_VEC0_META_TABLE)?
        {
            return Ok(false);
        }
        self.sqlite_vec0_schema_compatible()
    }

    #[cfg(all(test, ctx_sqlite_vec))]
    fn sqlite_vec0_ready(&self) -> Result<bool> {
        if !self.sqlite_vec0_search_ready()? {
            return Ok(false);
        }
        let stats = self
            .cached_stats()?
            .ok_or_else(|| anyhow!("trusted semantic stats are missing"))?;
        let slot = self
            .maintenance_state_i64(SQLITE_VEC0_ACTIVE_SLOT_STATE_KEY)?
            .unwrap_or(-1);
        let meta_rows = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM {SQLITE_VEC0_META_TABLE} WHERE slot = ?1 AND model_key = ?2"
            ),
            params![slot, semantic_model_key()],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(meta_rows.max(0) as usize == stats.embedded_chunks)
    }

    fn run_maintenance_slice(&mut self) -> Result<SemanticSidecarMaintenanceOutcome> {
        let nominal_bytes = self.maintenance_precharge_bytes()?;
        ctx_history_capture::pace_current_disk_io(nominal_bytes);
        let outcome = self.run_maintenance_slice_precharged(nominal_bytes)?;
        ctx_history_capture::pace_current_disk_io(outcome.supplemental_bytes);
        Ok(outcome)
    }

    fn maintenance_precharge_bytes(&self) -> Result<u64> {
        Ok(SEMANTIC_SIDECAR_MAINTENANCE_LOGICAL_BYTES)
    }

    fn run_maintenance_slice_precharged(
        &mut self,
        nominal_bytes: u64,
    ) -> Result<SemanticSidecarMaintenanceOutcome> {
        let sql_deadline =
            Instant::now() + StdDuration::from_millis(SEMANTIC_SIDECAR_MAINTENANCE_MAX_MILLIS);
        self.conn.progress_handler(
            SEMANTIC_SQL_PROGRESS_OPS,
            Some(move || Instant::now() >= sql_deadline),
        );
        let page_units = match self.maintenance_page_units() {
            Ok(units) => units,
            Err(error) => {
                self.conn.progress_handler(0, None::<fn() -> bool>);
                return Err(error);
            }
        };
        let pacing = self.begin_write_pacing(nominal_bytes);
        let result = (|| -> Result<SemanticSidecarMaintenanceOutcome> {
            if !self.active_model_tuple_matches()? {
                return Err(SemanticVectorStoreTerminal::new(
                    "active model tuple does not match this ctx build",
                )
                .into());
            }
            if self.global_maintenance_state_i64(PLAINTEXT_SANITIZED_GLOBAL_STATE_KEY)?
                != Some(PLAINTEXT_SANITIZED_STATE_VERSION)
            {
                self.sanitize_plaintext_slice()
            } else if !self.stats_and_summary_trusted()? {
                self.rebuild_stats_and_summary_slice()
            } else if !self.sqlite_vec0_search_ready()? && self.sqlite_vec0_runtime_available() {
                self.rebuild_projection_slice()
            } else if self.sqlite_vec0_search_ready()? {
                self.validate_projection_slice()
            } else {
                Ok(SemanticSidecarMaintenanceOutcome::ready(0, 0))
            }
        })();
        self.conn.progress_handler(0, None::<fn() -> bool>);
        let mut outcome = match result {
            Ok(outcome) => {
                self.grow_maintenance_page_units_after_success(page_units)?;
                outcome
            }
            Err(error) if Self::sqlite_operation_interrupted(&error) => {
                if page_units == 1 {
                    return Err(SemanticVectorStoreTerminal::new(
                        "semantic maintenance made no progress at the one-row floor",
                    )
                    .into());
                }
                self.set_maintenance_state_i64(
                    MAINTENANCE_PAGE_UNITS_STATE_KEY,
                    (page_units / 2).max(1) as i64,
                )?;
                SemanticSidecarMaintenanceOutcome::pending(0, 0)
            }
            Err(error) => return Err(error),
        };
        outcome.supplemental_bytes = self.finish_write_pacing(pacing);
        Ok(outcome)
    }

    fn sqlite_operation_interrupted(error: &anyhow::Error) -> bool {
        error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::SqliteFailure(inner, _))
                    if inner.code == rusqlite::ErrorCode::OperationInterrupted
            )
        })
    }

    fn maintenance_page_units(&self) -> Result<usize> {
        Ok(self
            .maintenance_state_i64(MAINTENANCE_PAGE_UNITS_STATE_KEY)?
            .unwrap_or(SEMANTIC_SIDECAR_MAINTENANCE_ROWS as i64)
            .clamp(1, SEMANTIC_SIDECAR_MAINTENANCE_ROWS as i64) as usize)
    }

    fn grow_maintenance_page_units_after_success(&self, page_units: usize) -> Result<()> {
        if page_units < SEMANTIC_SIDECAR_MAINTENANCE_ROWS {
            self.set_maintenance_state_i64(
                MAINTENANCE_PAGE_UNITS_STATE_KEY,
                page_units
                    .saturating_mul(2)
                    .min(SEMANTIC_SIDECAR_MAINTENANCE_ROWS) as i64,
            )?;
        }
        Ok(())
    }

    fn terminal_maintenance_fingerprint(&self) -> Result<String> {
        let user_version = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap_or(0);
        let schema_cookie = self
            .conn
            .query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))
            .unwrap_or(0);
        let canonical_generation = self
            .maintenance_state_i64(CANONICAL_GENERATION_STATE_KEY)?
            .unwrap_or(0);
        let model_tuple = self
            .conn
            .query_row(
                r#"
                SELECT backend, model_id, dimensions, distance, normalized
                FROM embedding_models
                WHERE model_key = ?1
                "#,
                [semantic_model_key()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        let sanitize_version = self
            .global_maintenance_state_i64(PLAINTEXT_SANITIZED_GLOBAL_STATE_KEY)?
            .unwrap_or(0);
        let cursor_version = self
            .global_maintenance_state_i64(PLAINTEXT_SANITIZE_CURSOR_VERSION_GLOBAL_STATE_KEY)?
            .unwrap_or(0);
        let cursor = self
            .global_maintenance_state_i64(PLAINTEXT_SANITIZE_ROWID_GLOBAL_STATE_KEY)?
            .unwrap_or(0);
        let stats_cursor = self
            .maintenance_state_i64(STATS_BUILD_CURSOR_STATE_KEY)?
            .unwrap_or(0);
        let projection_cursor = self
            .maintenance_state_i64(SQLITE_VEC0_BUILD_CURSOR_STATE_KEY)?
            .unwrap_or(0);
        let validation_cursor = self
            .maintenance_state_i64(SQLITE_VEC0_VALIDATE_CURSOR_STATE_KEY)?
            .unwrap_or(0);
        let stats_page = self.terminal_canonical_page_fingerprint(stats_cursor)?;
        let projection_page = self.terminal_canonical_page_fingerprint(projection_cursor)?;
        let validation_page = self.terminal_canonical_page_fingerprint(validation_cursor)?;
        let legacy_page = self.terminal_legacy_page_fingerprint()?;
        let plaintext_page = self.terminal_plaintext_page_fingerprint(cursor)?;
        Ok(format!(
            "schema={user_version}:{schema_cookie};projection_format={SEMANTIC_SQLITE_VEC0_PROJECTION_VERSION};model={:?};generation={canonical_generation};sanitize={sanitize_version}:{cursor_version}:{cursor};legacy={legacy_page};plaintext={plaintext_page};stats={stats_cursor}:{stats_page};projection={projection_cursor}:{projection_page};validation={validation_cursor}:{validation_page}",
            model_tuple
        ))
    }

    fn terminal_canonical_page_fingerprint(&self, cursor: i64) -> Result<String> {
        let content_prefix_bytes = SEMANTIC_DIMENSIONS
            .saturating_mul(std::mem::size_of::<f32>())
            .saturating_add(1);
        let mut stmt = self.conn.prepare(
            r#"
            SELECT rowid, event_id, model_key, event_seq, chunk_index,
                   source_text_sha256, chunk_text_sha256, dimensions,
                   length(embedding_f32),
                   hex(substr(CAST(embedding_f32 AS BLOB), 1, ?2))
            FROM event_embedding_chunks
            WHERE rowid > ?1
            ORDER BY rowid
            LIMIT ?3
            "#,
        )?;
        let rows = stmt.query_map(
            params![
                cursor,
                content_prefix_bytes as i64,
                SEMANTIC_SIDECAR_MAINTENANCE_ROWS as i64
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )?;
        let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Self::terminal_rows_fingerprint(&rows))
    }

    fn terminal_legacy_page_fingerprint(&self) -> Result<String> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT rowid, event_id, model_key, text_sha256,
                   length(preview_text),
                   hex(substr(CAST(preview_text AS BLOB), 1, 4096)),
                   dimensions, length(embedding_f32),
                   hex(substr(CAST(embedding_f32 AS BLOB), 1, ?1))
            FROM event_embeddings
            ORDER BY rowid
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(
            params![
                SEMANTIC_DIMENSIONS
                    .saturating_mul(std::mem::size_of::<f32>())
                    .saturating_add(1) as i64,
                SEMANTIC_SIDECAR_MAINTENANCE_ROWS as i64
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )?;
        let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Self::terminal_rows_fingerprint(&rows))
    }

    fn terminal_plaintext_page_fingerprint(&self, cursor: i64) -> Result<String> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT rowid, event_id, model_key, source_text_sha256,
                   chunk_text_sha256, length(chunk_text),
                   hex(substr(CAST(chunk_text AS BLOB), 1, 4096))
            FROM event_embedding_chunks
            WHERE rowid > ?1
            ORDER BY rowid
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(
            params![cursor, SEMANTIC_SIDECAR_MAINTENANCE_ROWS as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )?;
        let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Self::terminal_rows_fingerprint(&rows))
    }

    fn terminal_rows_fingerprint(rows: &[impl fmt::Debug]) -> String {
        let mut digest = Sha256::new();
        for row in rows {
            let encoded = format!("{row:?}");
            digest.update((encoded.len() as u64).to_le_bytes());
            digest.update(encoded.as_bytes());
        }
        format!("{:x}", digest.finalize())
    }

    fn active_terminal_maintenance_failure(&self) -> Result<Option<String>> {
        let Some(stored_fingerprint) = self
            .global_maintenance_state_string(TERMINAL_MAINTENANCE_FINGERPRINT_GLOBAL_STATE_KEY)?
        else {
            return Ok(None);
        };
        if stored_fingerprint != self.terminal_maintenance_fingerprint()? {
            return Ok(None);
        }
        self.global_maintenance_state_string(TERMINAL_MAINTENANCE_REASON_GLOBAL_STATE_KEY)
    }

    fn record_terminal_maintenance_failure(&self, reason: &str) -> Result<()> {
        let fingerprint = self.terminal_maintenance_fingerprint()?;
        let tx = self.conn.unchecked_transaction()?;
        Self::set_global_maintenance_state_string_in_transaction(
            &tx,
            TERMINAL_MAINTENANCE_FINGERPRINT_GLOBAL_STATE_KEY,
            &fingerprint,
        )?;
        Self::set_global_maintenance_state_string_in_transaction(
            &tx,
            TERMINAL_MAINTENANCE_REASON_GLOBAL_STATE_KEY,
            reason,
        )?;
        tx.commit()?;
        Ok(())
    }

    fn sanitize_plaintext_slice(&mut self) -> Result<SemanticSidecarMaintenanceOutcome> {
        let page_units = self.maintenance_page_units()?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let legacy_rows = Self::admitted_legacy_plaintext_rows(&tx, page_units)?;
        if !legacy_rows.is_empty() {
            let mut stmt = tx.prepare("DELETE FROM event_embeddings WHERE rowid = ?1")?;
            for (rowid, _) in &legacy_rows {
                stmt.execute([rowid])?;
            }
            drop(stmt);
            let logical_bytes = legacy_rows.iter().map(|(_, bytes)| *bytes).sum::<u64>();
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::pending(
                legacy_rows.len(),
                logical_bytes,
            ));
        }

        if Self::global_maintenance_state_i64_on_connection(
            &tx,
            PLAINTEXT_SANITIZE_CURSOR_VERSION_GLOBAL_STATE_KEY,
        )? != Some(PLAINTEXT_SANITIZED_STATE_VERSION)
        {
            Self::set_global_maintenance_state_i64_in_transaction(
                &tx,
                PLAINTEXT_SANITIZE_CURSOR_VERSION_GLOBAL_STATE_KEY,
                PLAINTEXT_SANITIZED_STATE_VERSION,
            )?;
            Self::set_global_maintenance_state_i64_in_transaction(
                &tx,
                PLAINTEXT_SANITIZE_ROWID_GLOBAL_STATE_KEY,
                0,
            )?;
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::pending(0, 0));
        }
        let cursor = Self::global_maintenance_state_i64_on_connection(
            &tx,
            PLAINTEXT_SANITIZE_ROWID_GLOBAL_STATE_KEY,
        )?
        .unwrap_or(0);
        let inspected_rows = Self::admitted_chunk_plaintext_rows(&tx, cursor, page_units)?;
        if !inspected_rows.is_empty() {
            let mut stmt =
                tx.prepare("UPDATE event_embedding_chunks SET chunk_text = '' WHERE rowid = ?1")?;
            for (rowid, bytes) in &inspected_rows {
                if *bytes > 0 {
                    stmt.execute([rowid])?;
                }
            }
            drop(stmt);
            let logical_bytes = inspected_rows.iter().map(|(_, bytes)| *bytes).sum::<u64>();
            let last_rowid = inspected_rows
                .last()
                .map(|(rowid, _)| *rowid)
                .unwrap_or(cursor);
            Self::set_global_maintenance_state_i64_in_transaction(
                &tx,
                PLAINTEXT_SANITIZE_ROWID_GLOBAL_STATE_KEY,
                last_rowid,
            )?;
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::pending(
                inspected_rows.len(),
                logical_bytes,
            ));
        }

        Self::set_global_maintenance_state_i64_in_transaction(
            &tx,
            PLAINTEXT_SANITIZED_GLOBAL_STATE_KEY,
            PLAINTEXT_SANITIZED_STATE_VERSION,
        )?;
        tx.commit()?;
        Ok(SemanticSidecarMaintenanceOutcome::pending(0, 0))
    }

    fn admitted_legacy_plaintext_rows(
        conn: &Connection,
        page_units: usize,
    ) -> Result<Vec<(i64, u64)>> {
        let mut stmt = conn.prepare(
            r#"
            SELECT rowid,
                   length(preview_text) + length(embedding_f32)
            FROM event_embeddings
            ORDER BY rowid
            LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map([page_units as i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?.max(0) as u64))
        })?;
        Self::admit_bounded_plaintext_rows(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn admitted_chunk_plaintext_rows(
        conn: &Connection,
        cursor: i64,
        page_units: usize,
    ) -> Result<Vec<(i64, u64)>> {
        let mut stmt = conn.prepare(
            r#"
            SELECT rowid, length(chunk_text)
            FROM event_embedding_chunks
            WHERE rowid > ?1
            ORDER BY rowid
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(params![cursor, page_units as i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?.max(0) as u64))
        })?;
        Self::admit_bounded_plaintext_rows(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn admit_bounded_plaintext_rows(rows: Vec<(i64, u64)>) -> Result<Vec<(i64, u64)>> {
        let mut admitted = Vec::new();
        let mut bytes = 0_u64;
        for row in rows {
            if row.1 > SEMANTIC_SIDECAR_MAINTENANCE_MAX_BYTES as u64 {
                return Err(SemanticVectorStoreTerminal::new(format!(
                    "legacy semantic plaintext row {} exceeds the {} byte sanitation ceiling",
                    row.0, SEMANTIC_SIDECAR_MAINTENANCE_MAX_BYTES
                ))
                .into());
            }
            if !admitted.is_empty()
                && bytes.saturating_add(row.1) > SEMANTIC_SIDECAR_MAINTENANCE_MAX_BYTES as u64
            {
                break;
            }
            bytes = bytes.saturating_add(row.1);
            admitted.push(row);
        }
        Ok(admitted)
    }

    fn rebuild_stats_and_summary_slice(&mut self) -> Result<SemanticSidecarMaintenanceOutcome> {
        let page_units = self.maintenance_page_units()?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if Self::stats_and_summary_trusted_on_connection(&tx)? {
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::pending(0, 0));
        }
        let canonical_generation =
            Self::maintenance_state_i64_in_transaction(&tx, CANONICAL_GENERATION_STATE_KEY)?
                .unwrap_or(0);
        if canonical_generation <= 0 {
            Self::set_maintenance_state_i64_in_transaction(&tx, CANONICAL_GENERATION_STATE_KEY, 1)?;
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::pending(0, 0));
        }
        let active_slot =
            Self::maintenance_state_i64_in_transaction(&tx, SUMMARY_ACTIVE_SLOT_STATE_KEY)?
                .filter(|slot| matches!(slot, 0 | 1));
        let target_slot = active_slot.map_or(0, |slot| 1 - slot);
        let build_generation =
            Self::maintenance_state_i64_in_transaction(&tx, STATS_BUILD_GENERATION_STATE_KEY)?;
        let build_slot =
            Self::maintenance_state_i64_in_transaction(&tx, STATS_BUILD_SLOT_STATE_KEY)?
                .filter(|slot| matches!(slot, 0 | 1));
        if build_generation != Some(canonical_generation) || build_slot != Some(target_slot) {
            for (key, value) in [
                (STATS_BUILD_GENERATION_STATE_KEY, canonical_generation),
                (STATS_BUILD_SLOT_STATE_KEY, target_slot),
                (STATS_BUILD_CLEARED_STATE_KEY, 0),
                (STATS_BUILD_CURSOR_STATE_KEY, 0),
                (STATS_BUILD_ITEMS_STATE_KEY, 0),
                (STATS_BUILD_CHUNKS_STATE_KEY, 0),
            ] {
                Self::set_maintenance_state_i64_in_transaction(&tx, key, value)?;
            }
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::pending(0, 0));
        }
        if Self::maintenance_state_i64_in_transaction(&tx, STATS_BUILD_CLEARED_STATE_KEY)?
            != Some(1)
        {
            let event_ids = {
                let mut stmt = tx.prepare(
                    r#"
                    SELECT event_id
                    FROM semantic_event_summary
                    WHERE slot = ?1 AND model_key = ?2
                    ORDER BY event_id
                    LIMIT ?3
                    "#,
                )?;
                let rows = stmt.query_map(
                    params![target_slot, semantic_model_key(), page_units as i64],
                    |row| row.get::<_, String>(0),
                )?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            if event_ids.is_empty() {
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    STATS_BUILD_CLEARED_STATE_KEY,
                    1,
                )?;
            } else {
                let mut stmt = tx.prepare(
                    "DELETE FROM semantic_event_summary WHERE slot = ?1 AND model_key = ?2 AND event_id = ?3",
                )?;
                for event_id in &event_ids {
                    stmt.execute(params![target_slot, semantic_model_key(), event_id])?;
                }
            }
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::pending(
                event_ids.len(),
                0,
            ));
        }

        let cursor = Self::maintenance_state_i64_in_transaction(&tx, STATS_BUILD_CURSOR_STATE_KEY)?
            .unwrap_or(0);
        let rows = {
            let mut stmt = tx.prepare(
                r#"
                SELECT rowid, model_key, event_id, event_seq, source_text_sha256,
                       dimensions, length(embedding_f32)
                FROM event_embedding_chunks
                WHERE rowid > ?1
                ORDER BY rowid
                LIMIT ?2
                "#,
            )?;
            let mapped = stmt.query_map(params![cursor, page_units as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?.max(0) as u64,
                ))
            })?;
            mapped.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if rows.is_empty() {
            let observed_generation =
                Self::maintenance_state_i64_in_transaction(&tx, CANONICAL_GENERATION_STATE_KEY)?;
            if observed_generation != Some(canonical_generation)
                || !Self::active_model_tuple_matches_on_connection(&tx)?
            {
                tx.commit()?;
                return Ok(SemanticSidecarMaintenanceOutcome::pending(0, 0));
            }
            let embedded_items =
                Self::maintenance_state_i64_in_transaction(&tx, STATS_BUILD_ITEMS_STATE_KEY)?
                    .unwrap_or(0);
            let embedded_chunks =
                Self::maintenance_state_i64_in_transaction(&tx, STATS_BUILD_CHUNKS_STATE_KEY)?
                    .unwrap_or(0);
            if embedded_items < 0 || embedded_chunks < 0 {
                return Err(SemanticVectorStoreTerminal::new(
                    "semantic maintenance counters are negative",
                )
                .into());
            }
            tx.execute(
                r#"
                INSERT INTO semantic_index_stats
                    (model_key, embedded_items, embedded_chunks, updated_at_ms,
                     trust_version, generation)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(model_key) DO UPDATE SET
                    embedded_items = excluded.embedded_items,
                    embedded_chunks = excluded.embedded_chunks,
                    updated_at_ms = excluded.updated_at_ms,
                    trust_version = excluded.trust_version,
                    generation = excluded.generation
                "#,
                params![
                    semantic_model_key(),
                    embedded_items,
                    embedded_chunks,
                    utc_now().timestamp_millis(),
                    SEMANTIC_SIDECAR_TRUST_VERSION,
                    canonical_generation
                ],
            )?;
            Self::set_maintenance_state_i64_in_transaction(
                &tx,
                SUMMARY_ACTIVE_SLOT_STATE_KEY,
                target_slot,
            )?;
            Self::set_maintenance_state_i64_in_transaction(
                &tx,
                SUMMARY_GENERATION_STATE_KEY,
                canonical_generation,
            )?;
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::pending(0, 0));
        }

        let started = Instant::now();
        let mut processed = 0_usize;
        let mut logical_bytes = 0_u64;
        let mut last_rowid = cursor;
        let mut added_items = 0_i64;
        let mut added_chunks = 0_i64;
        for (rowid, model_key, event_id, event_seq, source_hash, dimensions, bytes) in rows {
            if processed > 0
                && (logical_bytes as usize >= SEMANTIC_SIDECAR_MAINTENANCE_MAX_BYTES
                    || started.elapsed()
                        >= StdDuration::from_millis(SEMANTIC_SIDECAR_MAINTENANCE_MAX_MILLIS))
            {
                break;
            }
            processed = processed.saturating_add(1);
            last_rowid = rowid;
            if model_key != semantic_model_key() {
                continue;
            }
            let expected_bytes =
                SEMANTIC_DIMENSIONS.saturating_mul(std::mem::size_of::<f32>()) as u64;
            if dimensions != SEMANTIC_DIMENSIONS as i64 || bytes != expected_bytes {
                return Err(SemanticVectorStoreTerminal::new(format!(
                    "canonical semantic vector row {rowid} has an unsupported payload"
                ))
                .into());
            }
            logical_bytes = logical_bytes.saturating_add(bytes);
            let inserted = tx.execute(
                r#"
                INSERT OR IGNORE INTO semantic_event_summary
                    (slot, model_key, event_id, event_seq, source_text_sha256,
                     single_source_hash, chunk_count)
                VALUES (?1, ?2, ?3, ?4, ?5, 1, 1)
                "#,
                params![
                    target_slot,
                    semantic_model_key(),
                    event_id,
                    event_seq,
                    source_hash
                ],
            )?;
            if inserted == 1 {
                added_items += 1;
            } else {
                tx.execute(
                    r#"
                    UPDATE semantic_event_summary
                    SET event_seq = MAX(event_seq, ?4),
                        single_source_hash = CASE
                            WHEN source_text_sha256 = ?5 THEN single_source_hash
                            ELSE 0
                        END,
                        chunk_count = chunk_count + 1
                    WHERE slot = ?1 AND model_key = ?2 AND event_id = ?3
                    "#,
                    params![
                        target_slot,
                        semantic_model_key(),
                        event_id,
                        event_seq,
                        source_hash
                    ],
                )?;
            }
            added_chunks += 1;
        }
        Self::set_maintenance_state_i64_in_transaction(
            &tx,
            STATS_BUILD_CURSOR_STATE_KEY,
            last_rowid,
        )?;
        Self::increment_maintenance_state_i64_in_transaction(
            &tx,
            STATS_BUILD_ITEMS_STATE_KEY,
            added_items,
        )?;
        Self::increment_maintenance_state_i64_in_transaction(
            &tx,
            STATS_BUILD_CHUNKS_STATE_KEY,
            added_chunks,
        )?;
        tx.commit()?;
        Ok(SemanticSidecarMaintenanceOutcome::pending(
            processed,
            logical_bytes,
        ))
    }

    fn rebuild_projection_slice(&mut self) -> Result<SemanticSidecarMaintenanceOutcome> {
        let page_units = self.maintenance_page_units()?;
        if !self.ensure_sqlite_vec0_schema_for_maintenance()? {
            return Ok(SemanticSidecarMaintenanceOutcome::pending(0, 0));
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let canonical_generation =
            Self::maintenance_state_i64_in_transaction(&tx, CANONICAL_GENERATION_STATE_KEY)?
                .unwrap_or(0);
        if canonical_generation <= 0
            || !Self::stats_and_summary_trusted_on_connection(&tx)?
            || !Self::active_model_tuple_matches_on_connection(&tx)?
        {
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::pending(0, 0));
        }
        let active_slot =
            Self::maintenance_state_i64_in_transaction(&tx, SQLITE_VEC0_ACTIVE_SLOT_STATE_KEY)?
                .filter(|slot| matches!(slot, 0 | 1));
        if Self::maintenance_state_i64_in_transaction(&tx, SQLITE_VEC0_READY_STATE_KEY)?
            == Some(SEMANTIC_SQLITE_VEC0_PROJECTION_VERSION)
            && Self::maintenance_state_i64_in_transaction(&tx, SQLITE_VEC0_GENERATION_STATE_KEY)?
                == Some(canonical_generation)
            && active_slot.is_some()
        {
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::ready(0, 0));
        }
        let target_slot = active_slot.map_or(0, |slot| 1 - slot);
        let build_generation = Self::maintenance_state_i64_in_transaction(
            &tx,
            SQLITE_VEC0_BUILD_GENERATION_STATE_KEY,
        )?;
        let build_slot =
            Self::maintenance_state_i64_in_transaction(&tx, SQLITE_VEC0_BUILD_SLOT_STATE_KEY)?
                .filter(|slot| matches!(slot, 0 | 1));
        if build_generation != Some(canonical_generation) || build_slot != Some(target_slot) {
            for (key, value) in [
                (SQLITE_VEC0_BUILD_GENERATION_STATE_KEY, canonical_generation),
                (SQLITE_VEC0_BUILD_SLOT_STATE_KEY, target_slot),
                (SQLITE_VEC0_BUILD_CLEARED_STATE_KEY, 0),
                (SQLITE_VEC0_BUILD_CURSOR_STATE_KEY, 0),
            ] {
                Self::set_maintenance_state_i64_in_transaction(&tx, key, value)?;
            }
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::pending(0, 0));
        }
        if Self::maintenance_state_i64_in_transaction(&tx, SQLITE_VEC0_BUILD_CLEARED_STATE_KEY)?
            != Some(1)
        {
            let rowids = {
                let mut stmt = tx.prepare(&format!(
                    r#"
                    SELECT rowid
                    FROM {SQLITE_VEC0_META_TABLE}
                    WHERE slot = ?1 AND model_key = ?2
                    ORDER BY rowid
                    LIMIT ?3
                    "#
                ))?;
                let rows = stmt.query_map(
                    params![target_slot, semantic_model_key(), page_units as i64],
                    |row| row.get::<_, i64>(0),
                )?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            if rowids.is_empty() {
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SQLITE_VEC0_BUILD_CLEARED_STATE_KEY,
                    1,
                )?;
            } else {
                let mut vec_stmt =
                    tx.prepare(&format!("DELETE FROM {SQLITE_VEC0_TABLE} WHERE rowid = ?1"))?;
                let mut meta_stmt = tx.prepare(&format!(
                    "DELETE FROM {SQLITE_VEC0_META_TABLE} WHERE rowid = ?1"
                ))?;
                for rowid in &rowids {
                    vec_stmt.execute([rowid])?;
                    meta_stmt.execute([rowid])?;
                }
            }
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::pending(rowids.len(), 0));
        }

        let cursor =
            Self::maintenance_state_i64_in_transaction(&tx, SQLITE_VEC0_BUILD_CURSOR_STATE_KEY)?
                .unwrap_or(0);
        let rows = {
            let mut stmt = tx.prepare(
                r#"
                SELECT rowid, model_key, event_id, history_record_id, session_id,
                       event_seq, chunk_index, source_text_sha256, start_char, end_char,
                       dimensions, length(embedding_f32),
                       CASE WHEN length(embedding_f32) = ?3
                            THEN embedding_f32 ELSE NULL END
                FROM event_embedding_chunks
                WHERE rowid > ?1
                ORDER BY rowid
                LIMIT ?2
                "#,
            )?;
            let mapped = stmt.query_map(
                params![
                    cursor,
                    page_units as i64,
                    SEMANTIC_DIMENSIONS.saturating_mul(std::mem::size_of::<f32>()) as i64
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, Option<Vec<u8>>>(12)?,
                    ))
                },
            )?;
            mapped.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if rows.is_empty() {
            let stats_trusted = Self::stats_and_summary_trusted_on_connection(&tx)?;
            let observed_generation =
                Self::maintenance_state_i64_in_transaction(&tx, CANONICAL_GENERATION_STATE_KEY)?;
            let published = observed_generation == Some(canonical_generation) && stats_trusted;
            if published {
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SQLITE_VEC0_READY_STATE_KEY,
                    SEMANTIC_SQLITE_VEC0_PROJECTION_VERSION,
                )?;
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SQLITE_VEC0_GENERATION_STATE_KEY,
                    canonical_generation,
                )?;
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SQLITE_VEC0_ACTIVE_SLOT_STATE_KEY,
                    target_slot,
                )?;
            }
            tx.commit()?;
            return Ok(if published {
                SemanticSidecarMaintenanceOutcome::ready(0, 0)
            } else {
                SemanticSidecarMaintenanceOutcome::pending(0, 0)
            });
        }

        let started = Instant::now();
        let mut processed = 0_usize;
        let mut logical_bytes = 0_u64;
        let mut last_rowid = cursor;
        let mut meta_stmt = tx.prepare(&format!(
            r#"
            INSERT INTO {SQLITE_VEC0_META_TABLE}
                (slot, canonical_rowid, event_id, model_key, history_record_id,
                 session_id, event_seq, chunk_index, source_text_sha256, start_char, end_char)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#
        ))?;
        let mut vec_stmt = tx.prepare(&format!(
            "INSERT INTO {SQLITE_VEC0_TABLE}(rowid, embedding, embedding_coarse, slot, model_key) VALUES (?1, ?2, vec_quantize_binary(?2), ?3, ?4)"
        ))?;
        for (
            rowid,
            model_key,
            event_id,
            history_record_id,
            session_id,
            event_seq,
            chunk_index,
            source_hash,
            start_char,
            end_char,
            dimensions,
            embedding_bytes,
            embedding,
        ) in rows
        {
            if processed > 0
                && (logical_bytes as usize >= SEMANTIC_SIDECAR_MAINTENANCE_MAX_BYTES
                    || started.elapsed()
                        >= StdDuration::from_millis(SEMANTIC_SIDECAR_MAINTENANCE_MAX_MILLIS))
            {
                break;
            }
            processed = processed.saturating_add(1);
            last_rowid = rowid;
            if model_key != semantic_model_key() {
                continue;
            }
            let expected_bytes =
                SEMANTIC_DIMENSIONS.saturating_mul(std::mem::size_of::<f32>()) as i64;
            if dimensions != SEMANTIC_DIMENSIONS as i64
                || embedding_bytes != expected_bytes
                || embedding.is_none()
            {
                return Err(SemanticVectorStoreTerminal::new(format!(
                    "canonical semantic vector row {rowid} has an unsupported payload"
                ))
                .into());
            }
            let embedding = embedding.unwrap_or_default();
            logical_bytes = logical_bytes.saturating_add(embedding.len() as u64);
            meta_stmt.execute(params![
                target_slot,
                rowid,
                event_id,
                semantic_model_key(),
                history_record_id,
                session_id,
                event_seq,
                chunk_index,
                source_hash,
                start_char,
                end_char
            ])?;
            let projection_rowid = tx.last_insert_rowid();
            vec_stmt.execute(params![
                projection_rowid,
                &embedding,
                target_slot,
                semantic_model_key()
            ])?;
        }
        drop(meta_stmt);
        drop(vec_stmt);
        Self::set_maintenance_state_i64_in_transaction(
            &tx,
            SQLITE_VEC0_BUILD_CURSOR_STATE_KEY,
            last_rowid,
        )?;
        tx.commit()?;
        Ok(SemanticSidecarMaintenanceOutcome::pending(
            processed,
            logical_bytes,
        ))
    }

    fn validate_projection_slice(&mut self) -> Result<SemanticSidecarMaintenanceOutcome> {
        let page_units = self.maintenance_page_units()?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let canonical_generation =
            Self::maintenance_state_i64_in_transaction(&tx, CANONICAL_GENERATION_STATE_KEY)?
                .unwrap_or(0);
        let active_slot =
            Self::maintenance_state_i64_in_transaction(&tx, SQLITE_VEC0_ACTIVE_SLOT_STATE_KEY)?
                .filter(|slot| matches!(slot, 0 | 1));
        let projection_ready = canonical_generation > 0
            && Self::maintenance_state_i64_in_transaction(&tx, SQLITE_VEC0_READY_STATE_KEY)?
                == Some(SEMANTIC_SQLITE_VEC0_PROJECTION_VERSION)
            && Self::maintenance_state_i64_in_transaction(&tx, SQLITE_VEC0_GENERATION_STATE_KEY)?
                == Some(canonical_generation)
            && active_slot.is_some()
            && Self::stats_and_summary_trusted_on_connection(&tx)?
            && Self::active_model_tuple_matches_on_connection(&tx)?
            && Self::sqlite_vec0_schema_compatible_on_connection(&tx)?;
        let Some(active_slot) = active_slot.filter(|_| projection_ready) else {
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::pending(0, 0));
        };
        if Self::maintenance_state_i64_in_transaction(
            &tx,
            SQLITE_VEC0_VALIDATE_GENERATION_STATE_KEY,
        )? != Some(canonical_generation)
        {
            for (key, value) in [
                (
                    SQLITE_VEC0_VALIDATE_GENERATION_STATE_KEY,
                    canonical_generation,
                ),
                (SQLITE_VEC0_VALIDATE_PHASE_STATE_KEY, 0),
                (SQLITE_VEC0_VALIDATE_CURSOR_STATE_KEY, 0),
            ] {
                Self::set_maintenance_state_i64_in_transaction(&tx, key, value)?;
            }
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::ready(0, 0));
        }

        let phase =
            Self::maintenance_state_i64_in_transaction(&tx, SQLITE_VEC0_VALIDATE_PHASE_STATE_KEY)?
                .unwrap_or(0);
        let cursor =
            Self::maintenance_state_i64_in_transaction(&tx, SQLITE_VEC0_VALIDATE_CURSOR_STATE_KEY)?
                .unwrap_or(0);
        let expected_bytes = SEMANTIC_DIMENSIONS.saturating_mul(std::mem::size_of::<f32>()) as i64;
        let started = Instant::now();

        if phase == 0 {
            let rows = {
                let mut stmt = tx.prepare(
                    r#"
                    SELECT rowid, model_key, event_id, history_record_id, session_id,
                           event_seq, chunk_index, source_text_sha256, start_char, end_char,
                           dimensions, length(embedding_f32),
                           CASE WHEN length(embedding_f32) = ?3
                                THEN embedding_f32 ELSE NULL END
                    FROM event_embedding_chunks
                    WHERE rowid > ?1
                    ORDER BY rowid
                    LIMIT ?2
                    "#,
                )?;
                let mapped =
                    stmt.query_map(params![cursor, page_units as i64, expected_bytes], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, String>(7)?,
                            row.get::<_, i64>(8)?,
                            row.get::<_, i64>(9)?,
                            row.get::<_, i64>(10)?,
                            row.get::<_, i64>(11)?,
                            row.get::<_, Option<Vec<u8>>>(12)?,
                        ))
                    })?;
                mapped.collect::<rusqlite::Result<Vec<_>>>()?
            };
            if rows.is_empty() {
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SQLITE_VEC0_VALIDATE_PHASE_STATE_KEY,
                    1,
                )?;
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SQLITE_VEC0_VALIDATE_CURSOR_STATE_KEY,
                    0,
                )?;
                tx.commit()?;
                return Ok(SemanticSidecarMaintenanceOutcome::ready(0, 0));
            }

            let mut processed = 0_usize;
            let mut logical_bytes = 0_u64;
            let mut last_rowid = cursor;
            let mut drift = false;
            for (
                rowid,
                model_key,
                event_id,
                history_record_id,
                session_id,
                event_seq,
                chunk_index,
                source_hash,
                start_char,
                end_char,
                dimensions,
                embedding_bytes,
                embedding,
            ) in rows
            {
                if processed > 0
                    && (logical_bytes as usize >= SEMANTIC_SIDECAR_MAINTENANCE_MAX_BYTES
                        || started.elapsed()
                            >= StdDuration::from_millis(SEMANTIC_SIDECAR_MAINTENANCE_MAX_MILLIS))
                {
                    break;
                }
                processed = processed.saturating_add(1);
                last_rowid = rowid;
                if model_key != semantic_model_key() {
                    continue;
                }
                if dimensions != SEMANTIC_DIMENSIONS as i64
                    || embedding_bytes != expected_bytes
                    || embedding.is_none()
                {
                    return Err(SemanticVectorStoreTerminal::new(format!(
                        "canonical semantic vector row {rowid} has an unsupported payload"
                    ))
                    .into());
                }
                let embedding = embedding.unwrap_or_default();
                logical_bytes = logical_bytes.saturating_add(embedding.len() as u64);
                let projected = tx
                    .query_row(
                        &format!(
                            r#"
                            SELECT m.event_id, m.history_record_id, m.session_id, m.event_seq,
                                   m.chunk_index, m.source_text_sha256, m.start_char, m.end_char,
                                   length(v.embedding),
                                   CASE WHEN length(v.embedding) = ?4
                                        THEN v.embedding ELSE NULL END,
                                   length(v.embedding_coarse),
                                   v.embedding_coarse = vec_quantize_binary(?5),
                                   v.slot, v.model_key
                            FROM {SQLITE_VEC0_META_TABLE} AS m
                            JOIN {SQLITE_VEC0_TABLE} AS v ON v.rowid = m.rowid
                            WHERE m.slot = ?1 AND m.model_key = ?2 AND m.canonical_rowid = ?3
                            "#
                        ),
                        params![
                            active_slot,
                            semantic_model_key(),
                            rowid,
                            expected_bytes,
                            &embedding
                        ],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, Option<String>>(1)?,
                                row.get::<_, Option<String>>(2)?,
                                row.get::<_, i64>(3)?,
                                row.get::<_, i64>(4)?,
                                row.get::<_, String>(5)?,
                                row.get::<_, i64>(6)?,
                                row.get::<_, i64>(7)?,
                                row.get::<_, i64>(8)?,
                                row.get::<_, Option<Vec<u8>>>(9)?,
                                row.get::<_, i64>(10)?,
                                row.get::<_, i64>(11)? == 1,
                                row.get::<_, i64>(12)?,
                                row.get::<_, String>(13)?,
                            ))
                        },
                    )
                    .optional()?;
                drift = !projected.is_some_and(
                    |(
                        projected_event_id,
                        projected_history_record_id,
                        projected_session_id,
                        projected_event_seq,
                        projected_chunk_index,
                        projected_source_hash,
                        projected_start_char,
                        projected_end_char,
                        projected_bytes,
                        projected_embedding,
                        projected_coarse_bytes,
                        projected_coarse_matches,
                        projected_slot,
                        projected_model_key,
                    )| {
                        projected_event_id == event_id
                            && projected_history_record_id == history_record_id
                            && projected_session_id == session_id
                            && projected_event_seq == event_seq
                            && projected_chunk_index == chunk_index
                            && projected_source_hash == source_hash
                            && projected_start_char == start_char
                            && projected_end_char == end_char
                            && projected_bytes == expected_bytes
                            && projected_embedding.as_deref() == Some(embedding.as_slice())
                            && projected_coarse_bytes == SEMANTIC_BINARY_VECTOR_BYTES as i64
                            && projected_coarse_matches
                            && projected_slot == active_slot
                            && projected_model_key == semantic_model_key()
                    },
                );
                if drift {
                    break;
                }
            }
            if drift {
                Self::invalidate_projection_in_transaction(&tx)?;
                tx.commit()?;
                return Ok(SemanticSidecarMaintenanceOutcome::pending(
                    processed,
                    logical_bytes,
                ));
            }
            Self::set_maintenance_state_i64_in_transaction(
                &tx,
                SQLITE_VEC0_VALIDATE_CURSOR_STATE_KEY,
                last_rowid,
            )?;
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::ready(
                processed,
                logical_bytes,
            ));
        }

        let rows = {
            let mut stmt = tx.prepare(&format!(
                r#"
                SELECT rowid, canonical_rowid
                FROM {SQLITE_VEC0_META_TABLE}
                WHERE slot = ?1 AND model_key = ?2 AND rowid > ?3
                ORDER BY rowid
                LIMIT ?4
                "#
            ))?;
            let mapped = stmt.query_map(
                params![active_slot, semantic_model_key(), cursor, page_units as i64],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )?;
            mapped.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if rows.is_empty() {
            Self::set_maintenance_state_i64_in_transaction(
                &tx,
                SQLITE_VEC0_VALIDATE_PHASE_STATE_KEY,
                0,
            )?;
            Self::set_maintenance_state_i64_in_transaction(
                &tx,
                SQLITE_VEC0_VALIDATE_CURSOR_STATE_KEY,
                0,
            )?;
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::ready(0, 0));
        }
        let mut processed = 0_usize;
        let mut last_rowid = cursor;
        let mut drift = false;
        for (rowid, canonical_rowid) in rows {
            if processed > 0
                && started.elapsed()
                    >= StdDuration::from_millis(SEMANTIC_SIDECAR_MAINTENANCE_MAX_MILLIS)
            {
                break;
            }
            processed = processed.saturating_add(1);
            last_rowid = rowid;
            let canonical_exists = tx.query_row(
                r#"
                SELECT EXISTS(
                    SELECT 1 FROM event_embedding_chunks
                    WHERE rowid = ?1 AND model_key = ?2
                )
                "#,
                params![canonical_rowid, semantic_model_key()],
                |row| row.get::<_, i64>(0),
            )? == 1;
            let vector_exists = tx.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {SQLITE_VEC0_TABLE} WHERE rowid = ?1 AND slot = ?2 AND model_key = ?3)"
                ),
                params![rowid, active_slot, semantic_model_key()],
                |row| row.get::<_, i64>(0),
            )? == 1;
            if !canonical_exists || !vector_exists {
                drift = true;
                break;
            }
        }
        if drift {
            Self::invalidate_projection_in_transaction(&tx)?;
            tx.commit()?;
            return Ok(SemanticSidecarMaintenanceOutcome::pending(processed, 0));
        }
        Self::set_maintenance_state_i64_in_transaction(
            &tx,
            SQLITE_VEC0_VALIDATE_CURSOR_STATE_KEY,
            last_rowid,
        )?;
        tx.commit()?;
        Ok(SemanticSidecarMaintenanceOutcome::ready(processed, 0))
    }

    fn invalidate_projection_in_transaction(tx: &rusqlite::Transaction<'_>) -> Result<()> {
        for (key, value) in [
            (SQLITE_VEC0_READY_STATE_KEY, 0),
            (SQLITE_VEC0_GENERATION_STATE_KEY, 0),
            (SQLITE_VEC0_BUILD_GENERATION_STATE_KEY, 0),
            (SQLITE_VEC0_VALIDATE_GENERATION_STATE_KEY, 0),
        ] {
            Self::set_maintenance_state_i64_in_transaction(tx, key, value)?;
        }
        Ok(())
    }

    fn begin_write_pacing(&self, nominal_bytes: u64) -> SemanticSidecarWritePacing {
        SemanticSidecarWritePacing {
            wal_bytes_before: self.observed_wal_bytes(),
            nominal_bytes,
        }
    }

    fn finish_write_pacing(&self, pacing: SemanticSidecarWritePacing) -> u64 {
        self.observed_wal_bytes()
            .saturating_sub(pacing.wal_bytes_before)
            .saturating_sub(pacing.nominal_bytes)
    }

    fn observed_wal_bytes(&self) -> u64 {
        let mut wal_path = self.path.as_os_str().to_os_string();
        wal_path.push("-wal");
        fs::metadata(PathBuf::from(wal_path))
            .map(|metadata| metadata.len())
            .unwrap_or(0)
    }

    #[cfg(all(test, ctx_sqlite_vec))]
    fn sync_sqlite_vec0_from_chunks_if_needed(&mut self) -> Result<()> {
        for _ in 0..100_000 {
            if self.run_maintenance_slice()?.is_ready() && self.sqlite_vec0_search_ready()? {
                return Ok(());
            }
        }
        Err(anyhow!("semantic vector maintenance did not converge"))
    }
}
