const PENDING_REASON_REPAIR_BATCH_ROWS: usize = 512;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ImportMaintenanceProgress {
    pub(crate) processed_rows: usize,
    pub(crate) complete: bool,
}

pub(crate) fn repair_import_maintenance(
    store: &Store,
    policy: ImportExecutionPolicy,
) -> Result<ImportMaintenanceProgress> {
    let mut aggregate = ImportMaintenanceProgress::default();
    loop {
        let progress = store.repair_import_pending_reasons(PENDING_REASON_REPAIR_BATCH_ROWS)?;
        let mut processed_rows = progress.processed_rows;
        let bulk_complete = if progress.complete
            && !store.has_pending_provider_file_publications()?
            && !store.event_search_bulk_maintenance_outcome()?.is_complete()
        {
            processed_rows = processed_rows.saturating_add(1);
            store.advance_event_search_bulk_maintenance()?.is_complete()
        } else {
            true
        };
        aggregate.processed_rows = aggregate.processed_rows.saturating_add(processed_rows);
        aggregate.complete = progress.complete && bulk_complete;
        if aggregate.complete || policy != ImportExecutionPolicy::Drain {
            return Ok(aggregate);
        }
        if processed_rows == 0 {
            return Err(anyhow::Error::new(CaptureError::SystemInvariant(
                "import maintenance made no progress",
            )));
        }
    }
}

pub(crate) fn provider_publication_blocks_attempt(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<StoreError>(),
            Some(StoreError::ProviderFileReplacementBusy { .. })
        )
    })
}

pub(crate) fn import_work_progress_message(
    class: ImportWorkClass,
    provider: CaptureProvider,
) -> (&'static str, String) {
    match class {
        ImportWorkClass::Fresh => (
            "indexing",
            format!("indexing new/changed {} history", provider.as_str()),
        ),
        ImportWorkClass::Recovery => (
            "repairing",
            format!("repairing prior {} history", provider.as_str()),
        ),
    }
}

