use ctx_history_core::{
    AgentType, CaptureProvider, EntityTimestamps, Session, SessionEdge, SessionStatus,
};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, optional_ms_to_time, optional_timestamp_ms, optional_uuid_string,
    parse_optional_uuid, parse_text_enum, parse_uuid, timestamp_ms,
};
use crate::sync::sync_metadata_from_row;
use crate::{Result, Store, StoreError};

impl Store {
    pub fn upsert_session(&self, session: &Session) -> Result<()> {
        self.with_provider_file_publication_write(|| self.upsert_session_inner(session))
    }

    fn upsert_session_inner(&self, session: &Session) -> Result<()> {
        self.ensure_provider_file_session_write_allowed(session)?;
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
        self.track_provider_file_publication_session(session.id)?;
        Ok(())
    }

    pub fn get_session(&self, id: Uuid) -> Result<Session> {
        let tail = format!(
            "WHERE id = COALESCE(
                (SELECT session_id FROM session_aliases WHERE alias_id = ?1),
                ?1
            ) AND {}",
            crate::provider_files::session_material_visible_predicate("sessions")
        );
        self.conn
            .query_row(
                session_select_sql(&tail).as_str(),
                params![id.to_string()],
                session_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn sessions_by_id_prefix(&self, prefix: &str) -> Result<Vec<Session>> {
        let tail = format!(
            "WHERE id IN (
                SELECT id FROM sessions WHERE id LIKE ?1
                UNION
                SELECT session_id FROM session_aliases WHERE alias_id LIKE ?1
            ) AND {} ORDER BY id LIMIT 2",
            crate::provider_files::session_material_visible_predicate("sessions")
        );
        let mut stmt = self.conn.prepare(session_select_sql(&tail).as_str())?;
        let rows = stmt.query_map(params![format!("{prefix}%")], session_from_row)?;
        collect_rows(rows)
    }

    pub fn session_by_external_session(
        &self,
        provider: CaptureProvider,
        external_session_id: &str,
    ) -> Result<Option<Session>> {
        let visible = crate::provider_files::session_material_visible_predicate("sessions");
        self.conn
                .query_row(
                    session_select_sql(&format!(
                        "WHERE provider = ?1 AND external_session_id = ?2 AND {visible} ORDER BY started_at_ms DESC LIMIT 1",
                    ))
                    .as_str(),
                    params![provider.as_str(), external_session_id],
                    session_from_row,
                )
                .optional()
                .map_err(StoreError::from)
    }

    pub fn session_by_capture_source_and_external_session(
        &self,
        source_id: Uuid,
        provider: CaptureProvider,
        external_session_id: &str,
    ) -> Result<Option<Session>> {
        let visible = crate::provider_files::session_material_visible_predicate("sessions");
        self.conn
            .query_row(
                session_select_sql(&format!(
                    "WHERE capture_source_id = ?1 AND provider = ?2 AND external_session_id = ?3 AND {visible} ORDER BY created_at_ms, id LIMIT 1",
                ))
                .as_str(),
                params![
                    source_id.to_string(),
                    provider.as_str(),
                    external_session_id
                ],
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
        let tail = format!(
            "WHERE provider = ?1 AND external_session_id = ?2 AND {} ORDER BY started_at_ms DESC LIMIT ?3",
            crate::provider_files::session_material_visible_predicate("sessions")
        );
        let mut stmt = self.conn.prepare(session_select_sql(&tail).as_str())?;
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
        let tail = format!(
            "WHERE history_record_id = ?1 AND {} ORDER BY started_at_ms, id",
            crate::provider_files::session_material_visible_predicate("sessions")
        );
        let mut stmt = self.conn.prepare(session_select_sql(&tail).as_str())?;
        let rows = stmt.query_map(params![record_id.to_string()], session_from_row)?;
        collect_rows(rows)
    }

    pub fn assign_session_to_record(&self, session_id: Uuid, record_id: Uuid) -> Result<()> {
        self.with_provider_file_publication_write(|| {
            self.assign_session_to_record_inner(session_id, record_id)
        })
    }

    fn assign_session_to_record_inner(&self, session_id: Uuid, record_id: Uuid) -> Result<()> {
        self.ensure_provider_file_session_assignment_write_allowed(session_id, record_id)?;
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
        let tail = format!(
            "WHERE {} ORDER BY started_at_ms, id",
            crate::provider_files::session_material_visible_predicate("sessions")
        );
        let mut stmt = self.conn.prepare(session_select_sql(&tail).as_str())?;
        let rows = stmt.query_map([], session_from_row)?;
        collect_rows(rows)
    }

    pub fn upsert_session_edge(&self, edge: &SessionEdge) -> Result<()> {
        self.with_provider_file_publication_write(|| self.upsert_session_edge_inner(edge))
    }

    fn upsert_session_edge_inner(&self, edge: &SessionEdge) -> Result<()> {
        self.ensure_provider_file_session_edge_write_allowed(edge)?;
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
        self.track_provider_file_publication_session_edge(edge.id)?;
        Ok(())
    }

    pub fn session_edge_exists(&self, edge_id: Uuid) -> Result<bool> {
        let visible =
            crate::provider_files::session_edge_material_visible_predicate("session_edges");
        Ok(self
            .conn
            .query_row(
                &format!("SELECT 1 FROM session_edges WHERE id = ?1 AND {visible}"),
                params![edge_id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
}

pub(crate) fn session_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, history_record_id, parent_session_id, root_session_id, capture_source_id, provider, external_session_id, external_agent_id, agent_type, role_hint, is_primary, status, fidelity, transcript_blob_id, started_at_ms, ended_at_ms, created_at_ms, updated_at_ms, visibility, sync_state, sync_version, deleted_at_ms, metadata_json FROM sessions {tail}"
    )
}

pub(crate) fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_optional_uuid(row.get(1)?)?,
        parent_session_id: parse_optional_uuid(row.get(2)?)?,
        root_session_id: parse_optional_uuid(row.get(3)?)?,
        capture_source_id: parse_optional_uuid(row.get(4)?)?,
        provider: parse_text_enum::<CaptureProvider>(row.get::<_, String>(5)?)?,
        external_session_id: row.get(6)?,
        external_agent_id: row.get(7)?,
        agent_type: parse_text_enum::<AgentType>(row.get::<_, String>(8)?)?,
        role_hint: row.get(9)?,
        is_primary: row.get::<_, i64>(10)? != 0,
        status: parse_text_enum::<SessionStatus>(row.get::<_, String>(11)?)?,
        transcript_blob_id: parse_optional_uuid(row.get(13)?)?,
        started_at: ms_to_time(row.get(14)?)?,
        ended_at: optional_ms_to_time(row.get(15)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(16)?)?,
            updated_at: ms_to_time(row.get(17)?)?,
        },
        sync: sync_metadata_from_row(row, 18, 12, 19, 20, 21, 22)?,
    })
}
