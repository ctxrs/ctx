use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use ctx_history_capture::{catalog_codex_session_tree, CaptureError, CodexSessionCatalogOptions};
use ctx_history_core::CaptureProvider;
use ctx_history_store::{
    CatalogImportWork, CatalogIndexedStatus, EventSearchBulkGuard, ImportWorkClass,
    ProviderFilePublicationRetirementWork, SourceImportFileWork, Store,
};

use super::inventory::observe_source_root;
use super::manifest::{
    collect_source_import_files, persist_new_source_import_observation, persisted_import_identity,
};
use super::{
    import_error_scope, ImportFailureScope, PlannedImportSource, SourcePreinventory, SourceStats,
};

pub(crate) const IMPORT_SLICE_MAX_UNITS: usize = 64;
pub(crate) const IMPORT_SLICE_TARGET_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportExecutionPolicy {
    Drain,
    Interactive,
    Daemon,
}

impl ImportExecutionPolicy {
    pub(crate) fn fresh_slice_limit(self) -> Option<usize> {
        match self {
            Self::Daemon => Some(1),
            Self::Drain | Self::Interactive => None,
        }
    }

    pub(crate) fn recovery_slice_limit(self) -> Option<usize> {
        match self {
            Self::Drain => None,
            Self::Interactive | Self::Daemon => Some(1),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ImportPlan {
    pub(crate) sources: Vec<PlannedImportSource>,
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

    #[cfg(test)]
    pub(crate) fn made_durable_progress(&self) -> bool {
        self.durable_progress
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
}

impl ImportExecutionState {
    pub(crate) fn for_plan(plan: &ImportPlan) -> Self {
        Self {
            observed_preinventories: vec![None; plan.sources.len()],
            attempted_work: BTreeSet::new(),
        }
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
        match &self.work {
            SelectedImportWork::Catalog(selected) => {
                let SourcePreinventory::CodexSessionCatalog {
                    inventory_generation,
                    ..
                } = &self.preinventory
                else {
                    return Ok(0);
                };
                let mut persisted = 0usize;
                for work in selected {
                    let state = store.catalog_source_index_state(
                        work.session.provider,
                        &work.session.source_root,
                        &work.session.source_path,
                    )?;
                    let changed = store.record_catalog_source_import_result(
                        work.session.provider,
                        ctx_history_store::CatalogSourceIndexUpdate {
                            source_root: &work.session.source_root,
                            source_path: &work.session.source_path,
                            file_size_bytes: work.session.file_size_bytes,
                            file_modified_at_ms: work.session.file_modified_at_ms,
                            import_revision: work.session.import_revision,
                            inventory_generation: *inventory_generation,
                            file_sha256: state
                                .as_ref()
                                .and_then(|state| state.last_imported_file_sha256.as_deref()),
                            event_count: state
                                .as_ref()
                                .and_then(|state| state.last_imported_event_count),
                            indexed_at_ms: ctx_history_core::utc_now().timestamp_millis(),
                        },
                        CatalogIndexedStatus::Pending,
                        None,
                    )?;
                    persisted = persisted.saturating_add(changed);
                }
                Ok(persisted)
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
                let mut persisted = 0usize;
                for work in selected {
                    let changed = store.record_source_import_file_result(
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
                    )?;
                    persisted = persisted.saturating_add(changed);
                }
                Ok(persisted)
            }
        }
    }
}

impl ImportPlan {
    pub(crate) fn build(store: &Store, sources: Vec<PlannedImportSource>) -> Result<Self> {
        let (fresh_units, recovery_units) = import_work_counts(store, &sources)?;
        Ok(Self {
            sources,
            fresh_units,
            recovery_units,
        })
    }

    #[cfg(test)]
    pub(crate) fn select_slice(
        &self,
        store: &Store,
        class: ImportWorkClass,
        max_units: usize,
    ) -> Result<ImportSlice> {
        let state = ImportExecutionState::for_plan(self);
        self.select_slice_with_state(store, class, max_units, &state, None)
    }

    pub(crate) fn select_slice_for_execution_with_pre_lock_hook(
        &self,
        store: &Store,
        class: ImportWorkClass,
        max_units: usize,
        state: &mut ImportExecutionState,
        before_bulk_lock: impl FnOnce(),
    ) -> Result<Option<ExecutableImportSlice>> {
        let provisional = self.select_slice_with_state(store, class, max_units, state, None)?;
        if provisional.is_empty() {
            return Ok(None);
        }
        before_bulk_lock();
        let bulk_guard = store.begin_event_search_bulk_mode()?;
        match self.revalidate_slice(store, class, max_units, state, &provisional) {
            Ok((slice, validation_failures)) => Ok(Some(ExecutableImportSlice {
                slice,
                bulk_guard,
                validation_failures,
            })),
            Err(error) => {
                if let Err(finish_error) = store.finish_event_search_bulk_mode(&bulk_guard) {
                    return Err(finish_error.into());
                }
                Err(error)
            }
        }
    }

    pub(crate) fn pending_counts(&self, store: &Store) -> Result<(usize, usize)> {
        import_work_counts(store, &self.sources)
    }

    pub(crate) fn pending_count(&self, store: &Store, class: ImportWorkClass) -> Result<usize> {
        let (fresh, recovery) = self.pending_counts(store)?;
        Ok(match class {
            ImportWorkClass::Fresh => fresh,
            ImportWorkClass::Recovery => recovery,
        })
    }

    fn select_slice_with_state(
        &self,
        store: &Store,
        class: ImportWorkClass,
        max_units: usize,
        state: &ImportExecutionState,
        eligible_sources: Option<&BTreeSet<usize>>,
    ) -> Result<ImportSlice> {
        let slice_limit = IMPORT_SLICE_MAX_UNITS.min(max_units);
        if slice_limit == 0 {
            return Ok(ImportSlice::empty());
        }

        let mut candidates = Vec::new();
        let fetch_limit = slice_limit;
        if class == ImportWorkClass::Recovery {
            for work in store.list_provider_file_publication_retirement_work(fetch_limit)? {
                let candidate = ImportCandidate::Retirement(work);
                if !state.has_attempted(&candidate.identity()) {
                    candidates.push(candidate);
                }
                if candidates.len() >= slice_limit {
                    break;
                }
            }
        }
        for (source_index, plan) in self.sources.iter().enumerate() {
            if eligible_sources.is_some_and(|eligible| !eligible.contains(&source_index)) {
                continue;
            }
            let preinventory = state
                .observed_preinventories
                .get(source_index)
                .and_then(Option::as_ref)
                .unwrap_or(&plan.preinventory);
            let Some(work) = list_source_work(store, plan, preinventory, class, fetch_limit)?
            else {
                continue;
            };
            match work {
                SelectedImportWork::Catalog(work) => {
                    candidates.extend(
                        work.into_iter()
                            .filter_map(|work| {
                                let candidate = ImportCandidate::Catalog { source_index, work };
                                (!state.has_attempted(&candidate.identity())).then_some(candidate)
                            })
                            .take(slice_limit),
                    );
                }
                SelectedImportWork::SourceFiles(work) => {
                    candidates.extend(
                        work.into_iter()
                            .filter_map(|work| {
                                let candidate = ImportCandidate::SourceFile { source_index, work };
                                (!state.has_attempted(&candidate.identity())).then_some(candidate)
                            })
                            .take(slice_limit),
                    );
                }
            }
        }
        candidates.sort_by(|left, right| self.compare_candidates(left, right));

        let mut slice = ImportSlice::empty();
        for candidate in candidates {
            if slice.units >= slice_limit {
                break;
            }
            let bytes = candidate.estimated_bytes();
            let exceeds_target =
                slice.units > 0 && slice.bytes.saturating_add(bytes) > IMPORT_SLICE_TARGET_BYTES;
            if exceeds_target {
                break;
            }
            slice.units += 1;
            slice.bytes = slice.bytes.saturating_add(bytes);
            match candidate {
                ImportCandidate::Retirement(work) => slice.retirements.push(work),
                ImportCandidate::Catalog { source_index, work } => push_source_candidate(
                    &mut slice,
                    source_index,
                    self.selected_preinventory(state, source_index),
                    SelectedCandidate::Catalog(work),
                ),
                ImportCandidate::SourceFile { source_index, work } => push_source_candidate(
                    &mut slice,
                    source_index,
                    self.selected_preinventory(state, source_index),
                    SelectedCandidate::SourceFile(work),
                ),
            }
        }
        Ok(slice)
    }

    fn selected_preinventory(
        &self,
        state: &ImportExecutionState,
        source_index: usize,
    ) -> SourcePreinventory {
        state
            .observed_preinventories
            .get(source_index)
            .and_then(Option::as_ref)
            .unwrap_or(&self.sources[source_index].preinventory)
            .clone()
    }

    fn compare_candidates(&self, left: &ImportCandidate, right: &ImportCandidate) -> Ordering {
        let attempt_order = left
            .last_attempt_at_ms()
            .is_some()
            .cmp(&right.last_attempt_at_ms().is_some())
            .then_with(|| left.last_attempt_at_ms().cmp(&right.last_attempt_at_ms()));
        if attempt_order != Ordering::Equal {
            return attempt_order;
        }
        left.stable_identity(&self.sources)
            .cmp(&right.stable_identity(&self.sources))
    }

    fn revalidate_slice(
        &self,
        store: &Store,
        class: ImportWorkClass,
        max_units: usize,
        state: &mut ImportExecutionState,
        provisional: &ImportSlice,
    ) -> Result<(ImportSlice, Vec<SourceValidationFailure>)> {
        let mut eligible_sources = BTreeSet::new();
        let mut validation_failures = Vec::new();
        for selected in &provisional.sources {
            if state.observed_preinventories[selected.source_index].is_none() {
                let plan = &self.sources[selected.source_index];
                match observe_current_preinventory(store, plan) {
                    Ok(preinventory) => {
                        state.observed_preinventories[selected.source_index] = Some(preinventory);
                    }
                    Err(error) if import_error_scope(&error) == ImportFailureScope::Source => {
                        state.record_source_attempt(&selected.work);
                        validation_failures.push(SourceValidationFailure {
                            source_index: selected.source_index,
                            stats: selected.stats,
                            error,
                        });
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            eligible_sources.insert(selected.source_index);
        }
        let mut slice =
            self.select_slice_with_state(store, class, max_units, state, Some(&eligible_sources))?;
        retain_current_file_observations(&mut slice, state);
        Ok((slice, validation_failures))
    }
}

enum ImportCandidate {
    Retirement(ProviderFilePublicationRetirementWork),
    Catalog {
        source_index: usize,
        work: CatalogImportWork,
    },
    SourceFile {
        source_index: usize,
        work: SourceImportFileWork,
    },
}

impl ImportCandidate {
    fn estimated_bytes(&self) -> u64 {
        match self {
            Self::Retirement(work) => work.estimated_bytes,
            Self::Catalog { work, .. } => work.estimated_bytes,
            Self::SourceFile { work, .. } => work.estimated_bytes,
        }
    }

    fn last_attempt_at_ms(&self) -> Option<i64> {
        match self {
            Self::Retirement(work) => Some(work.last_attempt_at_ms),
            Self::Catalog { work, .. } => work.last_attempt_at_ms,
            Self::SourceFile { work, .. } => work.last_attempt_at_ms,
        }
    }

    fn identity(&self) -> String {
        match self {
            Self::Retirement(work) => retirement_work_identity(work),
            Self::Catalog { work, .. } => catalog_work_identity(work),
            Self::SourceFile { work, .. } => source_file_work_identity(work),
        }
    }

    fn stable_identity(&self, sources: &[PlannedImportSource]) -> String {
        match self {
            Self::Retirement(work) => format!(
                "{}\u{0}{}\u{0}{}\u{0}{}",
                work.provider.as_str(),
                work.material_source_format,
                work.material_source_root,
                work.source_path
            ),
            Self::Catalog { source_index, work } => {
                stable_source_work_identity(&sources[*source_index], &work.session.source_path)
            }
            Self::SourceFile { source_index, work } => {
                stable_source_work_identity(&sources[*source_index], &work.file.source_path)
            }
        }
    }
}

enum SelectedCandidate {
    Catalog(CatalogImportWork),
    SourceFile(SourceImportFileWork),
}

fn push_source_candidate(
    slice: &mut ImportSlice,
    source_index: usize,
    preinventory: SourcePreinventory,
    candidate: SelectedCandidate,
) {
    let bytes = match &candidate {
        SelectedCandidate::Catalog(work) => work.estimated_bytes,
        SelectedCandidate::SourceFile(work) => work.estimated_bytes,
    };
    if let Some(selected) = slice
        .sources
        .iter_mut()
        .find(|selected| selected.source_index == source_index)
    {
        match (&mut selected.work, candidate) {
            (SelectedImportWork::Catalog(work), SelectedCandidate::Catalog(candidate)) => {
                work.push(candidate);
            }
            (SelectedImportWork::SourceFiles(work), SelectedCandidate::SourceFile(candidate)) => {
                work.push(candidate);
            }
            _ => unreachable!("one source cannot mix catalog and source-file work"),
        }
        selected.stats.files = selected.stats.files.saturating_add(1);
        selected.stats.bytes = selected.stats.bytes.saturating_add(bytes);
        return;
    }

    let work = match candidate {
        SelectedCandidate::Catalog(work) => SelectedImportWork::Catalog(vec![work]),
        SelectedCandidate::SourceFile(work) => SelectedImportWork::SourceFiles(vec![work]),
    };
    slice.sources.push(SelectedImportSource {
        source_index,
        preinventory,
        work,
        stats: SourceStats {
            files: 1,
            bytes,
            change_token: None,
        },
    });
}

fn retain_current_file_observations(slice: &mut ImportSlice, state: &mut ImportExecutionState) {
    for selected in &mut slice.sources {
        match &mut selected.work {
            SelectedImportWork::Catalog(work) => work.retain(|work| {
                let current = file_observation_is_current(
                    Path::new(&work.session.source_path),
                    work.session.file_size_bytes,
                    work.session.file_modified_at_ms,
                );
                if !current {
                    state.mark_validation_skip(catalog_work_identity(work));
                }
                current
            }),
            SelectedImportWork::SourceFiles(work) => {
                let source_root_was_reobserved = matches!(
                    &selected.preinventory,
                    SourcePreinventory::SourceRoot { .. }
                );
                work.retain(|work| {
                    let current = source_root_was_reobserved
                        || file_observation_is_current(
                            Path::new(&work.file.source_path),
                            work.file.file_size_bytes,
                            work.file.file_modified_at_ms,
                        );
                    if !current {
                        state.mark_validation_skip(source_file_work_identity(work));
                    }
                    current
                });
            }
        }
        selected.stats.files = selected.work.unit_count();
        selected.stats.bytes = selected_work_bytes(&selected.work);
    }
    slice
        .sources
        .retain(|selected| selected.work.unit_count() > 0);
    slice.units = slice
        .retirements
        .len()
        .saturating_add(slice.sources.iter().map(|source| source.stats.files).sum());
    slice.bytes = slice
        .retirements
        .iter()
        .fold(0_u64, |total, work| {
            total.saturating_add(work.estimated_bytes)
        })
        .saturating_add(slice.sources.iter().fold(0_u64, |total, source| {
            total.saturating_add(source.stats.bytes)
        }));
}

fn file_observation_is_current(
    path: &Path,
    expected_bytes: u64,
    expected_modified_ms: i64,
) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return false;
    }
    metadata.len() == expected_bytes
        && metadata.modified().ok().map(super::catalog::system_time_ms)
            == Some(expected_modified_ms)
}

fn selected_work_bytes(work: &SelectedImportWork) -> u64 {
    match work {
        SelectedImportWork::Catalog(work) => work.iter().fold(0_u64, |total, work| {
            total.saturating_add(work.estimated_bytes)
        }),
        SelectedImportWork::SourceFiles(work) => work.iter().fold(0_u64, |total, work| {
            total.saturating_add(work.estimated_bytes)
        }),
    }
}

fn stable_source_work_identity(plan: &PlannedImportSource, source_path: &str) -> String {
    format!(
        "{}\u{0}{}\u{0}{}\u{0}{}",
        plan.source.provider.as_str(),
        plan.source.source_format,
        plan.source.path.display(),
        source_path
    )
}

fn retirement_work_identity(work: &ProviderFilePublicationRetirementWork) -> String {
    format!(
        "retirement\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
        work.provider.as_str(),
        work.material_source_format,
        work.material_source_root,
        work.source_path
    )
}

fn catalog_work_identity(work: &CatalogImportWork) -> String {
    format!(
        "catalog\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
        work.session.provider.as_str(),
        work.session.source_format,
        work.session.source_root,
        work.session.source_path,
        work.session.file_size_bytes,
        work.session.file_modified_at_ms,
        work.session.import_revision,
    )
}

fn source_file_work_identity(work: &SourceImportFileWork) -> String {
    format!(
        "source-file\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
        work.file.provider.as_str(),
        work.file.source_format,
        work.file.source_root,
        work.file.source_path,
        work.file.file_size_bytes,
        work.file.file_modified_at_ms,
        work.file.import_revision,
        work.file.metadata,
    )
}

fn observe_current_preinventory(
    store: &Store,
    plan: &PlannedImportSource,
) -> Result<SourcePreinventory> {
    match &plan.preinventory {
        SourcePreinventory::SourceRoot { .. } => {
            let (_, file) = observe_source_root(&plan.source)?;
            let persisted = persist_new_source_import_observation(
                store,
                &plan.source,
                std::slice::from_ref(&file),
            )?;
            Ok(SourcePreinventory::SourceRoot {
                file,
                inventory_generation: persisted.inventory_generation,
            })
        }
        SourcePreinventory::SourceImportFiles { .. } => {
            let files = collect_source_import_files(&plan.source).with_context(|| {
                format!(
                    "re-inventory import files from {}",
                    plan.source.path.display()
                )
            })?;
            let persisted = persist_new_source_import_observation(store, &plan.source, &files)?;
            Ok(SourcePreinventory::SourceImportFiles {
                files,
                inventory_generation: persisted.inventory_generation,
            })
        }
        SourcePreinventory::CodexSessionCatalog { .. } => {
            let source_root =
                super::catalog::codex_catalog_root_identity(&plan.source.path)?.to_owned();
            let inventory_generation = store
                .allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)?;
            let summary = catalog_codex_session_tree(
                &plan.source.path,
                store,
                CodexSessionCatalogOptions {
                    source_root: Some(plan.source.path.clone()),
                    observation_generation: Some(inventory_generation),
                    ..CodexSessionCatalogOptions::default()
                },
            )
            .map_err(|error| {
                anyhow::Error::new(error).context(format!(
                    "re-inventory Codex sessions from {}",
                    plan.source.path.display()
                ))
            })?;
            if !store.catalog_inventory_generation_is_complete(
                CaptureProvider::Codex,
                &source_root,
                inventory_generation,
            )? {
                return Err(anyhow::Error::new(CaptureError::InventorySuperseded));
            }
            Ok(SourcePreinventory::CodexSessionCatalog {
                summary,
                inventory_generation,
            })
        }
        SourcePreinventory::None => Ok(SourcePreinventory::None),
    }
}

fn list_source_work(
    store: &Store,
    plan: &PlannedImportSource,
    preinventory: &SourcePreinventory,
    class: ImportWorkClass,
    limit: usize,
) -> Result<Option<SelectedImportWork>> {
    match preinventory {
        SourcePreinventory::CodexSessionCatalog { .. } => {
            let source_root = super::catalog::codex_catalog_root_identity(&plan.source.path)?;
            Ok(Some(SelectedImportWork::Catalog(
                store.list_catalog_import_work(plan.source.provider, source_root, class, limit)?,
            )))
        }
        SourcePreinventory::SourceImportFiles { files, .. } => {
            let source_root = files
                .first()
                .map(|file| file.source_root.as_str())
                .map(Ok)
                .unwrap_or_else(|| persisted_import_identity(&plan.source.path, "source root"))?;
            Ok(Some(SelectedImportWork::SourceFiles(
                store.list_source_import_file_work(
                    plan.source.provider,
                    source_root,
                    class,
                    limit,
                )?,
            )))
        }
        SourcePreinventory::SourceRoot { file, .. } => Ok(Some(SelectedImportWork::SourceFiles(
            store.list_source_import_file_work(
                plan.source.provider,
                &file.source_root,
                class,
                limit,
            )?,
        ))),
        SourcePreinventory::None => Ok(None),
    }
}

fn import_work_counts(store: &Store, sources: &[PlannedImportSource]) -> Result<(usize, usize)> {
    let mut fresh = 0usize;
    let mut recovery = store.provider_file_publication_retirement_work_count()?;
    for plan in sources {
        let counts = match &plan.preinventory {
            SourcePreinventory::CodexSessionCatalog { .. } => {
                let source_root = super::catalog::codex_catalog_root_identity(&plan.source.path)?;
                (
                    store.catalog_import_work_count(
                        plan.source.provider,
                        source_root,
                        ImportWorkClass::Fresh,
                    )?,
                    store.catalog_import_work_count(
                        plan.source.provider,
                        source_root,
                        ImportWorkClass::Recovery,
                    )?,
                )
            }
            SourcePreinventory::SourceImportFiles { files, .. } => {
                let Some(first) = files.first() else {
                    continue;
                };
                source_import_counts(store, plan, &first.source_root)?
            }
            SourcePreinventory::SourceRoot { file, .. } => {
                source_import_counts(store, plan, &file.source_root)?
            }
            SourcePreinventory::None => continue,
        };
        fresh = fresh.saturating_add(counts.0);
        recovery = recovery.saturating_add(counts.1);
    }
    Ok((fresh, recovery))
}

fn source_import_counts(
    store: &Store,
    plan: &PlannedImportSource,
    source_root: &str,
) -> Result<(usize, usize)> {
    Ok((
        store.source_import_file_work_count(
            plan.source.provider,
            source_root,
            ImportWorkClass::Fresh,
        )?,
        store.source_import_file_work_count(
            plan.source.provider,
            source_root,
            ImportWorkClass::Recovery,
        )?,
    ))
}

#[cfg(test)]
fn admit_catalog_work(
    slice: &mut ImportSlice,
    work: Vec<CatalogImportWork>,
) -> Vec<CatalogImportWork> {
    admit_work(slice, work, |work| work.estimated_bytes)
}

#[cfg(test)]
fn admit_work<T>(
    slice: &mut ImportSlice,
    work: Vec<T>,
    estimated_bytes: impl Fn(&T) -> u64,
) -> Vec<T> {
    let mut admitted = Vec::new();
    for unit in work {
        let bytes = estimated_bytes(&unit);
        let exceeds_target =
            slice.units > 0 && slice.bytes.saturating_add(bytes) > IMPORT_SLICE_TARGET_BYTES;
        if slice.units >= IMPORT_SLICE_MAX_UNITS || exceeds_target {
            break;
        }
        slice.units += 1;
        slice.bytes = slice.bytes.saturating_add(bytes);
        admitted.push(unit);
        if slice.units >= IMPORT_SLICE_MAX_UNITS || slice.bytes >= IMPORT_SLICE_TARGET_BYTES {
            break;
        }
    }
    admitted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::import::{
        import_totals_json, import_work_progress_done, import_work_progress_message, ImportTotals,
    };
    use crate::provider_sources::explicit_path_source;
    use ctx_history_core::{AgentType, CaptureProvider};
    use ctx_history_store::{
        CatalogIndexedStatus, CatalogSession, ImportPendingReason, SourceImportFile,
        SourceImportFileIndexUpdate,
    };
    use serde_json::json;

    fn catalog_work(path: &str, bytes: u64) -> CatalogImportWork {
        CatalogImportWork {
            session: CatalogSession {
                provider: CaptureProvider::Codex,
                source_format: "codex_session_jsonl_tree".to_owned(),
                source_root: "/sessions".to_owned(),
                source_path: path.to_owned(),
                external_session_id: Some(path.to_owned()),
                parent_external_session_id: None,
                agent_type: AgentType::Primary,
                role_hint: None,
                external_agent_id: None,
                cwd: None,
                session_started_at_ms: None,
                file_size_bytes: bytes,
                file_modified_at_ms: 1,
                import_revision: 1,
                cataloged_at_ms: 1,
                metadata: json!({}),
            },
            reason: ImportPendingReason::FreshNew,
            estimated_bytes: bytes,
            last_attempt_at_ms: None,
        }
    }

    fn recovery_source(
        store: &Store,
        root: &str,
        attempted_at_ms: Option<i64>,
    ) -> (PlannedImportSource, SourceImportFile, u64) {
        let source = explicit_path_source(CaptureProvider::Pi, root.into());
        let file = SourceImportFile {
            provider: CaptureProvider::Pi,
            source_format: source.source_format.to_owned(),
            source_root: root.to_owned(),
            source_path: format!("{root}/session.jsonl"),
            file_size_bytes: 64,
            file_modified_at_ms: 100,
            import_revision: 1,
            observed_at_ms: 100,
            metadata: json!({}),
        };
        let generation = store
            .allocate_source_import_inventory_generation(CaptureProvider::Pi, root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&file))
            .unwrap();
        match attempted_at_ms {
            Some(indexed_at_ms) => {
                store
                    .record_source_import_file_result(
                        CaptureProvider::Pi,
                        SourceImportFileIndexUpdate {
                            source_root: root,
                            source_path: &file.source_path,
                            file_size_bytes: file.file_size_bytes,
                            file_modified_at_ms: file.file_modified_at_ms,
                            import_revision: file.import_revision,
                            inventory_generation: generation,
                            metadata: &file.metadata,
                            indexed_at_ms,
                        },
                        CatalogIndexedStatus::Failed,
                        Some("deterministic failure"),
                    )
                    .unwrap();
            }
            None => {
                store
                    .schedule_source_import_explicit_rescan(CaptureProvider::Pi, root, generation)
                    .unwrap();
            }
        }
        (
            PlannedImportSource {
                source,
                stats: SourceStats::default(),
                preinventory: SourcePreinventory::SourceImportFiles {
                    files: vec![file.clone()],
                    inventory_generation: generation,
                },
            },
            file,
            generation,
        )
    }

    #[test]
    fn slice_admits_one_oversized_unit() {
        let mut slice = ImportSlice::empty();
        let admitted = admit_catalog_work(
            &mut slice,
            vec![
                catalog_work("oversized", IMPORT_SLICE_TARGET_BYTES + 1),
                catalog_work("later", 1),
            ],
        );
        assert_eq!(admitted.len(), 1);
        assert_eq!(slice.units, 1);
        assert_eq!(slice.bytes, IMPORT_SLICE_TARGET_BYTES + 1);
    }

    #[test]
    fn slice_caps_units_and_bytes() {
        let mut slice = ImportSlice::empty();
        let admitted = admit_catalog_work(
            &mut slice,
            (0..100)
                .map(|index| catalog_work(&format!("unit-{index:03}"), 1))
                .collect(),
        );
        assert_eq!(admitted.len(), IMPORT_SLICE_MAX_UNITS);

        let mut slice = ImportSlice::empty();
        let admitted = admit_catalog_work(
            &mut slice,
            vec![
                catalog_work("first", IMPORT_SLICE_TARGET_BYTES - 1),
                catalog_work("second", 2),
            ],
        );
        assert_eq!(admitted.len(), 1);
    }

    #[test]
    fn locked_revalidation_preserves_missing_material_recovery_work() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("state.json");
        std::fs::write(&source_path, b"{}").unwrap();
        let source = explicit_path_source(CaptureProvider::CodeBuddy, source_path);
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let (stats, file) = observe_source_root(&source).unwrap();
        let first =
            persist_new_source_import_observation(&store, &source, std::slice::from_ref(&file))
                .unwrap();
        store
            .record_source_import_file_result(
                source.provider,
                SourceImportFileIndexUpdate {
                    source_root: &file.source_root,
                    source_path: &file.source_path,
                    file_size_bytes: file.file_size_bytes,
                    file_modified_at_ms: file.file_modified_at_ms,
                    import_revision: file.import_revision,
                    inventory_generation: first.inventory_generation,
                    metadata: &file.metadata,
                    indexed_at_ms: 1,
                },
                CatalogIndexedStatus::Indexed,
                None,
            )
            .unwrap();
        let current =
            persist_new_source_import_observation(&store, &source, std::slice::from_ref(&file))
                .unwrap();
        let plan = ImportPlan::build(
            &store,
            vec![PlannedImportSource {
                source,
                stats,
                preinventory: SourcePreinventory::SourceRoot {
                    file,
                    inventory_generation: current.inventory_generation,
                },
            }],
        )
        .unwrap();
        assert_eq!(plan.recovery_units, 1);
        let mut execution_state = ImportExecutionState::for_plan(&plan);

        let executable = plan
            .select_slice_for_execution_with_pre_lock_hook(
                &store,
                ImportWorkClass::Recovery,
                plan.recovery_units,
                &mut execution_state,
                || {},
            )
            .unwrap()
            .unwrap();
        assert_eq!(executable.slice.units, 1);
        let SelectedImportWork::SourceFiles(work) = &executable.slice.sources[0].work else {
            panic!("source-root recovery must select source-file work");
        };
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].reason, ImportPendingReason::MissingMaterial);
        store
            .finish_event_search_bulk_mode(&executable.bulk_guard)
            .unwrap();
    }

