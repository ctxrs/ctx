use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, Fidelity, ProviderCaptureEnvelope, ProviderEventEnvelope,
    ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, CapturedSqliteValue, NativeLocator,
    NativePosition, SourceObservation,
};
use crate::provider::file_touches::{
    visit_provider_file_touches_from_raw_value, ProviderFileTouchSourceContext,
    PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
};
use crate::provider::importer::{
    BoundedParserCheckpoint, CapturedBatchCursorFinish, CapturedBatchProjector,
    CertifiedProviderCursor, ProviderProjectionFatal, ProviderProjectionOutput,
    ProviderProjectionResult,
};
use crate::provider::normalization::{
    native_provider_capture, provider_line_from_index, NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    ZED_THREADS_SQLITE_SOURCE_FORMAT,
};

use super::event::{decode_zed_thread_events, ZedDecodedEvent};
use super::source::{decode_zed_storage_rejection, initial_zed_position};
use super::thread::{decode_zed_thread, zed_required_timestamp, ZedThreadRow};
use super::{ZED_MALFORMED_RECORD_KIND, ZED_RECORD_KIND};

pub(super) struct ZedCapturedBatchProjector {
    pub(super) context: ProviderAdapterContext,
    pub(super) raw_source_path: String,
    pub(super) user_version: i64,
    pub(super) schema_fingerprint: String,
}

