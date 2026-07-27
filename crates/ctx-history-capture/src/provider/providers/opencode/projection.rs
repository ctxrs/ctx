use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, EventType, Fidelity, ProviderCaptureEnvelope, ProviderCursorCheckpoint,
    ProviderCursorRange, ProviderSessionEnvelope, ProviderSourceEnvelope, ProviderSourceTrust,
    SessionStatus, PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use rusqlite::{params, Connection, OptionalExtension, Statement};
use serde_json::{json, Value};

use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, CapturedSqliteValue, NativePosition,
    SourceObservation,
};
use crate::provider::file_touches::{
    event_type_supports_structured_file_touches, ProviderFileTouchEnvelopeContext,
    ProviderFileTouchVisitor, PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
};
use crate::provider::importer::{
    provider_cursor_stream, BoundedParserCheckpoint, CapturedBatchCursorFinish,
    CapturedBatchProjector, CertifiedProviderCursor, ExistingSessionEventOutcome,
    ProviderProjectionFatal, ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::{
    provider_line_from_index, provider_required_timestamp_millis,
};
use crate::provider::sqlite::{
    sqlite_ident, with_sqlite_read_snapshot, ProviderSqliteSourceSnapshot,
};
use crate::{CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result};

use super::capture::{
    initial_opencode_position, opencode_observed_bytes, opencode_parent_ordinal,
    opencode_record_limit, validate_opencode_resume_position, with_opencode_length_preflight,
    OPENCODE_END_RECORD_KIND, OPENCODE_MESSAGE_PART_RECORD_KIND, OPENCODE_RECORD_KIND,
    OPENCODE_SESSION_PARENT_RECORD_KIND,
};
use super::normalization::{
    opencode_entry_type_from_data, opencode_event, opencode_event_cursor, opencode_event_time,
    opencode_message_part_identity_index, opencode_message_part_role, opencode_part_type,
    opencode_patch_file_touch_drafts, opencode_text_part_text, opencode_tool_part_event_data,
};
use super::schema::{
    opencode_session_hydration_sql, opencode_session_id_lookup_index,
    opencode_session_retained_text, parse_json_object_string, OpenCodeCapturedShape,
    OpenCodeMessageRow, OpenCodeSessionRow, OpenCodeSessionSql, OpenCodeSqliteDialect,
    OPENCODE_SESSION_PARENT_OVERHEAD_BYTES,
};

pub(super) struct OpenCodeCapturedBatchProjector<'connection, 'dialect> {
    context: ProviderAdapterContext,
    database_path: PathBuf,
    source_conn: &'connection Connection,
    source_snapshot: ProviderSqliteSourceSnapshot,
    dialect: &'dialect OpenCodeSqliteDialect,
    user_version: i64,
    schema_fingerprint: String,
    shape: OpenCodeCapturedShape,
    session_lookup_candidate: Statement<'connection>,
    session_lookup_hydration: Statement<'connection>,
    session_spool: Connection,
}

pub(super) struct OpenCodeProjectionSource<'connection> {
    pub(super) database_path: PathBuf,
    pub(super) conn: &'connection Connection,
    pub(super) snapshot: ProviderSqliteSourceSnapshot,
}

struct OpenCodeProjectedRecord {
    line_number: usize,
    capture: ProviderCaptureEnvelope,
    raw_value: Value,
    part_file_touch_source: Option<Value>,
    occurred_at: DateTime<Utc>,
    provider_event_index: u64,
    existing_session_event: bool,
}