    #[test]
    fn execution_policies_bound_only_the_intended_phases() {
        assert_eq!(ImportExecutionPolicy::Drain.fresh_slice_limit(), None);
        assert_eq!(ImportExecutionPolicy::Drain.recovery_slice_limit(), None);
        assert_eq!(ImportExecutionPolicy::Interactive.fresh_slice_limit(), None);
        assert_eq!(
            ImportExecutionPolicy::Interactive.recovery_slice_limit(),
            Some(1)
        );
        assert_eq!(ImportExecutionPolicy::Daemon.fresh_slice_limit(), Some(1));
        assert_eq!(
            ImportExecutionPolicy::Daemon.recovery_slice_limit(),
            Some(1)
        );
    }

    #[test]
    fn progress_and_json_distinguish_fresh_from_recovery() {
        assert_eq!(
            import_work_progress_message(ImportWorkClass::Fresh, CaptureProvider::Pi),
            ("indexing", "indexing new/changed pi history".to_owned())
        );
        assert_eq!(
            import_work_progress_message(ImportWorkClass::Recovery, CaptureProvider::Pi),
            ("repairing", "repairing prior pi history".to_owned())
        );
        let source = explicit_path_source(CaptureProvider::Pi, "/fixture/pi".into());
        assert_eq!(
            import_work_progress_done(ImportWorkClass::Fresh, &source),
            ("indexing", "Indexed new/changed Pi history.".to_owned())
        );
        assert_eq!(
            import_work_progress_done(ImportWorkClass::Recovery, &source),
            ("repairing", "Repaired prior Pi history.".to_owned())
        );

        let totals = ImportTotals {
            fresh_units_processed: 3,
            recovery_units_processed: 2,
            fresh_units_pending: 1,
            recovery_units_pending: 4,
            ..ImportTotals::default()
        };
        let snapshot = import_totals_json(&totals);
        assert_eq!(snapshot["fresh_units_processed"], 3);
        assert_eq!(snapshot["recovery_units_processed"], 2);
        assert_eq!(snapshot["fresh_units_pending"], 1);
        assert_eq!(snapshot["recovery_units_pending"], 4);
    }

