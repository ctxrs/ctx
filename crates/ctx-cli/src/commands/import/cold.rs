use std::{path::Path, time::Instant};

use anyhow::Result;

use ctx_history_capture::{
    build_codex_cold_store, CodexColdPromptHistoryOptions, CodexColdStoreOptions,
    CodexColdStoreOutcome, ProviderAdapterContext, ProviderImportSummary,
};
use ctx_history_core::CaptureProvider;

use crate::analytics::{ProviderRefreshSourceMode, ProviderRefreshTrigger};
use crate::progress::ProgressReporter;
use crate::provider_sources::SourceInfo;
use crate::ImportArgs;

use super::catalog::{import_record_for_source, source_stats};
use super::provider_refresh::{
    ProviderRefreshCollector, ProviderRefreshResourceObservation, ProviderRefreshRuntimeFacts,
};
use super::report::source_import_json;
use super::{
    CatalogTotals, ImportFailureScope, ImportFailureType, ImportReport, ImportRunOptions,
    ImportTotals, InventoryTotals,
};

pub(super) struct CodexColdSeed {
    pub(super) report: ImportReport,
    pub(super) consumed_sources: Vec<SourceInfo>,
}

impl CodexColdSeed {
    pub(super) fn remove_consumed_from(&self, requests: &mut Vec<SourceInfo>) {
        requests.retain(|request| {
            !self.consumed_sources.iter().any(|consumed| {
                consumed.provider == request.provider
                    && consumed.source_format == request.source_format
                    && consumed.path == request.path
            })
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn try_codex_cold_cli_import(
    args: &ImportArgs,
    requests: &[SourceInfo],
    db_path: &Path,
    provider_refreshes: &mut ProviderRefreshCollector,
    refresh_trigger: ProviderRefreshTrigger,
    options: &ImportRunOptions,
) -> Result<Option<CodexColdSeed>> {
    let eligible_command = (args.all
        || args
            .provider
            .is_some_and(|provider| provider.capture_provider() == CaptureProvider::Codex))
        && args.format.is_none()
        && !args.resume
        && !args.reset_cursor
        && args.history_source.is_none();
    if !eligible_command {
        return Ok(None);
    }

    let mut session_sources = requests
        .iter()
        .filter(|source| {
            source.provider == CaptureProvider::Codex
                && source.source_format == "codex_session_jsonl_tree"
                && source.path.is_dir()
        })
        .map(|source| source_stats(&source.path).map(|stats| (source, stats)))
        .collect::<Result<Vec<_>>>()?;
    session_sources.sort_by(|(left_source, left_stats), (right_source, right_stats)| {
        (left_stats.bytes, left_stats.files, &left_source.path).cmp(&(
            right_stats.bytes,
            right_stats.files,
            &right_source.path,
        ))
    });
    let Some((source, session_stats)) = session_sources.pop() else {
        return Ok(None);
    };
    let mut prompt_sources = requests.iter().filter(|source| {
        source.provider == CaptureProvider::Codex
            && source.source_format == "codex_history_jsonl"
            && source.path.is_file()
    });
    let prompt_source = prompt_sources.next();
    if prompt_sources.next().is_some() {
        return Ok(None);
    }
    let consumed_sources = std::iter::once(source)
        .chain(prompt_source)
        .cloned()
        .collect::<Vec<_>>();
    let mut stats_by_source = vec![session_stats];
    if let Some(prompt) = prompt_source {
        stats_by_source.push(source_stats(&prompt.path)?);
    }
    let total_bytes = stats_by_source
        .iter()
        .map(|stats| stats.bytes)
        .fold(0_u64, u64::saturating_add);
    let context = ProviderAdapterContext::default();
    let imported_at = context.imported_at;
    let progress = ProgressReporter::new(
        options.progress,
        options.json,
        options.operation,
        total_bytes,
    );
    progress.message("indexing", CaptureProvider::Codex.as_str());
    let resources = ProviderRefreshResourceObservation::begin();
    let started = Instant::now();
    let outcome = build_codex_cold_store(CodexColdStoreOptions {
        source_path: source.path.clone(),
        target_store_path: db_path.to_path_buf(),
        machine_id: context.machine_id,
        imported_at,
        history_record: Some(import_record_for_source(source)),
        max_source_files: None,
        max_total_bytes: None,
        prompt_history: prompt_source.map(|prompt| CodexColdPromptHistoryOptions {
            source_path: prompt.path.clone(),
            history_record: Some(import_record_for_source(prompt)),
        }),
    })?;
    let CodexColdStoreOutcome::Installed {
        catalog_summary,
        summary,
        prompt_history_summary,
        store,
    } = outcome
    else {
        progress.finish_line();
        return Ok(None);
    };
    let timings = store.timings;
    progress.notice(format!(
        "cold Store phases: schema={:.3}s core={:.3}s journal={:.3}s \
         indexes+fts={:.3}s validation={:.3}s (database={:.3}s search={:.3}s) \
         install={:.3}s",
        timings.schema_prepare.as_secs_f64(),
        timings.core_load.as_secs_f64(),
        timings.projection_journal_build.as_secs_f64(),
        timings.index_and_fts_build.as_secs_f64(),
        timings.validation.as_secs_f64(),
        timings.database_validation.as_secs_f64(),
        timings.search_validation.as_secs_f64(),
        timings.durable_install.as_secs_f64(),
    ));
    progress.done("indexing", CaptureProvider::Codex.as_str(), total_bytes);

    let elapsed = started.elapsed();
    let source_mode = if args.path.is_some() {
        ProviderRefreshSourceMode::ExplicitPath
    } else {
        ProviderRefreshSourceMode::Discovered
    };
    let summary_for = |request: &SourceInfo| -> Option<&ProviderImportSummary> {
        match request.source_format {
            "codex_session_jsonl_tree" => Some(&summary),
            "codex_history_jsonl" => prompt_history_summary.as_ref(),
            _ => None,
        }
    };
    let mut totals = ImportTotals::default();
    for (request, stats) in consumed_sources.iter().zip(&stats_by_source) {
        let request_summary = summary_for(request).ok_or_else(|| {
            anyhow::anyhow!("cold Codex result omitted one admitted source summary")
        })?;
        if request_summary.failed > 0 && !request_summary.has_accepted_content() {
            provider_refreshes.record_failure_with_facts(
                CaptureProvider::Codex,
                refresh_trigger,
                source_mode,
                stats,
                Some(request_summary),
                ProviderRefreshRuntimeFacts::observed_failure(
                    std::time::Duration::ZERO,
                    ImportFailureScope::Source,
                    ImportFailureType::RecordRejection,
                ),
            );
            totals.add_rejected_source(request_summary, stats);
        } else {
            provider_refreshes.record_success_with_facts(
                CaptureProvider::Codex,
                refresh_trigger,
                source_mode,
                request_summary,
                stats,
                ProviderRefreshRuntimeFacts::observed_success(
                    std::time::Duration::ZERO,
                    request_summary,
                ),
            );
            totals.add(request_summary, stats);
        }
    }
    provider_refreshes.record_combined_runtime(
        CaptureProvider::Codex,
        refresh_trigger,
        source_mode,
        elapsed,
        resources,
    );
    let source_reports = consumed_sources
        .iter()
        .zip(&stats_by_source)
        .map(|(request, stats)| {
            summary_for(request)
                .map(|request_summary| source_import_json(request, stats, request_summary))
                .ok_or_else(|| {
                    anyhow::anyhow!("cold Codex result omitted one admitted source summary")
                })
        })
        .collect::<Result<Vec<_>>>()?;

    let mut catalog = CatalogTotals::default();
    catalog.add(&catalog_summary);
    Ok(Some(CodexColdSeed {
        report: ImportReport {
            resume: false,
            totals,
            inventory: InventoryTotals {
                sources: consumed_sources.len(),
                source_files: stats_by_source.iter().map(|stats| stats.files).sum(),
                source_bytes: total_bytes,
                codex_catalog_sources: 1,
                codex_catalog_sessions: catalog_summary.cataloged_sessions,
                source_import_files: 0,
            },
            catalog,
            catalog_sources: Vec::new(),
            sources: source_reports,
        },
        consumed_sources,
    }))
}

#[cfg(test)]
#[path = "cold/tests.rs"]
mod tests;
