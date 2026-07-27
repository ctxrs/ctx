//! Verified source-backed result recovery for Warp protobuf task rows.

use ctx_history_core::EventType;
use rusqlite::{Connection, OptionalExtension};

use crate::{native_source::NativeSqliteValue, provider::providers::warp, CaptureError};

use super::super::{
    CompleteContentError, CompleteContentErrorKind, ResolvedResultContent, ResultContentRequest,
    SourceVerification, COMPLETE_CONTENT_MAX_BODY_BYTES,
};
use super::sqlite_logical_record_digest;

pub(super) fn resolve_result(
    conn: &Connection,
    request: &ResultContentRequest,
    rowid: i64,
    message_index: usize,
) -> Result<ResolvedResultContent, CompleteContentError> {
    let values = conn
        .query_row(
            "select rowid, cast(conversation_id as text), cast(task_id as text), task, \
                    cast(last_modified_at as text) \
             from agent_tasks where rowid = ?1",
            [rowid],
            |row| {
                Ok(vec![
                    NativeSqliteValue::Integer(row.get(0)?),
                    NativeSqliteValue::Text(row.get(1)?),
                    NativeSqliteValue::Text(row.get(2)?),
                    NativeSqliteValue::Blob(row.get(3)?),
                    NativeSqliteValue::Text(row.get(4)?),
                ])
            },
        )
        .optional()
        .map_err(|cause| map_capture_error(request, CaptureError::Sqlite(cause)))?
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    if sqlite_logical_record_digest(&values) != request.expected_record_digest {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let [_, _, NativeSqliteValue::Text(task_id), NativeSqliteValue::Blob(task), _] =
        values.as_slice()
    else {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    };
    let content = warp::warp_task_content_at(task, task_id, message_index)
        .map_err(|cause| map_capture_error(request, cause))?
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    if !matches!(
        content.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) || content.native_record_id != request.expected_native_record_id
        || !request
            .expected_content_ref
            .verifies(content.text.as_bytes())
        || content.text.len() > COMPLETE_CONTENT_MAX_BODY_BYTES
    {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(ResolvedResultContent {
        event_id: request.event_id,
        content: content.text,
        content_ref: request.expected_content_ref.clone(),
        verification: SourceVerification::VERIFIED,
    })
}

fn error(request: &ResultContentRequest, kind: CompleteContentErrorKind) -> CompleteContentError {
    CompleteContentError::new(kind, request.event_id)
}

fn map_capture_error(request: &ResultContentRequest, cause: CaptureError) -> CompleteContentError {
    let kind = match cause {
        CaptureError::Io(cause) if cause.kind() == std::io::ErrorKind::NotFound => {
            CompleteContentErrorKind::SourceMissing
        }
        CaptureError::Io(_) | CaptureError::InvalidProviderTranscriptPath { .. } => {
            CompleteContentErrorKind::SourceUnreadable
        }
        CaptureError::SourceChangedDuringCapture => CompleteContentErrorKind::SourceChanged,
        CaptureError::Sqlite(rusqlite::Error::SqliteFailure(failure, _))
            if matches!(
                failure.code,
                rusqlite::ErrorCode::TooBig | rusqlite::ErrorCode::OperationInterrupted
            ) =>
        {
            CompleteContentErrorKind::ContentTooLarge
        }
        _ => CompleteContentErrorKind::ContentVerificationFailed,
    };
    error(request, kind)
}