pub(crate) fn import_work_progress_done(
    class: ImportWorkClass,
    source: &SourceInfo,
) -> (&'static str, String) {
    match class {
        ImportWorkClass::Fresh => (
            "indexing",
            format!(
                "Indexed new/changed {} history.",
                source_provider_label(source)
            ),
        ),
        ImportWorkClass::Recovery => (
            "repairing",
            format!("Repaired prior {} history.", source_provider_label(source)),
        ),
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ImportTotals {
    pub(crate) durable_progress: bool,
    pub(crate) fresh_units_processed: usize,
    pub(crate) recovery_units_processed: usize,
    pub(crate) fresh_units_pending: usize,
    pub(crate) recovery_units_pending: usize,
    pub(crate) source_files: usize,
    pub(crate) source_bytes: u64,
    pub(crate) imported_sources: usize,
    pub(crate) sources_completed_with_rejections: usize,
    pub(crate) failed_sources: usize,
    pub(crate) imported_sessions: usize,
    pub(crate) imported_events: usize,
    pub(crate) imported_edges: usize,
    pub(crate) skipped_sessions: usize,
    pub(crate) skipped_events: usize,
    pub(crate) skipped_edges: usize,
    pub(crate) skipped: usize,
    pub(crate) failed: usize,
}

#[derive(Debug)]
pub(crate) struct ImportReport {
    pub(crate) resume: bool,
    pub(crate) totals: ImportTotals,
    pub(crate) inventory: InventoryTotals,
    pub(crate) catalog: CatalogTotals,
    pub(crate) catalog_sources: Vec<Value>,
    pub(crate) sources: Vec<Value>,
}

impl ImportReport {
    pub(crate) fn empty(resume: bool) -> Self {
        Self {
            resume,
            totals: ImportTotals::default(),
            inventory: InventoryTotals::default(),
            catalog: CatalogTotals::default(),
            catalog_sources: Vec::new(),
            sources: Vec::new(),
        }
    }

    pub(crate) fn resume_mode(&self) -> &'static str {
        resume_mode_name(self.resume)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ImportRunOptions {
    pub(crate) progress: ProgressArg,
    pub(crate) json: bool,
    pub(crate) print_human: bool,
    pub(crate) allow_empty_sources: bool,
    pub(crate) include_history_source_plugins: bool,
    pub(crate) operation: &'static str,
}

pub(crate) fn resume_mode_name(resume: bool) -> &'static str {
    if resume {
        "idempotent_rescan"
    } else {
        "normal_scan"
    }
}

impl ImportTotals {
    pub(crate) fn add(&mut self, summary: &ProviderImportSummary, stats: &SourceStats) {
        self.source_files += stats.files;
        self.source_bytes = self.source_bytes.saturating_add(stats.bytes);
        self.imported_sources += 1;
        self.sources_completed_with_rejections += usize::from(summary.failed > 0);
        self.imported_sessions += summary.imported_sessions;
        self.imported_events += summary.imported_events;
        self.imported_edges += summary.imported_edges;
        self.skipped_sessions += summary.skipped_sessions;
        self.skipped_events += summary.skipped_events;
        self.skipped_edges += summary.skipped_edges;
        self.skipped += summary.skipped;
        self.failed += summary.failed;
    }

    pub(crate) fn add_source_failure(&mut self, stats: &SourceStats) {
        self.source_files += stats.files;
        self.source_bytes = self.source_bytes.saturating_add(stats.bytes);
        self.failed_sources += 1;
    }

    pub(crate) fn add_rejected_source(
        &mut self,
        summary: &ProviderImportSummary,
        stats: &SourceStats,
    ) {
        self.add_source_failure(stats);
        self.skipped_sessions = self
            .skipped_sessions
            .saturating_add(summary.skipped_sessions);
        self.skipped_events = self.skipped_events.saturating_add(summary.skipped_events);
        self.skipped_edges = self.skipped_edges.saturating_add(summary.skipped_edges);
        self.skipped = self.skipped.saturating_add(summary.skipped);
        self.failed = self.failed.saturating_add(summary.failed);
    }
}

#[derive(Debug, Default)]
pub(crate) struct NativeSourceReports {
    sources: BTreeMap<usize, NativeSourceReport>,
}

#[derive(Debug, Default)]
struct NativeSourceReport {
    summary: ProviderImportSummary,
    stats: SourceStats,
    failure: Option<NativeSourceFailure>,
    reportable: bool,
}

#[derive(Debug)]
struct NativeSourceFailure {
    error: String,
    failure_type: ImportFailureType,
    rejected: bool,
}

impl NativeSourceReports {
    pub(crate) fn record_outcome(
        &mut self,
        source_index: usize,
        summary: &ProviderImportSummary,
        completed_stats: SourceStats,
        no_op_stats: Option<SourceStats>,
    ) {
        let report = self.sources.entry(source_index).or_default();
        report.summary.merge_from(summary.clone());
        report.stats.files = report.stats.files.saturating_add(completed_stats.files);
        report.stats.bytes = report.stats.bytes.saturating_add(completed_stats.bytes);
        report.stats.change_token = report.stats.change_token.or(completed_stats.change_token);
        let reportable_no_op = no_op_stats.is_some();
        if report.stats.files == 0 {
            if let Some(stats) = no_op_stats {
                report.stats = stats;
            }
        }
        report.reportable |= completed_stats.files > 0
            || reportable_no_op
            || summary != &ProviderImportSummary::default();
    }

    pub(crate) fn record_failure(
        &mut self,
        source_index: usize,
        stats: SourceStats,
        error: &anyhow::Error,
    ) {
        let report = self.sources.entry(source_index).or_default();
        report.stats.files = report.stats.files.saturating_add(stats.files);
        report.stats.bytes = report.stats.bytes.saturating_add(stats.bytes);
        report.stats.change_token = report.stats.change_token.or(stats.change_token);
        let rejected_summary = rejected_source_summary(error);
        if let Some(summary) = rejected_summary.as_ref() {
            report.summary.merge_from(summary.clone());
        }
        report.failure.get_or_insert_with(|| NativeSourceFailure {
            error: error_summary(error),
            failure_type: import_failure_type(error),
            rejected: rejected_summary.is_some(),
        });
        report.reportable = true;
    }

    pub(crate) fn apply_totals(&self, totals: &mut ImportTotals) {
        for report in self.sources.values().filter(|report| report.reportable) {
            let rejected_without_content = report.summary.failed > 0
                && !provider_summary_has_imported_content(&report.summary);
            let failed_without_content =
                report.failure.is_some() && !provider_summary_has_imported_content(&report.summary);
            if rejected_without_content || failed_without_content {
                if report.summary.failed > 0 {
                    totals.add_rejected_source(&report.summary, &report.stats);
                } else {
                    totals.add_source_failure(&report.stats);
                }
                continue;
            }

            totals.add(&report.summary, &report.stats);
            if report
                .failure
                .as_ref()
                .is_some_and(|failure| !failure.rejected)
            {
                totals.failed_sources = totals.failed_sources.saturating_add(1);
            }
        }
    }

    fn append_json(self, plan: &ImportPlan, imported_sources: &mut Vec<Value>) {
        for (source_index, report) in self
            .sources
            .into_iter()
            .filter(|(_, report)| report.reportable)
        {
            let source = &plan.sources[source_index].source;
            let rejected_without_content = report.summary.failed > 0
                && !provider_summary_has_imported_content(&report.summary);
            let failed_without_content =
                report.failure.is_some() && !provider_summary_has_imported_content(&report.summary);
            if rejected_without_content || failed_without_content {
                let failure = report.failure.unwrap_or_else(|| NativeSourceFailure {
                    error: format!(
                        "provider import reported {} failure(s)",
                        report.summary.failed
                    ),
                    failure_type: ImportFailureType::RecordRejection,
                    rejected: true,
                });
                imported_sources.push(source_failure_json(&ImportSourceFailure {
                    source: source.clone(),
                    stats: report.stats,
                    error: failure.error,
                    failure_type: failure.failure_type,
                    rejected_summary: (report.summary.failed > 0).then_some(report.summary),
                }));
                continue;
            }

            let mut value = source_import_json(source, &report.stats, &report.summary);
            if let Some(failure) = report.failure.filter(|failure| !failure.rejected) {
                value["status"] = json!("completed_with_source_failure");
                value["failure_scope"] = json!("source");
                value["failure_type"] = json!(failure.failure_type.as_str());
                value["error"] = json!(source_error_reason(source, &failure.error));
            }
            imported_sources.push(value);
        }
    }
}

pub(crate) fn provider_summary_has_imported_content(summary: &ProviderImportSummary) -> bool {
    summary.has_accepted_content()
}

pub(crate) fn provider_summary_import_status(
    summary: &ProviderImportSummary,
) -> CatalogIndexedStatus {
    if summary.failed == 0 {
        CatalogIndexedStatus::Indexed
    } else if provider_summary_has_imported_content(summary) {
        CatalogIndexedStatus::CompletedWithRejections
    } else {
        CatalogIndexedStatus::Rejected
    }
}

pub(crate) fn history_record_exists(store: &Store, record_id: Uuid) -> Result<bool> {
    match store.get_record(record_id) {
        Ok(_) => Ok(true),
        Err(StoreError::NotFound(_)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn cleanup_rejected_history_record(
    store: &Store,
    record_id: Uuid,
    existed_before_import: bool,
) -> Result<()> {
    let deleted = store.delete_orphan_record(record_id)?;
    if !deleted && !existed_before_import && history_record_exists(store, record_id)? {
        return Err(anyhow::Error::new(CaptureError::SystemInvariant(
            "rejected import left content attached to its history record",
        )));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct RejectedSourceError {
    message: String,
    summary: ProviderImportSummary,
}

impl std::fmt::Display for RejectedSourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RejectedSourceError {}

pub(crate) fn rejected_source_error(
    message: String,
    summary: &ProviderImportSummary,
) -> anyhow::Error {
    anyhow::Error::new(RejectedSourceError {
        message,
        summary: summary.clone(),
    })
}

pub(crate) fn rejected_source_summary(error: &anyhow::Error) -> Option<ProviderImportSummary> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<RejectedSourceError>())
        .map(|error| error.summary.clone())
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CatalogTotals {
    pub(crate) sources: usize,
    pub(crate) source_files: usize,
    pub(crate) source_bytes: u64,
    pub(crate) cataloged_sessions: usize,
    pub(crate) cached_sessions: usize,
    pub(crate) parsed_sessions: usize,
    pub(crate) skipped_sessions: usize,
    pub(crate) failed_sessions: usize,
}

impl CatalogTotals {
    pub(crate) fn add(&mut self, summary: &CatalogSummary) {
        self.sources += 1;
        self.source_files += summary.source_files;
        self.source_bytes = self.source_bytes.saturating_add(summary.source_bytes);
        self.cataloged_sessions += summary.cataloged_sessions;
        self.cached_sessions += summary.cached_sessions;
        self.parsed_sessions += summary.parsed_sessions;
        self.skipped_sessions += summary.skipped_sessions;
        self.failed_sessions += summary.failed_sessions;
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct InventoryTotals {
    pub(crate) sources: usize,
    pub(crate) source_files: usize,
    pub(crate) source_bytes: u64,
    pub(crate) codex_catalog_sources: usize,
    pub(crate) codex_catalog_sessions: usize,
    pub(crate) source_import_files: usize,
}

#[derive(Debug, Clone, Default)]
pub(crate) enum SourcePreinventory {
    #[default]
    None,
    CodexSessionCatalog {
        summary: CatalogSummary,
        inventory_generation: u64,
    },
    SourceImportFiles {
        files: Vec<SourceImportFile>,
        inventory_generation: u64,
    },
    SourceRoot {
        file: SourceImportFile,
        inventory_generation: u64,
    },
}

impl SourcePreinventory {
    pub(crate) fn codex_session_catalog(&self) -> Option<&CatalogSummary> {
        match self {
            Self::CodexSessionCatalog { summary, .. } => Some(summary),
            Self::None | Self::SourceImportFiles { .. } | Self::SourceRoot { .. } => None,
        }
    }

    pub(crate) fn source_import_files(&self) -> Option<&[SourceImportFile]> {
        match self {
            Self::SourceImportFiles { files, .. } => Some(files),
            Self::None | Self::CodexSessionCatalog { .. } | Self::SourceRoot { .. } => None,
        }
    }

    pub(crate) fn source_root_observation(&self) -> Option<(&SourceImportFile, u64)> {
        match self {
            Self::SourceRoot {
                file,
                inventory_generation,
            } => Some((file, *inventory_generation)),
            Self::None | Self::CodexSessionCatalog { .. } | Self::SourceImportFiles { .. } => None,
        }
    }

    pub(crate) fn inventory_generation(&self) -> Option<u64> {
        match self {
            Self::None => None,
            Self::CodexSessionCatalog {
                inventory_generation,
                ..
            }
            | Self::SourceImportFiles {
                inventory_generation,
                ..
            }
            | Self::SourceRoot {
                inventory_generation,
                ..
            } => Some(*inventory_generation),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SourceStats {
    pub(crate) files: usize,
    pub(crate) bytes: u64,
    pub(crate) change_token: Option<[u8; 32]>,
}

pub(crate) fn failed_inventory_pending_counts(
    store: &Store,
    failures: &[ImportSourceFailure],
) -> Result<(usize, usize)> {
    let mut fresh = 0usize;
    let mut recovery = 0usize;
    for failure in failures {
        let Some(source_root) = failure.source.path.to_str() else {
            continue;
        };
        let counts =
            bounded_unplanned_root_work_counts(store, failure.source.provider, source_root)?;
        fresh = capped_pending_add(fresh, counts.0);
        recovery = capped_pending_add(recovery, counts.1);
    }
    Ok((fresh, recovery))
}

pub(crate) fn capped_pending_add(left: usize, right: usize) -> usize {
    left.saturating_add(right).min(IMPORT_PENDING_REPORT_LIMIT)
}

#[derive(Debug, Clone)]
pub(crate) struct PlannedImportSource {
    pub(crate) source: SourceInfo,
    pub(crate) stats: SourceStats,
    pub(crate) preinventory: SourcePreinventory,
}
