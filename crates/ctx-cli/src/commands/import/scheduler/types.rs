pub(crate) const IMPORT_SLICE_MAX_UNITS: usize = 64;
pub(crate) const IMPORT_SLICE_TARGET_BYTES: u64 = 8 * 1024 * 1024;
pub(crate) const IMPORT_PENDING_REPORT_LIMIT: usize = 256;
const MEBIBYTE: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportExecutionPolicy {
    Drain,
    Interactive,
    Daemon,
}

impl ImportExecutionPolicy {
    pub(crate) fn fresh_slice_limit(self) -> Option<usize> {
        match self {
            Self::Interactive | Self::Daemon => Some(1),
            Self::Drain => None,
        }
    }

    pub(crate) fn recovery_slice_limit(self) -> Option<usize> {
        match self {
            Self::Drain => None,
            Self::Interactive | Self::Daemon => Some(1),
        }
    }

    pub(crate) fn disk_io_pacer(self) -> ctx_history_capture::DiskIoPacer {
        let (bytes_per_second, burst_bytes) = self.disk_io_limits();
        ctx_history_capture::DiskIoPacer::new(bytes_per_second, burst_bytes)
    }

    pub(crate) const fn disk_io_limits(self) -> (u64, u64) {
        match self {
            Self::Drain => (64 * MEBIBYTE, 8 * MEBIBYTE),
            Self::Interactive => (32 * MEBIBYTE, 4 * MEBIBYTE),
            Self::Daemon => (8 * MEBIBYTE, MEBIBYTE),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ImportPlan {
    pub(crate) sources: Vec<PlannedImportSource>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fresh_units: usize,
    pub(crate) recovery_units: usize,
}

#[derive(Debug)]
pub(crate) struct ImportSlice {
    pub(crate) sources: Vec<SelectedImportSource>,
    pub(crate) retirements: Vec<ProviderFilePublicationRetirementWork>,
    pub(crate) units: usize,
    pub(crate) bytes: u64,
}

pub(crate) struct ExecutableImportSlice {
    pub(crate) slice: ImportSlice,
    pub(crate) bulk_guard: EventSearchBulkGuard,
    pub(crate) validation_failures: Vec<SourceValidationFailure>,
}

#[derive(Debug)]
pub(crate) struct SourceValidationFailure {
    pub(crate) source_index: usize,
    pub(crate) stats: SourceStats,
    pub(crate) error: anyhow::Error,
}

#[derive(Debug, Default)]
pub(crate) struct ImportExecutionState {
    observed_preinventories: Vec<Option<SourcePreinventory>>,
    attempted_work: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ImportExecutionResult {
    pub(crate) selected_units: usize,
    pub(crate) completed_units: usize,
    pub(crate) deferred_units: usize,
    durable_progress: bool,
    admission_stopped: bool,
}

impl ImportExecutionResult {
    pub(crate) fn add_slice(
        &mut self,
        selected_units: usize,
        completed_units: usize,
        deferred_units: usize,
        maintenance_progress: bool,
    ) {
        self.selected_units = self.selected_units.saturating_add(selected_units);
        self.completed_units = self.completed_units.saturating_add(completed_units);
        self.deferred_units = self.deferred_units.saturating_add(deferred_units);
        self.durable_progress |= completed_units > 0 || maintenance_progress;
    }

    pub(crate) fn made_durable_progress(&self) -> bool {
        // Outer drain loops use this signal for admission. A committed group can
        // still require a quiet stop while WAL maintenance catches up.
        self.durable_progress && !self.admission_stopped
    }

    pub(crate) fn stop_admission(&mut self) {
        self.admission_stopped = true;
    }
}

impl ImportSlice {
    fn empty() -> Self {
        Self {
            sources: Vec::new(),
            retirements: Vec::new(),
            units: 0,
            bytes: 0,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.units == 0
    }
}

#[derive(Debug)]
pub(crate) struct SelectedImportSource {
    pub(crate) source_index: usize,
    pub(crate) preinventory: SourcePreinventory,
    pub(crate) work: SelectedImportWork,
    pub(crate) stats: SourceStats,
    attempts_persisted: bool,
}

#[derive(Debug)]
pub(crate) enum SelectedImportWork {
    Catalog(Vec<CatalogImportWork>),
    SourceFiles(Vec<SourceImportFileWork>),
}

impl SelectedImportWork {
    pub(crate) fn unit_count(&self) -> usize {
        match self {
            Self::Catalog(work) => work.len(),
            Self::SourceFiles(work) => work.len(),
        }
    }

    pub(crate) fn is_fresh_new_group(&self) -> bool {
        match self {
            Self::Catalog(work) => {
                !work.is_empty()
                    && work.iter().all(|candidate| {
                        candidate.reason == ImportPendingReason::FreshNew
                            && candidate.session.provider
                                == ctx_history_core::CaptureProvider::Codex
                            && candidate.session.source_format == CODEX_SESSION_SOURCE_FORMAT
                    })
            }
            Self::SourceFiles(work) => {
                !work.is_empty()
                    && work.iter().all(|candidate| {
                        candidate.reason == ImportPendingReason::FreshNew
                            && candidate.file.provider == ctx_history_core::CaptureProvider::Pi
                            && candidate.file.source_format == PI_SESSION_SOURCE_FORMAT
                    })
            }
        }
    }
}

impl ImportExecutionState {
    pub(crate) fn for_plan(plan: &ImportPlan) -> Self {
        Self {
            observed_preinventories: vec![None; plan.sources.len()],
            attempted_work: BTreeSet::new(),
        }
    }

    pub(crate) fn begin_new_pass(&mut self) {
        self.attempted_work.clear();
    }

    pub(crate) fn rebase_for_plan(
        &self,
        old_plan: &ImportPlan,
        new_plan: &ImportPlan,
        invalidated_paths: &BTreeSet<PathBuf>,
    ) -> Self {
        let mut rebased = Self::for_plan(new_plan);
        rebased.attempted_work = self.attempted_work.clone();
        for (new_index, new_source) in new_plan.sources.iter().enumerate() {
            if invalidated_paths.contains(&new_source.source.path) {
                continue;
            }
            let Some(old_index) = old_plan
                .sources
                .iter()
                .position(|old_source| old_source.source == new_source.source)
            else {
                continue;
            };
            rebased.observed_preinventories[new_index] = self
                .observed_preinventories
                .get(old_index)
                .cloned()
                .flatten();
        }
        rebased
    }

    pub(crate) fn record_retirement_attempt(
        &mut self,
        work: &ProviderFilePublicationRetirementWork,
    ) {
        self.attempted_work.insert(retirement_work_identity(work));
    }

    pub(crate) fn record_source_attempt(&mut self, work: &SelectedImportWork) {
        match work {
            SelectedImportWork::Catalog(work) => {
                self.attempted_work
                    .extend(work.iter().map(catalog_work_identity));
            }
            SelectedImportWork::SourceFiles(work) => {
                self.attempted_work
                    .extend(work.iter().map(source_file_work_identity));
            }
        }
    }

    pub(crate) fn record_source_outcome(
        &mut self,
        source_index: usize,
        work: &SelectedImportWork,
        post_import_preinventory: Option<SourcePreinventory>,
    ) {
        // Native source-file batches reobserve the complete source state after import.
        // Cache only that post-import observation: advancing the old generation alone
        // would pair fresh generation metadata with stale file metadata.
        if !matches!(work, SelectedImportWork::SourceFiles(_)) {
            return;
        }
        self.observed_preinventories[source_index] = post_import_preinventory;
    }

    fn has_attempted(&self, identity: &str) -> bool {
        self.attempted_work.contains(identity)
    }

    fn mark_validation_skip(&mut self, identity: String) {
        self.attempted_work.insert(identity);
    }
}

impl SelectedImportSource {
    pub(crate) fn persist_attempt_started(&self, store: &Store) -> Result<usize> {
        if self.attempts_persisted {
            return Ok(self.work.unit_count());
        }
        match &self.work {
            SelectedImportWork::Catalog(selected) => {
                let SourcePreinventory::CodexSessionCatalog {
                    inventory_generation,
                    ..
                } = &self.preinventory
                else {
                    return Ok(0);
                };
                selected.iter().try_fold(0usize, |persisted, work| {
                    Ok(persisted.saturating_add(persist_catalog_attempt_started(
                        store,
                        work,
                        *inventory_generation,
                    )?))
                })
            }
            SelectedImportWork::SourceFiles(selected) => {
                let inventory_generation = match &self.preinventory {
                    SourcePreinventory::SourceRoot {
                        inventory_generation,
                        ..
                    }
                    | SourcePreinventory::SourceImportFiles {
                        inventory_generation,
                        ..
                    } => *inventory_generation,
                    SourcePreinventory::CodexSessionCatalog { .. } | SourcePreinventory::None => {
                        return Ok(0)
                    }
                };
                selected.iter().try_fold(0usize, |persisted, work| {
                    Ok(
                        persisted.saturating_add(persist_source_file_attempt_started(
                            store,
                            work,
                            inventory_generation,
                        )?),
                    )
                })
            }
        }
    }
}

fn persist_catalog_attempt_started(
    store: &Store,
    work: &CatalogImportWork,
    inventory_generation: u64,
) -> Result<usize> {
    let state = store.catalog_source_index_state(
        work.session.provider,
        &work.session.source_root,
        &work.session.source_path,
    )?;
    store
        .record_observed_catalog_source_import_result(
            work.session.provider,
            ctx_history_store::CatalogSourceIndexUpdate {
                source_root: &work.session.source_root,
                source_path: &work.session.source_path,
                file_size_bytes: work.session.file_size_bytes,
                file_modified_at_ms: work.session.file_modified_at_ms,
                import_revision: work.session.import_revision,
                inventory_generation,
                file_sha256: state
                    .as_ref()
                    .and_then(|state| state.last_imported_file_sha256.as_deref()),
                event_count: state
                    .as_ref()
                    .and_then(|state| state.last_imported_event_count),
                indexed_at_ms: ctx_history_core::utc_now().timestamp_millis(),
            },
            &work.session.metadata,
            CatalogIndexedStatus::Pending,
            None,
        )
        .map_err(Into::into)
}

fn persist_source_file_attempt_started(
    store: &Store,
    work: &SourceImportFileWork,
    inventory_generation: u64,
) -> Result<usize> {
    store
        .record_source_import_file_result(
            work.file.provider,
            ctx_history_store::SourceImportFileIndexUpdate {
                source_root: &work.file.source_root,
                source_path: &work.file.source_path,
                file_size_bytes: work.file.file_size_bytes,
                file_modified_at_ms: work.file.file_modified_at_ms,
                import_revision: work.file.import_revision,
                inventory_generation,
                metadata: &work.file.metadata,
                indexed_at_ms: ctx_history_core::utc_now().timestamp_millis(),
            },
            CatalogIndexedStatus::Pending,
            None,
        )
        .map_err(Into::into)
}
