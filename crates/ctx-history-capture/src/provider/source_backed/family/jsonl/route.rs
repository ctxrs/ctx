use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
#[cfg(test)]
use ctx_history_core::ScannedSourceCounts;
use ctx_history_core::{
    CaptureProvider, CertifiedSource, CertifiedSourceDeletion, CertifiedSourceInventory,
    CoreRecord, SourceInventoryObservation, SourceKey, TypedKey,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::cell::Cell;
#[cfg(test)]
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Barrier,
};

use super::{
    observe_opened_file, revalidate_frozen_prefix, JsonlCheckpoint, JsonlFileObservation,
    JsonlProbe, JsonlRecordRef,
};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::source_backed::{
        source_backed_base_removals, source_backed_base_sources, SourceBackedGenerationSink,
        SourceBackedRevalidationTarget, SourceBackedRouteDriver, SourceBackedRouteError,
        SourceBackedRouteErrorKind, SourceBackedRouteResult,
    },
    CaptureError, Result,
};

const FAMILY_PARSER_REVISION: &str = "borrowed-jsonl-family-v1";
const FAMILY_POLICY_REVISION: &str = "borrowed-jsonl-certified-append-v1";
const FAMILY_FRONTIER_KIND: &str = "borrowed-jsonl-family-checkpoint-v1";
const FAMILY_SOURCE_REVISION_KIND: &str = "borrowed-jsonl-file-observation-v1";
const FAMILY_INVENTORY_AUTHORITY: &str = "borrowed-jsonl-provider-root-v1";
const FAMILY_INVENTORY_REVISION: &str = "borrowed-jsonl-inventory-v1";
const FAMILY_DISCOVERY_REVISION: &str = "borrowed-jsonl-discovery-v1";
const FAMILY_INVENTORY_DOMAIN: &[u8] = b"ctx-borrowed-jsonl-inventory-v1\0";
mod leaf;
#[cfg(test)]
use leaf::family_scanner_worker_count_policy;
use leaf::{physical_identity, scan_leaves, source_observation};
mod ownership;
use ownership::base_sources_for_root;
mod revalidation;
use revalidation::{
    binding_digest, inventory_observation, reset_terminal, revalidate_complete_inventory,
    revalidate_target,
};

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct JsonlFamilyScannerActivity {
    pub(crate) worker_count: usize,
    pub(crate) sources_started: usize,
    pub(crate) sources_completed: usize,
    pub(crate) peak_active_scanners: usize,
}

#[cfg(test)]
thread_local! {
    static FAMILY_SCANNER_WORKERS_OVERRIDE: Cell<Option<usize>> = const { Cell::new(None) };
    static FAMILY_SCANNER_ACTIVITY: Cell<JsonlFamilyScannerActivity> =
        const { Cell::new(JsonlFamilyScannerActivity {
            worker_count: 0,
            sources_started: 0,
            sources_completed: 0,
            peak_active_scanners: 0,
        }) };
}

#[cfg(test)]
pub(crate) fn jsonl_family_scanner_activity() -> JsonlFamilyScannerActivity {
    FAMILY_SCANNER_ACTIVITY.get()
}

#[cfg(test)]
struct JsonlFamilyScannerProbe {
    sources_started: AtomicUsize,
    sources_completed: AtomicUsize,
    active_scanners: AtomicUsize,
    peak_active_scanners: AtomicUsize,
    rendezvous_arrivals: AtomicUsize,
    rendezvous_target: usize,
    rendezvous: Barrier,
}

