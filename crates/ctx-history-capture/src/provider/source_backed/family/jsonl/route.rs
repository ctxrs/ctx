use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, CaptureProvider, CertifiedSource,
    CertifiedSourceAppend, CertifiedSourceDeletion, CertifiedSourceInventory,
    EventHydrationRequest, HydratedProviderRecord, HydrationFailure, HydrationFailureKind,
    ScannedSourceCounts, SourceFrontier, SourceInventoryObservation, SourceKey, SourceObservation,
    TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::cell::Cell;

use super::{
    observe_opened_file, JsonlCheckpoint, JsonlFileObservation, JsonlProbe, JsonlReader,
    JsonlRecordRef, JsonlSourceChange, JsonlSourceIdentity,
};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::source_backed::{
        source_backed_base_sources, SourceBackedGenerationSink, SourceBackedRevalidationTarget,
        SourceBackedRouteDriver, SourceBackedRouteError, SourceBackedRouteErrorKind,
        SourceBackedRouteResult,
    },
    CaptureError, Result,
};

const FAMILY_PARSER_REVISION: &str = "borrowed-jsonl-family-v1";
const FAMILY_POLICY_REVISION: &str = "borrowed-jsonl-replacement-v1";
const FAMILY_FRONTIER_KIND: &str = "borrowed-jsonl-family-checkpoint-v1";
const FAMILY_SOURCE_REVISION_KIND: &str = "borrowed-jsonl-file-observation-v1";
const FAMILY_INVENTORY_AUTHORITY: &str = "borrowed-jsonl-provider-root-v1";
const FAMILY_INVENTORY_REVISION: &str = "borrowed-jsonl-inventory-v1";
const FAMILY_DISCOVERY_REVISION: &str = "borrowed-jsonl-discovery-v1";
const FAMILY_INVENTORY_DOMAIN: &[u8] = b"ctx-borrowed-jsonl-inventory-v1\0";

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct JsonlFamilyWork {
    pub(crate) discoveries: usize,
    pub(crate) leaf_opens: usize,
    pub(crate) provider_projections: usize,
}

