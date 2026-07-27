use chrono::{DateTime, Duration, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use serde_json::json;

use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, CapturedSqliteValue, NativePosition,
    SourceObservation,
};
use crate::provider::importer::{
    emit_projected_normalization_units, BoundedParserCheckpoint, CapturedBatchCursorFinish,
    CapturedBatchProjector, CertifiedProviderCursor, ProviderProjectionFatal,
    ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::{
    native_provider_capture, provider_capped_json, provider_json_text, provider_line_from_index,
    provider_policy_body, provider_policy_event_text, provider_result_identifier_evidence,
    provider_result_outcome_evidence, provider_timestamp_seconds, text_id_index,
    NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    LINGMA_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::sqlite::decode_lingma_position;
use super::{LINGMA_MALFORMED_RECORD_KIND, LINGMA_RECORD_KIND, LINGMA_SKIPPED_RECORD_KIND};

const LINGMA_VALUE_COUNT: usize = 8;

pub(super) struct LingmaCapturedBatchProjector {
    context: ProviderAdapterContext,
    raw_source_path: String,
    user_version: i64,
    schema_fingerprint: String,
}

impl LingmaCapturedBatchProjector {
    pub(super) fn new(
        context: ProviderAdapterContext,
        raw_source_path: String,
        user_version: i64,
        schema_fingerprint: String,
    ) -> Self {
        Self {
            context,
            raw_source_path,
            user_version,
            schema_fingerprint,
        }
    }
}

impl CapturedBatchProjector for LingmaCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() == LINGMA_SKIPPED_RECORD_KIND {
            let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
                return Err(ProviderProjectionFatal::system_invariant(
                    "Lingma skipped-row marker requires SQLite logical values",
                ));
            };
            if values.len() != 1 || lingma_values_rowid(values).is_err() {
                return Err(ProviderProjectionFatal::system_invariant(
                    "Lingma skipped-row marker has an invalid rowid",
                ));
            }
            return Ok(());
        }
        if record.record_kind().as_str() == LINGMA_MALFORMED_RECORD_KIND {
            let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
                return Err(ProviderProjectionFatal::system_invariant(
                    "Lingma malformed-text marker requires SQLite logical values",
                ));
            };
            if values.len() != 1 || lingma_values_rowid(values).is_err() {
                return Err(ProviderProjectionFatal::system_invariant(
                    "Lingma malformed-text marker has an invalid rowid",
                ));
            }
            let line_number = usize::try_from(record.ordinal())
                .ok()
                .and_then(|ordinal| ordinal.checked_add(1))
                .ok_or_else(|| {
                    ProviderProjectionFatal::system_invariant(
                        "Lingma malformed-text record ordinal exceeds platform limits",
                    )
                })?;
            output.reject_record(
                line_number,
                "Lingma SQLite row contains malformed text encoding".to_owned(),
            );
            return Ok(());
        }
        if record.record_kind().as_str() != LINGMA_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "Lingma projector received an unexpected record kind",
            ));
        }
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Lingma projector requires SQLite logical values",
            ));
        };
        let row = decode_lingma_values(values).map_err(ProviderProjectionFatal::new)?;
        let session = lingma_row_session_info(&row, self.context.imported_at);
        emit_projected_normalization_units(
            output,
            lingma_row_normalization(
                row,
                &session,
                &self.raw_source_path,
                self.user_version,
                &self.schema_fingerprint,
                &self.context,
            ),
        )
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if decode_lingma_position(position)?.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Lingma initial cursor candidate is not at the SQLite source start".to_owned(),
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

struct LingmaChatRecordRow {
    rowid: i64,
    session_id: String,
    request_id: Option<String>,
    chat_prompt: String,
    summary: Option<String>,
    error_result: Option<String>,
    gmt_create: Option<i64>,
    extra: Option<String>,
}

#[derive(Debug, Clone)]
struct LingmaSessionInfo {
    id: String,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
}

pub(super) fn lingma_values_rowid(values: &[CapturedSqliteValue]) -> Result<i64> {
    lingma_required_integer(values, 0, "rowid")
}

fn decode_lingma_values(values: &[CapturedSqliteValue]) -> Result<LingmaChatRecordRow> {
    if values.len() != LINGMA_VALUE_COUNT {
        return Err(CaptureError::InvalidPayload(
            "Lingma logical row has an unexpected value count".to_owned(),
        ));
    }
    Ok(LingmaChatRecordRow {
        rowid: lingma_required_integer(values, 0, "rowid")?,
        session_id: lingma_required_text(values, 1, "session_id")?,
        request_id: lingma_optional_text(values, 2, "request_id")?,
        chat_prompt: lingma_required_text(values, 3, "chat_prompt")?,
        summary: lingma_optional_text(values, 4, "summary")?,
        error_result: lingma_optional_text(values, 5, "error_result")?,
        gmt_create: lingma_optional_integer(values, 6, "gmt_create")?,
        extra: lingma_optional_text(values, 7, "extra")?,
    })
}