#[cfg(test)]
impl JsonlFamilyScannerProbe {
    fn enter(&self) -> JsonlFamilyActiveScanner<'_> {
        self.sources_started.fetch_add(1, Ordering::SeqCst);
        let active = self
            .active_scanners
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        self.peak_active_scanners
            .fetch_max(active, Ordering::SeqCst);
        if self.rendezvous_arrivals.fetch_add(1, Ordering::SeqCst) < self.rendezvous_target {
            self.rendezvous.wait();
        }
        JsonlFamilyActiveScanner { probe: self }
    }

    fn snapshot(&self, worker_count: usize) -> JsonlFamilyScannerActivity {
        debug_assert_eq!(self.active_scanners.load(Ordering::SeqCst), 0);
        JsonlFamilyScannerActivity {
            worker_count,
            sources_started: self.sources_started.load(Ordering::SeqCst),
            sources_completed: self.sources_completed.load(Ordering::SeqCst),
            peak_active_scanners: self.peak_active_scanners.load(Ordering::SeqCst),
        }
    }
}

#[cfg(test)]
struct JsonlFamilyActiveScanner<'probe> {
    probe: &'probe JsonlFamilyScannerProbe,
}

#[cfg(test)]
impl Drop for JsonlFamilyActiveScanner<'_> {
    fn drop(&mut self) {
        self.probe.sources_completed.fetch_add(1, Ordering::SeqCst);
        self.probe.active_scanners.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
fn jsonl_family_scanner_probe(worker_count: usize) -> Option<Arc<JsonlFamilyScannerProbe>> {
    FAMILY_SCANNER_WORKERS_OVERRIDE.with(|workers| {
        workers.get().map(|_| {
            let rendezvous_target = worker_count.clamp(1, 4);
            Arc::new(JsonlFamilyScannerProbe {
                sources_started: AtomicUsize::new(0),
                sources_completed: AtomicUsize::new(0),
                active_scanners: AtomicUsize::new(0),
                peak_active_scanners: AtomicUsize::new(0),
                rendezvous_arrivals: AtomicUsize::new(0),
                rendezvous_target,
                rendezvous: Barrier::new(rendezvous_target),
            })
        })
    })
}

#[cfg(test)]
fn record_jsonl_family_scanner_activity(
    worker_count: usize,
    probe: Option<&JsonlFamilyScannerProbe>,
) {
    FAMILY_SCANNER_ACTIVITY.set(
        probe.map_or_else(JsonlFamilyScannerActivity::default, |probe| {
            probe.snapshot(worker_count)
        }),
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonlFamilyAppendMode {
    CertifiedSuffix,
    Replacement,
}

pub(crate) trait JsonlFamilyProjector: Send {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()>;

    fn finish(&mut self) -> Result<()> {
        Ok(())
    }

    fn finish_projecting(&mut self, _emit: &mut dyn FnMut(CoreRecord) -> Result<()>) -> Result<()> {
        self.finish()
    }

    fn rejected_records(&self) -> u64 {
        0
    }
}

pub(crate) trait JsonlFamilyAdapter: Send + Sync {
    fn provider(&self) -> CaptureProvider;
    fn source_format(&self) -> &'static str;
    fn schema_variant(&self) -> &'static str;
    fn parser_revision(&self) -> &'static str;
    fn append_mode(&self) -> JsonlFamilyAppendMode;

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory>;

    fn discovery_error_kind(&self, _error: &CaptureError) -> SourceBackedRouteErrorKind {
        SourceBackedRouteErrorKind::InvalidSource
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>>;

    fn owns(&self, source: &SourceKey) -> bool {
        source.provider() == self.provider().as_str()
            && source.source_format() == self.source_format()
            && source.schema_variant() == self.schema_variant()
            && source.provider_identity_version() == 1
    }
}

#[derive(Debug, Clone)]
pub(crate) struct JsonlFamilyLeaf {
    source: SourceKey,
    source_path: PathBuf,
    authority_path: PathBuf,
    authority: Arc<ProviderSourceRoot>,
    observation: JsonlFileObservation,
    binding: TypedKey,
    identity_probe: Option<JsonlProbe>,
    identity_probe_rejected_records: u64,
    whole_record: bool,
}

impl JsonlFamilyLeaf {
    pub(crate) fn bind_observed(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot>,
        authority_path: PathBuf,
        binding: TypedKey,
        observation: JsonlFileObservation,
    ) -> Self {
        Self {
            source,
            source_path,
            authority_path,
            authority,
            observation,
            binding,
            identity_probe: None,
            identity_probe_rejected_records: 0,
            whole_record: false,
        }
    }

    pub(crate) fn observe(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot>,
        authority_path: PathBuf,
        binding: TypedKey,
    ) -> Result<Self> {
        Self::observe_with_framing(
            source,
            source_path,
            authority,
            authority_path,
            binding,
            false,
        )
    }

    pub(crate) fn observe_whole_record(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot>,
        authority_path: PathBuf,
        binding: TypedKey,
    ) -> Result<Self> {
        Self::observe_with_framing(
            source,
            source_path,
            authority,
            authority_path,
            binding,
            true,
        )
    }

    pub(crate) fn observe_after_identity_probe(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot>,
        authority_path: PathBuf,
        binding: TypedKey,
        mut identity_probe: JsonlProbe,
        identity_probe_rejected_records: u64,
    ) -> Result<Self> {
        let opened = authority.open_file(&authority_path)?;
        let observation = observe_opened_file(&source_path, &opened)?;
        if observation != identity_probe.observation {
            revalidate_frozen_prefix(
                &source_path,
                &opened,
                &identity_probe.observation,
                identity_probe.complete_prefix_end,
                super::prefix_digest(&identity_probe.prefix_hasher),
            )?;
            identity_probe.observation = observation.clone();
        }
        drop(opened);
        Ok(Self {
            source,
            source_path,
            authority_path,
            authority,
            observation,
            binding,
            identity_probe: Some(identity_probe),
            identity_probe_rejected_records,
            whole_record: false,
        })
    }

    fn observe_with_framing(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot>,
        authority_path: PathBuf,
        binding: TypedKey,
        whole_record: bool,
    ) -> Result<Self> {
        let opened = authority.open_file(&authority_path)?;
        let observation = observe_opened_file(&source_path, &opened)?;
        drop(opened);
        Ok(Self {
            source,
            source_path,
            authority_path,
            authority,
            observation,
            binding,
            identity_probe: None,
            identity_probe_rejected_records: 0,
            whole_record,
        })
    }

    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub(crate) fn authority(&self) -> &Arc<ProviderSourceRoot> {
        &self.authority
    }

    pub(crate) fn observation(&self) -> &JsonlFileObservation {
        &self.observation
    }

    pub(crate) fn binding(&self) -> &TypedKey {
        &self.binding
    }

    pub(crate) fn open_verified(&self) -> Result<Arc<OpenedProviderSourceFile>> {
        let opened = self.authority.open_file(&self.authority_path)?;
        if observe_opened_file(&self.source_path, &opened)? != self.observation {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(Arc::new(opened))
    }

    fn open_for_revalidation(
        &self,
    ) -> Result<(Arc<OpenedProviderSourceFile>, JsonlFileObservation)> {
        let opened = self.authority.open_file(&self.authority_path)?;
        let current = observe_opened_file(&self.source_path, &opened)?;
        if current != self.observation {
            if self.whole_record || !self.observation.is_same_file_growth_to(&current) {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            if let Some(probe) = &self.identity_probe {
                revalidate_frozen_prefix(
                    &self.source_path,
                    &opened,
                    &probe.observation,
                    probe.complete_prefix_end,
                    super::prefix_digest(&probe.prefix_hasher),
                )?;
            }
        }
        Ok((Arc::new(opened), current))
    }

    fn open_for_scan(&self) -> Result<(Self, Arc<OpenedProviderSourceFile>)> {
        let opened = self.authority.open_file(&self.authority_path)?;
        let current = observe_opened_file(&self.source_path, &opened)?;
        if current == self.observation {
            return Ok((self.clone(), Arc::new(opened)));
        }
        if self.whole_record
            || current.length <= self.observation.length
            || !self.observation.admits_frozen_prefix_in(&current)
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let mut leaf = self.clone();
        leaf.observation = current.clone();
        if let Some(probe) = leaf.identity_probe.as_mut() {
            revalidate_frozen_prefix(
                &leaf.source_path,
                &opened,
                &probe.observation,
                probe.complete_prefix_end,
                super::prefix_digest(&probe.prefix_hasher),
            )?;
            probe.observation = current;
        }
        Ok((leaf, Arc::new(opened)))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct JsonlFamilyRejectedLeaf {
    source_path: PathBuf,
    authority_path: PathBuf,
    proof: TypedKey,
    rejected_records: u64,
}

impl JsonlFamilyRejectedLeaf {
    pub(crate) fn bind_observed(
        source_path: PathBuf,
        authority_path: PathBuf,
        proof: TypedKey,
        rejected_records: u64,
    ) -> Self {
        Self {
            source_path,
            authority_path,
            proof,
            rejected_records,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct JsonlFamilyInventory {
    root_missing: bool,
    observation: SourceInventoryObservation,
    authority: Option<Arc<ProviderSourceRoot>>,
    leaves: Vec<JsonlFamilyLeaf>,
    rejected_leaves: Vec<JsonlFamilyRejectedLeaf>,
}

impl JsonlFamilyInventory {
    pub(crate) fn present(
        provider: CaptureProvider,
        root: &Path,
        authority: Arc<ProviderSourceRoot>,
        leaves: Vec<JsonlFamilyLeaf>,
    ) -> Result<Self> {
        Self::present_with_rejected(provider, root, authority, leaves, Vec::new())
    }

    pub(crate) fn present_with_rejected(
        provider: CaptureProvider,
        root: &Path,
        authority: Arc<ProviderSourceRoot>,
        mut leaves: Vec<JsonlFamilyLeaf>,
        mut rejected_leaves: Vec<JsonlFamilyRejectedLeaf>,
    ) -> Result<Self> {
        leaves.sort_by(|left, right| left.source_path.cmp(&right.source_path));
        rejected_leaves.sort_by(|left, right| left.source_path.cmp(&right.source_path));
        let observation = inventory_observation(
            provider,
            root,
            false,
            Some(&authority),
            &leaves,
            &rejected_leaves,
        )?;
        Ok(Self {
            root_missing: false,
            observation,
            authority: Some(authority),
            leaves,
            rejected_leaves,
        })
    }

    pub(crate) fn missing(provider: CaptureProvider, root: &Path) -> Result<Self> {
        Ok(Self {
            root_missing: true,
            observation: inventory_observation(provider, root, true, None, &[], &[])?,
            authority: None,
            leaves: Vec::new(),
            rejected_leaves: Vec::new(),
        })
    }

    pub(crate) fn root_missing(&self) -> bool {
        self.root_missing
    }

    pub(crate) fn leaves(&self) -> &[JsonlFamilyLeaf] {
        &self.leaves
    }

    pub(crate) fn rejected_leaves(&self) -> &[JsonlFamilyRejectedLeaf] {
        &self.rejected_leaves
    }

    fn certify_against(&self, closing: &Self) -> Result<CertifiedSourceInventory> {
        if self.root_missing || closing.root_missing {
            return Err(CaptureError::InvalidPayload(
                "missing JSONL roots cannot certify an inventory".to_owned(),
            ));
        }
        CertifiedSourceInventory::certify(
            self.observation.clone(),
            closing.observation.clone(),
            FAMILY_DISCOVERY_REVISION,
            closing
                .leaves
                .iter()
                .map(|leaf| leaf.source.clone())
                .collect(),
        )
        .map_err(contract_error)
    }

    fn revalidate_root(&self) -> Result<()> {
        self.authority
            .as_ref()
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "JSONL inventory has no retained root authority".to_owned(),
                )
            })?
            .revalidate()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FamilyCheckpoint {
    version: u32,
    provider_parser_revision: String,
    binding_digest: [u8; 32],
    physical: JsonlCheckpoint,
    represented_physical_records: u64,
    rejected_records: u64,
    indexed_documents: u64,
}

impl FamilyCheckpoint {
    const VERSION: u32 = 3;

    fn valid_for(&self, adapter: &dyn JsonlFamilyAdapter, leaf: &JsonlFamilyLeaf) -> bool {
        self.version == Self::VERSION
            && self.provider_parser_revision == adapter.parser_revision()
            && binding_digest(leaf).is_ok_and(|digest| self.binding_digest == digest)
            && self.physical.is_internally_consistent()
            && self.physical.identity() == &physical_identity(adapter, leaf)
            && self
                .represented_physical_records
                .checked_add(self.rejected_records)
                .is_some_and(|classified| classified <= self.physical.next_physical_ordinal())
    }
}

#[derive(Debug, Clone)]
struct TerminalSourceEvidence {
    certificate: CertifiedSource,
    checkpoint: Option<JsonlCheckpoint>,
}

#[derive(Default)]
struct FamilyResident {
    ownership_initialized: bool,
    owned_sources: HashMap<[u8; 32], SourceKey>,
    terminal_sources: HashMap<[u8; 32], TerminalSourceEvidence>,
    certified_inventory: Option<CertifiedSourceInventory>,
}

pub(crate) fn jsonl_family_driver(
    adapter: Arc<dyn JsonlFamilyAdapter>,
    root: PathBuf,
) -> SourceBackedRouteDriver {
    let resident = Arc::new(Mutex::new(FamilyResident::default()));
    let scan_adapter = Arc::clone(&adapter);
    let scan_root = root.clone();
    let scan_resident = Arc::clone(&resident);
    let owns_adapter = Arc::clone(&adapter);
    let owns_resident = Arc::clone(&resident);
    let revalidation_resident = Arc::clone(&resident);
    let terminal_adapter = adapter;
    let terminal_root = root;
    let inventory_resident = Arc::clone(&resident);

    SourceBackedRouteDriver::new(
        move |sink| capture(&*scan_adapter, &scan_root, &scan_resident, sink),
        move |source| {
            owns_adapter.owns(source)
                && owns_resident.lock().is_ok_and(|resident| {
                    !resident.ownership_initialized
                        || resident
                            .owned_sources
                            .get(&source.exact_descriptor_digest())
                            .is_some_and(|owned| owned.exact_descriptor_eq(source))
                })
        },
        move |target| revalidate_target(&revalidation_resident, target),
    )
    .with_complete_inventory_revalidation(move |expected| {
        revalidate_complete_inventory(
            terminal_adapter.as_ref(),
            &terminal_root,
            &inventory_resident,
            expected,
        )
        .unwrap_or(false)
    })
}

fn capture(
    adapter: &dyn JsonlFamilyAdapter,
    root: &Path,
    resident: &Mutex<FamilyResident>,
    sink: &mut SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<()> {
    reset_terminal(resident)?;
    let opening = adapter
        .discover(root)
        .map_err(|error| route_discovery(adapter, error))?;
    if opening.root_missing() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unavailable,
            "provider JSONL root is unavailable",
        ));
    }
    if opening.leaves().is_empty() && !opening.rejected_leaves().is_empty() {
        let rejected_records =
            opening
                .rejected_leaves()
                .iter()
                .try_fold(0_u64, |total, leaf| {
                    total.checked_add(leaf.rejected_records).ok_or_else(|| {
                        SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Internal,
                            "provider JSONL rejected-record count overflow",
                        )
                    })
                })?;
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::InvalidSource,
            format!(
                "direct JSONL route rejected {rejected_records} records across {} sources; \
                 all provider-native session identity leaves were rejected",
                opening.rejected_leaves().len()
            ),
        ));
    }
    let bases = base_sources_for_root(adapter, &opening, sink).map_err(route_invalid)?;
    let mut owned_sources = HashMap::with_capacity(bases.len() + opening.leaves.len());
    for source in bases
        .iter()
        .map(|base| base.observation().source())
        .chain(opening.leaves().iter().map(JsonlFamilyLeaf::source))
    {
        let digest = source.exact_descriptor_digest();
        if owned_sources
            .insert(digest, source.clone())
            .is_some_and(|previous| !previous.exact_descriptor_eq(source))
        {
            return Err(route_invalid(
                "JSONL route source descriptor digest collision",
            ));
        }
    }
    for removal in source_backed_base_removals(sink) {
        let deletion = removal.deletion();
        let inventory = deletion.inventory();
        if adapter.owns(deletion.source())
            && inventory.authority_namespace() == FAMILY_INVENTORY_AUTHORITY
            && inventory.authority_key()
                == &TypedKey::bytes(root.as_os_str().as_encoded_bytes().to_vec())
                    .map_err(route_invalid)?
        {
            owned_sources.insert(
                deletion.source().exact_descriptor_digest(),
                deletion.source().clone(),
            );
        }
    }
    let bases_by_descriptor = bases_by_descriptor(&bases)?;
    let terminal_sources = scan_leaves(adapter, opening.leaves(), &bases_by_descriptor, sink)?;

    let closing = adapter
        .discover(root)
        .map_err(|error| route_discovery(adapter, error))?;
    let inventory = opening.certify_against(&closing).map_err(route_invalid)?;
    sink.certify_complete_inventory(inventory.clone())
        .map_err(route_internal)?;
    for base in bases {
        if !inventory.contains(base.observation().source()) {
            let deletion = CertifiedSourceDeletion::from_inventory(
                base.observation().source().clone(),
                &inventory,
            )
            .map_err(route_invalid)?;
            sink.delete_source(deletion, inventory.clone())
                .map_err(route_internal)?;
        }
    }
    let mut resident = resident
        .lock()
        .map_err(|_| route_internal("JSONL resident catalog lock was poisoned"))?;
    resident.ownership_initialized = true;
    resident.owned_sources = owned_sources;
    resident.terminal_sources = terminal_sources;
    resident.certified_inventory = Some(inventory);
    Ok(())
}

fn bases_by_descriptor(
    bases: &[CertifiedSource],
) -> SourceBackedRouteResult<HashMap<[u8; 32], &CertifiedSource>> {
    let mut by_descriptor = HashMap::with_capacity(bases.len());
    for base in bases {
        let source = base.observation().source();
        let digest = source.exact_descriptor_digest();
        if let Some(previous) = by_descriptor.insert(digest, base) {
            if !previous.observation().source().exact_descriptor_eq(source) {
                return Err(route_invalid(
                    "JSONL base source descriptor digest collision",
                ));
            }
            return Err(route_invalid("duplicate JSONL base source descriptor"));
        }
    }
    Ok(by_descriptor)
}

#[cfg(test)]
fn with_family_scanner_workers<T>(workers: usize, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<usize>);

    impl Drop for Restore {
        fn drop(&mut self) {
            FAMILY_SCANNER_WORKERS_OVERRIDE.set(self.0);
        }
    }

    let previous = FAMILY_SCANNER_WORKERS_OVERRIDE.replace(Some(workers));
    let _restore = Restore(previous);
    FAMILY_SCANNER_ACTIVITY.set(JsonlFamilyScannerActivity::default());
    run()
}

fn route_invalid(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}

fn route_discovery(
    adapter: &dyn JsonlFamilyAdapter,
    error: CaptureError,
) -> SourceBackedRouteError {
    SourceBackedRouteError::new(adapter.discovery_error_kind(&error), error.to_string())
}

fn route_internal(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
}

fn contract_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
#[path = "route/tests.rs"]
mod tests;
