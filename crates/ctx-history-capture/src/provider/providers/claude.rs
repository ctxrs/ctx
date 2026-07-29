use std::path::Path;

use ctx_history_store::Store;

use crate::{CaptureError, ClaudeProjectsImportOptions, ProviderImportSummary, Result};

mod complete_content;
pub(crate) mod nativepath;

pub(crate) use complete_content::{
    claude_complete_content_message_record, claude_complete_content_normalized_payload,
};

/// Compatibility signature retained until shared released-import callers disappear.
///
/// Claude production ingestion is source-backed; provider-local Store
/// publication, cursors, retirement, and output replay have been deleted.
pub(crate) fn import_claude_nativepath_projects(
    _path: &Path,
    _store: &mut Store,
    _options: ClaudeProjectsImportOptions,
) -> Result<ProviderImportSummary> {
    Err(CaptureError::UnsupportedSchema(
        "Claude Store ingestion was removed; use source-backed ingestion".to_owned(),
    ))
}
