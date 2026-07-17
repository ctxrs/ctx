use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use ctx_history_capture::{
    catalog_codex_session_paths_page, provider_source_for_persisted_format,
    BoundedSourcePathInventory, CaptureError, CatalogSummary, CodexSessionCatalogOptions,
    ProviderImportSupport, ProviderSourceStatus,
};
use ctx_history_core::{utc_now, CaptureProvider};
use ctx_history_store::{
    CatalogIndexedStatus, ProviderFileInventoryFamily, ProviderFilePublicationInventoryOwner,
    SourceImportFile, SourceImportFileIndexUpdate, Store,
};

use crate::commands::import::catalog::{codex_catalog_root_identity, source_stats, system_time_ms};
use crate::commands::import::manifest::{
    manifest_inventory_path_candidate, observe_source_import_paths_page,
    persist_new_source_import_observation, persist_source_import_files_page,
    persisted_import_identity, source_uses_import_file_manifest,
};
use crate::commands::import::{
    error_summary, import_error_scope, import_failure_type, CatalogTotals, ImportFailureScope,
    ImportSourceFailure, InventoryTotals, PlannedImportSource, SourcePreinventory, SourceStats,
};
use crate::provider_sources::SourceInfo;

#[derive(Debug, Default)]
pub(crate) struct ImportInventory {
    pub(crate) sources: Vec<PlannedImportSource>,
    pub(crate) failures: Vec<ImportSourceFailure>,
    pub(crate) totals: InventoryTotals,
    pub(crate) catalog: CatalogTotals,
    pub(crate) catalog_sources: Vec<Value>,
}

pub(crate) fn inventory_import_sources(
    store: &Store,
    sources: Vec<SourceInfo>,
    full_rescan: bool,
) -> Result<ImportInventory> {
    drain_import_inventory(store, sources, full_rescan, true)
}

