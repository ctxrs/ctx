#[derive(Debug, Clone, Default)]
pub(crate) struct ImportTotals {
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

    pub(crate) fn source_root_file(&self) -> Option<&SourceImportFile> {
        match self {
            Self::SourceRoot { file, .. } => Some(file),
            Self::None | Self::CodexSessionCatalog { .. } | Self::SourceImportFiles { .. } => None,
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

#[derive(Debug, Clone)]
pub(crate) struct PlannedImportSource {
    pub(crate) source: SourceInfo,
    pub(crate) stats: SourceStats,
    pub(crate) preinventory: SourcePreinventory,
}