fn lingma_row_session_info(
    row: &LingmaChatRecordRow,
    imported_at: DateTime<Utc>,
) -> LingmaSessionInfo {
    let started_at = lingma_timestamp(row.gmt_create, imported_at);
    LingmaSessionInfo {
        id: row.session_id.clone(),
        started_at,
        ended_at: started_at.checked_add_signed(Duration::milliseconds(100)),
    }
}

fn lingma_value<'a>(
    values: &'a [CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<&'a CapturedSqliteValue> {
    values.get(index).ok_or_else(|| {
        CaptureError::InvalidPayload(format!("Lingma logical row is missing {field}"))
    })
}

fn lingma_required_text(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<String> {
    match lingma_value(values, index, field)? {
        CapturedSqliteValue::Text(value) => Ok(value.clone()),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma logical row {field} must be text"
        ))),
    }
}

fn lingma_optional_text(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<String>> {
    match lingma_value(values, index, field)? {
        CapturedSqliteValue::Null => Ok(None),
        CapturedSqliteValue::Text(value) => Ok(Some(value.clone())),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma logical row {field} must be text or null"
        ))),
    }
}

fn lingma_required_integer(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<i64> {
    match lingma_value(values, index, field)? {
        CapturedSqliteValue::Integer(value) => Ok(*value),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma logical row {field} must be an integer"
        ))),
    }
}

fn lingma_optional_integer(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<i64>> {
    match lingma_value(values, index, field)? {
        CapturedSqliteValue::Null => Ok(None),
        CapturedSqliteValue::Integer(value) => Ok(Some(*value)),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma logical row {field} must be an integer or null"
        ))),
    }
}
fn lingma_row_normalization(
    row: LingmaChatRecordRow,
    session: &LingmaSessionInfo,
    raw_source_path: &str,
    user_version: i64,
    schema_fingerprint: &str,
    context: &ProviderAdapterContext,
) -> ProviderNormalizationResult {
    let occurred_at = lingma_timestamp(row.gmt_create, context.imported_at);
    let base_index = lingma_event_base_index(&row);
    let user_event = lingma_event(
        &row,
        LingmaEventDraft {
            provider_event_index: base_index,
            role: EventRole::User,
            event_type: EventType::Message,
            occurred_at,
            text: row.chat_prompt.clone(),
            body_kind: "chat_prompt",
            fidelity: Fidelity::Imported,
        },
    );
    let mut result = ProviderNormalizationResult {
        captures: vec![(
            provider_line_from_index(base_index),
            lingma_capture(
                session,
                LingmaCaptureContext {
                    raw_source_path,
                    user_version,
                    schema_fingerprint,
                    event: Some(user_event),
                },
                context,
            ),
        )],
        ..ProviderNormalizationResult::default()
    };

    if let Some((assistant_text, body_kind, event_type)) = lingma_assistant_text(&row) {
        let assistant_index = base_index.saturating_add(1);
        let assistant_event = lingma_event(
            &row,
            LingmaEventDraft {
                provider_event_index: assistant_index,
                role: EventRole::Assistant,
                event_type,
                occurred_at: occurred_at
                    .checked_add_signed(Duration::milliseconds(100))
                    .unwrap_or(occurred_at),
                text: assistant_text,
                body_kind,
                fidelity: Fidelity::SummaryOnly,
            },
        );
        result.captures.push((
            provider_line_from_index(assistant_index),
            lingma_capture(
                session,
                LingmaCaptureContext {
                    raw_source_path,
                    user_version,
                    schema_fingerprint,
                    event: Some(assistant_event),
                },
                context,
            ),
        ));
    }
    result
}

struct LingmaCaptureContext<'a> {
    raw_source_path: &'a str,
    user_version: i64,
    schema_fingerprint: &'a str,
    event: Option<ProviderEventEnvelope>,
}

