use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{AgentType, CaptureProvider, EventType, Fidelity, ProviderSourceTrust};
use ctx_history_store::Store;
use serde_json::{json, Value};

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, SourceObservation,
};
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, BoundedParserCheckpoint, CapturedBatchCursorFinish,
    CapturedBatchCursorMode, CapturedBatchProjector, CapturedSourceAdmission,
    CertifiedProviderCursor, ProviderProjectionFatal, ProviderProjectionOutput,
    ProviderProjectionResult,
};
use crate::provider::normalization::{
    native_event, native_provider_capture, provider_json_text, provider_line_from_index,
    provider_nonnegative_i64_to_u64, provider_required_timestamp_seconds, provider_role,
    provider_value_text, NativeEventDraft, NativeSessionDraft,
};
use crate::provider::sqlite::{
    open_provider_sqlite_readonly, sqlite_schema_fingerprint, with_sqlite_read_snapshot,
    ProviderSqliteSourceSnapshot,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    ProviderNormalizationResult, Result, HERMES_SQLITE_SOURCE_FORMAT,
};

mod layout;
mod sqlite;

use self::layout::{
    decode_hermes_message, decode_hermes_session, HermesMessageRow, HermesSchema, HermesSessionRow,
};
use self::sqlite::{
    decode_hermes_position, decode_hermes_storage_rejection, hermes_sqlite_batch_error,
    initial_hermes_position, HermesRowFetcher, HERMES_MALFORMED_RECORD_KIND,
    HERMES_MESSAGE_RECORD_KIND, HERMES_SESSION_RECORD_KIND,
};

const HERMES_CAPTURE_REVISION: u32 = 1;
const HERMES_POLICY_REVISION: u32 = 4;

pub(crate) fn hermes_decode_content(raw: Option<&str>) -> Value {
    let Some(raw) = raw else {
        return Value::Null;
    };
    if let Some(json) = raw.strip_prefix("\0json:") {
        return provider_json_text(json);
    }
    Value::String(raw.to_owned())
}

fn hermes_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "Hermes SQLite source must be a regular non-symlink file",
        "Hermes SQLite sidecar must be a regular non-symlink file",
    )
}

fn hermes_source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    schema_fingerprint: &str,
) -> String {
    format!(
        "hermes-sqlite-snapshot-v1:capture={HERMES_CAPTURE_REVISION};policy={HERMES_POLICY_REVISION};schema={schema_fingerprint};{}",
        snapshot.revision_component(),
    )
}

struct HermesCapturedBatchProjector {
    context: ProviderAdapterContext,
    database_path: PathBuf,
    user_version: i64,
    schema_fingerprint: String,
    schema: HermesSchema,
}

impl CapturedBatchProjector for HermesCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Hermes projector requires SQLite logical values",
            ));
        };
        let line = provider_line_from_index(record.ordinal().saturating_add(1));
        if record.record_kind().as_str() == HERMES_MALFORMED_RECORD_KIND {
            let reason = decode_hermes_storage_rejection(&self.schema, values)
                .map_err(ProviderProjectionFatal::new)?;
            output.reject_record(line, reason);
            return Ok(());
        }
        let projected = match record.record_kind().as_str() {
            HERMES_SESSION_RECORD_KIND => {
                decode_hermes_session(&self.schema, values, 0).and_then(|session| {
                    Ok(ProviderNormalizationResult {
                        captures: vec![(
                            line,
                            hermes_capture(
                                &self.database_path,
                                &self.context,
                                self.user_version,
                                &self.schema_fingerprint,
                                &session,
                                None,
                            )?,
                        )],
                        ..ProviderNormalizationResult::default()
                    })
                })
            }
            HERMES_MESSAGE_RECORD_KIND => {
                let message = decode_hermes_message(&self.schema, values)
                    .map_err(ProviderProjectionFatal::new)?;
                match hermes_existing_session_message_capture(
                    &self.database_path,
                    &self.context,
                    self.user_version,
                    &self.schema_fingerprint,
                    &message,
                ) {
                    Ok(capture) => {
                        // The session phase is authoritative. Persist only the
                        // event against that exact source-scoped Store session,
                        // leaving its complete metadata untouched.
                        output.emit_existing_session_event(line, capture)?;
                        return Ok(());
                    }
                    Err(error) => Err(error),
                }
            }
            _ => Err(CaptureError::SystemInvariant(
                "Hermes projector received an unexpected record kind",
            )),
        };
        match projected {
            Ok(normalization) => output.emit_normalization(normalization),
            Err(error) => {
                output.reject_record(line, error.to_string());
                Ok(())
            }
        }
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if decode_hermes_position(position)?.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Hermes initial cursor candidate is not at the SQLite source start".to_owned(),
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