impl<'connection, 'dialect> OpenCodeCapturedBatchProjector<'connection, 'dialect> {
    pub(super) fn new(
        context: ProviderAdapterContext,
        source: OpenCodeProjectionSource<'connection>,
        dialect: &'dialect OpenCodeSqliteDialect,
        user_version: i64,
        schema_fingerprint: String,
        shape: OpenCodeCapturedShape,
    ) -> Result<Self> {
        let session = OpenCodeSessionSql::new(source.conn)?;
        let session_index = opencode_session_id_lookup_index(source.conn)?;
        let session_retained_text = opencode_session_retained_text(&session);
        let session_lookup_candidate = source.conn.prepare(&format!(
            "select s.rowid, {OPENCODE_SESSION_PARENT_OVERHEAD_BYTES} + \
                    {session_retained_text} \
             from session s indexed by {} \
             where s.id collate binary = ?1 limit 1",
            sqlite_ident(&session_index),
        ))?;
        let session_lookup_hydration = source
            .conn
            .prepare(&opencode_session_hydration_sql(&session))?;
        // An empty SQLite filename creates a private, file-backed temporary database. Parent
        // records populate it one paced logical row at a time; there is no source-wide setup
        // scan before the first CapturedBatch. It bounds heap use, is discarded on Drop on every
        // exit path, and is only transient projection state: the source snapshot remains the
        // revalidated authority around every batch read.
        let session_spool = Connection::open("")?;
        session_spool.execute_batch(
            "create table session_parent_spool (
                id text primary key,
                parent_id text,
                title text not null,
                directory text not null,
                model text,
                agent text,
                time_created integer not null,
                time_updated integer not null,
                tokens_input integer not null,
                tokens_output integer not null,
                tokens_reasoning integer not null,
                tokens_cache_read integer not null,
                tokens_cache_write integer not null,
                capture_ordinal integer not null,
                emitted integer not null
            );
            create table session_parent_miss (
                id text primary key
            );",
        )?;
        Ok(Self {
            context,
            database_path: source.database_path,
            source_conn: source.conn,
            source_snapshot: source.snapshot,
            dialect,
            user_version,
            schema_fingerprint,
            shape,
            session_lookup_candidate,
            session_lookup_hydration,
            session_spool,
        })
    }

    fn spool_session_parent(&mut self, session: &OpenCodeSessionRow, ordinal: u64) -> Result<()> {
        let encoded_ordinal = i64::from_be_bytes(ordinal.to_be_bytes());
        self.session_spool.execute(
            "insert or replace into session_parent_spool values (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 0
            )",
            params![
                session.id,
                session.parent_id,
                session.title,
                session.directory,
                session.model,
                session.agent,
                session.time_created,
                session.time_updated,
                session.tokens_input,
                session.tokens_output,
                session.tokens_reasoning,
                session.tokens_cache_read,
                session.tokens_cache_write,
                encoded_ordinal,
            ],
        )?;
        Ok(())
    }

    fn spooled_session_parent(
        &self,
        provider_session_id: &str,
    ) -> Result<Option<(OpenCodeSessionRow, u64, bool)>> {
        self.session_spool
            .query_row(
                "select id, parent_id, title, directory, model, agent,
                        time_created, time_updated, tokens_input, tokens_output,
                        tokens_reasoning, tokens_cache_read, tokens_cache_write,
                        capture_ordinal, emitted
                 from session_parent_spool where id = ?1",
                [provider_session_id],
                |row| {
                    let encoded_ordinal: i64 = row.get(13)?;
                    Ok((
                        OpenCodeSessionRow {
                            id: row.get(0)?,
                            parent_id: row.get(1)?,
                            title: row.get(2)?,
                            directory: row.get(3)?,
                            model: row.get(4)?,
                            agent: row.get(5)?,
                            time_created: row.get(6)?,
                            time_updated: row.get(7)?,
                            tokens_input: row.get(8)?,
                            tokens_output: row.get(9)?,
                            tokens_reasoning: row.get(10)?,
                            tokens_cache_read: row.get(11)?,
                            tokens_cache_write: row.get(12)?,
                        },
                        u64::from_be_bytes(encoded_ordinal.to_be_bytes()),
                        row.get::<_, i64>(14)? != 0,
                    ))
                },
            )
            .optional()
            .map_err(CaptureError::from)
    }

    fn session_parent_miss_cached(&self, provider_session_id: &str) -> Result<bool> {
        self.session_spool
            .query_row(
                "select exists(select 1 from session_parent_miss where id = ?1)",
                [provider_session_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|found| found != 0)
            .map_err(CaptureError::from)
    }

    fn cache_session_parent_miss(&self, provider_session_id: &str) -> Result<()> {
        self.session_spool.execute(
            "insert or ignore into session_parent_miss(id) values (?1)",
            [provider_session_id],
        )?;
        Ok(())
    }

    fn lookup_session_parent(
        &mut self,
        provider_session_id: &str,
    ) -> Result<Option<(OpenCodeSessionRow, u64)>> {
        if !self.source_snapshot.revalidate(&self.database_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let source_conn = self.source_conn;
        let candidate = &mut self.session_lookup_candidate;
        let hydration = &mut self.session_lookup_hydration;
        let lookup = with_sqlite_read_snapshot(source_conn, || {
            let candidate = with_opencode_length_preflight(source_conn, || {
                candidate
                    .query_row([provider_session_id], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                    })
                    .optional()
            })?;
            let Some((rowid, retained_bytes)) = candidate else {
                return Ok(None);
            };
            if opencode_observed_bytes(retained_bytes)? > opencode_record_limit()? {
                return Ok(None);
            }
            let parent_ordinal = opencode_parent_ordinal(rowid);
            let session = hydration.query_row([rowid], |row| {
                Ok(OpenCodeSessionRow {
                    id: row.get(0)?,
                    parent_id: row.get(1)?,
                    title: row.get(2)?,
                    directory: row.get(3)?,
                    model: row.get(4)?,
                    agent: row.get(5)?,
                    time_created: row.get(6)?,
                    time_updated: row.get(7)?,
                    tokens_input: row.get(8)?,
                    tokens_output: row.get(9)?,
                    tokens_reasoning: row.get(10)?,
                    tokens_cache_read: row.get(11)?,
                    tokens_cache_write: row.get(12)?,
                })
            })?;
            Ok((session.id == provider_session_id).then_some((session, parent_ordinal)))
        });
        if !self.source_snapshot.revalidate(&self.database_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        lookup
    }

    fn cached_or_lookup_session_parent(
        &mut self,
        provider_session_id: &str,
    ) -> Result<Option<(OpenCodeSessionRow, u64, bool)>> {
        if let Some(parent) = self.spooled_session_parent(provider_session_id)? {
            return Ok(Some(parent));
        }
        if self.session_parent_miss_cached(provider_session_id)? {
            return Ok(None);
        }
        let Some((parent, parent_ordinal)) = self.lookup_session_parent(provider_session_id)?
        else {
            self.cache_session_parent_miss(provider_session_id)?;
            return Ok(None);
        };
        self.spool_session_parent(&parent, parent_ordinal)?;
        self.spooled_session_parent(provider_session_id)
    }

    fn mark_session_parent_emitted(&self, provider_session_id: &str) -> Result<()> {
        let changed = self.session_spool.execute(
            "update session_parent_spool set emitted = 1 where id = ?1 and emitted = 0",
            [provider_session_id],
        )?;
        if changed > 1 {
            return Err(CaptureError::SystemInvariant(
                "OpenCode parent spool updated multiple rows",
            ));
        }
        Ok(())
    }

    fn decode_message_part_parent(
        &self,
        values: &[CapturedSqliteValue],
    ) -> Result<(OpenCodeSessionRow, u64)> {
        if values.len() != 14 {
            return Err(CaptureError::SystemInvariant(
                "OpenCode session parent has an invalid value count",
            ));
        }
        let parent_ordinal = opencode_parent_ordinal(opencode_integer_value(values, 0)?);
        Ok((
            OpenCodeSessionRow {
                id: opencode_text_value(values, 1)?.to_owned(),
                parent_id: opencode_optional_text_value(values, 2)?,
                title: opencode_text_value(values, 3)?.to_owned(),
                directory: opencode_text_value(values, 4)?.to_owned(),
                model: opencode_optional_text_value(values, 5)?,
                agent: opencode_optional_text_value(values, 6)?,
                time_created: opencode_integer_value(values, 7)?,
                time_updated: opencode_integer_value(values, 8)?,
                tokens_input: opencode_integer_value(values, 9)?,
                tokens_output: opencode_integer_value(values, 10)?,
                tokens_reasoning: opencode_integer_value(values, 11)?,
                tokens_cache_read: opencode_integer_value(values, 12)?,
                tokens_cache_write: opencode_integer_value(values, 13)?,
            },
            parent_ordinal,
        ))
    }

    fn decode_child(&self, values: &[CapturedSqliteValue]) -> Result<(OpenCodeCapturedRow, u64)> {
        if values.len() != 14 {
            return Err(CaptureError::SystemInvariant(
                "OpenCode child row has an invalid value count",
            ));
        }
        let provider_event_index = u64::try_from(opencode_integer_value(values, 0)?)
            .map_err(|_| CaptureError::SystemInvariant("OpenCode child ordinal is negative"))?;
        if opencode_integer_value(values, 1)? == 0 {
            return Err(CaptureError::InvalidPayload(
                "OpenCode child references a missing, oversized, or mismatched parent session"
                    .to_owned(),
            ));
        }
        let source_session_id = opencode_text_value(values, 3)?.to_owned();
        if source_session_id.trim().is_empty() {
            return Err(CaptureError::InvalidPayload(
                "OpenCode child has an empty resolved session id".to_owned(),
            ));
        }
        let time_created = opencode_integer_value(values, 7)?;
        Ok((
            OpenCodeCapturedRow {
                session_found: true,
                session: OpenCodeSessionRow {
                    id: source_session_id.clone(),
                    parent_id: None,
                    title: source_session_id.clone(),
                    directory: String::new(),
                    model: None,
                    agent: None,
                    time_created,
                    time_updated: opencode_integer_value(values, 8)?,
                    tokens_input: 0,
                    tokens_output: 0,
                    tokens_reasoning: 0,
                    tokens_cache_read: 0,
                    tokens_cache_write: 0,
                },
                message_id: opencode_text_value(values, 2)?.to_owned(),
                source_session_id,
                entry_type: opencode_text_value(values, 4)?.to_owned(),
                seq: (opencode_integer_value(values, 5)? != 0)
                    .then(|| opencode_integer_value(values, 6))
                    .transpose()?,
                time_created,
                time_updated: opencode_integer_value(values, 8)?,
                message_data: opencode_text_value(values, 9)?.to_owned(),
                part_data: opencode_text_value(values, 10)?.to_owned(),
                part_id: opencode_text_value(values, 11)?.to_owned(),
                part_type: opencode_text_value(values, 12)?.to_owned(),
                source_table: opencode_text_value(values, 13)?.to_owned(),
            },
            provider_event_index,
        ))
    }

    fn project_message_part_session(
        &self,
        session: OpenCodeSessionRow,
        provider_event_index: u64,
    ) -> Result<OpenCodeProjectedRecord> {
        let source_session_id = session.id.clone();
        let time_created = session.time_created;
        let time_updated = session.time_updated;
        let mut projected = self.project_normalization(
            OpenCodeCapturedRow {
                session_found: true,
                session,
                message_id: format!("session-parent:{source_session_id}"),
                source_session_id,
                entry_type: "message".to_owned(),
                seq: None,
                time_created,
                time_updated,
                message_data: "{}".to_owned(),
                part_data: String::new(),
                part_id: String::new(),
                part_type: String::new(),
                source_table: OpenCodeCapturedShape::Message.label().to_owned(),
            },
            provider_event_index,
        )?;
        projected.capture.event = None;
        projected.capture.source.cursor = None;
        projected.raw_value = Value::Null;
        projected.existing_session_event = false;
        Ok(projected)
    }

    fn project_normalization(
        &self,
        captured: OpenCodeCapturedRow,
        provider_event_index: u64,
    ) -> Result<OpenCodeProjectedRecord> {
        if !captured.session_found || captured.session.id != captured.source_session_id {
            return Err(CaptureError::InvalidPayload(format!(
                "{} message {} references missing session {}",
                self.dialect.display_name, captured.message_id, captured.source_session_id
            )));
        }
        if captured.seq.is_some_and(|seq| seq < 0) {
            return Err(CaptureError::InvalidPayload(format!(
                "{} {} seq must be nonnegative",
                self.dialect.display_name, captured.source_table
            )));
        }
        let row = captured.message_row()?;
        let occurred_at = match opencode_event_time(&row.data, self.dialect)? {
            Some(time) => time,
            None => provider_required_timestamp_millis(
                captured.time_created,
                self.dialect.session_message_time_created_field,
            )?,
        };
        let started_at = provider_required_timestamp_millis(
            captured.session.time_created,
            self.dialect.session_time_created_field,
        )?;
        let raw_source_path = self.database_path.display().to_string();
        let event = row.row.event.as_ref().map(|message| {
            opencode_event(
                message,
                &row.data,
                occurred_at,
                provider_event_index,
                self.dialect,
            )
        });
        let is_subagent = captured.session.parent_id.is_some();
        let source_cursor = row.row.event.as_ref().map(|message| ProviderCursorRange {
            before: None,
            after: Some(ProviderCursorCheckpoint {
                stream: provider_cursor_stream(self.dialect.provider, self.dialect.source_format),
                cursor: opencode_event_cursor(message, &row.data),
                observed_at: occurred_at,
            }),
        });
        let capture = ProviderCaptureEnvelope {
            schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
            provider: self.dialect.provider,
            source: ProviderSourceEnvelope {
                source_format: self.dialect.source_format.to_owned(),
                machine_id: self.context.machine_id.clone(),
                observed_at: self.context.imported_at,
                raw_source_path: Some(raw_source_path.clone()),
                source_root: self
                    .context
                    .source_root_display()
                    .or_else(|| Some(raw_source_path.clone())),
                trust: ProviderSourceTrust::ProviderNative,
                fidelity: Fidelity::Imported,
                cursor: source_cursor,
                idempotency_key: Some(format!(
                    "provider-source:{}:{}:{}",
                    self.dialect.provider.as_str(),
                    self.dialect.source_format,
                    captured.session.id
                )),
                metadata: json!({
                    "adapter": self.dialect.source_format,
                    "sqlite_user_version": self.user_version,
                    "schema_fingerprint": self.schema_fingerprint,
                    "selected_message_table": self.shape.label(),
                    "capture_policy": "bounded_sessions_first_child_local_v3",
                }),
            },
            session: ProviderSessionEnvelope {
                provider_session_id: captured.session.id.clone(),
                parent_provider_session_id: captured.session.parent_id.clone(),
                root_provider_session_id: captured.session.parent_id.clone(),
                external_agent_id: captured.session.agent.clone(),
                agent_type: if is_subagent {
                    AgentType::Subagent
                } else {
                    AgentType::Primary
                },
                role_hint: captured
                    .session
                    .agent
                    .clone()
                    .or_else(|| Some(if is_subagent { "subagent" } else { "primary" }.to_owned())),
                is_primary: !is_subagent,
                status: SessionStatus::Imported,
                started_at,
                ended_at: None,
                cwd: Some(captured.session.directory.clone()),
                fidelity: Fidelity::Imported,
                idempotency_key: Some(format!(
                    "provider-session:{}:{}",
                    self.dialect.provider.as_str(),
                    captured.session.id
                )),
                artifacts: Vec::new(),
                metadata: json!({
                    "source_format": self.dialect.source_format,
                    "title": captured.session.title,
                    "model": parse_json_object_string(captured.session.model.as_deref()),
                    "agent": captured.session.agent,
                    "time_updated": captured.session.time_updated,
                    "tokens": {
                        "input": captured.session.tokens_input,
                        "output": captured.session.tokens_output,
                        "reasoning": captured.session.tokens_reasoning,
                        "cache_read": captured.session.tokens_cache_read,
                        "cache_write": captured.session.tokens_cache_write,
                    },
                    "legacy_projection": {
                        "selected_message_table": self.shape.label(),
                        "import_policy": "the first structurally supported table is authoritative and each row is retained or rejected independently",
                        "message_part_role_policy": "use an explicit part-local user/assistant/system role, otherwise default to assistant without re-reading parent message content",
                    },
                }),
            },
            event,
        };
        Ok(OpenCodeProjectedRecord {
            line_number: provider_line_from_index(provider_event_index),
            capture,
            raw_value: row.data,
            part_file_touch_source: row.part_file_touch_source,
            occurred_at,
            provider_event_index,
            existing_session_event: true,
        })
    }
}

impl CapturedBatchProjector for OpenCodeCapturedBatchProjector<'_, '_> {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "OpenCode projector requires SQLite logical values",
            ));
        };
        if record.record_kind().as_str() == OPENCODE_END_RECORD_KIND {
            return Ok(());
        }
        if record.record_kind().as_str() == OPENCODE_SESSION_PARENT_RECORD_KIND {
            return match self.decode_message_part_parent(values) {
                Ok((session, parent_ordinal)) => self
                    .spool_session_parent(&session, parent_ordinal)
                    .map_err(ProviderProjectionFatal::new),
                Err(error) => {
                    output.reject_record(
                        provider_line_from_index(record.ordinal().saturating_add(1)),
                        error.to_string(),
                    );
                    Ok(())
                }
            };
        }
        let (projected, rejection_line) = match record.record_kind().as_str() {
            OPENCODE_RECORD_KIND => match self.decode_child(values) {
                Ok((row, provider_event_index)) => {
                    let mut projected = self.project_normalization(row, provider_event_index);
                    if let Ok(projected) = projected.as_mut() {
                        projected.line_number =
                            provider_line_from_index(record.ordinal().saturating_add(1));
                    }
                    (
                        projected,
                        provider_line_from_index(record.ordinal().saturating_add(1)),
                    )
                }
                Err(error) => (
                    Err(error),
                    provider_line_from_index(record.ordinal().saturating_add(1)),
                ),
            },
            OPENCODE_MESSAGE_PART_RECORD_KIND => match self.decode_child(values) {
                Ok((row, provider_event_index)) => {
                    let mut projected = self.project_normalization(row, provider_event_index);
                    if let Ok(projected) = projected.as_mut() {
                        projected.line_number =
                            provider_line_from_index(record.ordinal().saturating_add(1));
                    }
                    (
                        projected,
                        provider_line_from_index(record.ordinal().saturating_add(1)),
                    )
                }
                Err(error) => (
                    Err(error),
                    provider_line_from_index(record.ordinal().saturating_add(1)),
                ),
            },
            _ => {
                return Err(ProviderProjectionFatal::system_invariant(
                    "OpenCode projector received an unexpected record kind",
                ));
            }
        };
        match projected {
            Ok(projected) => {
                let OpenCodeProjectedRecord {
                    line_number,
                    capture,
                    raw_value,
                    part_file_touch_source,
                    occurred_at,
                    provider_event_index,
                    existing_session_event,
                } = projected;
                let provider_session_id = capture.session.provider_session_id.clone();
                let event = capture.event.clone();
                output.use_explicit_file_touches();
                if existing_session_event {
                    let Some((parent, parent_ordinal, parent_emitted)) = self
                        .cached_or_lookup_session_parent(&provider_session_id)
                        .map_err(ProviderProjectionFatal::new)?
                    else {
                        output.reject_record(
                            line_number,
                            format!(
                                "{} child references missing captured parent session {}",
                                self.dialect.display_name, provider_session_id
                            ),
                        );
                        return Ok(());
                    };
                    if !parent_emitted {
                        let parent = match self.project_message_part_session(parent, parent_ordinal)
                        {
                            Ok(parent) => parent,
                            Err(error) => {
                                output.reject_record(line_number, error.to_string());
                                return Ok(());
                            }
                        };
                        output.emit_normalization(ProviderNormalizationResult {
                            captures: vec![(parent.line_number, parent.capture)],
                            ..ProviderNormalizationResult::default()
                        })?;
                        self.mark_session_parent_emitted(&provider_session_id)
                            .map_err(ProviderProjectionFatal::new)?;
                    }
                    if event.is_some()
                        && output.emit_existing_session_event(line_number, capture)?
                            == ExistingSessionEventOutcome::Rejected
                    {
                        return Ok(());
                    }
                } else {
                    output.emit_normalization(ProviderNormalizationResult {
                        captures: vec![(line_number, capture)],
                        ..ProviderNormalizationResult::default()
                    })?;
                }
                let raw_source_path = self.database_path.display().to_string();
                let mut visitor = ProviderFileTouchVisitor::new(
                    ProviderFileTouchEnvelopeContext {
                        provider: self.dialect.provider,
                        provider_session_id: &provider_session_id,
                        source_format: self.dialect.source_format,
                        raw_source_path: Some(raw_source_path.as_str()),
                        source_root: Some(raw_source_path.as_str()),
                        occurred_at,
                        provider_event_index: Some(provider_event_index),
                        provider_touch_base_index: provider_event_index << 16,
                        line_number,
                    },
                    |file_touch| {
                        output.emit_normalization(ProviderNormalizationResult {
                            files_touched: vec![file_touch],
                            ..ProviderNormalizationResult::default()
                        })
                    },
                );
                // Retain the legacy raw-before-explicit order while sharing one deduplication and
                // identity sequence so the two phases cannot emit aliased touch indices.
                if let Some(event) = event {
                    if matches!(
                        event.event_type,
                        EventType::ToolCall
                            | EventType::ToolOutput
                            | EventType::CommandOutput
                            | EventType::FileTouched
                    ) {
                        visitor.visit_raw_value(
                            &raw_value,
                            event_type_supports_structured_file_touches(event.event_type),
                        )?;
                    }
                }
                if let Some(part_data) = part_file_touch_source.as_ref() {
                    visitor.visit_drafts(opencode_patch_file_touch_drafts(
                        part_data,
                        raw_value
                            .get("part_id")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        raw_value
                            .get("part_type")
                            .and_then(Value::as_str)
                            .unwrap_or("patch"),
                    ))?;
                }
                let outcome = visitor.finish();
                if outcome.limit_exceeded() {
                    output
                        .reject_record(line_number, PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned());
                }
                Ok(())
            }
            Err(error) => {
                output.reject_record(rejection_line, error.to_string());
                Ok(())
            }
        }
    }

    fn project_structural_rejection(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        crate::provider::importer::project_default_structural_rejection(record, output)
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if *position != initial_opencode_position(self.shape)? {
            return Err(CaptureError::InvalidPayload(
                "OpenCode initial cursor candidate is not at the SQLite source start".to_owned(),
            ));
        }
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&())?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        validate_opencode_resume_position(batch.range_end(), self.shape)?;
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&())?,
            )?,
        ))
    }
}