impl CapturedBatchProjector for ZedCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Zed projector requires SQLite logical values",
            ));
        };
        if record.record_kind().as_str() == ZED_MALFORMED_RECORD_KIND {
            let (rowid, storage_error) =
                decode_zed_storage_rejection(values).map_err(ProviderProjectionFatal::new)?;
            output.reject_record(
                zed_line_number(rowid, 0),
                storage_error.rejection_reason().to_owned(),
            );
            return Ok(());
        }
        if record.record_kind().as_str() != ZED_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "Zed projector received an unexpected record kind",
            ));
        }
        let row = decode_zed_thread(values).map_err(ProviderProjectionFatal::new)?;
        project_zed_thread_row(
            &row,
            record.locator(),
            values,
            &self.raw_source_path,
            self.user_version,
            &self.schema_fingerprint,
            &self.context,
            output,
        )
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if *position != initial_zed_position()? {
            return Err(CaptureError::InvalidPayload(
                "Zed initial cursor candidate is not at the SQLite source start".to_owned(),
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

#[allow(clippy::too_many_arguments)]
fn project_zed_thread_row(
    row: &ZedThreadRow,
    locator: &NativeLocator,
    values: &[CapturedSqliteValue],
    raw_source_path: &str,
    user_version: i64,
    schema_fingerprint: &str,
    context: &ProviderAdapterContext,
    output: &mut dyn ProviderProjectionOutput,
) -> ProviderProjectionResult<()> {
    let row_line = zed_line_number(row.rowid, 0);
    let decoded = match decode_zed_thread_events(row) {
        Ok(decoded) => decoded,
        Err(error) => {
            output.reject_record(row_line, error.to_string());
            return Ok(());
        }
    };
    let created_at = match row
        .created_at
        .as_deref()
        .map(|raw| zed_required_timestamp(raw, "Zed thread created_at"))
        .transpose()
    {
        Ok(timestamp) => timestamp.unwrap_or(decoded.row_updated_at()),
        Err(error) => {
            output.reject_record(row_line, error.to_string());
            return Ok(());
        }
    };
    let thread = decoded.thread();
    let messages = decoded.messages();
    let folder_paths = zed_folder_paths(row.folder_paths.as_deref());
    let cwd = zed_ordered_folder_paths(&folder_paths, row.folder_paths_order.as_deref())
        .into_iter()
        .next();

    output.use_explicit_file_touches();
    if messages.is_empty() {
        return output.emit_normalization(ProviderNormalizationResult {
            captures: vec![(
                row_line,
                zed_capture(
                    ZedCaptureDraft {
                        row,
                        thread,
                        started_at: created_at,
                        ended_at: Some(decoded.event_occurred_at()),
                        cwd,
                        folder_paths,
                        raw_source_path,
                        user_version,
                        schema_fingerprint,
                        event: None,
                    },
                    context,
                ),
            )],
            ..ProviderNormalizationResult::default()
        });
    }

    for decoded_event in decoded.events(&row.id) {
        let ZedDecodedEvent {
            mut event,
            complete_text,
            message,
            first_for_message,
        } = match decoded_event {
            Ok(event) => event,
            Err(error) => {
                output.reject_record(row_line, error.to_string());
                continue;
            }
        };
        let line = zed_line_number(row.rowid, event.provider_event_index);
        crate::complete_content::sqlite::attach_sqlite_complete_content_locator(
            &mut event,
            locator,
            values,
            || complete_text,
        )
        .map_err(ProviderProjectionFatal::new)?;
        output.emit_normalization(ProviderNormalizationResult {
            captures: vec![(
                line,
                zed_capture(
                    ZedCaptureDraft {
                        row,
                        thread,
                        started_at: created_at,
                        ended_at: Some(decoded.event_occurred_at()),
                        cwd: cwd.clone(),
                        folder_paths: folder_paths.clone(),
                        raw_source_path,
                        user_version,
                        schema_fingerprint,
                        event: Some(event.clone()),
                    },
                    context,
                ),
            )],
            ..ProviderNormalizationResult::default()
        })?;
        if !first_for_message {
            continue;
        }
        let touch_outcome = visit_provider_file_touches_from_raw_value(
            ProviderFileTouchSourceContext::new(
                CaptureProvider::Zed,
                &row.id,
                ZED_THREADS_SQLITE_SOURCE_FORMAT,
                Some(raw_source_path),
                Some(raw_source_path),
            ),
            message,
            &event,
            line,
            |file_touch| {
                output.emit_normalization(ProviderNormalizationResult {
                    files_touched: vec![file_touch],
                    ..ProviderNormalizationResult::default()
                })
            },
        )?;
        if touch_outcome.limit_exceeded() {
            output.reject_record(line, PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned());
        }
    }
    Ok(())
}

struct ZedCaptureDraft<'a> {
    row: &'a ZedThreadRow,
    thread: &'a Value,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    cwd: Option<String>,
    folder_paths: Vec<String>,
    raw_source_path: &'a str,
    user_version: i64,
    schema_fingerprint: &'a str,
    event: Option<ProviderEventEnvelope>,
}

fn zed_capture(
    draft: ZedCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let title = draft
        .thread
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or(&draft.row.summary);
    let model = draft.thread.get("model").cloned().unwrap_or(Value::Null);
    let token_usage = draft
        .thread
        .get("cumulative_token_usage")
        .cloned()
        .unwrap_or(Value::Null);
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Zed,
            source_format: ZED_THREADS_SQLITE_SOURCE_FORMAT,
            provider_session_id: draft.row.id.clone(),
            parent_provider_session_id: draft.row.parent_id.clone(),
            root_provider_session_id: draft.row.parent_id.clone(),
            external_agent_id: Some("zed".to_owned()),
            agent_type: if draft.row.parent_id.is_some() {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
            role_hint: Some(
                if draft.row.parent_id.is_some() {
                    "subagent"
                } else {
                    "primary"
                }
                .to_owned(),
            ),
            is_primary: draft.row.parent_id.is_none(),
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: draft.cwd,
            fidelity: Fidelity::Imported,
            raw_source_path: draft.raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": ZED_THREADS_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": draft.user_version,
                "schema_fingerprint": draft.schema_fingerprint,
                "source_path": draft.raw_source_path,
                "upstream_schema_anchor": {
                    "repository": "zed-industries/zed",
                    "commit": "e3b73c6b30cdc09e820823fe44542b89850d4be1",
                    "files": [
                        "crates/agent/src/db.rs",
                        "crates/agent/src/thread.rs"
                    ],
                    "thread_version": draft.thread.get("version").and_then(Value::as_str)
                },
            }),
            session_metadata: json!({
                "source_format": ZED_THREADS_SQLITE_SOURCE_FORMAT,
                "title": title,
                "summary": draft.row.summary,
                "parent_id": draft.row.parent_id,
                "folder_paths": draft.folder_paths,
                "folder_paths_order": draft.row.folder_paths_order,
                "created_at": draft.row.created_at,
                "updated_at": draft.row.updated_at,
                "data_type": draft.row.data_type,
                "model": model,
                "profile": draft.thread.get("profile").cloned().unwrap_or(Value::Null),
                "speed": draft.thread.get("speed").cloned().unwrap_or(Value::Null),
                "thinking_enabled": draft.thread.get("thinking_enabled").cloned().unwrap_or(Value::Null),
                "thinking_effort": draft.thread.get("thinking_effort").cloned().unwrap_or(Value::Null),
                "cumulative_token_usage": token_usage,
                "message_timestamps": "Zed DbThread messages do not carry per-message timestamps; ctx uses the thread updated_at for events.",
            }),
        },
        context,
        draft.event,
    )
}

fn zed_line_number(rowid: i64, message_index: u64) -> usize {
    let row = u64::try_from(rowid.max(0)).unwrap_or(0);
    provider_line_from_index(row.saturating_mul(10_000).saturating_add(message_index))
}

fn zed_folder_paths(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

fn zed_ordered_folder_paths(paths: &[String], order: Option<&str>) -> Vec<String> {
    let Some(order) = order else {
        return paths.to_vec();
    };
    let indices = order
        .split(',')
        .filter_map(|item| item.parse::<usize>().ok())
        .collect::<Vec<_>>();
    if indices.len() != paths.len() {
        return paths.to_vec();
    }
    let mut ordered = paths
        .iter()
        .cloned()
        .zip(indices)
        .collect::<Vec<(String, usize)>>();
    ordered.sort_by_key(|(_, index)| *index);
    ordered.into_iter().map(|(path, _)| path).collect()
}

#[cfg(test)]
#[path = "projection/tests.rs"]
mod tests;
