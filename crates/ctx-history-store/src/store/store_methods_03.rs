#[allow(unused_imports)]
use super::*;

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
        Ok(CatalogCounts {
            total,
            indexed,
            stale,
            pending,
            failed,
        })
    }

    pub fn upsert_session(&self, session: &Session) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO sessions
            (
                id, history_record_id, parent_session_id, root_session_id, capture_source_id,
                provider, external_session_id, external_agent_id, agent_type, role_hint,
                is_primary, status, fidelity, transcript_blob_id, started_at_ms, ended_at_ms,
                created_at_ms, updated_at_ms, visibility, sync_state, sync_version,
                deleted_at_ms, metadata_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
            ON CONFLICT(id) DO UPDATE SET
                history_record_id = excluded.history_record_id,
                parent_session_id = excluded.parent_session_id,
                root_session_id = excluded.root_session_id,
                capture_source_id = excluded.capture_source_id,
                provider = excluded.provider,
                external_session_id = excluded.external_session_id,
                external_agent_id = excluded.external_agent_id,
                agent_type = excluded.agent_type,
                role_hint = excluded.role_hint,
                is_primary = excluded.is_primary,
                status = excluded.status,
                fidelity = excluded.fidelity,
                transcript_blob_id = excluded.transcript_blob_id,
                started_at_ms = excluded.started_at_ms,
                ended_at_ms = excluded.ended_at_ms,
                updated_at_ms = excluded.updated_at_ms,
                visibility = excluded.visibility,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                deleted_at_ms = excluded.deleted_at_ms,
                metadata_json = excluded.metadata_json
            "#,
            params![
                session.id.to_string(),
                optional_uuid_string(session.history_record_id),
                optional_uuid_string(session.parent_session_id),
                optional_uuid_string(session.root_session_id),
                optional_uuid_string(session.capture_source_id),
                session.provider.as_str(),
                session.external_session_id.as_deref(),
                session.external_agent_id.as_deref(),
                session.agent_type.as_str(),
                session.role_hint.as_deref(),
                session.is_primary as i64,
                session.status.as_str(),
                session.sync.fidelity.as_str(),
                optional_uuid_string(session.transcript_blob_id),
                timestamp_ms(session.started_at),
                optional_timestamp_ms(session.ended_at),
                timestamp_ms(session.timestamps.created_at),
                timestamp_ms(session.timestamps.updated_at),
                session.sync.visibility.as_str(),
                session.sync.sync_state.as_str(),
                session.sync.sync_version as i64,
                optional_timestamp_ms(session.sync.deleted_at),
                serde_json::to_string(&session.sync.metadata)?,
            ],
        )?;
        Ok(())
    }

    pub fn get_session(&self, id: Uuid) -> Result<Session> {
        self.conn
            .query_row(
                session_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                session_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn sessions_by_id_prefix(&self, prefix: &str) -> Result<Vec<Session>> {
        let mut stmt = self
            .conn
            .prepare(session_select_sql("WHERE id LIKE ?1 ORDER BY id LIMIT 2").as_str())?;
        let rows = stmt.query_map(params![format!("{prefix}%")], session_from_row)?;
        collect_rows(rows)
    }

    pub fn session_by_external_session(
        &self,
        provider: CaptureProvider,
        external_session_id: &str,
    ) -> Result<Option<Session>> {
        self.conn
            .query_row(
                session_select_sql(
                    "WHERE provider = ?1 AND external_session_id = ?2 ORDER BY started_at_ms DESC LIMIT 1",
                )
                .as_str(),
                params![provider.as_str(), external_session_id],
                session_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn sessions_by_external_session_limited(
        &self,
        provider: CaptureProvider,
        external_session_id: &str,
        limit: usize,
    ) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            session_select_sql(
                "WHERE provider = ?1 AND external_session_id = ?2 ORDER BY started_at_ms DESC LIMIT ?3",
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(
            params![
                provider.as_str(),
                external_session_id,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            session_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn sessions_for_record(&self, record_id: Uuid) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            session_select_sql("WHERE history_record_id = ?1 ORDER BY started_at_ms, id").as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], session_from_row)?;
        collect_rows(rows)
    }

    pub fn assign_session_to_record(&self, session_id: Uuid, record_id: Uuid) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET history_record_id = ?1 WHERE id = ?2",
            params![record_id.to_string(), session_id.to_string()],
        )?;
        self.conn.execute(
            "UPDATE events SET history_record_id = ?1 WHERE session_id = ?2",
            params![record_id.to_string(), session_id.to_string()],
        )?;
        self.conn.execute(
            "UPDATE runs SET history_record_id = ?1 WHERE session_id = ?2",
            params![record_id.to_string(), session_id.to_string()],
        )?;
        Ok(())
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self
            .conn
            .prepare(session_select_sql("ORDER BY started_at_ms, id").as_str())?;
        let rows = stmt.query_map([], session_from_row)?;
        collect_rows(rows)
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

    pub fn upsert_session_edge(&self, edge: &SessionEdge) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO session_edges
            (id, from_session_id, to_session_id, edge_type, confidence, source_id, created_at_ms, updated_at_ms, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            ON CONFLICT(id) DO UPDATE SET
                from_session_id = excluded.from_session_id,
                to_session_id = excluded.to_session_id,
                edge_type = excluded.edge_type,
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
                edge.id.to_string(),
                edge.from_session_id.to_string(),
                edge.to_session_id.to_string(),
                edge.edge_type.as_str(),
                edge.confidence.as_str(),
                optional_uuid_string(edge.source_id),
                timestamp_ms(edge.timestamps.created_at),
                timestamp_ms(edge.timestamps.updated_at),
                edge.sync.visibility.as_str(),
                edge.sync.fidelity.as_str(),
                edge.sync.sync_state.as_str(),
                edge.sync.sync_version as i64,
                optional_timestamp_ms(edge.sync.deleted_at),
                serde_json::to_string(&edge.sync.metadata)?,
            ],
        )?;
        Ok(())
    }

    pub fn session_edge_exists(&self, edge_id: Uuid) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM session_edges WHERE id = ?1",
                params![edge_id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn upsert_run(&self, run: &Run) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO runs
            (id, history_record_id, session_id, run_type, status, started_at_ms, ended_at_ms, exit_code, cwd, command_preview, input_blob_id, output_blob_id, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
            ON CONFLICT(id) DO UPDATE SET
                history_record_id = excluded.history_record_id,
                session_id = excluded.session_id,
                run_type = excluded.run_type,
                status = excluded.status,
                started_at_ms = excluded.started_at_ms,
                ended_at_ms = excluded.ended_at_ms,
                exit_code = excluded.exit_code,
                cwd = excluded.cwd,
                command_preview = excluded.command_preview,
                input_blob_id = excluded.input_blob_id,
                output_blob_id = excluded.output_blob_id,
                updated_at_ms = excluded.updated_at_ms,
                source_id = excluded.source_id,
                visibility = excluded.visibility,
                fidelity = excluded.fidelity,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                deleted_at_ms = excluded.deleted_at_ms,
                metadata_json = excluded.metadata_json
            "#,
            params![
                run.id.to_string(),
                optional_uuid_string(run.history_record_id),
                optional_uuid_string(run.session_id),
                run.run_type.as_str(),
                run.status.as_str(),
                timestamp_ms(run.started_at),
                optional_timestamp_ms(run.ended_at),
                run.exit_code,
                run.cwd.as_deref(),
                run.command_preview.as_deref(),
                optional_uuid_string(run.input_blob_id),
                optional_uuid_string(run.output_blob_id),
                timestamp_ms(run.timestamps.created_at),
                timestamp_ms(run.timestamps.updated_at),
                optional_uuid_string(run.source_id),
                run.sync.visibility.as_str(),
                run.sync.fidelity.as_str(),
                run.sync.sync_state.as_str(),
                run.sync.sync_version as i64,
                optional_timestamp_ms(run.sync.deleted_at),
                serde_json::to_string(&run.sync.metadata)?,
            ],
        )?;
        Ok(())
    }

    pub fn insert_run_if_absent(&self, run: &Run) -> Result<bool> {
        let changed = self
            .conn
            .prepare_cached(
                r#"
                INSERT OR IGNORE INTO runs
                (id, history_record_id, session_id, run_type, status, started_at_ms, ended_at_ms, exit_code, cwd, command_preview, input_blob_id, output_blob_id, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
                "#,
            )?
            .execute(params![
                run.id.to_string(),
                optional_uuid_string(run.history_record_id),
                optional_uuid_string(run.session_id),
                run.run_type.as_str(),
                run.status.as_str(),
                timestamp_ms(run.started_at),
                optional_timestamp_ms(run.ended_at),
                run.exit_code,
                run.cwd.as_deref(),
                run.command_preview.as_deref(),
                optional_uuid_string(run.input_blob_id),
                optional_uuid_string(run.output_blob_id),
                timestamp_ms(run.timestamps.created_at),
                timestamp_ms(run.timestamps.updated_at),
                optional_uuid_string(run.source_id),
                run.sync.visibility.as_str(),
                run.sync.fidelity.as_str(),
                run.sync.sync_state.as_str(),
                run.sync.sync_version as i64,
                optional_timestamp_ms(run.sync.deleted_at),
                serde_json::to_string(&run.sync.metadata)?,
            ])?;
        Ok(changed > 0)
    }

    pub fn get_run(&self, id: Uuid) -> Result<Run> {
        self.conn
            .query_row(
                run_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                run_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn runs_for_session(&self, session_id: Uuid) -> Result<Vec<Run>> {
        let mut stmt = self
            .conn
            .prepare(run_select_sql("WHERE session_id = ?1 ORDER BY started_at_ms, id").as_str())?;
        let rows = stmt.query_map(params![session_id.to_string()], run_from_row)?;
        collect_rows(rows)
    }

    pub fn runs_for_record(&self, record_id: Uuid) -> Result<Vec<Run>> {
        let mut stmt = self.conn.prepare(
            run_select_sql(
                r#"
                WHERE history_record_id = ?1
                   OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                ORDER BY started_at_ms, id
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], run_from_row)?;
        collect_rows(rows)
    }

    pub(crate) fn list_runs(&self) -> Result<Vec<Run>> {
        let mut stmt = self
            .conn
            .prepare(run_select_sql("ORDER BY started_at_ms, id").as_str())?;
        let rows = stmt.query_map([], run_from_row)?;
        collect_rows(rows)
    }

    pub fn provider_event_dedupe_key(
        provider: CaptureProvider,
        external_session_id: &str,
        provider_index: u64,
        payload_hash: &str,
    ) -> String {
        format!(
            "provider:{}:{}:{}:{}",
            provider.as_str(),
            external_session_id,
            provider_index,
            payload_hash
        )
    }

    pub fn provider_source_event_dedupe_key(
        source_id: Uuid,
        provider_index: u64,
        payload_hash: &str,
    ) -> String {
        format!("provider-source:{source_id}:{provider_index}:{payload_hash}")
    }

    pub fn upsert_event(&self, event: &Event) -> Result<Uuid> {
        let event_id = if let Some(dedupe_key) = &event.dedupe_key {
            reject_provider_event_hash_conflict(&self.conn, dedupe_key)?;
            if let Some(existing_id) = self
                .conn
                .query_row(
                    "SELECT id FROM events WHERE dedupe_key = ?1",
                    params![dedupe_key],
                    |row| parse_uuid(row.get::<_, String>(0)?),
                )
                .optional()?
            {
                return Ok(existing_id);
            }
            event.id
        } else {
            event.id
        };

        self.conn.execute(
            r#"
            INSERT INTO events
            (id, seq, history_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, redaction_state, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
            ON CONFLICT(id) DO UPDATE SET
                seq = excluded.seq,
                history_record_id = excluded.history_record_id,
                session_id = excluded.session_id,
                run_id = excluded.run_id,
                event_type = excluded.event_type,
                role = excluded.role,
                occurred_at_ms = excluded.occurred_at_ms,
                capture_source_id = excluded.capture_source_id,
                payload_json = excluded.payload_json,
                payload_blob_id = excluded.payload_blob_id,
                dedupe_key = excluded.dedupe_key,
                visibility = excluded.visibility,
                redaction_state = excluded.redaction_state,
                fidelity = excluded.fidelity,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                deleted_at_ms = excluded.deleted_at_ms,
                metadata_json = excluded.metadata_json
            "#,
            params![
                event_id.to_string(),
                event.seq as i64,
                optional_uuid_string(event.history_record_id),
                optional_uuid_string(event.session_id),
                optional_uuid_string(event.run_id),
                event.event_type.as_str(),
                event.role.map(|role| role.as_str()),
                timestamp_ms(event.occurred_at),
                optional_uuid_string(event.capture_source_id),
                serde_json::to_string(&event.payload)?,
                optional_uuid_string(event.payload_blob_id),
                event.dedupe_key.as_deref(),
                event.sync.visibility.as_str(),
                event.redaction_state.as_str(),
                event.sync.fidelity.as_str(),
                event.sync.sync_state.as_str(),
                event.sync.sync_version as i64,
                optional_timestamp_ms(event.sync.deleted_at),
                serde_json::to_string(&event.sync.metadata)?,
            ],
        )?;
        upsert_event_search_projection_for_event(&self.conn, event_id, event)?;
        if let Some(dedupe_key) = &event.dedupe_key {
            return self.event_id_by_dedupe_key(dedupe_key);
        }
        Ok(event_id)
    }

    pub fn insert_event_if_absent(&self, event: &Event) -> Result<bool> {
        let changed = self
            .conn
            .prepare_cached(
                r#"
                INSERT OR IGNORE INTO events
                (id, seq, history_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, redaction_state, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                "#,
            )?
            .execute(params![
                event.id.to_string(),
                event.seq as i64,
                optional_uuid_string(event.history_record_id),
                optional_uuid_string(event.session_id),
                optional_uuid_string(event.run_id),
                event.event_type.as_str(),
                event.role.map(|role| role.as_str()),
                timestamp_ms(event.occurred_at),
                optional_uuid_string(event.capture_source_id),
                serde_json::to_string(&event.payload)?,
                optional_uuid_string(event.payload_blob_id),
                event.dedupe_key.as_deref(),
                event.sync.visibility.as_str(),
                event.redaction_state.as_str(),
                event.sync.fidelity.as_str(),
                event.sync.sync_state.as_str(),
                event.sync.sync_version as i64,
                optional_timestamp_ms(event.sync.deleted_at),
                serde_json::to_string(&event.sync.metadata)?,
            ])?;
        if changed == 0 {
            if let Some(dedupe_key) = &event.dedupe_key {
                reject_provider_event_hash_conflict(&self.conn, dedupe_key)?;
            }
        }
        if changed > 0 {
            insert_event_search_projection_for_event(&self.conn, event)?;
        }
        Ok(changed > 0)
    }

    pub fn event_id_by_dedupe_key(&self, dedupe_key: &str) -> Result<Uuid> {
        self.conn
            .query_row(
                "SELECT id FROM events WHERE dedupe_key = ?1",
                params![dedupe_key],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn event_id_by_seq(&self, seq: u64) -> Result<Uuid> {
        self.conn
            .query_row(
                "SELECT id FROM events WHERE seq = ?1",
                params![seq as i64],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn get_event(&self, id: Uuid) -> Result<Event> {
        self.conn
            .query_row(
                event_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                event_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn events_by_id_prefix(&self, prefix: &str) -> Result<Vec<Event>> {
        let mut stmt = self
            .conn
            .prepare(event_select_sql("WHERE id LIKE ?1 ORDER BY id LIMIT 2").as_str())?;
        let rows = stmt.query_map(params![format!("{prefix}%")], event_from_row)?;
        collect_rows(rows)
    }
}
