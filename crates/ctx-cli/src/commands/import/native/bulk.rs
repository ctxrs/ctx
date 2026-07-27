use std::path::Path;

use anyhow::Result;

use ctx_history_capture::{
    CaptureWorkLimit, CodexSessionImportProgressCallback, ImportProfile, ProviderImportSummary,
};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::commands::import::SourcePreinventory;
use crate::provider_sources::SourceInfo;

use super::NativeSourceRun;

#[allow(clippy::too_many_arguments)]
pub(super) fn import_one_source_inner_at_path(
    store: &mut Store,
    source: &SourceInfo,
    input_path: &Path,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
    preinventory: &SourcePreinventory,
    capture_work_limit: CaptureWorkLimit,
    inventory_observation_token: Option<String>,
    import_profile: &ImportProfile,
) -> Result<ProviderImportSummary> {
    // One source import can contain hundreds or thousands of independently
    // committed NativePath groups. Keep FTS merge suppression active across
    // that whole bounded source operation. Provider groups retain their own
    // event/FTS/cursor transactions and source revalidation, while nested bulk
    // guards become in-memory depth counts instead of repeated durable
    // maintenance handoffs. Store epochs bind every nested publication to the
    // one live outer lock, so this applies uniformly to every provider.
    let codex_catalog_noop = !full_rescan
        && source.provider == CaptureProvider::Codex
        && source.source_format == "codex_session_jsonl_tree"
        && store
            .list_pending_catalog_sessions(
                CaptureProvider::Codex,
                &source.path.display().to_string(),
            )?
            .is_empty();
    let bulk_guard = (!codex_catalog_noop)
        .then(|| store.begin_event_search_bulk_mode())
        .transpose()?;
    let import_result = NativeSourceRun::new(
        store,
        source,
        progress,
        full_rescan,
        preinventory,
        capture_work_limit,
        inventory_observation_token,
        import_profile,
    )
    .run(input_path);
    let finish_result = bulk_guard
        .as_ref()
        .map(|guard| store.finish_event_search_bulk_mode(guard))
        .unwrap_or(Ok(()));
    let summary = match (import_result, finish_result) {
        (Ok(summary), Ok(())) => summary,
        (_, Err(error)) => return Err(error.into()),
        (Err(error), Ok(())) => return Err(error),
    };
    Ok(summary)
}