fn lingma_capture(
    session: &LingmaSessionInfo,
    draft: LingmaCaptureContext<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Lingma,
            source_format: LINGMA_SQLITE_SOURCE_FORMAT,
            provider_session_id: session.id.clone(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            started_at: session.started_at,
            ended_at: session.ended_at,
            cwd: None,
            fidelity: Fidelity::Partial,
            raw_source_path: draft.raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": LINGMA_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": draft.user_version,
                "schema_fingerprint": draft.schema_fingerprint,
                "source_path": draft.raw_source_path,
                "source_table": "chat_record",
                "source_fidelity": "user prompts plus assistant summaries/errors",
                "session_metadata_fidelity": "row-local temporal bounds; source-wide title and row count intentionally omitted",
                "assistant_content_caveat": "WayLog labels Lingma as summaries-only; original assistant answers may be encrypted, transformed, or unavailable in this DB."
            }),
            session_metadata: json!({
                "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
                "session_id": session.id,
                "source_table": "chat_record",
                "source_fidelity": "partial",
                "session_metadata_fidelity": "row-local temporal bounds; source-wide title and row count intentionally omitted",
                "assistant_content_caveat": "assistant events imported from summary/error_result, not guaranteed full assistant message bodies"
            }),
        },
        context,
        draft.event,
    )
}

struct LingmaEventDraft {
    provider_event_index: u64,
    role: EventRole,
    event_type: EventType,
    occurred_at: DateTime<Utc>,
    text: String,
    body_kind: &'static str,
    fidelity: Fidelity,
}

fn lingma_event(row: &LingmaChatRecordRow, draft: LingmaEventDraft) -> ProviderEventEnvelope {
    let role_name = match draft.role {
        EventRole::User => "user",
        EventRole::Assistant => "assistant",
        EventRole::System => "system",
        EventRole::Tool => "tool",
        EventRole::Unknown => "unknown",
    };
    let body = json!({
        "rowid": row.rowid,
        "session_id": row.session_id,
        "request_id": row.request_id,
        "role": role_name,
        "body_kind": draft.body_kind,
        "chat_prompt": row.chat_prompt,
        "summary": row.summary,
        "error_result": row.error_result,
        "gmt_create": row.gmt_create,
        "extra": row.extra.as_deref().map(provider_json_text),
    });
    let retained_text = provider_policy_event_text(draft.event_type, &draft.text, &body);
    let result_evidence = provider_result_identifier_evidence(draft.event_type, &draft.text, &body);
    let result_outcome = provider_result_outcome_evidence(draft.event_type, &body);
    ProviderEventEnvelope {
        provider_event_index: draft.provider_event_index,
        provider_event_hash: Some(format!(
            "{}:{}:{role_name}",
            row.session_id,
            lingma_request_identity(row)
        )),
        cursor: Some(format!(
            "chat_record:{}:rowid:{}:{role_name}",
            row.session_id, row.rowid
        )),
        event_type: draft.event_type,
        role: Some(draft.role),
        occurred_at: draft.occurred_at,
        fidelity: draft.fidelity,
        idempotency_key: Some(format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::Lingma.as_str(),
            row.session_id,
            draft.provider_event_index
        )),
        artifacts: Vec::new(),
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "body": provider_capped_json(&provider_policy_body(draft.event_type, &body), PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": "lingma_chat_record",
            "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
            "rowid": row.rowid,
            "session_id": row.session_id,
            "request_id": row.request_id,
            "body_kind": draft.body_kind,
            "gmt_create": row.gmt_create,
            "content_fidelity": if draft.fidelity == Fidelity::SummaryOnly { "summary_only" } else { "imported" },
            "assistant_content_caveat": if draft.role == EventRole::Assistant {
                Some("summary/error_result only; original assistant body may be encrypted or unavailable")
            } else {
                None
            },
        }),
    }
}

fn lingma_event_base_index(row: &LingmaChatRecordRow) -> u64 {
    let rowid = u64::try_from(row.rowid).unwrap_or_else(|_| text_id_index(&row.session_id, 0));
    rowid.saturating_sub(1).saturating_mul(2)
}

fn lingma_timestamp(raw: Option<i64>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    raw.map(|timestamp| provider_timestamp_seconds(Some(timestamp as f64), fallback))
        .unwrap_or(fallback)
}

fn lingma_assistant_text(row: &LingmaChatRecordRow) -> Option<(String, &'static str, EventType)> {
    if let Some(summary) = row
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return Some((summary.to_owned(), "summary", EventType::Message));
    }
    row.error_result
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty() && *text != "{}")
        .map(|error| {
            (
                format!("Lingma error result: {error}"),
                "error_result",
                EventType::Notice,
            )
        })
}

fn lingma_request_identity(row: &LingmaChatRecordRow) -> String {
    row.request_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("rowid-{}", row.rowid))
}
