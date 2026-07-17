const PENDING_REASON_REPAIR_BATCH_ROWS: usize = 512;
const PENDING_REASON_REPAIR_BATCH_BYTES: usize = 512 * 1024;
const PENDING_REASON_REPAIR_SQLITE_TIME: std::time::Duration = std::time::Duration::from_millis(25);
const PROVIDER_SESSION_REPAIR_BATCH_ROWS: usize = 128;
const PROVIDER_SESSION_REPAIR_BATCH_BYTES: usize = 512 * 1024;
const PROVIDER_SESSION_REPAIR_SQLITE_TIME: std::time::Duration =
    std::time::Duration::from_millis(25);

#[cfg(test)]
thread_local! {
    static INJECTED_IMPORT_MAINTENANCE_PROGRESS_STEPS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn inject_import_maintenance_progress_steps(steps: usize) {
    INJECTED_IMPORT_MAINTENANCE_PROGRESS_STEPS.with(|remaining| remaining.set(steps));
}

#[cfg(test)]
fn take_injected_import_maintenance_progress_step() -> bool {
    INJECTED_IMPORT_MAINTENANCE_PROGRESS_STEPS.with(|remaining| {
        let steps = remaining.get();
        remaining.set(steps.saturating_sub(1));
        steps > 0
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportMaintenancePendingReason {
    MaintenanceWriter,
    EventSearchBulkLock,
    WalCheckpoint {
        log_frames: i64,
        checkpointed_frames: i64,
    },
}

impl ImportMaintenancePendingReason {
    pub(crate) fn diagnostic(self) -> String {
        match self {
            Self::MaintenanceWriter => {
                "import maintenance is waiting for another database writer".to_owned()
            }
            Self::EventSearchBulkLock => {
                "search-index maintenance is waiting for another bulk importer".to_owned()
            }
            Self::WalCheckpoint {
                log_frames,
                checkpointed_frames,
            } => format!(
                "search-index maintenance is waiting for a WAL reader ({log_frames} log frames, {checkpointed_frames} checkpointed)"
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportMaintenanceStep {
    Complete,
    Progress,
    Pending(ImportMaintenancePendingReason),
}

impl ImportMaintenanceStep {
    pub(crate) fn made_durable_progress(self) -> bool {
        self == Self::Progress
    }

    pub(crate) fn pending_reason(self) -> Option<ImportMaintenancePendingReason> {
        match self {
            Self::Pending(reason) => Some(reason),
            Self::Complete | Self::Progress => None,
        }
    }
}

pub(crate) fn repair_import_maintenance(store: &Store) -> Result<ImportMaintenanceStep> {
    #[cfg(test)]
    if take_injected_import_maintenance_progress_step() {
        return Ok(ImportMaintenanceStep::Progress);
    }

    let (provider_session_rows, _, provider_sessions_complete) = match store
        .repair_provider_session_duplicates(
            PROVIDER_SESSION_REPAIR_BATCH_ROWS,
            PROVIDER_SESSION_REPAIR_BATCH_BYTES,
            PROVIDER_SESSION_REPAIR_SQLITE_TIME,
        ) {
        Ok(progress) => progress,
        Err(error) if maintenance_writer_blocked(&error) => {
            return Ok(ImportMaintenanceStep::Pending(
                ImportMaintenancePendingReason::MaintenanceWriter,
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if provider_session_rows > 0 {
        return Ok(ImportMaintenanceStep::Progress);
    }
    if !provider_sessions_complete {
        return Err(anyhow::Error::new(CaptureError::SystemInvariant(
            "provider-session repair made no progress",
        )));
    }

    let progress = match store.repair_import_pending_reasons(
        PENDING_REASON_REPAIR_BATCH_ROWS,
        PENDING_REASON_REPAIR_BATCH_BYTES,
        PENDING_REASON_REPAIR_SQLITE_TIME,
    ) {
        Ok(progress) => progress,
        Err(error) if maintenance_writer_blocked(&error) => {
            return Ok(ImportMaintenanceStep::Pending(
                ImportMaintenancePendingReason::MaintenanceWriter,
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if progress.visited_rows > 0 {
        return Ok(ImportMaintenanceStep::Progress);
    }
    if !progress.complete {
        return Err(anyhow::Error::new(CaptureError::SystemInvariant(
            "import pending-reason repair made no progress",
        )));
    }
    if store.has_pending_provider_file_publications()?
        || store.event_search_bulk_maintenance_outcome()?.is_complete()
    {
        return Ok(ImportMaintenanceStep::Complete);
    }

    match store.advance_event_search_bulk_maintenance() {
        Ok(EventSearchBulkMaintenanceOutcome::Complete) => Ok(ImportMaintenanceStep::Complete),
        Ok(EventSearchBulkMaintenanceOutcome::Pending) => Ok(ImportMaintenanceStep::Progress),
        Err(StoreError::BulkSearchImportBusy) => Ok(ImportMaintenanceStep::Pending(
            ImportMaintenancePendingReason::EventSearchBulkLock,
        )),
        Err(StoreError::WalCheckpointBusy {
            log_frames,
            checkpointed_frames,
        }) => Ok(ImportMaintenanceStep::Pending(
            ImportMaintenancePendingReason::WalCheckpoint {
                log_frames,
                checkpointed_frames,
            },
        )),
        Err(error) => Err(error.into()),
    }
}

fn maintenance_writer_blocked(error: &StoreError) -> bool {
    match error {
        StoreError::ImportPendingWorkRepairBusy => true,
        StoreError::Sql(rusqlite::Error::SqliteFailure(error, _)) => matches!(
            error.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        ),
        _ => false,
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

    pub(crate) fn remove_source_failure(&mut self, stats: &SourceStats) {
        self.source_files = self.source_files.saturating_sub(stats.files);
        self.source_bytes = self.source_bytes.saturating_sub(stats.bytes);
        self.failed_sources = self.failed_sources.saturating_sub(1);
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ImportSourceIdentity {
    provider: String,
    source_format: String,
    source_path: PathBuf,
}

impl ImportSourceIdentity {
    pub(crate) fn new(source: &SourceInfo) -> Self {
        Self {
            provider: source.provider.as_str().to_owned(),
            source_format: source.source_format.to_owned(),
            source_path: source.path.clone(),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct ImportInventoryFailures {
    failures: BTreeMap<ImportSourceIdentity, ImportSourceFailure>,
}

#[derive(Debug, Default)]
pub(crate) struct ImportInventoryFailureChanges {
    pub(crate) removed: Vec<ImportSourceFailure>,
    pub(crate) added: Vec<ImportSourceFailure>,
    pub(crate) newly_failed: Vec<ImportSourceFailure>,
}

impl ImportInventoryFailures {
    pub(crate) fn new(failures: Vec<ImportSourceFailure>) -> Self {
        Self {
            failures: failures
                .into_iter()
                .map(|failure| (ImportSourceIdentity::new(&failure.source), failure))
                .collect(),
        }
    }

    pub(crate) fn reconcile(
        &mut self,
        successful_sources: &[PlannedImportSource],
        failures: Vec<ImportSourceFailure>,
    ) -> ImportInventoryFailureChanges {
        let mut changes = ImportInventoryFailureChanges::default();
        for source in successful_sources {
            if let Some(failure) = self
                .failures
                .remove(&ImportSourceIdentity::new(&source.source))
            {
                changes.removed.push(failure);
            }
        }
        for failure in failures {
            let identity = ImportSourceIdentity::new(&failure.source);
            let was_failed = self.failures.contains_key(&identity);
            if let Some(previous) = self.failures.insert(identity, failure.clone()) {
                changes.removed.push(previous);
            }
            changes.added.push(failure.clone());
            if !was_failed {
                changes.newly_failed.push(failure);
            }
        }
        changes
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.failures.is_empty()
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &ImportSourceFailure> {
        self.failures.values()
    }

    pub(crate) fn pending_counts(
        &self,
        store: &Store,
        plan: &ImportPlan,
    ) -> Result<(usize, usize)> {
        let planned = plan
            .sources
            .iter()
            .map(|source| ImportSourceIdentity::new(&source.source))
            .collect::<BTreeSet<_>>();
        let mut pending = (0usize, 0usize);
        for failure in self
            .failures
            .iter()
            .filter(|(identity, _)| !planned.contains(*identity))
            .map(|(_, failure)| failure)
        {
            let counts = failed_inventory_pending_counts(store, std::slice::from_ref(failure))?;
            if counts == (0, 0) {
                pending.0 = capped_pending_add(pending.0, 1);
            } else {
                pending.0 = capped_pending_add(pending.0, counts.0);
                pending.1 = capped_pending_add(pending.1, counts.1);
            }
        }
        Ok(pending)
    }
}

pub(crate) fn stable_reinventory_sources(
    plan: &ImportPlan,
    failures: &ImportInventoryFailures,
) -> Vec<SourceInfo> {
    let mut sources = BTreeMap::new();
    for source in plan.sources.iter().map(|planned| &planned.source) {
        sources.insert(ImportSourceIdentity::new(source), source.clone());
    }
    for failure in failures.values() {
        sources.insert(
            ImportSourceIdentity::new(&failure.source),
            failure.source.clone(),
        );
    }
    sources.into_values().collect()
}

#[derive(Debug, Default)]
pub(crate) struct NativeSourceReports {
    sources: BTreeMap<ImportSourceIdentity, NativeSourceReport>,
}

#[derive(Debug)]
struct NativeSourceReport {
    source: SourceInfo,
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
    fn source_report(&mut self, source: &SourceInfo) -> &mut NativeSourceReport {
        self.sources
            .entry(ImportSourceIdentity::new(source))
            .or_insert_with(|| NativeSourceReport {
                source: source.clone(),
                summary: ProviderImportSummary::default(),
                stats: SourceStats::default(),
                failure: None,
                reportable: false,
            })
    }

    pub(crate) fn record_outcome(
        &mut self,
        source: &SourceInfo,
        summary: &ProviderImportSummary,
        completed_stats: SourceStats,
        no_op_stats: Option<SourceStats>,
    ) {
        let report = self.source_report(source);
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
        source: &SourceInfo,
        stats: SourceStats,
        error: &anyhow::Error,
    ) {
        let report = self.source_report(source);
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

    pub(crate) fn record_inventory_failure(&mut self, failure: &ImportSourceFailure) {
        let report = self.source_report(&failure.source);
        if !report.reportable {
            report.stats = failure.stats;
        }
        report.failure = Some(NativeSourceFailure {
            error: failure.error.clone(),
            failure_type: failure.failure_type,
            rejected: failure.rejected_summary.is_some(),
        });
        if let Some(summary) = failure.rejected_summary.as_ref() {
            report.summary.merge_from(summary.clone());
        }
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

    pub(crate) fn append_json(self, _plan: &ImportPlan, imported_sources: &mut Vec<Value>) {
        for report in self
            .sources
            .into_values()
            .filter(|report| report.reportable)
        {
            let source = &report.source;
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
