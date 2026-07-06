#[allow(unused_imports)]
use super::*;

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_busy_timeout(path, BUSY_TIMEOUT)
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let object_dir = path
            .parent()
            .map(|parent| parent.join(OBJECTS_DIR))
            .unwrap_or_else(|| PathBuf::from(OBJECTS_DIR));
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        configure_read_only_connection(&conn, BUSY_TIMEOUT)?;
        let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if user_version != SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion(user_version));
        }
        Ok(Self {
            path,
            object_dir,
            conn,
            busy_timeout: BUSY_TIMEOUT,
        })
    }

    pub fn open_with_busy_timeout(path: impl AsRef<Path>, busy_timeout: Duration) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut migrated_legacy_layout = false;
        if let Some(parent) = path.parent() {
            migrated_legacy_layout = migrate_legacy_history_layout(parent)?;
            fs::create_dir_all(parent)?;
            restrict_private_dir(parent)?;
        }
        let object_dir = path
            .parent()
            .map(|parent| parent.join(OBJECTS_DIR))
            .unwrap_or_else(|| PathBuf::from(OBJECTS_DIR));
        fs::create_dir_all(&object_dir)?;
        restrict_private_dir(&object_dir)?;
        if let Some(spool_dir) = path.parent().map(|parent| parent.join(SPOOL_DIR)) {
            fs::create_dir_all(&spool_dir)?;
            restrict_private_dir(&spool_dir)?;
        }
        let conn = Connection::open(&path)?;
        restrict_private_file(&path)?;
        configure_connection(&conn, busy_timeout)?;
        let store = Self {
            path,
            object_dir,
            conn,
            busy_timeout,
        };
        store.migrate()?;
        if migrated_legacy_layout {
            store.normalize_legacy_blob_paths()?;
        }
        store.ensure_search_projection_initialized()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn raw_sql_query(&self, sql: &str, options: RawSqlOptions) -> Result<RawSqlResult> {
        let sql = sql.trim();
        if sql.is_empty() {
            return Err(StoreError::RawSqlEmpty);
        }
        validate_raw_sql_options(&options)?;
        validate_raw_sql_statement_bytes(sql, &options)?;
        reject_sql_tail(&self.conn, sql)?;
        let _limits = RawSqlLimitGuard::apply(&self.conn, &options)?;

        let mut stmt = self.conn.prepare(sql)?;
        if stmt.parameter_count() > 0 {
            return Err(StoreError::RawSqlHasParameters);
        }
        if !stmt.readonly() {
            return Err(StoreError::RawSqlNotReadOnly);
        }
        let column_count = stmt.column_count();
        if column_count == 0 {
            return Err(StoreError::RawSqlNoColumns);
        }
        if column_count > options.max_columns {
            return Err(StoreError::RawSqlTooManyColumns {
                columns: column_count,
                max_columns: options.max_columns,
            });
        }
        validate_raw_sql_result_preview_budget(&options, column_count)?;

        let columns = stmt
            .column_names()
            .into_iter()
            .map(|name| RawSqlColumn {
                name: name.to_owned(),
            })
            .collect::<Vec<_>>();
        let started = Instant::now();
        let timeout = options.timeout;
        let progress_started = started;
        self.conn
            .progress_handler(1_000, Some(move || progress_started.elapsed() >= timeout));

        let query_result = (|| -> Result<RawSqlResult> {
            let mut rows = stmt.query([])?;
            let mut output_rows = Vec::new();
            let mut rows_truncated = false;
            let mut values_truncated = false;

            while let Some(row) = rows.next()? {
                if output_rows.len() >= options.max_rows {
                    rows_truncated = true;
                    break;
                }
                let mut output_row = Vec::with_capacity(column_count);
                for index in 0..column_count {
                    let value = raw_sql_value(row.get_ref(index)?, options.max_value_bytes);
                    if value.is_truncated() {
                        values_truncated = true;
                    }
                    output_row.push(value);
                }
                output_rows.push(output_row);
            }

            Ok(RawSqlResult {
                returned_rows: output_rows.len(),
                columns,
                rows: output_rows,
                truncated: RawSqlTruncation {
                    rows: rows_truncated,
                    values: values_truncated,
                },
                elapsed: started.elapsed(),
                limits: RawSqlLimits {
                    max_rows: options.max_rows,
                    max_columns: options.max_columns,
                    max_value_bytes: options.max_value_bytes,
                    max_sql_bytes: options.max_sql_bytes,
                    timeout_ms: duration_ms(options.timeout),
                },
            })
        })();

        self.conn.progress_handler(0, None::<fn() -> bool>);

        match query_result {
            Err(StoreError::Sql(rusqlite::Error::SqliteFailure(error, _)))
                if error.code == ErrorCode::OperationInterrupted
                    && started.elapsed() >= options.timeout =>
            {
                Err(StoreError::RawSqlTimedOut {
                    timeout_ms: duration_ms(options.timeout),
                })
            }
            other => other,
        }
    }

    pub fn begin_immediate_batch(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(())
    }

    pub fn commit_batch(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    pub fn rollback_batch(&self) -> Result<()> {
        self.conn.execute_batch("ROLLBACK")?;
        Ok(())
    }

    pub fn checkpoint_wal_passive(&self) -> Result<()> {
        self.conn
            .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()))?;
        Ok(())
    }

    pub fn checkpoint_wal_truncate(&self) -> Result<()> {
        self.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }

    pub fn checkpoint_wal_passive_if_larger_than(&self, min_bytes: u64) -> Result<bool> {
        let Some(wal_bytes) = self.wal_bytes()? else {
            return Ok(false);
        };
        if wal_bytes < min_bytes {
            return Ok(false);
        }
        self.checkpoint_wal_passive()?;
        Ok(true)
    }

    pub fn checkpoint_wal_truncate_if_larger_than(&self, min_bytes: u64) -> Result<bool> {
        let Some(wal_bytes) = self.wal_bytes()? else {
            return Ok(false);
        };
        if wal_bytes < min_bytes {
            return Ok(false);
        }
        self.checkpoint_wal_truncate()?;
        Ok(true)
    }

    pub(crate) fn wal_path(&self) -> PathBuf {
        let mut path = self.path.as_os_str().to_os_string();
        path.push("-wal");
        PathBuf::from(path)
    }

    pub(crate) fn wal_bytes(&self) -> Result<Option<u64>> {
        match fs::metadata(self.wal_path()) {
            Ok(metadata) => Ok(Some(metadata.len())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(StoreError::Io(err)),
        }
    }

    pub fn migrate(&self) -> Result<()> {
        configure_connection(&self.conn, self.busy_timeout)?;
        let user_version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if user_version > SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion(user_version));
        }
        if user_version < 1 {
            migrate_to_v1(&self.conn)?;
        }
        if user_version < 2 {
            migrate_to_v2(&self.conn)?;
        }
        if user_version < 3 {
            migrate_to_v3(&self.conn)?;
        }
        if user_version < 4 {
            migrate_to_v4(&self.conn)?;
        }
        if user_version < 5 {
            migrate_to_v5(&self.conn)?;
        }
        if user_version < 6 {
            migrate_to_v6(&self.conn)?;
        }
        if user_version < 7 {
            migrate_to_v7(&self.conn)?;
        }
        if user_version < 8 {
            migrate_to_v8(&self.conn)?;
        }
        if user_version < 9 {
            migrate_to_v9(&self.conn)?;
        }
        if user_version < 10 {
            migrate_to_v10(&self.conn)?;
        }
        if user_version < 11 {
            migrate_to_v11(&self.conn)?;
        }
        if user_version < 12 {
            migrate_to_v12(&self.conn)?;
        }
        if user_version < 13 {
            migrate_to_v13(&self.conn)?;
        }
        if user_version < 14 {
            migrate_to_v14(&self.conn)?;
        }
        if user_version < 15 {
            migrate_to_v15(&self.conn)?;
        }
        if user_version < 16 {
            migrate_to_v16(&self.conn)?;
        }
        if user_version < 42 {
            migrate_to_v42(&self.conn)?;
        }
        create_fts_tables_if_supported(&self.conn)?;
        Ok(())
    }

    pub fn schema(&self) -> Result<String> {
        let mut stmt = self.conn.prepare(
            "SELECT sql FROM sqlite_master
             WHERE type IN ('table', 'index', 'view') AND sql IS NOT NULL
             ORDER BY type, name",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut schema = Vec::new();
        for row in rows {
            schema.push(row?);
        }
        Ok(schema.join(";\n"))
    }

    pub fn refresh_search_index(&self) -> Result<()> {
        self.rebuild_search_projection()
    }

    pub fn optimize_search_index(&self) -> Result<()> {
        for table in ["ctx_history_search", "event_search", "artifact_search"] {
            if table_exists(&self.conn, table)? {
                self.conn.execute(
                    format!("INSERT INTO {table}({table}) VALUES ('optimize')").as_str(),
                    [],
                )?;
            }
        }
        Ok(())
    }

    pub fn event_search_projection_needs_backfill(&self) -> Result<bool> {
        if !table_exists(&self.conn, "event_search")? {
            return Ok(false);
        }
        Ok(table_row_count(&self.conn, "events")? > 0
            && table_row_count(&self.conn, "event_search")? == 0)
    }

    pub fn upsert_capture_source(&self, source: &CaptureSource) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO capture_sources
            (
                id, kind, provider, machine_id, process_id, cwd, raw_source_path,
                external_session_id, started_at_ms, ended_at_ms, fidelity,
                visibility, sync_state, sync_version, metadata_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            ON CONFLICT(id) DO UPDATE SET
                kind = excluded.kind,
                provider = excluded.provider,
                machine_id = excluded.machine_id,
                process_id = excluded.process_id,
                cwd = excluded.cwd,
                raw_source_path = excluded.raw_source_path,
                external_session_id = excluded.external_session_id,
                started_at_ms = excluded.started_at_ms,
                ended_at_ms = excluded.ended_at_ms,
                fidelity = excluded.fidelity,
                visibility = excluded.visibility,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                metadata_json = excluded.metadata_json
            "#,
            params![
                source.id.to_string(),
                source.descriptor.kind.as_str(),
                source.descriptor.provider.as_str(),
                source.descriptor.machine_id.as_str(),
                source.descriptor.process_id.map(i64::from),
                source.descriptor.cwd.as_deref(),
                source.descriptor.raw_source_path.as_deref(),
                source.descriptor.external_session_id.as_deref(),
                timestamp_ms(source.started_at),
                optional_timestamp_ms(source.ended_at),
                source.sync.fidelity.as_str(),
                source.sync.visibility.as_str(),
                source.sync.sync_state.as_str(),
                source.sync.sync_version as i64,
                serde_json::to_string(&source.sync.metadata)?,
            ],
        )?;
        Ok(())
    }

    pub fn get_capture_source(&self, id: Uuid) -> Result<CaptureSource> {
        self.conn
            .query_row(
                "SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json FROM capture_sources WHERE id = ?1",
                params![id.to_string()],
                capture_source_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn list_capture_sources(&self) -> Result<Vec<CaptureSource>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json FROM capture_sources ORDER BY started_at_ms, id",
        )?;
        let rows = stmt.query_map([], capture_source_from_row)?;
        collect_rows(rows)
    }

    pub fn capture_source_count(&self) -> Result<usize> {
        let count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM capture_sources", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn capture_source_by_external_session(
        &self,
        provider: CaptureProvider,
        external_session_id: &str,
    ) -> Result<Option<CaptureSource>> {
        self.conn
            .query_row(
                "SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json FROM capture_sources WHERE provider = ?1 AND external_session_id = ?2 ORDER BY started_at_ms DESC LIMIT 1",
                params![provider.as_str(), external_session_id],
                capture_source_from_row,
            )
            .optional()
            .map_err(StoreError::from)
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
}
