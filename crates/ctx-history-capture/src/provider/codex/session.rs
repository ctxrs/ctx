use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

#[cfg(test)]
use crate::captured_batch::jsonl::initial_jsonl_position;
use crate::common::io::{collect_jsonl_paths, ensure_regular_provider_transcript_file};
use crate::provider::providers::native_jsonl::visit_native_jsonl_files;
use crate::{CodexSessionImportOptions, ProviderImportSummary, Result};

pub(super) const CODEX_CAPTURE_REVISION: u32 = 8;
pub(super) const CODEX_POLICY_REVISION: u32 = 3;
const CODEX_RECORD_KIND: &str = "codex-session-jsonl-v1";
const CODEX_MAX_TOOL_CONTEXTS: usize = 24;
const CODEX_MAX_TOOL_CALL_ID_BYTES: usize = 1024;
const CODEX_MAX_TOOL_NAME_BYTES: usize = 512;
const CODEX_MAX_TOOL_PREVIEW_BYTES: usize = 4 * 1024;
const CODEX_HEADER_ANCHOR_DOMAIN: &[u8] = b"ctx-codex-session-meta-anchor-sha256-v1\0";

mod correlation;
mod filter;
mod header;
mod import;
mod projection;
mod resume;
mod selection;
mod source_file;

#[cfg(test)]
use crate::provider::codex::events::CodexToolCallContext;
pub(crate) use filter::contains_bytes;
#[cfg(test)]
pub(crate) use filter::should_parse_codex_session_line;
#[cfg(test)]
pub(crate) use import::join_codex_import_worker;
use import::{import_codex_session_file_batched, import_codex_session_paths_batched};
pub(crate) use selection::apply_codex_session_import_bounds;
use selection::codex_common_source_root;
#[cfg(test)]
pub(crate) use source_file::codex_session_file_conversation_scan;
#[cfg(test)]
use source_file::count_codex_source_file_opens;

pub fn import_codex_session_jsonl(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    import_codex_session_file_batched(path.as_ref(), store, &options, None, true)
}

pub fn import_codex_session_jsonl_tail(
    path: impl AsRef<Path>,
    start_offset: u64,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    if start_offset == 0 {
        return import_codex_session_jsonl(path, store, options);
    }
    ensure_regular_provider_transcript_file(path)?;
    if start_offset > fs::metadata(path)?.len() {
        return Ok(ProviderImportSummary::default());
    }
    import_codex_session_file_batched(path, store, &options, Some(start_offset), true)
}
pub fn import_codex_session_paths(
    paths: Vec<PathBuf>,
    store: &mut Store,
    mut options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    for path in &paths {
        ensure_regular_provider_transcript_file(path)?;
    }
    if options.source_path.is_none() {
        options.source_path = codex_common_source_root(&paths);
    }
    import_codex_session_paths_batched(paths, store, &options, 0)
}

pub fn import_codex_session_tree(
    root: impl AsRef<Path>,
    store: &mut Store,
    mut options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let root = root.as_ref();
    if options.source_path.is_none() {
        options.source_path = Some(root.to_path_buf());
    }
    if options.max_session_files.is_none()
        && options.max_total_bytes.is_none()
        && options.progress.is_none()
    {
        let mut merged = ProviderImportSummary::default();
        visit_native_jsonl_files(root, CaptureProvider::Codex, &mut |path| {
            merged.merge(import_codex_session_file_batched(
                path, store, &options, None, false,
            )?);
            Ok(())
        })?;
        return Ok(merged);
    }
    // Limits select the newest fitting paths, while progress promises exact totals up front.
    let mut paths = Vec::new();
    collect_jsonl_paths(root, &mut paths)?;
    let skipped_by_bounds = apply_codex_session_import_bounds(
        &mut paths,
        options.max_session_files,
        options.max_total_bytes,
    )?;
    import_codex_session_paths_batched(paths, store, &options, skipped_by_bounds)
}

#[cfg(test)]
#[path = "session/tests/mod.rs"]
mod tests;
