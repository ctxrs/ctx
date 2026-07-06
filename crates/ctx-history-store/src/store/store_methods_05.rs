#[allow(unused_imports)]
use super::*;

impl Store {
    pub fn files_touched_for_record_matching(
        &self,
        record_id: Uuid,
        file: &str,
    ) -> Result<Vec<FileTouched>> {
        let Some((exact, suffix)) = file_touch_match_values(file) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
            file_touched_select_sql(
                r#"
                WHERE (
                    history_record_id = ?1
                    OR run_id IN (
                         SELECT id FROM runs
                         WHERE history_record_id = ?1
                            OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                    )
                    OR event_id IN (
                         SELECT id FROM events
                         WHERE history_record_id = ?1
                            OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                    )
                )
                AND (
                    path = ?2
                    OR old_path = ?2
                    OR path LIKE ?3 ESCAPE '\'
                    OR old_path LIKE ?3 ESCAPE '\'
                )
                ORDER BY updated_at_ms DESC, id
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(
            params![record_id.to_string(), exact, suffix],
            file_touched_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn file_touch_scope(&self, file: &str) -> Result<FileTouchScope> {
        let Some((exact, suffix)) = file_touch_match_values(file) else {
            return Ok(FileTouchScope::default());
        };
        let mut scope = FileTouchScope::default();
        let mut stmt = self.conn.prepare(
            r#"
            SELECT
                COALESCE(
                    ft.history_record_id,
                    e.history_record_id,
                    r.history_record_id,
                    event_session.history_record_id,
                    run_session.history_record_id,
                    source_session.history_record_id
                ),
                COALESCE(e.session_id, r.session_id, source_session.id),
                ft.run_id,
                ft.event_id,
                ft.source_id
            FROM files_touched ft
            LEFT JOIN events e ON e.id = ft.event_id
            LEFT JOIN runs r ON r.id = ft.run_id
            LEFT JOIN sessions event_session ON event_session.id = e.session_id
            LEFT JOIN sessions run_session ON run_session.id = r.session_id
            LEFT JOIN sessions source_session ON source_session.capture_source_id = ft.source_id
            WHERE ft.path = ?1
               OR ft.old_path = ?1
               OR ft.path LIKE ?2 ESCAPE '\'
               OR ft.old_path LIKE ?2 ESCAPE '\'
            "#,
        )?;
        let rows = stmt.query_map(params![exact, suffix], |row| {
            Ok((
                parse_optional_uuid(row.get(0)?)?,
                parse_optional_uuid(row.get(1)?)?,
                parse_optional_uuid(row.get(2)?)?,
                parse_optional_uuid(row.get(3)?)?,
                parse_optional_uuid(row.get(4)?)?,
            ))
        })?;
        for row in rows {
            let (record_id, session_id, run_id, event_id, source_id) = row?;
            if let Some(id) = record_id {
                scope.history_record_ids.insert(id);
            }
            if let Some(id) = session_id {
                scope.session_ids.insert(id);
            }
            if let Some(id) = run_id {
                scope.run_ids.insert(id);
            }
            if let Some(id) = event_id {
                scope.event_ids.insert(id);
            }
            if let Some(id) = source_id {
                scope.source_ids.insert(id);
            }
        }
        Ok(scope)
    }

    pub fn upsert_history_record_link(&self, link: &HistoryRecordLink) -> Result<Uuid> {
        self.conn.execute(
            r#"
            INSERT INTO history_record_links
            (id, history_record_id, target_type, target_id, link_type, confidence, source_id, created_at_ms, updated_at_ms, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            ON CONFLICT(history_record_id, target_type, target_id, link_type) DO UPDATE SET
                confidence = excluded.confidence,
                source_id = excluded.source_id,
                updated_at_ms = excluded.updated_at_ms,
                visibility = excluded.visibility,
                fidelity = excluded.fidelity,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                deleted_at_ms = excluded.deleted_at_ms,
                metadata_json = excluded.metadata_json
            "#,
            params![
                link.id.to_string(),
                link.history_record_id.to_string(),
                link.target_type.as_str(),
                link.target_id.to_string(),
                link.link_type.as_str(),
                link.confidence.as_str(),
                optional_uuid_string(link.source_id),
                timestamp_ms(link.timestamps.created_at),
                timestamp_ms(link.timestamps.updated_at),
                link.sync.visibility.as_str(),
                link.sync.fidelity.as_str(),
                link.sync.sync_state.as_str(),
                link.sync.sync_version as i64,
                optional_timestamp_ms(link.sync.deleted_at),
                serde_json::to_string(&link.sync.metadata)?,
            ],
        )?;
        self.conn
            .query_row(
                "SELECT id FROM history_record_links WHERE history_record_id = ?1 AND target_type = ?2 AND target_id = ?3 AND link_type = ?4",
                params![
                    link.history_record_id.to_string(),
                    link.target_type.as_str(),
                    link.target_id.to_string(),
                    link.link_type.as_str()
                ],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub(crate) fn list_history_record_links(&self) -> Result<Vec<HistoryRecordLink>> {
        let mut stmt = self
            .conn
            .prepare(history_record_link_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], history_record_link_from_row)?;
        collect_rows(rows)
    }

    pub fn upsert_sync_cursor(&self, cursor: &SyncCursor) -> Result<Uuid> {
        if let Some(existing) =
            self.get_sync_cursor(cursor.team_id.as_deref(), &cursor.device_id, &cursor.stream)?
        {
            self.conn.execute(
                r#"
                UPDATE sync_cursors
                SET cursor = ?1, last_synced_at_ms = ?2, updated_at_ms = ?3
                WHERE id = ?4
                "#,
                params![
                    cursor.cursor.as_str(),
                    optional_timestamp_ms(cursor.last_synced_at),
                    timestamp_ms(cursor.timestamps.updated_at),
                    existing.id.to_string(),
                ],
            )?;
            return Ok(existing.id);
        }

        self.conn.execute(
            r#"
            INSERT INTO sync_cursors
            (id, team_id, device_id, stream, cursor, last_synced_at_ms, created_at_ms, updated_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(team_id, device_id, stream) DO UPDATE SET
                cursor = excluded.cursor,
                last_synced_at_ms = excluded.last_synced_at_ms,
                updated_at_ms = excluded.updated_at_ms
            "#,
            params![
                cursor.id.to_string(),
                cursor.team_id.as_deref(),
                cursor.device_id.as_str(),
                cursor.stream.as_str(),
                cursor.cursor.as_str(),
                optional_timestamp_ms(cursor.last_synced_at),
                timestamp_ms(cursor.timestamps.created_at),
                timestamp_ms(cursor.timestamps.updated_at),
            ],
        )?;
        self.conn
            .query_row(
                "SELECT id FROM sync_cursors WHERE team_id IS ?1 AND device_id = ?2 AND stream = ?3",
                params![cursor.team_id.as_deref(), cursor.device_id.as_str(), cursor.stream.as_str()],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn get_sync_cursor(
        &self,
        team_id: Option<&str>,
        device_id: &str,
        stream: &str,
    ) -> Result<Option<SyncCursor>> {
        self.conn
            .query_row(
                "SELECT id, team_id, device_id, stream, cursor, last_synced_at_ms, created_at_ms, updated_at_ms FROM sync_cursors WHERE team_id IS ?1 AND device_id = ?2 AND stream = ?3",
                params![team_id, device_id, stream],
                sync_cursor_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn insert_record(&self, record: &HistoryRecord) -> Result<()> {
        let created_at_ms = timestamp_ms(record.created_at);
        let updated_at_ms = timestamp_ms(record.updated_at);
        self.conn.execute(
            r#"
            INSERT INTO history_records
            (
                id, title, summary, status, started_at_ms, last_activity_at_ms,
                created_at_ms, updated_at_ms, body, tags_json, kind, workspace,
                created_at, updated_at
            )
            VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                record.id.to_string(),
                record.title,
                record.body,
                created_at_ms,
                updated_at_ms,
                record.body,
                serde_json::to_string(&record.tags)?,
                record.kind,
                record.workspace,
                record.created_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
            ],
        )?;
        upsert_record_search_projection(&self.conn, record)?;
        Ok(())
    }

    pub fn upsert_record(&self, record: &HistoryRecord) -> Result<()> {
        self.upsert_record_row(record)?;
        upsert_record_search_projection(&self.conn, record)?;
        Ok(())
    }

    pub fn upsert_records(&self, records: &[HistoryRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        self.begin_immediate_batch()?;
        for record in records {
            if let Err(err) = self.upsert_record_row(record) {
                let _ = self.rollback_batch();
                return Err(err);
            }
        }
        if let Err(err) = self.commit_batch() {
            let _ = self.rollback_batch();
            return Err(err);
        }
        for record in records {
            upsert_record_search_projection(&self.conn, record)?;
        }
        Ok(())
    }

    pub(crate) fn upsert_record_row(&self, record: &HistoryRecord) -> Result<()> {
        let created_at_ms = timestamp_ms(record.created_at);
        let updated_at_ms = timestamp_ms(record.updated_at);
        self.conn.execute(
            r#"
            INSERT INTO history_records
            (
                id, title, summary, status, started_at_ms, last_activity_at_ms,
                created_at_ms, updated_at_ms, body, tags_json, kind, workspace,
                created_at, updated_at
            )
            VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                summary = excluded.summary,
                status = excluded.status,
                started_at_ms = excluded.started_at_ms,
                last_activity_at_ms = excluded.last_activity_at_ms,
                created_at_ms = excluded.created_at_ms,
                updated_at_ms = excluded.updated_at_ms,
                body = excluded.body,
                tags_json = excluded.tags_json,
                kind = excluded.kind,
                workspace = excluded.workspace,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at
            "#,
            params![
                record.id.to_string(),
                record.title,
                record.body,
                created_at_ms,
                updated_at_ms,
                record.body,
                serde_json::to_string(&record.tags)?,
                record.kind,
                record.workspace,
                record.created_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_record(&self, id: Uuid) -> Result<HistoryRecord> {
        self.conn
            .query_row(
                record_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                record_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn list_records(&self, limit: usize) -> Result<Vec<HistoryRecord>> {
        self.list_records_page(limit, 0)
    }

    pub fn list_records_page(&self, limit: usize, offset: usize) -> Result<Vec<HistoryRecord>> {
        let mut stmt = self.conn.prepare(
            record_select_sql("ORDER BY created_at DESC, id LIMIT ?1 OFFSET ?2").as_str(),
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], record_from_row)?;
        collect_rows(rows)
    }

    pub fn search_records(&self, query: &str, limit: usize) -> Result<Vec<HistoryRecord>> {
        self.search_records_page(query, limit, 0)
    }

    pub fn search_records_page(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<HistoryRecord>> {
        if fts_match_query(query).is_none() {
            return Ok(Vec::new());
        }
        if let Some(records) = self.search_records_fts(query, limit, offset)? {
            return Ok(records);
        }
        let like = format!("%{}%", query);
        let mut stmt = self.conn.prepare(
            record_select_sql(
                "WHERE title LIKE ?1 OR body LIKE ?1 OR tags_json LIKE ?1 ORDER BY created_at DESC, id LIMIT ?2 OFFSET ?3",
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![like, limit as i64, offset as i64], record_from_row)?;
        collect_rows(rows)
    }

    pub(crate) fn search_records_fts(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Option<Vec<HistoryRecord>>> {
        if !table_exists(&self.conn, "ctx_history_search")? {
            return Ok(None);
        }
        let Some(match_query) = fts_match_query(query) else {
            return Ok(Some(Vec::new()));
        };
        let has_event_search = table_exists(&self.conn, "event_search")?;
        let has_artifact_search = table_exists(&self.conn, "artifact_search")?;
        let sql = if has_event_search && has_artifact_search {
            r#"
            WITH matches(record_id, score) AS (
                SELECT record_id, bm25(ctx_history_search)
                FROM ctx_history_search
                WHERE ctx_history_search MATCH ?1
                UNION ALL
                SELECT history_record_id, bm25(event_search)
                FROM event_search
                WHERE event_search MATCH ?1 AND history_record_id IS NOT NULL
                UNION ALL
                SELECT history_record_id, bm25(artifact_search)
                FROM artifact_search
                WHERE artifact_search MATCH ?1 AND history_record_id IS NOT NULL
            )
            SELECT record_id
            FROM matches
            WHERE record_id IS NOT NULL
            GROUP BY record_id
            ORDER BY MIN(score), record_id
            LIMIT ?2 OFFSET ?3
            "#
        } else {
            r#"
            SELECT record_id
            FROM ctx_history_search
            WHERE ctx_history_search MATCH ?1
            ORDER BY bm25(ctx_history_search), record_id
            LIMIT ?2 OFFSET ?3
            "#
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![match_query, limit as i64, offset as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut records = Vec::new();
        for row in rows {
            records.push(self.get_record(parse_uuid(row?)?)?);
        }
        Ok(Some(records))
    }

    pub fn max_events_per_history_record(&self) -> Result<i64> {
        let max_events = self.conn.query_row(
            r#"
            SELECT COALESCE(MAX(event_count), 0)
            FROM (
                SELECT COUNT(*) AS event_count
                FROM events
                GROUP BY history_record_id
            )
            "#,
            [],
            |row| row.get(0),
        )?;
        Ok(max_events)
    }

    pub fn has_at_least_events(&self, threshold: i64) -> Result<bool> {
        if threshold <= 0 {
            return Ok(true);
        }
        let exists = self.conn.query_row(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM events
                LIMIT 1 OFFSET ?1
            )
            "#,
            params![threshold - 1],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(exists != 0)
    }

    pub fn has_provider_data(&self, provider: CaptureProvider) -> Result<bool> {
        let exists = self.conn.query_row(
            r#"
            SELECT
                EXISTS(
                    SELECT 1
                    FROM sessions
                    WHERE provider = ?1
                    LIMIT 1
                )
                OR EXISTS(
                    SELECT 1
                    FROM capture_sources
                    WHERE provider = ?1
                    LIMIT 1
                )
            "#,
            params![provider.as_str()],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(exists != 0)
    }

    pub fn search_event_hits(&self, query: &str, limit: usize) -> Result<Vec<EventSearchHit>> {
        self.search_event_hits_page(query, limit, 0)
    }

    pub fn search_event_hits_page(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<EventSearchHit>> {
        if !table_exists(&self.conn, "event_search")? {
            return Ok(Vec::new());
        }
        let Some(match_query) = fts_match_query(query) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
            r#"
            SELECT event_search.event_id,
                   COALESCE(e.history_record_id, event_search.history_record_id, s.history_record_id, rs.history_record_id),
                   COALESCE(e.session_id, event_search.session_id, s.id, rs.id),
                   e.run_id,
                   e.seq,
                   e.event_type,
                   e.role,
                   e.occurred_at_ms,
                   event_search.safe_preview_text,
                   bm25(event_search),
                   COALESCE(s.provider, rs.provider, event_source.provider, session_source.provider, run_source.provider),
                   COALESCE(s.external_session_id, rs.external_session_id),
                   COALESCE(s.parent_session_id, rs.parent_session_id),
                   COALESCE(s.root_session_id, rs.root_session_id),
                   COALESCE(s.agent_type, rs.agent_type),
                   COALESCE(s.is_primary, rs.is_primary),
                   COALESCE(event_source.cwd, session_source.cwd, run_source.cwd),
                   COALESCE(event_source.raw_source_path, session_source.raw_source_path, run_source.raw_source_path),
                   e.payload_json,
                   COALESCE(event_source.metadata_json, session_source.metadata_json, run_source.metadata_json),
                   wr.title,
                   wr.kind,
                   wr.workspace
            FROM event_search
            JOIN events e ON e.id = event_search.event_id
            LEFT JOIN runs r ON r.id = e.run_id
            LEFT JOIN sessions s ON s.id = COALESCE(e.session_id, event_search.session_id)
            LEFT JOIN sessions rs ON rs.id = r.session_id
            LEFT JOIN capture_sources event_source ON event_source.id = e.capture_source_id
            LEFT JOIN capture_sources session_source ON session_source.id = COALESCE(s.capture_source_id, rs.capture_source_id)
            LEFT JOIN capture_sources run_source ON run_source.id = r.source_id
            LEFT JOIN history_records wr ON wr.id = COALESCE(e.history_record_id, event_search.history_record_id, s.history_record_id, rs.history_record_id, r.history_record_id)
            WHERE event_search MATCH ?1
            ORDER BY bm25(event_search), e.occurred_at_ms DESC, e.seq DESC, event_search.event_id
            LIMIT ?2 OFFSET ?3
            "#,
        )?;
        let rows = stmt.query_map(
            params![match_query, limit.max(1) as i64, offset as i64],
            |row| {
                let payload_json = row.get::<_, String>(18)?;
                let source_metadata_json = row.get::<_, Option<String>>(19)?;
                let source_identity =
                    event_search_source_identity(source_metadata_json.as_deref())?;
                Ok(EventSearchHit {
                    event_id: parse_uuid(row.get::<_, String>(0)?)?,
                    history_record_id: parse_optional_uuid(row.get(1)?)?,
                    session_id: parse_optional_uuid(row.get(2)?)?,
                    run_id: parse_optional_uuid(row.get(3)?)?,
                    seq: nonnegative_i64_to_u64(row.get(4)?)?,
                    event_type: parse_text_enum::<EventType>(row.get::<_, String>(5)?)?,
                    role: parse_optional_text_enum::<EventRole>(row.get(6)?)?,
                    occurred_at: ms_to_time(row.get(7)?)?,
                    preview: row.get(8)?,
                    score: row.get(9)?,
                    provider: parse_optional_text_enum::<CaptureProvider>(row.get(10)?)?,
                    session_external_session_id: row.get(11)?,
                    history_source: source_identity.history_source,
                    history_source_plugin: source_identity.history_source_plugin,
                    provider_key: source_identity.provider_key,
                    source_id: source_identity.source_id,
                    source_format: source_identity.source_format,
                    session_parent_session_id: parse_optional_uuid(row.get(12)?)?,
                    session_root_session_id: parse_optional_uuid(row.get(13)?)?,
                    agent_type: parse_optional_text_enum::<AgentType>(row.get(14)?)?,
                    session_is_primary: row.get::<_, Option<i64>>(15)?.map(|value| value != 0),
                    cwd: row.get(16)?,
                    raw_source_path: row.get(17)?,
                    cursor: event_search_cursor(&payload_json, source_metadata_json.as_deref())?,
                    record_title: row.get(20)?,
                    record_kind: row.get(21)?,
                    record_workspace: row.get(22)?,
                })
            },
        )?;
        collect_rows(rows)
    }
}