    #[test]
    fn fresh_work_is_selected_before_a_global_failed_and_revision_backlog() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let backlog_root = "/fixture/backlog";
        let mut backlog = (0..100)
            .map(|index| SourceImportFile {
                provider: CaptureProvider::Pi,
                source_format: "pi_session_jsonl".to_owned(),
                source_root: backlog_root.to_owned(),
                source_path: format!("{backlog_root}/{index:03}.jsonl"),
                file_size_bytes: 128,
                file_modified_at_ms: 1000 + index,
                import_revision: 1,
                observed_at_ms: 2000,
                metadata: json!({}),
            })
            .collect::<Vec<_>>();
        let generation = store
            .allocate_source_import_inventory_generation(CaptureProvider::Pi, backlog_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, &backlog)
            .unwrap();
        for file in &backlog[..50] {
            store
                .record_source_import_file_result(
                    CaptureProvider::Pi,
                    SourceImportFileIndexUpdate {
                        source_root: backlog_root,
                        source_path: &file.source_path,
                        file_size_bytes: file.file_size_bytes,
                        file_modified_at_ms: file.file_modified_at_ms,
                        import_revision: file.import_revision,
                        inventory_generation: generation,
                        metadata: &file.metadata,
                        indexed_at_ms: 3000,
                    },
                    CatalogIndexedStatus::Failed,
                    Some("retry"),
                )
                .unwrap();
        }
        for file in &backlog[50..] {
            store
                .record_source_import_file_result(
                    CaptureProvider::Pi,
                    SourceImportFileIndexUpdate {
                        source_root: backlog_root,
                        source_path: &file.source_path,
                        file_size_bytes: file.file_size_bytes,
                        file_modified_at_ms: file.file_modified_at_ms,
                        import_revision: file.import_revision,
                        inventory_generation: generation,
                        metadata: &file.metadata,
                        indexed_at_ms: 3000,
                    },
                    CatalogIndexedStatus::Indexed,
                    None,
                )
                .unwrap();
        }
        for file in &mut backlog[50..] {
            file.import_revision = 2;
        }
        let generation = store
            .allocate_source_import_inventory_generation(CaptureProvider::Pi, backlog_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, &backlog)
            .unwrap();

