use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, HistoryRecord};
use ctx_history_store::{ColdStoreBuild, ColdStoreBuildReceipt};

use super::{
    discover_codex_catalog_sources, import_codex_native_prompt_history,
    import_codex_native_session_files_with_catalog, import_codex_native_session_root_with_catalog,
};
use crate::{
    provider::codex::catalog::ensure_catalog_source_bound, CaptureError, CaptureWorkLimit,
    CatalogSummary, CodexHistoryImportOptions, CodexSessionImportOptions, ImportProfile,
    ProviderImportSummary, Result,
};

#[derive(Debug, Clone)]
pub struct CodexColdPromptHistoryOptions {
    pub source_path: PathBuf,
    pub history_record: Option<HistoryRecord>,
}

#[derive(Debug, Clone)]
pub struct CodexColdStoreOptions {
    pub source_path: PathBuf,
    pub target_store_path: PathBuf,
    pub machine_id: String,
    pub imported_at: DateTime<Utc>,
    pub history_record: Option<HistoryRecord>,
    pub max_source_files: Option<usize>,
    pub max_total_bytes: Option<u64>,
    pub prompt_history: Option<CodexColdPromptHistoryOptions>,
}

#[derive(Debug)]
// This public one-shot outcome predates the cold-store fast path. Boxing either
// installed field would change its public shape for a size paid only on return.
#[allow(clippy::large_enum_variant)]
pub enum CodexColdStoreOutcome {
    /// Every existing regular Store stays on the ordinary incremental path.
    OrdinaryStoreRequired,
    Installed {
        catalog_summary: CatalogSummary,
        summary: ProviderImportSummary,
        prompt_history_summary: Option<ProviderImportSummary>,
        store: ColdStoreBuildReceipt,
    },
}

/// Builds a fresh Core-only Codex Store through the current canonical
/// NativePath root lifecycle in one adjacent final-format SQLite generation.
#[doc(hidden)]
pub fn build_codex_cold_store(options: CodexColdStoreOptions) -> Result<CodexColdStoreOutcome> {
    build_codex_cold_store_with_hooks(options, |_| Ok(()), |_| Ok(()))
}

pub(crate) fn build_codex_cold_store_with_hooks<AfterCapture, BeforeInstall>(
    options: CodexColdStoreOptions,
    after_capture: AfterCapture,
    before_install: BeforeInstall,
) -> Result<CodexColdStoreOutcome>
where
    AfterCapture: FnOnce(&ctx_history_store::Store) -> Result<()>,
    BeforeInstall: FnOnce(&Path) -> Result<()>,
{
    build_codex_cold_store_with_begin_and_hooks(
        options,
        |target| ColdStoreBuild::begin(target),
        || Ok(()),
        after_capture,
        before_install,
    )
}

#[cfg(test)]
pub(crate) fn build_codex_cold_store_with_begin_hook<Begin, BeforeCapture>(
    options: CodexColdStoreOptions,
    begin: Begin,
    before_capture: BeforeCapture,
) -> Result<CodexColdStoreOutcome>
where
    Begin:
        FnOnce(&Path) -> std::result::Result<Option<ColdStoreBuild>, ctx_history_store::StoreError>,
    BeforeCapture: FnOnce() -> Result<()>,
{
    build_codex_cold_store_with_begin_and_hooks(
        options,
        begin,
        before_capture,
        |_| Ok(()),
        |_| Ok(()),
    )
}