#[cfg(test)]
thread_local! {
    static FAMILY_DISCOVERIES: Cell<usize> = const { Cell::new(0) };
    static FAMILY_LEAF_OPENS: Cell<usize> = const { Cell::new(0) };
    static FAMILY_PROVIDER_PROJECTIONS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_jsonl_family_work() {
    FAMILY_DISCOVERIES.set(0);
    FAMILY_LEAF_OPENS.set(0);
    FAMILY_PROVIDER_PROJECTIONS.set(0);
}

#[cfg(test)]
pub(crate) fn jsonl_family_work() -> JsonlFamilyWork {
    JsonlFamilyWork {
        discoveries: FAMILY_DISCOVERIES.get(),
        leaf_opens: FAMILY_LEAF_OPENS.get(),
        provider_projections: FAMILY_PROVIDER_PROJECTIONS.get(),
    }
}

pub(crate) trait JsonlFamilyProjector: Send {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(LexicalDocument) -> Result<()>,
    ) -> Result<()>;

    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

pub(crate) trait JsonlFamilyHydrator {
    fn hydrate(
        &mut self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure>;

    fn finish(&mut self) -> std::result::Result<(), HydrationFailure> {
        Ok(())
    }
}

pub(crate) trait JsonlFamilyAdapter: Send + Sync {
    fn provider(&self) -> CaptureProvider;
    fn source_format(&self) -> &'static str;
    fn schema_variant(&self) -> &'static str;
    fn parser_revision(&self) -> &'static str;

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory>;

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>>;

    fn hydrator(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
    ) -> std::result::Result<Box<dyn JsonlFamilyHydrator>, HydrationFailure>;

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
}

impl JsonlFamilyLeaf {
    pub(crate) fn observe(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot>,
        authority_path: PathBuf,
        binding: TypedKey,
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
        })
    }

    pub(crate) fn observe_after_identity_probe(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot>,
        authority_path: PathBuf,
        binding: TypedKey,
        identity_probe: JsonlProbe,
    ) -> Result<Self> {
        let opened = authority.open_file(&authority_path)?;
        let observation = observe_opened_file(&source_path, &opened)?;
        if observation != identity_probe.observation {
            return Err(CaptureError::SourceChangedDuringCapture);
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
        #[cfg(test)]
        FAMILY_LEAF_OPENS.with(|count| count.set(count.get().saturating_add(1)));
        let opened = self.authority.open_file(&self.authority_path)?;
        if observe_opened_file(&self.source_path, &opened)? != self.observation {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(Arc::new(opened))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct JsonlFamilyInventory {
    root_missing: bool,
    observation: SourceInventoryObservation,
    authority: Option<Arc<ProviderSourceRoot>>,
    leaves: Vec<JsonlFamilyLeaf>,
}

impl JsonlFamilyInventory {
    pub(crate) fn present(
        provider: CaptureProvider,
        root: &Path,
        authority: Arc<ProviderSourceRoot>,
        mut leaves: Vec<JsonlFamilyLeaf>,
    ) -> Result<Self> {
        leaves.sort_by(|left, right| left.source_path.cmp(&right.source_path));
        let observation = inventory_observation(provider, root, false, Some(&authority), &leaves)?;
        Ok(Self {
            root_missing: false,
            observation,
            authority: Some(authority),
            leaves,
        })
    }

    pub(crate) fn missing(provider: CaptureProvider, root: &Path) -> Result<Self> {
        Ok(Self {
            root_missing: true,
            observation: inventory_observation(provider, root, true, None, &[])?,
            authority: None,
            leaves: Vec::new(),
        })
    }

    pub(crate) fn root_missing(&self) -> bool {
        self.root_missing
    }

    pub(crate) fn leaves(&self) -> &[JsonlFamilyLeaf] {
        &self.leaves
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
    indexed_documents: u64,
}

impl FamilyCheckpoint {
    const VERSION: u32 = 2;

    fn valid_for(&self, adapter: &dyn JsonlFamilyAdapter, leaf: &JsonlFamilyLeaf) -> bool {
        self.version == Self::VERSION
            && self.provider_parser_revision == adapter.parser_revision()
            && binding_digest(leaf).is_ok_and(|digest| self.binding_digest == digest)
            && self.physical.is_internally_consistent()
            && self.physical.identity() == &physical_identity(adapter, leaf)
            && self.represented_physical_records <= self.physical.next_physical_ordinal()
    }
}

#[derive(Debug, Clone)]
struct TerminalSourceEvidence {
    certificate: CertifiedSource,
    leaf: JsonlFamilyLeaf,
}

#[derive(Default)]
struct FamilyResident {
    terminal_sources: HashMap<[u8; 32], TerminalSourceEvidence>,
    certified_inventory: Option<CertifiedSourceInventory>,
    hydration_inventory: Option<JsonlFamilyInventory>,
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
    let revalidation_resident = Arc::clone(&resident);
    let hydration_adapter = Arc::clone(&adapter);
    let hydration_root = root.clone();
    let hydration_resident = Arc::clone(&resident);
    let batch_adapter = Arc::clone(&adapter);
    let batch_root = root;
    let batch_resident = Arc::clone(&resident);
    let inventory_resident = Arc::clone(&resident);

    SourceBackedRouteDriver::new(
        move |sink| capture(&*scan_adapter, &scan_root, &scan_resident, sink),
        move |source| owns_adapter.owns(source),
        move |target| revalidate_target(&revalidation_resident, target).unwrap_or(false),
        move |request| {
            hydrate_single(
                &*hydration_adapter,
                &hydration_root,
                &hydration_resident,
                request,
            )
        },
    )
    .with_batch_hydration(move |request| {
        hydrate_batch(&*batch_adapter, &batch_root, &batch_resident, request)
    })
    .with_complete_inventory_revalidation(move |expected| {
        inventory_resident
            .lock()
            .ok()
            .and_then(|resident| resident.certified_inventory.as_ref().cloned())
            .as_ref()
            == Some(expected)
    })
}

fn capture(
    adapter: &dyn JsonlFamilyAdapter,
    root: &Path,
    resident: &Mutex<FamilyResident>,
    sink: &mut SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<()> {
    reset_terminal(resident)?;
    let opening = discover(adapter, root).map_err(route_invalid)?;
    if opening.root_missing() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unavailable,
            "provider JSONL root is unavailable",
        ));
    }
    let bases = source_backed_base_sources(sink, |source| adapter.owns(source));
    let mut terminal_sources = HashMap::with_capacity(opening.leaves.len());

    for leaf in opening.leaves() {
        let base = bases.iter().find(|base| {
            base.observation()
                .source()
                .exact_descriptor_eq(leaf.source())
        });
        let evidence = scan_leaf(adapter, leaf, base, sink)?;
        if terminal_sources
            .insert(leaf.source().exact_descriptor_digest(), evidence)
            .is_some()
        {
            return Err(route_invalid("duplicate JSONL source identity"));
        }
    }

    let closing = discover(adapter, root).map_err(route_invalid)?;
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
    resident.terminal_sources = terminal_sources;
    resident.certified_inventory = Some(inventory);
    resident.hydration_inventory = Some(closing);
    Ok(())
}

fn scan_leaf(
    adapter: &dyn JsonlFamilyAdapter,
    leaf: &JsonlFamilyLeaf,
    base: Option<&CertifiedSource>,
    sink: &mut SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<TerminalSourceEvidence> {
    let previous = base.and_then(|base| decode_checkpoint(adapter, leaf, base).ok());
    let exact_terminal = previous.as_ref().is_some_and(|checkpoint| {
        checkpoint.physical.terminal()
            && checkpoint.physical.source_observation() == leaf.observation()
    });
    let opened = leaf.open_verified().map_err(route_invalid)?;
    let mut reader = JsonlReader::open(
        physical_identity(adapter, leaf),
        Arc::clone(&opened),
        exact_terminal
            .then_some(previous.as_ref())
            .flatten()
            .map(|checkpoint| &checkpoint.physical),
        leaf.identity_probe.clone(),
    )
    .map_err(route_invalid)?;

    if reader.source_change() == JsonlSourceChange::Unchanged {
        let base = base.ok_or_else(|| route_invalid("unchanged JSONL source has no base"))?;
        let staged = sink
            .begin_source_append(leaf.source.clone())
            .map_err(route_internal)?;
        if staged != base {
            return Err(route_invalid("JSONL no-op base changed before staging"));
        }
        while reader
            .visit_page(&mut |_record| -> Result<()> { Ok(()) })
            .map_err(route_invalid)?
            .is_some()
        {}
        let outcome = reader
            .outcome()
            .ok_or_else(|| route_invalid("JSONL no-op scan has no terminal checkpoint"))?;
        let decoded = previous.ok_or_else(|| route_invalid("JSONL no-op checkpoint is absent"))?;
        if outcome.checkpoint() != &decoded.physical {
            return Err(route_invalid("JSONL no-op checkpoint changed"));
        }
        let frontier = base
            .frontier()
            .ok_or_else(|| route_invalid("JSONL no-op base frontier is absent"))?;
        let append = CertifiedSourceAppend::certify(
            base,
            base.clone(),
            frontier.certified_prefix_bytes(),
            *frontier.certified_prefix_digest(),
        )
        .map_err(route_invalid)?;
        sink.certify_source_append(append).map_err(route_internal)?;
        return Ok(TerminalSourceEvidence {
            certificate: base.clone(),
            leaf: leaf.clone(),
        });
    }

    sink.begin_source(leaf.source.clone())
        .map_err(route_internal)?;
    let mut projector = adapter
        .projector(leaf, opened, DateTime::<Utc>::UNIX_EPOCH)
        .map_err(route_invalid)?;
    let mut physical_records = u64::from(leaf.identity_probe.is_some());
    let mut represented_records = 0_u64;
    let mut documents = 0_u64;
    while reader
        .visit_page(&mut |record| -> Result<()> {
            physical_records = checked_increment(physical_records)?;
            let before = documents;
            #[cfg(test)]
            FAMILY_PROVIDER_PROJECTIONS.with(|count| count.set(count.get().saturating_add(1)));
            projector.project(record, &mut |document| {
                if !document.source.exact_descriptor_eq(leaf.source()) {
                    return Err(CaptureError::InvalidPayload(
                        "JSONL projector changed the bound source".to_owned(),
                    ));
                }
                sink.add_document(document)
                    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
                documents = checked_increment(documents)?;
                Ok(())
            })?;
            if documents != before {
                represented_records = checked_increment(represented_records)?;
            }
            Ok(())
        })
        .map_err(route_invalid)?
        .is_some()
    {}
    projector.finish().map_err(route_invalid)?;
    let outcome = reader
        .outcome()
        .ok_or_else(|| route_invalid("JSONL replacement scan has no terminal checkpoint"))?;
    if physical_records != outcome.checkpoint().next_physical_ordinal() {
        return Err(route_invalid(
            "JSONL physical record count did not reconcile",
        ));
    }
    let checkpoint = FamilyCheckpoint {
        version: FamilyCheckpoint::VERSION,
        provider_parser_revision: adapter.parser_revision().to_owned(),
        binding_digest: binding_digest(leaf).map_err(route_invalid)?,
        physical: outcome.checkpoint().clone(),
        represented_physical_records: represented_records,
        indexed_documents: documents,
    };
    let certificate = certify(adapter, leaf, checkpoint)?;
    sink.certify_source(certificate.clone())
        .map_err(route_internal)?;
    Ok(TerminalSourceEvidence {
        certificate,
        leaf: leaf.clone(),
    })
}

fn certify(
    adapter: &dyn JsonlFamilyAdapter,
    leaf: &JsonlFamilyLeaf,
    checkpoint: FamilyCheckpoint,
) -> SourceBackedRouteResult<CertifiedSource> {
    if !checkpoint.valid_for(adapter, leaf) {
        return Err(route_invalid("JSONL checkpoint is internally inconsistent"));
    }
    let ignored = checkpoint
        .physical
        .next_physical_ordinal()
        .checked_sub(checkpoint.represented_physical_records)
        .ok_or_else(|| route_invalid("JSONL ignored count underflowed"))?;
    let complete_records = checkpoint
        .indexed_documents
        .checked_add(ignored)
        .ok_or_else(|| route_invalid("JSONL complete count overflowed"))?;
    let frontier = SourceFrontier::new(
        FAMILY_FRONTIER_KIND,
        TypedKey::bytes(serde_json::to_vec(&checkpoint).map_err(route_invalid)?)
            .map_err(route_invalid)?,
        checkpoint.physical.complete_prefix_end(),
        *checkpoint.physical.complete_prefix_sha256(),
    )
    .map_err(route_invalid)?;
    CertifiedSource::certify_with_frontier(
        source_observation(&leaf.source, &leaf.observation).map_err(route_invalid)?,
        source_observation(&leaf.source, &leaf.observation).map_err(route_invalid)?,
        FAMILY_PARSER_REVISION,
        *checkpoint.physical.complete_prefix_sha256(),
        ScannedSourceCounts {
            complete_records,
            retained_records: checkpoint.indexed_documents,
            rejected_records: 0,
            ignored_records: ignored,
            indexed_documents: checkpoint.indexed_documents,
            certified_bytes: checkpoint.physical.complete_prefix_end(),
        },
        Some(frontier),
    )
    .map_err(route_invalid)
}

fn decode_checkpoint(
    adapter: &dyn JsonlFamilyAdapter,
    leaf: &JsonlFamilyLeaf,
    certificate: &CertifiedSource,
) -> Result<FamilyCheckpoint> {
    certificate.validate_contract().map_err(contract_error)?;
    leaf.source
        .validate_exact_descriptor(certificate.observation().source())
        .map_err(contract_error)?;
    if certificate.parser_revision() != FAMILY_PARSER_REVISION {
        return Err(CaptureError::InvalidPayload(
            "JSONL base parser revision changed".to_owned(),
        ));
    }
    let frontier = certificate
        .frontier()
        .ok_or_else(|| CaptureError::InvalidPayload("JSONL base frontier is absent".to_owned()))?;
    if frontier.checkpoint_kind() != FAMILY_FRONTIER_KIND {
        return Err(CaptureError::InvalidPayload(
            "JSONL base frontier kind changed".to_owned(),
        ));
    }
    let TypedKey::Bytes(bytes) = frontier.checkpoint() else {
        return Err(CaptureError::InvalidPayload(
            "JSONL base checkpoint is malformed".to_owned(),
        ));
    };
    let checkpoint: FamilyCheckpoint = serde_json::from_slice(bytes)?;
    let ignored = checkpoint
        .physical
        .next_physical_ordinal()
        .checked_sub(checkpoint.represented_physical_records)
        .ok_or_else(|| CaptureError::InvalidPayload("JSONL base counts are invalid".to_owned()))?;
    let counts = certificate.counts();
    if !checkpoint.valid_for(adapter, leaf)
        || checkpoint.physical.complete_prefix_end() != frontier.certified_prefix_bytes()
        || checkpoint.physical.complete_prefix_sha256() != frontier.certified_prefix_digest()
        || checkpoint.physical.complete_prefix_sha256() != certificate.content_digest()
        || checkpoint.indexed_documents != counts.retained_records
        || checkpoint.indexed_documents != counts.indexed_documents
        || ignored != counts.ignored_records
        || checkpoint.indexed_documents.checked_add(ignored) != Some(counts.complete_records)
        || counts.rejected_records != 0
        || checkpoint.physical.complete_prefix_end() != counts.certified_bytes
        || certificate.observation()
            != &source_observation(&leaf.source, checkpoint.physical.source_observation())?
    {
        return Err(CaptureError::InvalidPayload(
            "JSONL base checkpoint does not reconcile".to_owned(),
        ));
    }
    Ok(checkpoint)
}

fn physical_identity(
    adapter: &dyn JsonlFamilyAdapter,
    leaf: &JsonlFamilyLeaf,
) -> JsonlSourceIdentity {
    JsonlSourceIdentity::new(
        adapter.provider().as_str(),
        adapter.parser_revision(),
        FAMILY_POLICY_REVISION,
        leaf.source.exact_descriptor_digest(),
        leaf.source_path.clone(),
    )
}

fn source_observation(
    source: &SourceKey,
    observation: &JsonlFileObservation,
) -> Result<SourceObservation> {
    SourceObservation::new(
        source.clone(),
        FAMILY_SOURCE_REVISION_KIND,
        serde_json::to_vec(observation)?,
    )
    .map_err(contract_error)
}

fn reset_terminal(resident: &Mutex<FamilyResident>) -> SourceBackedRouteResult<()> {
    let mut resident = resident
        .lock()
        .map_err(|_| route_internal("JSONL resident catalog lock was poisoned"))?;
    resident.terminal_sources.clear();
    resident.certified_inventory = None;
    Ok(())
}

fn revalidate_target(
    resident: &Mutex<FamilyResident>,
    target: SourceBackedRevalidationTarget<'_>,
) -> Result<bool> {
    let resident = resident.lock().map_err(|_| {
        CaptureError::InvalidPayload("JSONL resident catalog lock was poisoned".to_owned())
    })?;
    match target {
        SourceBackedRevalidationTarget::Source(expected) => {
            let Some(evidence) = resident
                .terminal_sources
                .get(&expected.observation().source().exact_descriptor_digest())
            else {
                return Ok(false);
            };
            if evidence.certificate != *expected {
                return Ok(false);
            }
            let opened = evidence.leaf.open_verified()?;
            let current = observe_opened_file(evidence.leaf.source_path(), opened.as_ref())?;
            Ok(current == *evidence.leaf.observation()
                && evidence.leaf.authority.revalidate().is_ok())
        }
        SourceBackedRevalidationTarget::Deletion(deletion) => Ok(resident
            .certified_inventory
            .as_ref()
            .is_some_and(|inventory| deletion.verifies(inventory))),
    }
}

fn hydrate_single(
    adapter: &dyn JsonlFamilyAdapter,
    root: &Path,
    resident: &Mutex<FamilyResident>,
    request: &EventHydrationRequest,
) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
    let mut records = hydrate_group(adapter, root, resident, std::slice::from_ref(request))?;
    records.pop().ok_or_else(|| {
        hydration_error(
            HydrationFailureKind::InvalidLocator,
            "JSONL single hydration returned no record",
        )
    })
}

fn hydrate_batch(
    adapter: &dyn JsonlFamilyAdapter,
    root: &Path,
    resident: &Mutex<FamilyResident>,
    request: &BatchHydrationRequest,
) -> std::result::Result<BatchHydrationResult, HydrationFailure> {
    let records = hydrate_group(adapter, root, resident, request.events())?;
    let result = BatchHydrationResult::new(records)
        .map_err(|error| hydration_error(HydrationFailureKind::InvalidLocator, error))?;
    result.validate_for_request(request)?;
    Ok(result)
}

fn hydrate_group(
    adapter: &dyn JsonlFamilyAdapter,
    root: &Path,
    resident: &Mutex<FamilyResident>,
    requests: &[EventHydrationRequest],
) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    let source = requests[0].locator().source();
    if requests
        .iter()
        .any(|request| !request.locator().source().exact_descriptor_eq(source))
    {
        return Err(hydration_error(
            HydrationFailureKind::InvalidLocator,
            "JSONL hydration batch spans exact sources",
        ));
    }
    let result = (|| {
        let mut resident = resident.lock().map_err(|_| {
            hydration_error(
                HydrationFailureKind::TemporarilyUnavailable,
                "JSONL resident catalog lock was poisoned",
            )
        })?;
        if resident.hydration_inventory.is_none() {
            let inventory = discover(adapter, root).map_err(|error| {
                hydration_error(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
            if inventory.root_missing() {
                return Err(hydration_error(
                    HydrationFailureKind::TemporarilyUnavailable,
                    "provider JSONL root is unavailable",
                ));
            }
            resident.hydration_inventory = Some(inventory);
        }
        let inventory = resident.hydration_inventory.as_ref().ok_or_else(|| {
            hydration_error(
                HydrationFailureKind::TemporarilyUnavailable,
                "JSONL resident inventory is absent",
            )
        })?;
        let leaf = inventory
            .leaves()
            .iter()
            .find(|leaf| leaf.source().exact_descriptor_eq(source))
            .ok_or_else(|| {
                hydration_error(
                    HydrationFailureKind::ConfirmedDeleted,
                    "exact JSONL source is absent from the resident inventory",
                )
            })?;
        let opened = leaf
            .open_verified()
            .map_err(|error| hydration_error(HydrationFailureKind::StaleRecordEvidence, error))?;
        let mut hydrator = adapter.hydrator(leaf, Arc::clone(&opened))?;
        let mut records = Vec::with_capacity(requests.len());
        for request in requests {
            let record = hydrator.hydrate(request)?;
            if record.event_id != request.event_id() {
                return Err(hydration_error(
                    HydrationFailureKind::InvalidLocator,
                    "JSONL hydrator changed the requested event identity",
                ));
            }
            records.push(record);
        }
        hydrator.finish()?;
        if observe_opened_file(leaf.source_path(), opened.as_ref())
            .map_err(|error| hydration_error(HydrationFailureKind::StaleRecordEvidence, error))?
            != *leaf.observation()
            || inventory.revalidate_root().is_err()
        {
            return Err(hydration_error(
                HydrationFailureKind::StaleRecordEvidence,
                "JSONL source changed during grouped hydration",
            ));
        }
        Ok(records)
    })();
    if result.as_ref().is_err_and(|failure| {
        matches!(
            failure.kind,
            HydrationFailureKind::StaleRecordEvidence | HydrationFailureKind::ConfirmedDeleted
        )
    }) {
        if let Ok(mut resident) = resident.lock() {
            resident.hydration_inventory = None;
        }
    }
    result
}

fn inventory_observation(
    provider: CaptureProvider,
    root: &Path,
    missing: bool,
    authority: Option<&ProviderSourceRoot>,
    leaves: &[JsonlFamilyLeaf],
) -> Result<SourceInventoryObservation> {
    let mut digest = Sha256::new();
    digest.update(FAMILY_INVENTORY_DOMAIN);
    digest.update([u8::from(missing)]);
    digest.update((leaves.len() as u64).to_be_bytes());
    if let Some(authority) = authority {
        digest.update(authority.authority_fingerprint());
    }
    for leaf in leaves {
        digest.update(leaf.source.exact_descriptor_digest());
        digest.update(
            (leaf.authority_path.as_os_str().as_encoded_bytes().len() as u64).to_be_bytes(),
        );
        digest.update(leaf.authority_path.as_os_str().as_encoded_bytes());
        digest.update(binding_digest(leaf)?);
    }
    let revision = digest.finalize().to_vec();
    SourceInventoryObservation::new(
        provider.as_str(),
        FAMILY_INVENTORY_AUTHORITY,
        TypedKey::bytes(root.as_os_str().as_encoded_bytes().to_vec()).map_err(contract_error)?,
        FAMILY_INVENTORY_REVISION,
        revision,
    )
    .map_err(contract_error)
}

fn binding_digest(leaf: &JsonlFamilyLeaf) -> Result<[u8; 32]> {
    Ok(Sha256::digest(serde_json::to_vec(leaf.binding())?).into())
}

fn discover(adapter: &dyn JsonlFamilyAdapter, root: &Path) -> Result<JsonlFamilyInventory> {
    #[cfg(test)]
    FAMILY_DISCOVERIES.with(|count| count.set(count.get().saturating_add(1)));
    adapter.discover(root)
}

fn checked_increment(value: u64) -> Result<u64> {
    value.checked_add(1).ok_or(CaptureError::SystemInvariant(
        "JSONL work counter overflowed",
    ))
}

fn route_invalid(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}

fn route_internal(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
}

fn hydration_error(kind: HydrationFailureKind, detail: impl std::fmt::Display) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.to_string(),
    }
}

fn contract_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