        let fresh_root = "/fixture/fresh";
        let fresh = SourceImportFile {
            provider: CaptureProvider::Pi,
            source_format: "pi_session_jsonl".to_owned(),
            source_root: fresh_root.to_owned(),
            source_path: format!("{fresh_root}/new.jsonl"),
            file_size_bytes: 64,
            file_modified_at_ms: 5000,
            import_revision: 1,
            observed_at_ms: 5000,
            metadata: json!({}),
        };
        let fresh_generation = store
            .allocate_source_import_inventory_generation(CaptureProvider::Pi, fresh_root)
            .unwrap();
        store
            .upsert_source_import_files(fresh_generation, std::slice::from_ref(&fresh))
            .unwrap();

        let plan = ImportPlan::build(
            &store,
            vec![
                PlannedImportSource {
                    source: explicit_path_source(CaptureProvider::Pi, backlog_root.into()),
                    stats: SourceStats::default(),
                    preinventory: SourcePreinventory::SourceImportFiles {
                        files: backlog,
                        inventory_generation: generation,
                    },
                },
                PlannedImportSource {
                    source: explicit_path_source(CaptureProvider::Pi, fresh_root.into()),
                    stats: SourceStats::default(),
                    preinventory: SourcePreinventory::SourceImportFiles {
                        files: vec![fresh],
                        inventory_generation: fresh_generation,
                    },
                },
            ],
        )
        .unwrap();
        assert_eq!(plan.fresh_units, 1);
        assert_eq!(plan.recovery_units, 100);

