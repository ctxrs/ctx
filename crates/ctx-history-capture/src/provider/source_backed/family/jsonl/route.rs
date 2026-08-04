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
use ctx_history_index::BaseEventIdentityLookup;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    observe_opened_file, revalidate_frozen_prefix, JsonlCheckpoint, JsonlFileObservation,
    JsonlOversizedRecordPolicy, JsonlProbe, JsonlRecordRef,
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
#[cfg(test)]
use leaf::{prepare_leaf, JsonlLeafOutput, JsonlLeafOutputEvent};
mod ownership;
use ownership::base_sources_for_root;
mod revalidation;
use revalidation::{
    binding_digest, inventory_observation, reset_terminal, revalidate_complete_inventory,
    revalidate_target,
};
mod scanner;
#[cfg(test)]
use scanner::{
    jsonl_family_scanner_activity, jsonl_family_scanner_probe,
    record_jsonl_family_scanner_activity, with_family_scanner_workers, JsonlFamilyScannerActivity,
    JsonlFamilyScannerProbe, FAMILY_SCANNER_WORKERS_OVERRIDE,
};
pub(crate) use scanner::{
    JsonlFamilyAppendMode, JsonlFamilyOptimizedLeafOutcome, JsonlFamilyProjectionMode,
    JsonlFamilyPublication, JsonlFamilyWorkerContext,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonlFamilyRootMissingMode {
    /// A missing provider-owned root is not evidence that every prior source
    /// was deleted; leave the route unavailable.
    Unavailable,
    /// One explicitly registered authority disappeared. Certify an empty
    /// inventory so the shared family can delete its formerly owned sources.
    AuthoritativeEmpty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonlFamilyInventoryMode {
    /// The complete discovered tree must remain byte-for-byte identical from
    /// opening through terminal revalidation.
    Exact,
    /// The opening membership is the generation boundary. Captured members
    /// must retain their certified ordinary-file prefixes, deleted members
    /// must remain absent, and newly discovered members are deferred to the
    /// next refresh.
    FrozenOpeningAllowAdditions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonlFamilyBaseScope {
    /// Compatibility mode for family adapters whose source identity is unique
    /// across every route for that provider/schema tuple.
    ProviderFamily,
    /// Reuse only sources previously committed by this exact route. Adapters
    /// whose explicit and automatic routes can overlap must select this mode.
    Route,
}

pub(crate) trait JsonlFamilyProjector: Send {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()>;

    fn finish(&mut self) -> Result<()> {
        Ok(())
    }

    fn finish_projecting(
        &mut self,
        _worker: &mut JsonlFamilyWorkerContext,
        _emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        self.finish()
    }

    fn rejected_records(&self) -> u64 {
        0
    }

    /// Opaque, contract-bounded provider state to carry into the next certified
    /// suffix projection. The family persists the value without interpreting it.
    fn provider_checkpoint(&self) -> Result<Option<TypedKey>> {
        Ok(None)
    }
}

pub(crate) trait JsonlFamilyAdapter: Send + Sync {
    fn provider(&self) -> CaptureProvider;
    fn source_format(&self) -> &'static str;
    fn schema_variant(&self) -> &'static str;
    fn parser_revision(&self) -> &'static str;
    /// Projection-local identity scheme revision. Changing this invalidates
    /// the family checkpoint and forces a replacement scan without changing
    /// the provider parser revision recorded by Core.
    fn event_identity_revision(&self) -> &'static str {
        ""
    }
    fn append_mode(&self) -> JsonlFamilyAppendMode;

    fn oversized_record_policy(&self) -> JsonlOversizedRecordPolicy {
        JsonlOversizedRecordPolicy::RejectSource
    }

    fn root_missing_mode(&self) -> JsonlFamilyRootMissingMode {
        JsonlFamilyRootMissingMode::Unavailable
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::Exact
    }

    fn base_scope(&self) -> JsonlFamilyBaseScope {
        JsonlFamilyBaseScope::ProviderFamily
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory>;

    fn discovery_error_kind(&self, _error: &CaptureError) -> SourceBackedRouteErrorKind {
        SourceBackedRouteErrorKind::InvalidSource
    }

    fn scan_error_kind(&self, _error: &CaptureError) -> SourceBackedRouteErrorKind {
        SourceBackedRouteErrorKind::InvalidSource
    }

    /// Applies a deterministic provider-declared dependency order before the
    /// shared family scheduler starts any leaf workers. Adapters may reorder
    /// the supplied leaves but must not add or remove them.
    fn order_leaf_scans(&self, _leaves: &mut [JsonlFamilyLeaf]) -> Result<()> {
        Ok(())
    }

    /// Performs adapter-owned preparation that must complete before any leaf
    /// worker starts and may conservatively cap this capture's worker count.
    /// The default has no preparation and keeps the shared scheduler budget.
    fn prepare_leaf_scans(
        &self,
        _leaves: &[JsonlFamilyLeaf],
        _bases: &HashMap<[u8; 32], &CertifiedSource>,
    ) -> Result<Option<usize>> {
        Ok(None)
    }

    /// Returns the dependency phase for one leaf after `prepare_leaf_scans`.
    /// The shared scheduler runs every leaf in a phase concurrently, joins all
    /// of those workers, and only then starts the next phase. Adapters that use
    /// this hook must order leaves by nondecreasing phase. The default keeps
    /// every leaf in one fully parallel phase.
    fn leaf_scan_phase(&self, _leaf: &JsonlFamilyLeaf) -> Result<usize> {
        Ok(0)
    }

    /// Returns an independent dependency partition for one leaf. When every
    /// selected leaf has a partition, the shared scheduler admits a bounded
    /// wave of partitions and runs each dependency-phase frontier across that
    /// wave in parallel. Partition-local adapter state remains live from the
    /// begin hook through the matching finish hook.
    fn leaf_scan_partition(&self, _leaf: &JsonlFamilyLeaf) -> Result<Option<u64>> {
        Ok(None)
    }

    /// Prepares partition-local state immediately before its first leaf runs.
    fn begin_leaf_scan_partition(&self, _partition: u64) -> Result<()> {
        Ok(())
    }

    /// Releases partition-local state after all of its leaves have joined.
    fn finish_leaf_scan_partition(&self, _partition: u64) -> Result<()> {
        Ok(())
    }

    /// Pins unpartitioned leaves to one persistent worker-state slot across
    /// dependency phases. Partitioned scans use size-balanced frontier lanes
    /// instead. Equal affinities must denote leaves that may safely serialize
    /// on one worker; the default leaves assignment round-robin.
    fn leaf_worker_affinity(&self, _leaf: &JsonlFamilyLeaf) -> Result<Option<u64>> {
        Ok(None)
    }

    /// Releases adapter-owned scan-only state after all leaf workers have
    /// joined. Terminal source and inventory revalidation must keep only the
    /// evidence they need beyond this boundary.
    fn finish_leaf_scans(&self) -> Result<()> {
        Ok(())
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>>;

    /// Constructs a projector for a cold/replacement scan or from the opaque
    /// provider state persisted at the validated prefix frontier. Any scan with
    /// an exact prior source receives an event-identity lookup pinned to the
    /// writer base. `mode` distinguishes append continuation from replacement
    /// reconciliation; cold scans receive no lookup.
    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        _base_event_lookup: Option<BaseEventIdentityLookup>,
        _mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        if checkpoint.is_some() {
            return Err(CaptureError::InvalidPayload(
                "JSONL adapter does not accept provider checkpoint state".to_owned(),
            ));
        }
        self.projector(leaf, source_file, imported_at)
    }

    /// Optional optimized execution for one JSONL leaf.
    ///
    /// Returning `None` selects the family's ordinary framed reader and
    /// per-record projector. Returning an outcome lets an adapter retain a
    /// native prefilter/parser or a bounded staged replay when flattening that
    /// work into `project` would add passes, hashes, or unbounded buffering.
    /// The shared family still validates the terminal certificate and owns all
    /// writer publication through `emit_page`.
    fn scan_optimized_leaf(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _base: Option<&CertifiedSource>,
        _base_event_lookup: &BaseEventIdentityLookup,
        _worker: &mut JsonlFamilyWorkerContext,
        _emit_page: &mut dyn FnMut(JsonlFamilyPublication, Vec<CoreRecord>) -> Result<()>,
    ) -> Result<Option<JsonlFamilyOptimizedLeafOutcome>> {
        Ok(None)
    }

    /// Resolves the ordinary path represented by a committed base. Optimized
    /// adapters with their own bounded frontier format may override this; the
    /// default decodes the shared family checkpoint.
    fn base_source_path(&self, certificate: &CertifiedSource) -> Result<PathBuf> {
        default_base_source_path(self, certificate)
    }

    /// Revalidates one terminal leaf at commit time. Optimized adapters may
    /// retain provider-native evidence; the default uses the shared physical
    /// checkpoint and exact-prefix rules.
    fn revalidate_leaf(
        &self,
        leaf: &JsonlFamilyLeaf,
        certificate: &CertifiedSource,
        checkpoint: Option<&JsonlCheckpoint>,
    ) -> Result<bool> {
        default_revalidate_leaf(leaf, certificate, checkpoint)
    }

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

    pub(super) fn estimated_scan_bytes(&self) -> u64 {
        self.observation.length
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
    authorities: Vec<Arc<ProviderSourceRoot>>,
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
        leaves: Vec<JsonlFamilyLeaf>,
        rejected_leaves: Vec<JsonlFamilyRejectedLeaf>,
    ) -> Result<Self> {
        Self::present_multi_with_rejected(provider, root, vec![authority], leaves, rejected_leaves)
    }

    pub(crate) fn present_multi(
        provider: CaptureProvider,
        root: &Path,
        authorities: Vec<Arc<ProviderSourceRoot>>,
        leaves: Vec<JsonlFamilyLeaf>,
    ) -> Result<Self> {
        Self::present_multi_with_rejected(provider, root, authorities, leaves, Vec::new())
    }

    pub(crate) fn present_multi_with_rejected(
        provider: CaptureProvider,
        root: &Path,
        mut authorities: Vec<Arc<ProviderSourceRoot>>,
        mut leaves: Vec<JsonlFamilyLeaf>,
        mut rejected_leaves: Vec<JsonlFamilyRejectedLeaf>,
    ) -> Result<Self> {
        if authorities.is_empty() {
            return Err(CaptureError::InvalidPayload(
                "present JSONL inventory has no retained root authority".to_owned(),
            ));
        }
        authorities.sort_by(|left, right| left.named_path().cmp(right.named_path()));
        for pair in authorities.windows(2) {
            if pair[0].named_path() == pair[1].named_path() {
                return Err(CaptureError::InvalidPayload(format!(
                    "present JSONL inventory has duplicate root authority {}",
                    pair[0].named_path().display()
                )));
            }
        }
        for leaf in &leaves {
            let retained = authorities.iter().any(|authority| {
                authority.named_path() == leaf.authority.named_path()
                    && authority.authority_fingerprint() == leaf.authority.authority_fingerprint()
            });
            if !retained {
                return Err(CaptureError::InvalidPayload(format!(
                    "JSONL leaf {} is outside the retained root authorities",
                    leaf.source_path.display()
                )));
            }
        }
        leaves.sort_by(|left, right| left.source_path.cmp(&right.source_path));
        rejected_leaves.sort_by(|left, right| left.source_path.cmp(&right.source_path));
        let observation = inventory_observation(
            provider,
            root,
            false,
            &authorities,
            &leaves,
            &rejected_leaves,
        )?;
        Ok(Self {
            root_missing: false,
            observation,
            authorities,
            leaves,
            rejected_leaves,
        })
    }

    pub(crate) fn missing(provider: CaptureProvider, root: &Path) -> Result<Self> {
        Ok(Self {
            root_missing: true,
            observation: inventory_observation(provider, root, true, &[], &[], &[])?,
            authorities: Vec::new(),
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

    #[cfg(test)]
    fn certify_against(&self, closing: &Self) -> Result<CertifiedSourceInventory> {
        self.certify_selected_against(
            closing,
            closing
                .leaves
                .iter()
                .map(|leaf| leaf.source.clone())
                .collect(),
        )
    }

    fn certify_selected_against(
        &self,
        closing: &Self,
        sources: Vec<SourceKey>,
    ) -> Result<CertifiedSourceInventory> {
        if self.root_missing != closing.root_missing {
            return Err(CaptureError::InvalidPayload(
                "JSONL root availability changed during capture".to_owned(),
            ));
        }
        CertifiedSourceInventory::certify(
            self.observation.clone(),
            closing.observation.clone(),
            FAMILY_DISCOVERY_REVISION,
            sources,
        )
        .map_err(contract_error)
    }

    fn revalidate_root(&self) -> Result<()> {
        if self.root_missing {
            return Ok(());
        }
        if self.authorities.is_empty() {
            return Err(CaptureError::InvalidPayload(
                "JSONL inventory has no retained root authority".to_owned(),
            ));
        }
        for authority in &self.authorities {
            authority.revalidate()?;
        }
        Ok(())
    }

    fn revalidate_root_same_object(&self) -> Result<()> {
        if self.root_missing {
            return Ok(());
        }
        if self.authorities.is_empty() {
            return Err(CaptureError::InvalidPayload(
                "JSONL inventory has no retained root authority".to_owned(),
            ));
        }
        for authority in &self.authorities {
            authority.revalidate_same_object()?;
        }
        Ok(())
    }

    fn retains_authorities_from(&self, opening: &Self) -> bool {
        self.root_missing == opening.root_missing
            && opening.authorities.iter().all(|expected| {
                self.authorities.iter().any(|current| {
                    current.named_path() == expected.named_path()
                        && current.same_object_as(expected)
                })
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FamilyCheckpoint {
    version: u32,
    provider_parser_revision: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    event_identity_revision: String,
    binding_digest: [u8; 32],
    physical: JsonlCheckpoint,
    represented_physical_records: u64,
    rejected_records: u64,
    indexed_documents: u64,
    provider_checkpoint: Option<TypedKey>,
}

impl FamilyCheckpoint {
    const VERSION: u32 = 4;

    fn valid_for(&self, adapter: &dyn JsonlFamilyAdapter, leaf: &JsonlFamilyLeaf) -> bool {
        self.version == Self::VERSION
            && self.provider_parser_revision == adapter.parser_revision()
            && self.event_identity_revision == adapter.event_identity_revision()
            && binding_digest(leaf).is_ok_and(|digest| self.binding_digest == digest)
            && self.physical.is_internally_consistent()
            && self.physical.identity() == &physical_identity(adapter, leaf)
            && self
                .provider_checkpoint
                .as_ref()
                .is_none_or(|checkpoint| checkpoint.validate_contract().is_ok())
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

fn default_base_source_path(
    adapter: &(impl JsonlFamilyAdapter + ?Sized),
    certificate: &CertifiedSource,
) -> Result<PathBuf> {
    certificate.validate_contract().map_err(contract_error)?;
    if certificate.parser_revision() != adapter.parser_revision() {
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
    if checkpoint.physical.identity().source_descriptor_digest()
        != &certificate.observation().source().exact_descriptor_digest()
    {
        return Err(CaptureError::InvalidPayload(
            "JSONL base checkpoint source changed".to_owned(),
        ));
    }
    Ok(checkpoint.physical.identity().source_path().clone())
}

fn default_revalidate_leaf(
    leaf: &JsonlFamilyLeaf,
    certificate: &CertifiedSource,
    checkpoint: Option<&JsonlCheckpoint>,
) -> Result<bool> {
    if leaf.whole_record {
        if source_observation(leaf.source(), leaf.observation())? != *certificate.observation() {
            return Ok(false);
        }
        drop(leaf.open_verified()?);
    } else if let Some(checkpoint) = checkpoint {
        let (opened, _) = leaf.open_for_revalidation()?;
        revalidate_frozen_prefix(
            leaf.source_path(),
            opened.as_ref(),
            checkpoint.source_observation(),
            checkpoint.complete_prefix_end(),
            *checkpoint.complete_prefix_sha256(),
        )?;
    } else if source_observation(leaf.source(), leaf.observation())? != *certificate.observation() {
        return Ok(false);
    }
    Ok(true)
}

#[derive(Default)]
struct FamilyResident {
    ownership_initialized: bool,
    owned_sources: HashMap<[u8; 32], SourceKey>,
    terminal_sources: HashMap<[u8; 32], TerminalSourceEvidence>,
    certified_inventory: Option<CertifiedSourceInventory>,
    opening_inventory: Option<JsonlFamilyInventory>,
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
    .with_fallible_complete_inventory_revalidation(move |expected| {
        match revalidate_complete_inventory(
            terminal_adapter.as_ref(),
            &terminal_root,
            &inventory_resident,
            expected,
        ) {
            Ok(revalidated) => Ok(revalidated),
            Err(error)
                if terminal_adapter.scan_error_kind(&error)
                    == SourceBackedRouteErrorKind::SourceChanged =>
            {
                Ok(false)
            }
            Err(error) => Err(route_scan(terminal_adapter.as_ref(), error)),
        }
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
    if opening.root_missing()
        && adapter.root_missing_mode() == JsonlFamilyRootMissingMode::Unavailable
    {
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
    let bases = base_sources_for_root(adapter, &opening, root, sink)?;
    let mut selected_leaves = opening
        .leaves()
        .iter()
        .filter(|leaf| {
            adapter.base_scope() == JsonlFamilyBaseScope::ProviderFamily
                || !sink.source_owned_by_other_route(leaf.source())
        })
        .cloned()
        .collect::<Vec<_>>();
    adapter
        .order_leaf_scans(&mut selected_leaves)
        .map_err(|error| route_scan(adapter, error))?;
    let mut owned_sources = HashMap::with_capacity(bases.len() + selected_leaves.len());
    for source in bases
        .iter()
        .map(|base| base.observation().source())
        .chain(selected_leaves.iter().map(JsonlFamilyLeaf::source))
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
    let bases_by_descriptor = bases_by_descriptor(&bases)?;
    let base_event_lookup = sink.writer.base_event_identity_lookup();
    let terminal_sources = scan_leaves(
        adapter,
        &selected_leaves,
        &bases_by_descriptor,
        base_event_lookup,
        sink,
    );
    let finish_leaf_scans = adapter
        .finish_leaf_scans()
        .map_err(|error| route_scan(adapter, error));
    let terminal_sources = terminal_sources?;
    finish_leaf_scans?;

    let selected_sources = selected_leaves
        .iter()
        .map(|leaf| leaf.source().clone())
        .collect::<Vec<_>>();
    let inventory = match adapter.inventory_mode() {
        JsonlFamilyInventoryMode::Exact => {
            let closing = adapter
                .discover(root)
                .map_err(|error| route_discovery(adapter, error))?;
            opening
                .certify_selected_against(&closing, selected_sources)
                .map_err(route_invalid)?
        }
        JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions => opening
            .certify_selected_against(&opening, selected_sources)
            .map_err(route_invalid)?,
    };
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
    resident.opening_inventory = Some(opening);
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

fn route_invalid(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}

fn route_discovery(
    adapter: &dyn JsonlFamilyAdapter,
    error: CaptureError,
) -> SourceBackedRouteError {
    SourceBackedRouteError::new(adapter.discovery_error_kind(&error), error.to_string())
}

fn route_scan(adapter: &dyn JsonlFamilyAdapter, error: CaptureError) -> SourceBackedRouteError {
    SourceBackedRouteError::new(adapter.scan_error_kind(&error), error.to_string())
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