struct OpenCodeCapturedRow {
    session_found: bool,
    session: OpenCodeSessionRow,
    message_id: String,
    source_session_id: String,
    entry_type: String,
    seq: Option<i64>,
    time_created: i64,
    time_updated: i64,
    message_data: String,
    part_data: String,
    part_id: String,
    part_type: String,
    source_table: String,
}

struct OpenCodeProjectedMessage {
    row: OpenCodeOptionalMessage,
    data: Value,
    part_file_touch_source: Option<Value>,
}

struct OpenCodeOptionalMessage {
    event: Option<OpenCodeMessageRow>,
}

impl OpenCodeCapturedRow {
    fn message_row(&self) -> Result<OpenCodeProjectedMessage> {
        if self.source_table == OpenCodeCapturedShape::MessagePart.label() {
            return self.message_part_row();
        }
        let data = serde_json::from_str::<Value>(&self.message_data).map_err(|error| {
            CaptureError::InvalidPayload(format!(
                "invalid JSON in {} message {}: {error}",
                self.source_table, self.message_id
            ))
        })?;
        let entry_type = opencode_entry_type_from_data(&self.entry_type, &self.message_data);
        let seq = self.seq.unwrap_or_else(|| {
            opencode_message_part_identity_index(&self.source_session_id, &self.message_id)
        });
        Ok(OpenCodeProjectedMessage {
            row: OpenCodeOptionalMessage {
                event: Some(OpenCodeMessageRow {
                    id: self.message_id.clone(),
                    session_id: self.source_session_id.clone(),
                    entry_type,
                    seq,
                    time_created: self.time_created,
                    time_updated: self.time_updated,
                }),
            },
            data,
            part_file_touch_source: None,
        })
    }