        let fresh_slice = plan
            .select_slice(&store, ImportWorkClass::Fresh, plan.fresh_units)
            .unwrap();
        assert_eq!(fresh_slice.units, 1);
        assert_eq!(fresh_slice.sources[0].source_index, 1);
        let recovery_slice = plan
            .select_slice(&store, ImportWorkClass::Recovery, plan.recovery_units)
            .unwrap();
        assert_eq!(recovery_slice.units, IMPORT_SLICE_MAX_UNITS);
        assert_eq!(recovery_slice.sources[0].source_index, 0);
    }

    #[test]
    fn global_recovery_prefers_unattempted_work_from_a_later_source() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let (first, _, _) = recovery_source(&store, "/fixture/first", Some(100));
        let (later, _, _) = recovery_source(&store, "/fixture/later", None);
        let plan = ImportPlan::build(&store, vec![first, later]).unwrap();

        let slice = plan
            .select_slice(&store, ImportWorkClass::Recovery, 1)
            .unwrap();
        assert_eq!(slice.units, 1);
        assert_eq!(slice.sources[0].source_index, 1);
    }

    #[test]
    fn failed_recovery_source_rotates_behind_the_older_other_source() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let (first, first_file, first_generation) =
            recovery_source(&store, "/fixture/first", Some(100));
        let (later, _, _) = recovery_source(&store, "/fixture/later", Some(200));
        let plan = ImportPlan::build(&store, vec![first, later]).unwrap();

        let first_slice = plan
            .select_slice(&store, ImportWorkClass::Recovery, 1)
            .unwrap();
        assert_eq!(first_slice.sources[0].source_index, 0);
        store
            .record_source_import_file_result(
                CaptureProvider::Pi,
                SourceImportFileIndexUpdate {
                    source_root: &first_file.source_root,
                    source_path: &first_file.source_path,
                    file_size_bytes: first_file.file_size_bytes,
                    file_modified_at_ms: first_file.file_modified_at_ms,
                    import_revision: first_file.import_revision,
                    inventory_generation: first_generation,
                    metadata: &first_file.metadata,
                    indexed_at_ms: 300,
                },
                CatalogIndexedStatus::Failed,
                Some("still failing"),
            )
            .unwrap();

        let second_slice = plan
            .select_slice(&store, ImportWorkClass::Recovery, 1)
            .unwrap();
        assert_eq!(second_slice.sources[0].source_index, 1);
    }

    #[test]
    fn one_execution_does_not_select_the_same_pending_unit_twice() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let (source, _, _) = recovery_source(&store, "/fixture/deferred", None);
        let plan = ImportPlan::build(&store, vec![source]).unwrap();
        let mut state = ImportExecutionState::for_plan(&plan);

        let first = plan
            .select_slice_with_state(&store, ImportWorkClass::Recovery, 1, &state, None)
            .unwrap();
        assert_eq!(first.units, 1);
        first.sources[0].persist_attempt_started(&store).unwrap();
        state.record_source_attempt(&first.sources[0].work);
        let second = plan
            .select_slice_with_state(&store, ImportWorkClass::Recovery, 1, &state, None)
            .unwrap();
        assert!(second.is_empty());
    }

    #[test]
    fn post_import_cache_replaces_file_metadata_with_the_new_observation() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let (source, file, _) = recovery_source(&store, "/fixture/reobserved", None);
        let plan = ImportPlan::build(&store, vec![source]).unwrap();
        let mut state = ImportExecutionState::for_plan(&plan);
        let selected = plan
            .select_slice_with_state(&store, ImportWorkClass::Recovery, 1, &state, None)
            .unwrap();
        let selected_work = &selected.sources[0].work;

        let mut reobserved = file;
        reobserved.file_size_bytes = 128;
        reobserved.file_modified_at_ms = 200;
        state.record_source_outcome(
            0,
            selected_work,
            Some(SourcePreinventory::SourceImportFiles {
                files: vec![reobserved.clone()],
                inventory_generation: 99,
            }),
        );

        let SourcePreinventory::SourceImportFiles {
            files,
            inventory_generation,
        } = plan.selected_preinventory(&state, 0)
        else {
            panic!("manifest source must cache its post-import observation");
        };
        assert_eq!(inventory_generation, 99);
        assert_eq!(files, vec![reobserved]);
    }

    #[test]
    fn selected_but_still_pending_work_is_not_completed_progress() {
        let mut result = ImportExecutionResult::default();
        result.add_slice(1, 0, 1, false);
        assert_eq!(result.selected_units, 1);
        assert_eq!(result.completed_units, 0);
        assert_eq!(result.deferred_units, 1);
        assert!(!result.made_durable_progress());
    }

    #[test]
    fn attempted_fresh_window_does_not_hide_later_work_in_the_same_source() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let root = "/fixture/fresh-window";
        let source = explicit_path_source(CaptureProvider::Pi, root.into());
        let files = (0..65)
            .map(|index| SourceImportFile {
                provider: CaptureProvider::Pi,
                source_format: source.source_format.to_owned(),
                source_root: root.to_owned(),
                source_path: format!("{root}/{index:03}.jsonl"),
                file_size_bytes: 1,
                file_modified_at_ms: 1,
                import_revision: 1,
                observed_at_ms: 1,
                metadata: json!({}),
            })
            .collect::<Vec<_>>();
        let generation = store
            .allocate_source_import_inventory_generation(CaptureProvider::Pi, root)
            .unwrap();
        store
            .upsert_source_import_files(generation, &files)
            .unwrap();
        let plan = ImportPlan::build(
            &store,
            vec![PlannedImportSource {
                source,
                stats: SourceStats::default(),
                preinventory: SourcePreinventory::SourceImportFiles {
                    files,
                    inventory_generation: generation,
                },
            }],
        )
        .unwrap();
        let mut state = ImportExecutionState::for_plan(&plan);

        let first = plan
            .select_slice_with_state(
                &store,
                ImportWorkClass::Fresh,
                IMPORT_SLICE_MAX_UNITS,
                &state,
                None,
            )
            .unwrap();
        assert_eq!(first.units, IMPORT_SLICE_MAX_UNITS);
        first.sources[0].persist_attempt_started(&store).unwrap();
        state.record_source_attempt(&first.sources[0].work);
        let second = plan
            .select_slice_with_state(&store, ImportWorkClass::Fresh, 1, &state, None)
            .unwrap();
        assert_eq!(second.units, 1);
        let SelectedImportWork::SourceFiles(work) = &second.sources[0].work else {
            panic!("fresh manifest work must use source-file selection");
        };
        assert!(work[0].file.source_path.ends_with("/064.jsonl"));
    }
}