pub(crate) fn import_hermes_sqlite_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    context.source_path = Some(path.to_path_buf());
    let canonical_path = fs::canonicalize(path)?;
    let snapshot = hermes_source_snapshot(path)?;
    let cursor_path = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Hermes,
        HERMES_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let schema = HermesSchema::detect(&conn)?;
    let source = SourceObservation::new(
        CaptureProvider::Hermes,
        HERMES_SQLITE_SOURCE_FORMAT,
        format!("hermes-sqlite:{cursor_path}"),
        hermes_source_revision(&snapshot, &schema_fingerprint),
        cursor_stream,
        HERMES_CAPTURE_REVISION,
        HERMES_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(hermes_captured_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let initial_position = initial_hermes_position()?;
    let mut start_position = initial_position.clone();
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.matches_revisions(
                    source.source_revision(),
                    source.capture_revision(),
                    source.policy_revision(),
                ) =>
            {
                let _: () = certified.parser_checkpoint().deserialize()?;
                decode_hermes_position(certified.native_position())?;
                start_position = certified.native_position().clone();
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    let source_exhausted = Cell::new(false);
    let producer_source_exhausted = &source_exhausted;
    let mut fetcher = HermesRowFetcher::new(&conn, &schema)?;
    let mut producer = Some(SqliteLogicalRowBatchProducer::new(
        source,
        start_position,
        move |position| {
            let row = fetcher.fetch(position)?;
            if row.is_none() {
                producer_source_exhausted.set(true);
            }
            Ok(row)
        },
    ));
    let mut projector = HermesCapturedBatchProjector {
        context: context.clone(),
        database_path: path.to_path_buf(),
        user_version,
        schema_fingerprint,
        schema,
    };
    drain_captured_batches(
        store,
        &admission,
        import_options,
        &context.machine_id,
        context.imported_at,
        expected_store_cursor,
        &initial_position,
        cursor_mode,
        &stream,
        &mut projector,
        || {
            let Some(active_producer) = producer.as_mut() else {
                return Ok(None);
            };
            if !snapshot.revalidate(path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let batch = with_sqlite_read_snapshot(&conn, || {
                active_producer
                    .next_batch()
                    .map_err(hermes_sqlite_batch_error)
            })?;
            if !snapshot.revalidate(path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            if source_exhausted.get() {
                producer.take();
            }
            Ok(batch)
        },
        || snapshot.revalidate(path),
    )
}

fn hermes_existing_session_message_capture(
    path: &Path,
    context: &ProviderAdapterContext,
    user_version: i64,
    schema_fingerprint: &str,
    row: &HermesMessageRow,
) -> Result<ctx_history_core::ProviderCaptureEnvelope> {
    let provider_event_index = provider_nonnegative_i64_to_u64(row.id, "Hermes message id")?;
    let occurred_at =
        provider_required_timestamp_seconds(row.timestamp, "Hermes message timestamp")?;
    let content = hermes_decode_content(row.content.as_deref());
    let text = provider_value_text(&content).unwrap_or_else(|| {
        row.tool_name
            .as_ref()
            .map(|name| format!("tool: {name}"))
            .unwrap_or_else(|| format!("Hermes {}", row.role))
    });
    let event = native_event(NativeEventDraft {
        provider: CaptureProvider::Hermes,
        source_format: HERMES_SQLITE_SOURCE_FORMAT,
        provider_session_id: row.session_id.clone(),
        provider_event_index,
        provider_event_hash: Some(format!("message:{}", row.id)),
        cursor: format!("messages:id:{}", row.id),
        event_type: hermes_event_type(row),
        role: Some(provider_role(Some(&row.role))),
        occurred_at,
        text,
        body: json!({
            "message_id": row.id,
            "role": row.role,
            "content": content,
            "tool_call_id": row.tool_call_id,
            "tool_calls": row.tool_calls.as_deref().map(provider_json_text),
            "tool_name": row.tool_name,
            "reasoning": row.reasoning,
            "reasoning_content": row.reasoning_content,
            "reasoning_details": row.reasoning_details.as_deref().map(provider_json_text),
            "codex_reasoning_items": row.codex_reasoning_items.as_deref().map(provider_json_text),
            "codex_message_items": row.codex_message_items.as_deref().map(provider_json_text),
        }),
        metadata: json!({
            "source": "hermes_state_db",
            "source_format": HERMES_SQLITE_SOURCE_FORMAT,
            "message_id": row.id,
            "platform_message_id": row.platform_message_id,
            "token_count": row.token_count,
            "finish_reason": row.finish_reason,
            "observed": row.observed != 0,
            "active": row.active != 0,
            "compacted": row.compacted != 0,
        }),
    });
    Ok(native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Hermes,
            source_format: HERMES_SQLITE_SOURCE_FORMAT,
            provider_session_id: row.session_id.clone(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Unknown,
            role_hint: None,
            is_primary: false,
            started_at: occurred_at,
            ended_at: None,
            cwd: None,
            fidelity: Fidelity::Imported,
            raw_source_path: path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": HERMES_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": user_version,
                "schema_fingerprint": schema_fingerprint,
                "upstream_schema_version_at_research": 17,
                "capture_policy": "bounded_structural_one_pass_v1",
            }),
            // The existing-session event seam resolves the authoritative
            // session emitted in the first phase and never persists this stub.
            session_metadata: json!({}),
        },
        context,
        Some(event),
    ))
}

fn hermes_capture(
    path: &Path,
    context: &ProviderAdapterContext,
    user_version: i64,
    schema_fingerprint: &str,
    session: &HermesSessionRow,
    event: Option<ctx_history_core::ProviderEventEnvelope>,
) -> Result<ctx_history_core::ProviderCaptureEnvelope> {
    let started_at =
        provider_required_timestamp_seconds(session.started_at, "Hermes session started_at")?;
    let ended_at = session
        .ended_at
        .map(|timestamp| provider_required_timestamp_seconds(timestamp, "Hermes session ended_at"))
        .transpose()?;
    Ok(native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Hermes,
            source_format: HERMES_SQLITE_SOURCE_FORMAT,
            provider_session_id: session.id.clone(),
            parent_provider_session_id: session.parent_session_id.clone(),
            root_provider_session_id: None,
            external_agent_id: Some(session.source.clone()),
            agent_type: if session.parent_session_id.is_some() {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
            role_hint: Some(session.source.clone()),
            is_primary: session.parent_session_id.is_none(),
            started_at,
            ended_at,
            cwd: session.cwd.clone(),
            fidelity: Fidelity::Imported,
            raw_source_path: path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": HERMES_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": user_version,
                "schema_fingerprint": schema_fingerprint,
                "upstream_schema_version_at_research": 17,
                "capture_policy": "bounded_structural_one_pass_v1",
            }),
            session_metadata: json!({
                "source_format": HERMES_SQLITE_SOURCE_FORMAT,
                "source": session.source,
                "title": session.title,
                "model": session.model,
                "model_config": session.model_config.as_deref().map(provider_json_text),
                "end_reason": session.end_reason,
                "message_count": session.message_count,
                "tool_call_count": session.tool_call_count,
                "tokens": {
                    "input": session.input_tokens,
                    "output": session.output_tokens,
                    "cache_read": session.cache_read_tokens,
                    "cache_write": session.cache_write_tokens,
                    "reasoning": session.reasoning_tokens,
                },
                "git": {
                    "branch": session.git_branch,
                    "repo_root": session.git_repo_root,
                },
                "billing": {
                    "provider": session.billing_provider,
                    "base_url": session.billing_base_url,
                    "mode": session.billing_mode,
                    "estimated_cost_usd": session.estimated_cost_usd,
                    "actual_cost_usd": session.actual_cost_usd,
                },
                "archived": session.archived != 0,
            }),
        },
        context,
        event,
    ))
}

fn hermes_event_type(row: &HermesMessageRow) -> EventType {
    if row.role == "tool" {
        EventType::ToolOutput
    } else if row
        .tool_calls
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || row
            .tool_name
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    {
        EventType::ToolCall
    } else {
        EventType::Message
    }
}

pub(super) fn hermes_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
#[path = "hermes/tests.rs"]
mod tests;
