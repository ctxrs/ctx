#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileTouchScope {
    pub history_record_ids: BTreeSet<Uuid>,
    pub session_ids: BTreeSet<Uuid>,
    pub run_ids: BTreeSet<Uuid>,
    pub event_ids: BTreeSet<Uuid>,
    pub source_ids: BTreeSet<Uuid>,
}

impl FileTouchScope {
    pub fn is_empty(&self) -> bool {
        self.history_record_ids.is_empty()
            && self.session_ids.is_empty()
            && self.run_ids.is_empty()
            && self.event_ids.is_empty()
            && self.source_ids.is_empty()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedProviderEventDedupeKey {
    pub(crate) provider: String,
    pub(crate) external_session_id: String,
    pub(crate) source_id: Option<String>,
    pub(crate) provider_index: u64,
    pub(crate) payload_hash: String,
}

impl ParsedProviderEventDedupeKey {
    pub(crate) fn has_same_event_identity(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.external_session_id == other.external_session_id
            && self.source_id == other.source_id
            && self.provider_index == other.provider_index
    }
}

pub(crate) fn existing_capture_source_by_id(
    tx: &Transaction<'_>,
    id: Uuid,
) -> Result<Option<CaptureSource>> {
    tx.query_row(
        "SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json FROM capture_sources WHERE id = ?1",
        params![id.to_string()],
        capture_source_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

pub(crate) fn upsert_imported_capture_source_tx(
    tx: &Transaction<'_>,
    source: &CaptureSource,
) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO capture_sources
        (id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json)
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

pub(crate) fn upsert_session_tx(tx: &Transaction<'_>, session: &Session) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO sessions
        (id, history_record_id, parent_session_id, root_session_id, capture_source_id, provider, external_session_id, external_agent_id, agent_type, role_hint, is_primary, status, fidelity, transcript_blob_id, started_at_ms, ended_at_ms, created_at_ms, updated_at_ms, visibility, sync_state, sync_version, deleted_at_ms, metadata_json)
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

pub(crate) fn upsert_run_tx(tx: &Transaction<'_>, run: &Run) -> Result<()> {
    tx.execute(
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

pub(crate) fn upsert_summary_tx(tx: &Transaction<'_>, summary: &Summary) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO summaries
        (id, history_record_id, session_id, kind, model_or_source, text, citations_json, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
        ON CONFLICT(id) DO UPDATE SET
            history_record_id = excluded.history_record_id,
            session_id = excluded.session_id,
            kind = excluded.kind,
            model_or_source = excluded.model_or_source,
            text = excluded.text,
            citations_json = excluded.citations_json,
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
            summary.id.to_string(),
            optional_uuid_string(summary.history_record_id),
            optional_uuid_string(summary.session_id),
            summary.kind.as_str(),
            summary.model_or_source.as_deref(),
            summary.text.as_str(),
            serde_json::to_string(&summary.citations)?,
            timestamp_ms(summary.timestamps.created_at),
            timestamp_ms(summary.timestamps.updated_at),
            optional_uuid_string(summary.source_id),
            summary.sync.visibility.as_str(),
            summary.sync.fidelity.as_str(),
            summary.sync.sync_state.as_str(),
            summary.sync.sync_version as i64,
            optional_timestamp_ms(summary.sync.deleted_at),
            serde_json::to_string(&summary.sync.metadata)?,
        ],
    )?;
    Ok(())
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

pub(crate) fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_optional_uuid(row.get(1)?)?,
        session_id: parse_optional_uuid(row.get(2)?)?,
        run_type: parse_text_enum::<RunType>(row.get::<_, String>(3)?)?,
        status: parse_text_enum::<RunStatus>(row.get::<_, String>(4)?)?,
        started_at: ms_to_time(row.get(5)?)?,
        ended_at: optional_ms_to_time(row.get(6)?)?,
        exit_code: row.get(7)?,
        cwd: row.get(8)?,
        command_preview: row.get(9)?,
        input_blob_id: parse_optional_uuid(row.get(10)?)?,
        output_blob_id: parse_optional_uuid(row.get(11)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(12)?)?,
            updated_at: ms_to_time(row.get(13)?)?,
        },
        source_id: parse_optional_uuid(row.get(14)?)?,
        sync: sync_metadata_from_row(row, 15, 16, 17, 18, 19, 20)?,
    })
}

pub(crate) fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        seq: nonnegative_i64_to_u64(row.get(1)?)?,
        history_record_id: parse_optional_uuid(row.get(2)?)?,
        session_id: parse_optional_uuid(row.get(3)?)?,
        run_id: parse_optional_uuid(row.get(4)?)?,
        event_type: parse_text_enum::<EventType>(row.get::<_, String>(5)?)?,
        role: row
            .get::<_, Option<String>>(6)?
            .map(parse_text_enum::<EventRole>)
            .transpose()?,
        occurred_at: ms_to_time(row.get(7)?)?,
        capture_source_id: parse_optional_uuid(row.get(8)?)?,
        payload: parse_json(row.get::<_, String>(9)?)?,
        payload_blob_id: parse_optional_uuid(row.get(10)?)?,
        dedupe_key: row.get(11)?,
        redaction_state: parse_text_enum::<RedactionState>(row.get::<_, String>(13)?)?,
        sync: sync_metadata_from_row(row, 12, 14, 15, 16, 17, 18)?,
    })
}

pub(crate) fn summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Summary> {
    Ok(Summary {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_optional_uuid(row.get(1)?)?,
        session_id: parse_optional_uuid(row.get(2)?)?,
        kind: parse_text_enum::<ctx_history_core::SummaryKind>(row.get::<_, String>(3)?)?,
        model_or_source: row.get(4)?,
        text: row.get(5)?,
        citations: serde_json::from_str(&row.get::<_, String>(6)?)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(7)?)?,
            updated_at: ms_to_time(row.get(8)?)?,
        },
        source_id: parse_optional_uuid(row.get(9)?)?,
        sync: sync_metadata_from_row(row, 10, 11, 12, 13, 14, 15)?,
    })
}