    fn message_part_row(&self) -> Result<OpenCodeProjectedMessage> {
        let part_data = serde_json::from_str::<Value>(&self.part_data).map_err(|error| {
            CaptureError::InvalidPayload(format!(
                "invalid JSON in message part {}: {error}",
                self.part_id
            ))
        })?;
        let role = opencode_message_part_role(&part_data);
        let part_type = opencode_part_type(Some(&self.part_type), &part_data);
        let part_seq = opencode_message_part_identity_index(&self.message_id, &self.part_id);
        let is_patch = part_type == "patch";
        let (event_data, emits_event) =
            if let Some(text) = opencode_text_part_text(&part_type, &part_data) {
                let emits_event = matches!(role.as_str(), "assistant" | "user" | "system")
                    && !text.trim().is_empty();
                (
                    Some(json!({
                    "role": role.clone(),
                    "time": { "created": self.time_created },
                    "text": text,
                    "source_table": "message+part",
                    "message_id": self.message_id.clone(),
                    "part_id": self.part_id.clone(),
                    "part_type": part_type.clone(),
                    })),
                    emits_event,
                )
            } else if let Some(tool) = opencode_tool_part_event_data(
                &self.message_id,
                &self.part_id,
                &part_type,
                self.time_created,
                &part_data,
            ) {
                (Some(tool), true)
            } else if is_patch {
                (
                    Some(json!({
                        "role": role.clone(),
                        "time": { "created": self.time_created },
                        "source_table": "message+part",
                        "message_id": self.message_id.clone(),
                        "part_id": self.part_id.clone(),
                        "part_type": part_type.clone(),
                    })),
                    false,
                )
            } else {
                (None, false)
            };
        let event = emits_event.then(|| OpenCodeMessageRow {
            id: format!("{}:{}", self.message_id, self.part_id),
            session_id: self.source_session_id.clone(),
            entry_type: if matches!(part_type.as_str(), "tool" | "tool_result" | "result") {
                "tool".to_owned()
            } else {
                role
            },
            seq: part_seq,
            time_created: self.time_created,
            time_updated: self.time_updated,
        });
        Ok(OpenCodeProjectedMessage {
            row: OpenCodeOptionalMessage { event },
            data: event_data.unwrap_or(Value::Null),
            part_file_touch_source: is_patch.then_some(part_data),
        })
    }
}

pub(super) fn opencode_text_value(values: &[CapturedSqliteValue], index: usize) -> Result<&str> {
    match values.get(index) {
        Some(CapturedSqliteValue::Text(value)) => Ok(value),
        _ => Err(CaptureError::SystemInvariant(
            "OpenCode logical row has an invalid text value",
        )),
    }
}

pub(super) fn opencode_optional_text_value(
    values: &[CapturedSqliteValue],
    index: usize,
) -> Result<Option<String>> {
    Ok(match opencode_text_value(values, index)? {
        "" => None,
        value => Some(value.to_owned()),
    })
}

pub(super) fn opencode_integer_value(values: &[CapturedSqliteValue], index: usize) -> Result<i64> {
    match values.get(index) {
        Some(CapturedSqliteValue::Integer(value)) => Ok(*value),
        _ => Err(CaptureError::SystemInvariant(
            "OpenCode logical row has an invalid integer value",
        )),
    }
}
