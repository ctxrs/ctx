use std::path::Path;

use ctx_history_store::Store;

use crate::captured_batch::jsonl::JsonlBatchError;
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result,
};

mod assistant;
mod capture;
mod checkpoint;
mod normalize;
mod projector;
mod session_tree;
mod source;

pub(crate) use assistant::{
    junie_buffer_result_text, junie_buffer_step_output, junie_merge_buffered_agent_event,
    JunieAssistantBuffer,
};

const JUNIE_CAPTURE_REVISION: u32 = 2;
const JUNIE_POLICY_REVISION: u32 = 5;
const JUNIE_SOURCE_REVISION_SCHEMA: &str = "junie-session-events-v2";
const JUNIE_RECORD_KIND: &str = "junie-session-events-jsonl-v1";
const JUNIE_END_RECORD_KIND: &str = "junie-session-events-end-v1";
const JUNIE_END_LOCATOR_KIND: &str = "junie-session-events-end-locator-v1";
const JUNIE_JSONL_LOCATOR_KIND: &str = "jsonl-source-item-byte-range-v1";
const MAX_JUNIE_CHECKPOINT_FAILURES: usize = 16;
const MAX_JUNIE_FAILURE_BYTES: usize = 4 * 1024;
// Match the existing Junie discovery probe's physical index-entry budget.
const MAX_JUNIE_INDEX_ENTRIES: usize = 10_000;
// An entire Junie index gets the same byte allowance as one provider JSONL
// record. The checked-in fixture is one 167-byte entry.
const MAX_JUNIE_INDEX_BYTES: usize = crate::MAX_PROVIDER_JSONL_LINE_BYTES;
const MAX_JUNIE_INDEX_METADATA_BYTES: usize = 32 * 1024;
const MAX_JUNIE_PARSER_STATE_BYTES: usize = 192 * 1024;
const MAX_JUNIE_TRANSIENT_TURN_BYTES: usize = crate::MAX_PROVIDER_JSONL_LINE_BYTES;

pub(crate) fn import_junie_session_events_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let configured_source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    context.source_path = Some(path.to_path_buf());
    context.source_root = Some(configured_source_root);

    let mut merged = ProviderImportSummary::default();
    let source_count =
        session_tree::visit_junie_session_event_paths(path, &mut |session_path, ordinal| {
            let summary = capture::import_junie_session_events_file_batched(
                &session_path,
                ordinal,
                store,
                &context,
                &import_options,
            )?;
            merged.merge(summary);
            Ok(())
        })?;

    if source_count == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Junie index.jsonl entries with session events.jsonl files were found",
        });
    }
    if !merged.has_accepted_content() && merged.failed == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Junie session events were empty or unsupported",
        });
    }
    Ok(merged)
}

fn coalesce_junie_session_summary(mut summary: ProviderImportSummary) -> ProviderImportSummary {
    let retained_imported_sessions = usize::from(summary.imported_sessions != 0);
    let retained_skipped_sessions =
        usize::from(retained_imported_sessions == 0 && summary.skipped_sessions != 0);
    let duplicate_imported_sessions = summary
        .imported_sessions
        .saturating_sub(retained_imported_sessions);
    let duplicate_skipped_sessions = summary
        .skipped_sessions
        .saturating_sub(retained_skipped_sessions);
    summary.imported = summary.imported.saturating_sub(duplicate_imported_sessions);
    summary.skipped = summary.skipped.saturating_sub(duplicate_skipped_sessions);
    summary.imported_sessions = retained_imported_sessions;
    summary.skipped_sessions = retained_skipped_sessions;
    summary
}

fn junie_jsonl_batch_error(error: JsonlBatchError) -> CaptureError {
    match error {
        JsonlBatchError::Io(error) => CaptureError::Io(error),
        other => CaptureError::InvalidPayload(other.to_string()),
    }
}

fn junie_captured_batch_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
mod tests;