fn build_codex_cold_store_with_begin_and_hooks<Begin, BeforeCapture, AfterCapture, BeforeInstall>(
    options: CodexColdStoreOptions,
    begin: Begin,
    before_capture: BeforeCapture,
    after_capture: AfterCapture,
    before_install: BeforeInstall,
) -> Result<CodexColdStoreOutcome>
where
    Begin:
        FnOnce(&Path) -> std::result::Result<Option<ColdStoreBuild>, ctx_history_store::StoreError>,
    BeforeCapture: FnOnce() -> Result<()>,
    AfterCapture: FnOnce(&ctx_history_store::Store) -> Result<()>,
    BeforeInstall: FnOnce(&Path) -> Result<()>,
{
    let Some(mut builder) = begin(&options.target_store_path)? else {
        return Ok(CodexColdStoreOutcome::OrdinaryStoreRequired);
    };
    before_capture()?;
    let history_record_id = options.history_record.as_ref().map(|record| record.id);
    if let Some(record) = &options.history_record {
        builder.store()?.upsert_record(record)?;
    }
    if let Some(record) = options
        .prompt_history
        .as_ref()
        .and_then(|prompt| prompt.history_record.as_ref())
    {
        builder.store()?.upsert_record(record)?;
    }
    let import_options = CodexSessionImportOptions {
        machine_id: options.machine_id.clone(),
        source_path: Some(options.source_path.clone()),
        imported_at: options.imported_at,
        history_record_id,
        max_session_files: options.max_source_files,
        max_total_bytes: options.max_total_bytes,
        progress: None,
        capture_work_limit: CaptureWorkLimit::Drain,
        inventory_observation_token: None,
        import_profile: ImportProfile::CoreOnly,
    };
    let (catalog_summary, summary) = if options.source_path.is_dir() {
        import_codex_native_session_root_with_catalog(
            &options.source_path,
            builder.store_mut()?,
            import_options,
        )?
    } else {
        import_codex_native_session_files_with_catalog(
            vec![options.source_path.clone()],
            builder.store_mut()?,
            import_options,
        )?
    };
    let prompt_history_summary = options
        .prompt_history
        .as_ref()
        .map(|prompt| {
            import_codex_native_prompt_history(
                &prompt.source_path,
                builder.store_mut()?,
                CodexHistoryImportOptions {
                    machine_id: options.machine_id.clone(),
                    source_path: Some(prompt.source_path.clone()),
                    imported_at: options.imported_at,
                    history_record_id: prompt.history_record.as_ref().map(|record| record.id),
                    capture_work_limit: CaptureWorkLimit::Drain,
                    inventory_observation_token: None,
                    import_profile: ImportProfile::CoreOnly,
                },
            )
        })
        .transpose()?;
    after_capture(builder.store()?)?;
    builder.activate_projection_journal(ctx_pro_host_protocol::PROTOCOL_FINGERPRINT)?;

    let root = options.source_path.display().to_string();
    let sessions = builder.store()?.list_catalog_sessions_for_source_bounded(
        CaptureProvider::Codex,
        &root,
        crate::provider::codex::catalog::CODEX_CATALOG_MAX_SOURCES,
    )?;
    ensure_catalog_source_bound(sessions.len())?;
    let discovery = discover_codex_catalog_sources(&sessions);
    if !discovery.rejections.is_empty() || discovery.ineligible != 0 {
        return Err(CaptureError::InvalidPayload(
            "Codex cold Store final source inventory is incomplete".to_owned(),
        ));
    }
    let counts = builder.counts()?;
    let mut combined = summary.clone();
    if let Some(prompt_summary) = &prompt_history_summary {
        combined.merge_from(prompt_summary.clone());
    }
    let expected_sources = discovery
        .sources
        .len()
        .saturating_add(usize::from(prompt_history_summary.is_some()));
    if counts.sessions > combined.imported_sessions
        || counts.events > combined.imported_events
        || counts.session_edges > combined.imported_edges
        || counts.sources > expected_sources
        || counts.capture_sources > expected_sources
        || counts.batches > expected_sources
    {
        return Err(CaptureError::SystemInvariant(
            "Codex cold Store authority exceeds the admitted canonical capture summary",
        ));
    }
    let store = builder.finish_with_pre_install(|stage_path| {
        before_install(stage_path).map_err(|error| {
            ctx_history_store::StoreError::ColdStoreValidation(format!(
                "Codex pre-install fence failed: {error}"
            ))
        })?;
        Ok(())
    })?;
    Ok(CodexColdStoreOutcome::Installed {
        catalog_summary,
        summary,
        prompt_history_summary,
        store,
    })
}