fn drain_import_inventory(
    store: &Store,
    sources: Vec<SourceInfo>,
    full_rescan: bool,
    include_publication_owner: bool,
) -> Result<ImportInventory> {
    let mut cursor =
        ImportInventoryCursor::new(store, sources, full_rescan, include_publication_owner)?;
    let mut inventory = ImportInventory::default();
    loop {
        match cursor.advance(store)? {
            ImportInventoryCursorStep::Pending(_) => std::thread::yield_now(),
            ImportInventoryCursorStep::SourceComplete(page) => {
                merge_import_inventory(&mut inventory, page)
            }
            ImportInventoryCursorStep::Complete => return Ok(inventory),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ImportInventorySliceProgress {
    pub(crate) operations: u64,
    pub(crate) path_bytes: u64,
    pub(crate) discovered_files: usize,
}

pub(crate) enum ImportInventoryCursorStep {
    Pending(ImportInventorySliceProgress),
    SourceComplete(ImportInventory),
    Complete,
}

pub(crate) enum DirtySourcePathInventoryOutcome {
    Applied {
        updated_plan: Option<PlannedImportSource>,
    },
    RequiresSourceInventory,
}

pub(crate) fn inventory_dirty_source_path(
    store: &Store,
    source: &SourceInfo,
    changed_path: &Path,
) -> Result<DirtySourcePathInventoryOutcome> {
    if is_incremental_codex_session_tree(source) {
        return inventory_dirty_codex_path(store, source, changed_path);
    }
    if source_uses_import_file_manifest(source) {
        return Ok(DirtySourcePathInventoryOutcome::RequiresSourceInventory);
    }
    inventory_dirty_single_root_path(store, source)
}

fn inventory_dirty_codex_path(
    store: &Store,
    source: &SourceInfo,
    changed_path: &Path,
) -> Result<DirtySourcePathInventoryOutcome> {
    if changed_path == source.path
        || !changed_path.starts_with(&source.path)
        || !changed_path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
    {
        return Ok(DirtySourcePathInventoryOutcome::RequiresSourceInventory);
    }
    let source_root = codex_catalog_root_identity(&source.path)?;
    let Some(inventory_generation) =
        store.current_catalog_inventory_generation(CaptureProvider::Codex, source_root)?
    else {
        return Ok(DirtySourcePathInventoryOutcome::RequiresSourceInventory);
    };
    if !store.catalog_inventory_generation_is_complete(
        CaptureProvider::Codex,
        source_root,
        inventory_generation,
    )? {
        return Ok(DirtySourcePathInventoryOutcome::RequiresSourceInventory);
    }
    ctx_history_capture::pace_current_filesystem_operation(changed_path.as_os_str().len() as u64);
    match fs::symlink_metadata(changed_path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            match catalog_codex_session_paths_page(
                vec![changed_path.to_path_buf()],
                &source.path,
                store,
                inventory_generation,
                CodexSessionCatalogOptions {
                    source_root: Some(source.path.clone()),
                    observation_generation: Some(inventory_generation),
                    ..CodexSessionCatalogOptions::default()
                },
            ) {
                Ok(summary) if summary.failed_sessions == 0 => {}
                Ok(_) => return Ok(DirtySourcePathInventoryOutcome::RequiresSourceInventory),
                Err(CaptureError::InventorySuperseded) => {
                    return Ok(DirtySourcePathInventoryOutcome::RequiresSourceInventory)
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let source_path = changed_path.to_str().ok_or_else(|| {
                anyhow::Error::new(CaptureError::InvalidProviderTranscriptPath {
                    path: changed_path.to_path_buf(),
                    reason: "Codex catalog session path is not valid UTF-8",
                })
            })?;
            store.mark_catalog_inventory_paths_stale(
                CaptureProvider::Codex,
                source_root,
                &[source_path.to_owned()],
                utc_now().timestamp_millis(),
                inventory_generation,
            )?;
        }
        Ok(_) => return Ok(DirtySourcePathInventoryOutcome::RequiresSourceInventory),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("stat changed Codex session {}", changed_path.display()))
        }
    }
    Ok(DirtySourcePathInventoryOutcome::Applied { updated_plan: None })
}

fn inventory_dirty_single_root_path(
    store: &Store,
    source: &SourceInfo,
) -> Result<DirtySourcePathInventoryOutcome> {
    let source_root = persisted_import_identity(&source.path, "source root")?;
    let Some(inventory_generation) =
        store.current_source_import_inventory_generation(source.provider, source_root)?
    else {
        return Ok(DirtySourcePathInventoryOutcome::RequiresSourceInventory);
    };
    if !store.source_import_inventory_generation_is_complete(
        source.provider,
        source_root,
        inventory_generation,
    )? {
        return Ok(DirtySourcePathInventoryOutcome::RequiresSourceInventory);
    }
    ctx_history_capture::pace_current_filesystem_operation(source.path.as_os_str().len() as u64);
    match fs::symlink_metadata(&source.path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            let (stats, file) = observe_source_root(source)?;
            persist_source_import_files_page(
                store,
                inventory_generation,
                std::slice::from_ref(&file),
            )?;
            Ok(DirtySourcePathInventoryOutcome::Applied {
                updated_plan: Some(PlannedImportSource {
                    source: source.clone(),
                    stats,
                    preinventory: SourcePreinventory::SourceRoot {
                        file,
                        inventory_generation,
                    },
                }),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            store.mark_source_import_inventory_paths_stale(
                source.provider,
                source_root,
                &[source_root.to_owned()],
                utc_now().timestamp_millis(),
                inventory_generation,
            )?;
            Ok(DirtySourcePathInventoryOutcome::Applied { updated_plan: None })
        }
        Ok(_) => Ok(DirtySourcePathInventoryOutcome::RequiresSourceInventory),
        Err(error) => Err(error)
            .with_context(|| format!("stat changed import source {}", source.path.display())),
    }
}

pub(crate) struct ImportInventoryCursor {
    sources: Vec<SourceInfo>,
    next_source: usize,
    full_rescan: bool,
    publication_plan: Option<PlannedImportSource>,
    active: Option<SourceInventoryCursor>,
}

impl ImportInventoryCursor {
    pub(crate) fn new(
        store: &Store,
        mut sources: Vec<SourceInfo>,
        full_rescan: bool,
        include_publication_owner: bool,
    ) -> Result<Self> {
        let publication_plan = match store.effective_provider_file_publication_inventory_owner()? {
            Some(owner) => {
                sources.retain(|source| !source_matches_publication_owner(source, &owner));
                include_publication_owner
                    .then(|| publication_owner_plan(owner))
                    .transpose()?
            }
            None => None,
        };
        Ok(Self {
            sources,
            next_source: 0,
            full_rescan,
            publication_plan,
            active: None,
        })
    }

    pub(crate) fn advance(&mut self, store: &Store) -> Result<ImportInventoryCursorStep> {
        if let Some(plan) = self.publication_plan.take() {
            return Ok(ImportInventoryCursorStep::SourceComplete(
                import_inventory_for_plan(plan, None, 1),
            ));
        }
        if self.active.is_none() {
            let Some(source) = self.sources.get(self.next_source).cloned() else {
                return Ok(ImportInventoryCursorStep::Complete);
            };
            self.active = Some(SourceInventoryCursor::new(source, self.full_rescan));
        }
        let source = self.sources[self.next_source].clone();
        let step = self
            .active
            .as_mut()
            .ok_or_else(|| {
                anyhow::Error::new(CaptureError::SystemInvariant(
                    "active source inventory is missing",
                ))
            })?
            .advance(store);
        match step {
            Ok(SourceInventoryStep::Pending(progress)) => {
                Ok(ImportInventoryCursorStep::Pending(progress))
            }
            Ok(SourceInventoryStep::Complete(plan, catalog, source_files)) => {
                self.active = None;
                self.next_source = self.next_source.saturating_add(1);
                Ok(ImportInventoryCursorStep::SourceComplete(
                    import_inventory_for_plan(plan, catalog, source_files),
                ))
            }
            Err(error) if import_error_scope(&error) == ImportFailureScope::Source => {
                self.active = None;
                self.next_source = self.next_source.saturating_add(1);
                let mut page = ImportInventory::default();
                page.totals.sources = 1;
                page.failures.push(ImportSourceFailure {
                    source,
                    stats: SourceStats::default(),
                    error: error_summary(&error),
                    failure_type: import_failure_type(&error),
                    rejected_summary: None,
                });
                Ok(ImportInventoryCursorStep::SourceComplete(page))
            }
            Err(error) => Err(error),
        }
    }
}

fn merge_import_inventory(target: &mut ImportInventory, mut page: ImportInventory) {
    target.sources.append(&mut page.sources);
    target.failures.append(&mut page.failures);
    target.totals.sources = target.totals.sources.saturating_add(page.totals.sources);
    target.totals.source_files = target
        .totals
        .source_files
        .saturating_add(page.totals.source_files);
    target.totals.source_bytes = target
        .totals
        .source_bytes
        .saturating_add(page.totals.source_bytes);
    target.totals.codex_catalog_sources = target
        .totals
        .codex_catalog_sources
        .saturating_add(page.totals.codex_catalog_sources);
    target.totals.codex_catalog_sessions = target
        .totals
        .codex_catalog_sessions
        .saturating_add(page.totals.codex_catalog_sessions);
    target.totals.source_import_files = target
        .totals
        .source_import_files
        .saturating_add(page.totals.source_import_files);
    target.catalog.sources = target.catalog.sources.saturating_add(page.catalog.sources);
    target.catalog.source_files = target
        .catalog
        .source_files
        .saturating_add(page.catalog.source_files);
    target.catalog.source_bytes = target
        .catalog
        .source_bytes
        .saturating_add(page.catalog.source_bytes);
    target.catalog.cataloged_sessions = target
        .catalog
        .cataloged_sessions
        .saturating_add(page.catalog.cataloged_sessions);
    target.catalog.cached_sessions = target
        .catalog
        .cached_sessions
        .saturating_add(page.catalog.cached_sessions);
    target.catalog.parsed_sessions = target
        .catalog
        .parsed_sessions
        .saturating_add(page.catalog.parsed_sessions);
    target.catalog.skipped_sessions = target
        .catalog
        .skipped_sessions
        .saturating_add(page.catalog.skipped_sessions);
    target.catalog.failed_sessions = target
        .catalog
        .failed_sessions
        .saturating_add(page.catalog.failed_sessions);
    target.catalog_sources.append(&mut page.catalog_sources);
}

fn import_inventory_for_plan(
    plan: PlannedImportSource,
    catalog: Option<CatalogSummary>,
    source_import_files: usize,
) -> ImportInventory {
    let mut inventory = ImportInventory::default();
    inventory.totals.sources = 1;
    inventory.totals.source_files = plan.stats.files;
    inventory.totals.source_bytes = plan.stats.bytes;
    inventory.totals.source_import_files = source_import_files;
    if let Some(summary) = catalog {
        inventory.catalog.add(&summary);
        inventory.totals.codex_catalog_sources = 1;
        inventory.totals.codex_catalog_sessions = summary.cataloged_sessions;
        inventory
            .catalog_sources
            .push(catalog_source_json(&plan, &summary));
    }
    inventory.sources.push(plan);
    inventory
}

enum SourceInventoryStep {
    Pending(ImportInventorySliceProgress),
    Complete(PlannedImportSource, Option<CatalogSummary>, usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InventoryFailurePoint {
    RootAfterObservation,
    ManifestAfterObservation,
}

#[cfg(test)]
thread_local! {
    static INVENTORY_FAILURE_ONCE: std::cell::Cell<Option<InventoryFailurePoint>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(test)]
pub(crate) fn inject_inventory_failure_once(point: InventoryFailurePoint) {
    INVENTORY_FAILURE_ONCE.with(|slot| slot.set(Some(point)));
}

#[cfg(test)]
fn maybe_fail_inventory_boundary(point: InventoryFailurePoint) -> Result<()> {
    let fail = INVENTORY_FAILURE_ONCE.with(|slot| {
        if slot.get() == Some(point) {
            slot.set(None);
            true
        } else {
            false
        }
    });
    if fail {
        return Err(anyhow::Error::new(CaptureError::SystemInvariant(
            "injected inventory boundary failure",
        )));
    }
    Ok(())
}

#[cfg(not(test))]
#[inline]
fn maybe_fail_inventory_boundary(_point: InventoryFailurePoint) -> Result<()> {
    Ok(())
}

enum SourceInventoryCursor {
    Codex(CodexInventoryCursor),
    Manifest(ManifestInventoryCursor),
    Root(RootInventoryCursor),
}

impl SourceInventoryCursor {
    fn new(source: SourceInfo, full_rescan: bool) -> Self {
        if is_incremental_codex_session_tree(&source) {
            Self::Codex(CodexInventoryCursor::new(source, full_rescan))
        } else if source_uses_import_file_manifest(&source) {
            Self::Manifest(ManifestInventoryCursor::new(source, full_rescan))
        } else {
            Self::Root(RootInventoryCursor::new(source, full_rescan))
        }
    }

    fn advance(&mut self, store: &Store) -> Result<SourceInventoryStep> {
        match self {
            Self::Codex(cursor) => cursor.advance(store),
            Self::Manifest(cursor) => cursor.advance(store),
            Self::Root(cursor) => cursor.advance(store),
        }
    }
}

enum RootInventoryPhase {
    Inspect,
    Discover,
    Process,
    Complete,
}

struct RootInventoryCursor {
    source: SourceInfo,
    full_rescan: bool,
    phase: RootInventoryPhase,
    paths: Option<BoundedSourcePathInventory>,
    path_cursor: Option<Vec<u8>>,
    stats: super::catalog::BoundedSourceStatsAccumulator,
    completed: Option<PlannedImportSource>,
}

impl RootInventoryCursor {
    fn new(source: SourceInfo, full_rescan: bool) -> Self {
        Self {
            source,
            full_rescan,
            phase: RootInventoryPhase::Inspect,
            paths: None,
            path_cursor: None,
            stats: super::catalog::BoundedSourceStatsAccumulator::default(),
            completed: None,
        }
    }

    fn advance(&mut self, store: &Store) -> Result<SourceInventoryStep> {
        match self.phase {
            RootInventoryPhase::Inspect => {
                ctx_history_capture::pace_current_filesystem_operation(
                    self.source.path.as_os_str().len() as u64,
                );
                let metadata = fs::symlink_metadata(&self.source.path).with_context(|| {
                    format!("stat import source {}", self.source.path.display())
                })?;
                if metadata.file_type().is_dir() {
                    self.paths = Some(BoundedSourcePathInventory::new(&self.source.path));
                    self.phase = RootInventoryPhase::Discover;
                    return Ok(SourceInventoryStep::Pending(ImportInventorySliceProgress {
                        operations: 1,
                        path_bytes: self.source.path.as_os_str().len() as u64,
                        discovered_files: 0,
                    }));
                }
                let plan =
                    inventory_single_root_source(store, self.source.clone(), self.full_rescan)?;
                self.completed = Some(plan);
                self.phase = RootInventoryPhase::Complete;
                Ok(SourceInventoryStep::Pending(ImportInventorySliceProgress {
                    operations: 1,
                    path_bytes: self.source.path.as_os_str().len() as u64,
                    discovered_files: 1,
                }))
            }
            RootInventoryPhase::Discover => {
                let paths = self.paths.as_mut().ok_or_else(|| {
                    anyhow::Error::new(CaptureError::SystemInvariant(
                        "root source traversal is missing",
                    ))
                })?;
                let slice = paths.advance()?;
                if slice.complete {
                    self.phase = RootInventoryPhase::Process;
                }
                Ok(SourceInventoryStep::Pending(ImportInventorySliceProgress {
                    operations: slice.operations,
                    path_bytes: slice.path_bytes,
                    discovered_files: slice.discovered_files,
                }))
            }
            RootInventoryPhase::Process => {
                let paths = self.paths.as_ref().ok_or_else(|| {
                    anyhow::Error::new(CaptureError::SystemInvariant(
                        "root source traversal is missing",
                    ))
                })?;
                let page = paths.paths_page(self.path_cursor.as_deref(), 16)?;
                let mut staged_stats = self.stats.clone();
                staged_stats.observe_paths(&self.source.path, &page.paths)?;
                maybe_fail_inventory_boundary(InventoryFailurePoint::RootAfterObservation)?;
                if page.complete {
                    let stats = staged_stats.clone().finish();
                    let file = source_root_observation_from_stats(&self.source, stats)?;
                    let persisted = persist_new_source_import_observation(
                        store,
                        &self.source,
                        std::slice::from_ref(&file),
                    )?;
                    if self.full_rescan {
                        schedule_pending_source_resume(
                            store,
                            self.source.provider,
                            &file.source_root,
                            persisted.inventory_generation,
                        )?;
                    }
                    self.completed = Some(PlannedImportSource {
                        source: self.source.clone(),
                        stats,
                        preinventory: SourcePreinventory::SourceRoot {
                            file,
                            inventory_generation: persisted.inventory_generation,
                        },
                    });
                    self.stats = staged_stats;
                    self.path_cursor = page.next_cursor;
                    self.phase = RootInventoryPhase::Complete;
                } else {
                    self.stats = staged_stats;
                    self.path_cursor = page.next_cursor;
                }
                Ok(SourceInventoryStep::Pending(ImportInventorySliceProgress {
                    discovered_files: paths.metrics().discovered_files,
                    ..ImportInventorySliceProgress::default()
                }))
            }
            RootInventoryPhase::Complete => self
                .completed
                .take()
                .map(|plan| SourceInventoryStep::Complete(plan, None, 1))
                .ok_or_else(|| {
                    anyhow::Error::new(CaptureError::SystemInvariant(
                        "root source inventory advanced after completion",
                    ))
                }),
        }
    }
}

enum CodexInventoryPhase {
    Discover,
    Process,
    Stale,
    Resume,
    Complete,
}

struct CodexInventoryCursor {
    source: SourceInfo,
    full_rescan: bool,
    paths: BoundedSourcePathInventory,
    phase: CodexInventoryPhase,
    path_cursor: Option<Vec<u8>>,
    stale_cursor: Option<i64>,
    rescan_cursor: Option<i64>,
    inventory_generation: Option<u64>,
    summary: CatalogSummary,
}

impl CodexInventoryCursor {
    fn new(source: SourceInfo, full_rescan: bool) -> Self {
        let paths = BoundedSourcePathInventory::new_jsonl(&source.path);
        Self {
            source,
            full_rescan,
            paths,
            phase: CodexInventoryPhase::Discover,
            path_cursor: None,
            stale_cursor: None,
            rescan_cursor: None,
            inventory_generation: None,
            summary: CatalogSummary::default(),
        }
    }

    fn advance(&mut self, store: &Store) -> Result<SourceInventoryStep> {
        let source_root = persisted_import_identity(&self.source.path, "source root")?.to_owned();
        match self.phase {
            CodexInventoryPhase::Discover => {
                let slice = self.paths.advance()?;
                if slice.complete {
                    self.inventory_generation = Some(store.allocate_catalog_inventory_generation(
                        CaptureProvider::Codex,
                        &source_root,
                    )?);
                    self.phase = CodexInventoryPhase::Process;
                }
                Ok(SourceInventoryStep::Pending(ImportInventorySliceProgress {
                    operations: slice.operations,
                    path_bytes: slice.path_bytes,
                    discovered_files: slice.discovered_files,
                }))
            }
            CodexInventoryPhase::Process => {
                let page = self.paths.paths_page(self.path_cursor.as_deref(), 64)?;
                if !page.paths.is_empty() {
                    let page_summary = catalog_codex_session_paths_page(
                        page.paths,
                        &self.source.path,
                        store,
                        self.generation()?,
                        CodexSessionCatalogOptions {
                            source_root: Some(self.source.path.clone()),
                            observation_generation: Some(self.generation()?),
                            ..CodexSessionCatalogOptions::default()
                        },
                    )?;
                    merge_catalog_summary_bounded(&mut self.summary, page_summary);
                }
                self.path_cursor = page.next_cursor;
                if page.complete {
                    self.phase = CodexInventoryPhase::Stale;
                }
                Ok(SourceInventoryStep::Pending(ImportInventorySliceProgress {
                    discovered_files: self.paths.metrics().discovered_files,
                    ..ImportInventorySliceProgress::default()
                }))
            }
            CodexInventoryPhase::Stale => {
                if !store.catalog_inventory_generation_is_current(
                    CaptureProvider::Codex,
                    &source_root,
                    self.generation()?,
                )? {
                    return Err(CaptureError::InventorySuperseded.into());
                }
                let paths = store.list_catalog_inventory_paths_page(
                    CaptureProvider::Codex,
                    &source_root,
                    self.stale_cursor,
                    64,
                )?;
                let mut missing = Vec::new();
                for (_, source_path) in &paths {
                    if !self.paths.contains_path(Path::new(source_path))? {
                        missing.push(source_path.clone());
                    }
                }
                store.mark_catalog_inventory_paths_stale(
                    CaptureProvider::Codex,
                    &source_root,
                    &missing,
                    utc_now().timestamp_millis(),
                    self.generation()?,
                )?;
                self.stale_cursor = paths.last().map(|(cursor, _)| *cursor);
                if paths.len() < 64 {
                    if !store.complete_catalog_inventory_generation(
                        CaptureProvider::Codex,
                        &source_root,
                        self.generation()?,
                    )? {
                        return Err(CaptureError::InventorySuperseded.into());
                    }
                    self.phase = if self.full_rescan {
                        CodexInventoryPhase::Resume
                    } else {
                        CodexInventoryPhase::Complete
                    };
                }
                Ok(SourceInventoryStep::Pending(
                    ImportInventorySliceProgress::default(),
                ))
            }
            CodexInventoryPhase::Resume => {
                let (_, _, next_cursor, complete) = store
                    .schedule_catalog_source_explicit_rescan_page(
                        CaptureProvider::Codex,
                        &source_root,
                        self.generation()?,
                        self.rescan_cursor,
                        64,
                    )?;
                self.rescan_cursor = next_cursor;
                if complete {
                    self.phase = CodexInventoryPhase::Complete;
                }
                Ok(SourceInventoryStep::Pending(
                    ImportInventorySliceProgress::default(),
                ))
            }
            CodexInventoryPhase::Complete => {
                let summary = std::mem::take(&mut self.summary);
                let plan = PlannedImportSource {
                    source: self.source.clone(),
                    stats: SourceStats {
                        files: summary.source_files,
                        bytes: summary.source_bytes,
                        change_token: None,
                    },
                    preinventory: SourcePreinventory::CodexSessionCatalog {
                        summary: summary.clone(),
                        inventory_generation: self.generation()?,
                    },
                };
                Ok(SourceInventoryStep::Complete(plan, Some(summary), 0))
            }
        }
    }

    fn generation(&self) -> Result<u64> {
        self.inventory_generation.ok_or_else(|| {
            anyhow::Error::new(CaptureError::SystemInvariant(
                "Codex inventory generation is missing",
            ))
        })
    }
}

enum ManifestInventoryPhase {
    Discover,
    Select,
    Process,
    Stale,
    Resume,
    Complete,
}

struct ManifestInventoryCursor {
    source: SourceInfo,
    full_rescan: bool,
    paths: BoundedSourcePathInventory,
    phase: ManifestInventoryPhase,
    path_cursor: Option<Vec<u8>>,
    stale_cursor: Option<i64>,
    rescan_cursor: Option<i64>,
    inventory_generation: Option<u64>,
    source_files: usize,
    source_bytes: u64,
}

impl ManifestInventoryCursor {
    fn new(source: SourceInfo, full_rescan: bool) -> Self {
        let paths = BoundedSourcePathInventory::new(&source.path);
        Self {
            source,
            full_rescan,
            paths,
            phase: ManifestInventoryPhase::Discover,
            path_cursor: None,
            stale_cursor: None,
            rescan_cursor: None,
            inventory_generation: None,
            source_files: 0,
            source_bytes: 0,
        }
    }

    fn advance(&mut self, store: &Store) -> Result<SourceInventoryStep> {
        let source_root = persisted_import_identity(&self.source.path, "source root")?.to_owned();
        match self.phase {
            ManifestInventoryPhase::Discover => {
                let slice = self.paths.advance()?;
                if slice.complete {
                    self.phase = ManifestInventoryPhase::Select;
                }
                Ok(SourceInventoryStep::Pending(ImportInventorySliceProgress {
                    operations: slice.operations,
                    path_bytes: slice.path_bytes,
                    discovered_files: slice.discovered_files,
                }))
            }
            ManifestInventoryPhase::Select => {
                let page = self.paths.paths_page(self.path_cursor.as_deref(), 64)?;
                let candidates = page
                    .paths
                    .iter()
                    .filter_map(|path| {
                        manifest_inventory_path_candidate(&self.source, path)
                            .map(|(group_key, rank)| (group_key, rank, path.clone()))
                    })
                    .collect::<Vec<_>>();
                self.paths.select_path_candidates(&candidates)?;
                if page.complete {
                    let inventory_generation = store.allocate_source_import_inventory_generation(
                        self.source.provider,
                        &source_root,
                    )?;
                    self.path_cursor = None;
                    self.inventory_generation = Some(inventory_generation);
                    self.phase = ManifestInventoryPhase::Process;
                } else {
                    self.path_cursor = page.next_cursor;
                }
                Ok(SourceInventoryStep::Pending(
                    ImportInventorySliceProgress::default(),
                ))
            }
            ManifestInventoryPhase::Process => {
                let page = self
                    .paths
                    .selected_paths_page(self.path_cursor.as_deref(), 64)?;
                let files = if page.paths.is_empty() {
                    Vec::new()
                } else {
                    observe_source_import_paths_page(&self.source, page.paths)?
                };
                let page_files = files.len();
                let page_bytes = files.iter().fold(0_u64, |bytes, file| {
                    bytes.saturating_add(file.file_size_bytes)
                });
                let source_files = self.source_files.saturating_add(page_files);
                let source_bytes = self.source_bytes.saturating_add(page_bytes);
                if page.complete && source_files == 0 {
                    return Err(anyhow::anyhow!(
                        "no importable {} history files found under {}",
                        self.source.provider.as_str(),
                        self.source.path.display()
                    ));
                }
                maybe_fail_inventory_boundary(InventoryFailurePoint::ManifestAfterObservation)?;
                if !files.is_empty() {
                    persist_source_import_files_page(store, self.generation()?, &files)?;
                }
                self.source_files = source_files;
                self.source_bytes = source_bytes;
                self.path_cursor = page.next_cursor;
                if page.complete {
                    self.phase = ManifestInventoryPhase::Stale;
                }
                Ok(SourceInventoryStep::Pending(
                    ImportInventorySliceProgress::default(),
                ))
            }
            ManifestInventoryPhase::Stale => {
                if store.current_source_import_inventory_generation(
                    self.source.provider,
                    &source_root,
                )? != Some(self.generation()?)
                {
                    return Err(CaptureError::InventorySuperseded.into());
                }
                let paths = store.list_source_import_inventory_paths_page(
                    self.source.provider,
                    &source_root,
                    self.stale_cursor,
                    64,
                )?;
                let mut missing = Vec::new();
                for (_, source_path) in &paths {
                    if !self.paths.contains_selected_path(Path::new(source_path))? {
                        missing.push(source_path.clone());
                    }
                }
                store.mark_source_import_inventory_paths_stale(
                    self.source.provider,
                    &source_root,
                    &missing,
                    utc_now().timestamp_millis(),
                    self.generation()?,
                )?;
                self.stale_cursor = paths.last().map(|(cursor, _)| *cursor);
                if paths.len() < 64 {
                    if !store.complete_source_import_inventory_generation(
                        self.source.provider,
                        &source_root,
                        self.generation()?,
                    )? {
                        return Err(CaptureError::InventorySuperseded.into());
                    }
                    self.phase = if self.full_rescan {
                        ManifestInventoryPhase::Resume
                    } else {
                        ManifestInventoryPhase::Complete
                    };
                }
                Ok(SourceInventoryStep::Pending(
                    ImportInventorySliceProgress::default(),
                ))
            }
            ManifestInventoryPhase::Resume => {
                let (_, _, next_cursor, complete) = store
                    .schedule_source_import_explicit_rescan_page(
                        self.source.provider,
                        &source_root,
                        self.generation()?,
                        self.rescan_cursor,
                        64,
                    )?;
                self.rescan_cursor = next_cursor;
                if complete {
                    self.phase = ManifestInventoryPhase::Complete;
                }
                Ok(SourceInventoryStep::Pending(
                    ImportInventorySliceProgress::default(),
                ))
            }
            ManifestInventoryPhase::Complete => {
                let plan = PlannedImportSource {
                    source: self.source.clone(),
                    stats: SourceStats {
                        files: self.source_files,
                        bytes: self.source_bytes,
                        change_token: None,
                    },
                    preinventory: SourcePreinventory::SourceImportFiles {
                        files: Vec::new(),
                        inventory_generation: self.generation()?,
                    },
                };
                Ok(SourceInventoryStep::Complete(plan, None, self.source_files))
            }
        }
    }

    fn generation(&self) -> Result<u64> {
        self.inventory_generation.ok_or_else(|| {
            anyhow::Error::new(CaptureError::SystemInvariant(
                "manifest inventory generation is missing",
            ))
        })
    }
}

fn merge_catalog_summary_bounded(target: &mut CatalogSummary, page: CatalogSummary) {
    const RETAINED_FAILURES: usize = 64;
    let retained = RETAINED_FAILURES.saturating_sub(target.failures.len());
    target.source_files = target.source_files.saturating_add(page.source_files);
    target.source_bytes = target.source_bytes.saturating_add(page.source_bytes);
    target.cataloged_sessions = target
        .cataloged_sessions
        .saturating_add(page.cataloged_sessions);
    target.cached_sessions = target.cached_sessions.saturating_add(page.cached_sessions);
    target.parsed_sessions = target.parsed_sessions.saturating_add(page.parsed_sessions);
    target.skipped_sessions = target
        .skipped_sessions
        .saturating_add(page.skipped_sessions);
    target.failed_sessions = target.failed_sessions.saturating_add(page.failed_sessions);
    target
        .failures
        .extend(page.failures.into_iter().take(retained));
}

fn catalog_source_json(plan: &PlannedImportSource, summary: &CatalogSummary) -> Value {
    json!({
        "provider": plan.source.provider.as_str(),
        "path": plan.source.path.clone(),
        "source_format": plan.source.source_format,
        "source_files": summary.source_files,
        "source_bytes": summary.source_bytes,
        "cataloged_sessions": summary.cataloged_sessions,
        "cached_sessions": summary.cached_sessions,
        "parsed_sessions": summary.parsed_sessions,
        "skipped_sessions": summary.skipped_sessions,
        "failed_sessions": summary.failed_sessions,
    })
}

pub(crate) fn source_matches_publication_owner(
    source: &SourceInfo,
    owner: &ProviderFilePublicationInventoryOwner,
) -> bool {
    source.provider == owner.provider
        && persisted_import_identity(&source.path, "source root")
            .is_ok_and(|source_root| source_root == owner.source_root)
}

fn publication_owner_plan(
    owner: ProviderFilePublicationInventoryOwner,
) -> Result<PlannedImportSource> {
    let source = provider_source_for_persisted_format(
        owner.provider,
        PathBuf::from(&owner.source_root),
        &owner.source_format,
    )
    .ok_or_else(|| {
        anyhow::Error::new(CaptureError::SystemInvariant(
            "persisted publication owner has an unsupported source format",
        ))
    })?;
    let preinventory = match owner.inventory_family {
        ProviderFileInventoryFamily::Catalog => SourcePreinventory::CodexSessionCatalog {
            summary: CatalogSummary::default(),
            inventory_generation: owner.inventory_generation,
        },
        ProviderFileInventoryFamily::SourceImport => SourcePreinventory::SourceImportFiles {
            files: Vec::new(),
            inventory_generation: owner.inventory_generation,
        },
    };
    Ok(PlannedImportSource {
        source,
        stats: SourceStats {
            files: 1,
            bytes: owner.file_size_bytes,
            change_token: None,
        },
        preinventory,
    })
}

pub(crate) fn inventory_available_sources(
    store: &Store,
    sources: &[SourceInfo],
) -> Result<ImportInventory> {
    let available = sources
        .iter()
        .filter(|source| {
            source.exists
                && source.status == ProviderSourceStatus::Available
                && source.import_support == ProviderImportSupport::Native
        })
        .cloned()
        .collect::<Vec<_>>();
    inventory_import_sources(store, available, false)
}

fn inventory_single_root_source(
    store: &Store,
    source: SourceInfo,
    resume: bool,
) -> Result<PlannedImportSource> {
    let (stats, root_file) = observe_source_root(&source)?;
    let persisted =
        persist_new_source_import_observation(store, &source, std::slice::from_ref(&root_file))?;
    if resume {
        schedule_pending_source_resume(
            store,
            source.provider,
            &root_file.source_root,
            persisted.inventory_generation,
        )?;
    }
    Ok(PlannedImportSource {
        source,
        stats,
        preinventory: SourcePreinventory::SourceRoot {
            file: root_file,
            inventory_generation: persisted.inventory_generation,
        },
    })
}

fn schedule_pending_source_resume(
    store: &Store,
    provider: CaptureProvider,
    source_root: &str,
    inventory_generation: u64,
) -> Result<()> {
    for file in store.list_pending_source_import_files(provider, source_root)? {
        store.record_source_import_file_result(
            file.provider,
            SourceImportFileIndexUpdate {
                source_root: &file.source_root,
                source_path: &file.source_path,
                file_size_bytes: file.file_size_bytes,
                file_modified_at_ms: file.file_modified_at_ms,
                import_revision: file.import_revision,
                inventory_generation,
                metadata: &file.metadata,
                indexed_at_ms: utc_now().timestamp_millis(),
            },
            CatalogIndexedStatus::Pending,
            None,
        )?;
    }
    Ok(())
}

fn is_incremental_codex_session_tree(source: &SourceInfo) -> bool {
    source.provider == CaptureProvider::Codex && source.source_format == "codex_session_jsonl_tree"
}

pub(crate) fn observe_source_root(source: &SourceInfo) -> Result<(SourceStats, SourceImportFile)> {
    let stats = source_stats(&source.path)
        .with_context(|| format!("inventory import source {}", source.path.display()))?;
    let file = source_root_observation_from_stats(source, stats)?;
    Ok((stats, file))
}

fn source_root_observation_from_stats(
    source: &SourceInfo,
    stats: SourceStats,
) -> Result<SourceImportFile> {
    ctx_history_capture::pace_current_filesystem_operation(source.path.as_os_str().len() as u64);
    let metadata = fs::metadata(&source.path)
        .with_context(|| format!("stat import source {}", source.path.display()))?;
    let source_identity = persisted_import_identity(&source.path, "source root")?;
    let file = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        source_root: source_identity.to_owned(),
        source_path: source_identity.to_owned(),
        file_size_bytes: stats.bytes,
        file_modified_at_ms: system_time_ms(metadata.modified().unwrap_or(UNIX_EPOCH)),
        import_revision: source.import_revision,
        observed_at_ms: system_time_ms(SystemTime::now()),
        metadata: json!({
            "inventory_unit": "source_root",
            "source_files": stats.files,
            "change_token_v1": stats
                .change_token
                .unwrap_or_default()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(""),
        }),
    };
    Ok(file)
}
