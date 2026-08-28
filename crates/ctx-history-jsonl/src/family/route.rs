use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use super::{
    observe_opened_file, observe_opened_file_allow_append, revalidate_frozen_prefix,
    JsonlCheckpoint, JsonlFileObservation, JsonlOversizedRecordPolicy, JsonlPhysicalDigest,
    JsonlPhysicalEncoding, JsonlPhysicalStream, JsonlProbe, JsonlRecordFraming,
    JsonlResumableSha256, OpenedProviderSourceFile, OpenedProviderSourcePath,
    ProviderSourceDirectory, ProviderSourceRoot,
};
use super::{
    JsonlFamilyError, JsonlFamilyRuntime, JsonlResult, JsonlRuntimeError, JsonlRuntimeLookup,
};
use chrono::{DateTime, Utc};
use ctx_history_capture_runtime::SourceBackedRouteErrorKind;
use ctx_history_capture_runtime::{
    SourceBackedGenerationSink, SourceBackedRecordRejectionDrafts, SourceBackedRevalidationTarget,
    SourceBackedRouteError, SourceBackedRouteResult,
};
use ctx_history_core::{
    CaptureProvider, CertifiedSource, CertifiedSourceDeletion, CertifiedSourceInventory,
    CoreRecord, ProjectionContractError, SourceFrontier, SourceInventoryObservation, SourceKey,
    TypedKey,
};
use ctx_history_source_io::{
    open_provider_source_path_mapped as open_provider_source_path, opened_file_prefix_sha256,
    PROVIDER_JSONL_INVENTORY_MAX_DEPTH, PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
    PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const FAMILY_POLICY_REVISION: &str = "borrowed-jsonl-certified-append-v2-semantic-page-fit";
const FAMILY_FRONTIER_KIND: &str = "borrowed-jsonl-family-checkpoint-v1";
const FAMILY_SOURCE_REVISION_KIND: &str = "borrowed-jsonl-file-observation-v1";
const FAMILY_INVENTORY_AUTHORITY: &str = "borrowed-jsonl-provider-root-v1";
const FAMILY_INVENTORY_REVISION: &str = "borrowed-jsonl-inventory-v1";
const FAMILY_DISCOVERY_REVISION: &str = "borrowed-jsonl-discovery-v1";
const FAMILY_INVENTORY_DOMAIN: &[u8] = b"ctx-borrowed-jsonl-inventory-v1\0";
pub const JSONL_FAMILY_MAX_LEAF_TERMINAL_DEPENDENCIES: usize = 8;
pub const JSONL_FAMILY_MAX_LEAF_TERMINAL_PRESENT_BYTES: usize = 1024 * 1024;
type JsonlSemanticExecutorResult<R> =
    JsonlResult<Option<Box<dyn JsonlFamilySemanticExecutor<Runtime = R>>>, JsonlRuntimeError<R>>;
type JsonlOptimizedLeafResult<R> = JsonlResult<
    Option<JsonlFamilyOptimizedLeafOutcome<JsonlRuntimeError<R>>>,
    JsonlRuntimeError<R>,
>;
pub type JsonlFamilyBoundLeafResult<R> =
    JsonlResult<Option<JsonlFamilyLeaf<JsonlRuntimeError<R>>>, JsonlRuntimeError<R>>;
type JsonlPartialLeavesResult<R> =
    JsonlResult<Option<Vec<JsonlFamilyLeaf<JsonlRuntimeError<R>>>>, JsonlRuntimeError<R>>;
mod leaf;
#[cfg(any(test, feature = "test-support"))]
pub use leaf::checkpoint_admitted_revision_for_test;
#[cfg(test)]
use leaf::family_scanner_worker_count_policy;
use leaf::{base_for_leaf, decode_checkpoint, scan_leaves, TerminalSourceEvidence};
#[cfg(test)]
use leaf::{prepare_leaf, JsonlLeafOutput, JsonlLeafOutputEvent};
mod errors;
use errors::{
    contract_error, normalized_jsonl_error_kind, route_discovery, route_internal, route_invalid,
    route_scan,
};
mod ownership;
use ownership::{
    base_sources_for_route, quarantined_member_is_route_local, route_local_disposition_counts,
};
mod membership;
pub use membership::{JsonlFamilyAppendTrustContract, JsonlFamilyMembershipObservation};
mod projector;
pub use projector::{JsonlFamilyProjector, JsonlFamilyProjectorPreflightError};
mod resident;
use resident::{AuthenticatedSourceObservation, FamilyResident};
mod revalidation;
#[cfg(test)]
use revalidation::revalidate_target;
#[cfg(any(test, feature = "test-support"))]
pub use revalidation::set_before_jsonl_terminal_physical_revalidation_hook;
use revalidation::{
    binding_digest, continuation_binding_digest, inventory_observation, reset_terminal,
    revalidate_complete_inventory, revalidate_target_fallible,
};
mod scanner;
#[cfg(test)]
pub(crate) use scanner::with_family_scanner_workers;
#[cfg(test)]
use scanner::{
    jsonl_family_scanner_activity, jsonl_family_scanner_probe,
    record_jsonl_family_scanner_activity, JsonlFamilyScannerActivity, JsonlFamilyScannerProbe,
    FAMILY_SCANNER_WORKERS_OVERRIDE,
};
use scanner::{physical_identity, source_observation};
pub use scanner::{
    JsonlFamilyAppendMode, JsonlFamilyExecutionIo, JsonlFamilyExecutionPosition,
    JsonlFamilyOptimizedLeafOutcome, JsonlFamilyProjectionMode, JsonlFamilyPublication,
    JsonlFamilySemanticPage, JsonlFamilySemanticPreflight, JsonlFamilySemanticSummary,
    JsonlFamilyWorkerContext,
};
mod terminal;
pub use terminal::JsonlFamilyTerminalProof;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlFamilyRootMissingMode {
    /// A missing provider-owned root is not evidence that every prior source
    /// was deleted; leave the route unavailable.
    Unavailable,
    /// One explicitly registered authority disappeared. Certify an empty
    /// inventory so the shared family can delete its formerly owned sources.
    AuthoritativeEmpty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlFamilyInventoryMode {
    /// The complete discovered tree must remain byte-for-byte identical from
    /// opening through terminal revalidation.
    Exact,
    /// The opening membership is the generation boundary. Captured members
    /// must retain their certified ordinary-file prefixes, deleted members
    /// must remain absent, and newly discovered members are deferred to the
    /// next refresh.
    FrozenOpeningAllowAdditions,
}

/// One exact workset member opened beneath a retained provider root. Shared
/// JSONL owns the filesystem capability; adapters may bind only provider
/// identity and semantic state to it.
pub struct JsonlFamilyOpenedMember<'a, E: JsonlFamilyError> {
    source_path: PathBuf,
    authority_path: PathBuf,
    authority: Arc<ProviderSourceRoot<E>>,
    opened: &'a OpenedProviderSourceFile<E>,
    observation: JsonlFileObservation,
}

impl<'a, E: JsonlFamilyError> JsonlFamilyOpenedMember<'a, E> {
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub fn authority_path(&self) -> &Path {
        &self.authority_path
    }

    pub fn authority(&self) -> &Arc<ProviderSourceRoot<E>> {
        &self.authority
    }

    pub fn opened(&self) -> &'a OpenedProviderSourceFile<E> {
        self.opened
    }

    pub fn observation(&self) -> &JsonlFileObservation {
        &self.observation
    }
}

pub trait JsonlFamilySemanticExecutor: Send {
    type Runtime: JsonlFamilyRuntime;

    /// Runs before writer staging or record emission. Append executors may ask
    /// the family to reopen and retry this leaf once as a replacement.
    fn preflight(
        &mut self,
        input: &mut JsonlFamilyExecutionIo<Self::Runtime>,
    ) -> JsonlResult<JsonlFamilySemanticPreflight, JsonlRuntimeError<Self::Runtime>>;

    /// Produces one bounded semantic page from shared-owned physical input.
    /// Returning `None` means the input is exhausted and no page remains.
    fn next_page(
        &mut self,
        input: &mut JsonlFamilyExecutionIo<Self::Runtime>,
        worker: &mut JsonlFamilyWorkerContext<Self::Runtime>,
    ) -> JsonlResult<Option<JsonlFamilySemanticPage>, JsonlRuntimeError<Self::Runtime>>;

    /// Returns only semantic classification and opaque continuation state.
    fn finish(
        self: Box<Self>,
    ) -> JsonlResult<JsonlFamilySemanticSummary, JsonlRuntimeError<Self::Runtime>>;
}

pub trait JsonlFamilyAdapter: Send + Sync {
    type Runtime: JsonlFamilyRuntime;

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

    /// Selects the physical record framing for ordinary JSONL leaves. The
    /// family copies this policy into the reader once when the leaf opens;
    /// whole-record leaves retain their separate exact-file behavior.
    fn record_framing(&self) -> JsonlRecordFraming {
        JsonlRecordFraming::ordinary()
    }

    /// Selects the bounded physical units owned by the shared reader. Raw
    /// JSONL remains the compatibility default; adapters may select
    /// concatenated checksummed Zstandard frames per leaf.
    fn physical_encoding(
        &self,
        _leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlPhysicalEncoding {
        JsonlPhysicalEncoding::RawJsonl
    }

    /// Binds the complete admitted EOF, including an unfinished tail, with a
    /// raw SHA-256 digest owned and revalidated by the shared family.
    fn bind_admitted_eof(&self) -> bool {
        false
    }

    /// Explicit provider trust contract: the source authority promises that a
    /// retained object can only be extended. The shared reader rejects object
    /// replacement and truncation, but this contract—not filesystem metadata—
    /// is the authority excluding same-object rewrite plus growth.
    /// Explicitly opts this adapter into same-object physical continuation.
    /// Implementations must still admit each leaf and checkpoint separately.
    fn append_only_same_object_v1(&self) -> bool {
        false
    }

    fn append_trust_contract(&self) -> JsonlFamilyAppendTrustContract {
        if self.append_only_same_object_v1() {
            JsonlFamilyAppendTrustContract::AppendOnlySameObjectV1
        } else {
            JsonlFamilyAppendTrustContract::StrictPrefixAuthentication
        }
    }

    fn accepts_direct_append_checkpoint(&self, _checkpoint: &TypedKey) -> bool {
        false
    }

    fn allows_direct_append_for_leaf(
        &self,
        _leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
    ) -> bool {
        false
    }

    fn oversized_record_policy(&self) -> JsonlOversizedRecordPolicy {
        JsonlOversizedRecordPolicy::RejectSource
    }

    fn root_missing_mode(&self) -> JsonlFamilyRootMissingMode {
        JsonlFamilyRootMissingMode::Unavailable
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::Exact
    }

    fn discover(
        &self,
        root: &Path,
    ) -> JsonlResult<
        JsonlFamilyInventory<JsonlRuntimeError<Self::Runtime>>,
        JsonlRuntimeError<Self::Runtime>,
    >;

    /// Retained roots under which a bounded member workset may be opened.
    /// Returning `None` selects the existing exhaustive path.
    fn partial_member_roots(&self, _root: &Path) -> Option<Vec<PathBuf>> {
        None
    }

    /// Binds provider identity and semantic state to one securely opened
    /// member. Returning `None` conservatively selects exhaustive discovery.
    fn bind_partial_member(
        &self,
        _member: &JsonlFamilyOpenedMember<'_, JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlFamilyBoundLeafResult<Self::Runtime> {
        Ok(None)
    }

    /// Prepares provider-owned exhaustive discovery only after partial
    /// admission proved insufficient.
    fn prepare_partial_member_fallback(&self) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>> {
        Ok(())
    }

    /// Observes only physical route membership. Implementations must not parse
    /// identities or hash transcript bodies; content authority belongs to the
    /// task-local terminal proofs returned by leaf scans.
    fn observe_terminal_membership(
        &self,
        root: &Path,
        opening: &JsonlFamilyInventory<JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlResult<
        JsonlFamilyMembershipObservation<JsonlRuntimeError<Self::Runtime>>,
        JsonlRuntimeError<Self::Runtime>,
    > {
        JsonlFamilyMembershipObservation::observe(root, opening)
    }

    fn discovery_error_kind(
        &self,
        _error: &JsonlRuntimeError<Self::Runtime>,
    ) -> SourceBackedRouteErrorKind {
        SourceBackedRouteErrorKind::InvalidSource
    }

    fn scan_error_kind(
        &self,
        _error: &JsonlRuntimeError<Self::Runtime>,
    ) -> SourceBackedRouteErrorKind {
        SourceBackedRouteErrorKind::InvalidSource
    }

    /// Applies a deterministic provider-declared dependency order before the
    /// shared family scheduler starts any leaf workers. Adapters may reorder
    /// the supplied leaves but must not add or remove them.
    fn order_leaf_scans(
        &self,
        _leaves: &mut [JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>],
    ) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>> {
        Ok(())
    }

    /// Performs adapter-owned preparation that must complete before any leaf
    /// worker starts and may conservatively cap this capture's worker count.
    /// The default has no preparation and keeps the shared scheduler budget.
    fn prepare_leaf_scans(
        &self,
        _leaves: &[JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>],
        _bases: &HashMap<[u8; 32], &CertifiedSource>,
    ) -> JsonlResult<Option<usize>, JsonlRuntimeError<Self::Runtime>> {
        Ok(None)
    }

    /// Returns the dependency phase for one leaf after `prepare_leaf_scans`.
    /// The shared scheduler runs every leaf in a phase concurrently, joins all
    /// of those workers, and only then starts the next phase. Adapters that use
    /// this hook must order leaves by nondecreasing phase. The default keeps
    /// every leaf in one fully parallel phase.
    fn leaf_scan_phase(
        &self,
        _leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlResult<usize, JsonlRuntimeError<Self::Runtime>> {
        Ok(0)
    }

    /// Returns an independent dependency partition for one leaf. When every
    /// selected leaf has a partition, the shared scheduler admits a bounded
    /// wave of partitions and runs each dependency-phase frontier across that
    /// wave on fixed logical cache lanes. Partition-local adapter state remains
    /// live from the begin hook through the matching finish hook.
    fn leaf_scan_partition(
        &self,
        _leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlResult<Option<u64>, JsonlRuntimeError<Self::Runtime>> {
        Ok(None)
    }

    /// Conservatively narrows the shared maximum of 16 simultaneously live
    /// dependency partitions. Adapters may lower but never raise the shared
    /// ceiling; returning zero is invalid.
    fn leaf_scan_partition_wave_limit(&self) -> usize {
        16
    }

    /// Prepares partition-local state immediately before its first leaf runs.
    fn begin_leaf_scan_partition(
        &self,
        _partition: u64,
    ) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>> {
        Ok(())
    }

    /// Releases partition-local state after all of its leaves have joined.
    fn finish_leaf_scan_partition(
        &self,
        _partition: u64,
    ) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>> {
        Ok(())
    }

    /// Pins unpartitioned leaves to one persistent worker-state slot across
    /// dependency phases. Partitioned scans use size-balanced frontier lanes
    /// instead. Equal affinities must denote leaves that may safely serialize
    /// on one worker; the default leaves assignment round-robin.
    fn leaf_worker_affinity(
        &self,
        _leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlResult<Option<u64>, JsonlRuntimeError<Self::Runtime>> {
        Ok(None)
    }

    /// Releases adapter-owned scan-only state after all leaf workers have
    /// joined. Terminal source and inventory revalidation must keep only the
    /// evidence they need beyond this boundary.
    fn finish_leaf_scans(&self) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>> {
        Ok(())
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
        _source_file: Arc<OpenedProviderSourceFile<JsonlRuntimeError<Self::Runtime>>>,
        _imported_at: DateTime<Utc>,
    ) -> JsonlResult<
        Box<dyn JsonlFamilyProjector<Runtime = Self::Runtime>>,
        JsonlRuntimeError<Self::Runtime>,
    > {
        Err(JsonlRuntimeError::<Self::Runtime>::system_invariant(
            "missing JSONL projector",
        ))
    }

    /// Constructs a projector for a cold/replacement scan or from the opaque
    /// provider state persisted at the validated prefix frontier. Any scan with
    /// an exact prior source receives an event-identity lookup pinned to the
    /// writer base. `mode` distinguishes append continuation from replacement
    /// reconciliation; cold scans receive no lookup.
    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
        source_file: Arc<OpenedProviderSourceFile<JsonlRuntimeError<Self::Runtime>>>,
        imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        _base_event_lookup: Option<JsonlRuntimeLookup<Self::Runtime>>,
        _mode: JsonlFamilyProjectionMode,
    ) -> JsonlResult<
        Box<dyn JsonlFamilyProjector<Runtime = Self::Runtime>>,
        JsonlRuntimeError<Self::Runtime>,
    > {
        if checkpoint.is_some() {
            return Err(JsonlRuntimeError::<Self::Runtime>::invalid_payload(
                "JSONL adapter does not accept provider checkpoint state".to_owned(),
            ));
        }
        self.projector(leaf, source_file, imported_at)
    }

    /// Optional bounded semantic executor. The family has already selected the
    /// physical projection mode and retains all lifecycle/publication authority.
    fn semantic_executor(
        &self,
        _leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
        _checkpoint: Option<&TypedKey>,
        _base_event_lookup: Option<JsonlRuntimeLookup<Self::Runtime>>,
        _mode: JsonlFamilyProjectionMode,
    ) -> JsonlSemanticExecutorResult<Self::Runtime> {
        Ok(None)
    }

    /// Removes one unit of provider-declared optional checkpoint evidence.
    /// The shared family calls this only when the completed FamilyCheckpoint
    /// fails the real SourceFrontier typed-key contract. Returning `None`
    /// omits provider continuation state, so the next mutation safely retries
    /// replacement instead of making cold ingestion fail. Durable provider
    /// authority must never be removed by this hook.
    fn shed_optional_provider_checkpoint_evidence(
        &self,
        _checkpoint: &TypedKey,
    ) -> JsonlResult<Option<TypedKey>, JsonlRuntimeError<Self::Runtime>> {
        Ok(None)
    }

    /// Legacy optimized execution retained for adapters outside the Codex
    /// convergence tranche. New adapters must use `semantic_executor`.
    fn scan_optimized_leaf(
        &self,
        _leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
        _base: Option<&CertifiedSource>,
        _base_event_lookup: &JsonlRuntimeLookup<Self::Runtime>,
        _worker: &mut JsonlFamilyWorkerContext<Self::Runtime>,
        _emit_page: &mut dyn FnMut(
            JsonlFamilyPublication,
            u64,
            Vec<CoreRecord>,
        ) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlOptimizedLeafResult<Self::Runtime> {
        Ok(None)
    }

    /// Resolves the ordinary path represented by a committed base. Optimized
    /// adapters with their own bounded frontier format may override this; the
    /// default decodes the shared family checkpoint.
    fn base_source_path(
        &self,
        certificate: &CertifiedSource,
    ) -> JsonlResult<PathBuf, JsonlRuntimeError<Self::Runtime>> {
        default_base_source_path(self, certificate)
    }

    fn owns(&self, source: &SourceKey) -> bool {
        source.provider() == self.provider().as_str()
            && source.source_format() == self.source_format()
            && source.schema_variant() == self.schema_variant()
            && source.provider_identity_version() == 1
    }
}

#[derive(Debug)]
struct JsonlFamilyExactPresentDependency<E: JsonlFamilyError> {
    source_path: PathBuf,
    authority_path: PathBuf,
    authority: Arc<ProviderSourceRoot<E>>,
    observation: JsonlFileObservation,
    content_length: u64,
    content_sha256: [u8; 32],
}

impl<E: JsonlFamilyError> Clone for JsonlFamilyExactPresentDependency<E> {
    fn clone(&self) -> Self {
        Self {
            source_path: self.source_path.clone(),
            authority_path: self.authority_path.clone(),
            authority: Arc::clone(&self.authority),
            observation: self.observation.clone(),
            content_length: self.content_length,
            content_sha256: self.content_sha256,
        }
    }
}

impl<E: JsonlFamilyError> JsonlFamilyExactPresentDependency<E> {
    fn revalidate(&self) -> JsonlResult<(), E> {
        let opened = self.authority.open_file(&self.authority_path)?;
        if observe_opened_file(&self.source_path, &opened)? != self.observation {
            return Err(E::source_changed());
        }
        let content_length = usize::try_from(self.content_length).map_err(|_| {
            E::invalid_payload("JSONL exact dependency length exceeds usize".to_owned())
        })?;
        let content = opened.read_exact_range(
            0,
            content_length,
            JSONL_FAMILY_MAX_LEAF_TERMINAL_PRESENT_BYTES,
        )?;
        if u64::try_from(content.len()).ok() != Some(self.content_length)
            || <[u8; 32]>::from(Sha256::digest(content)) != self.content_sha256
        {
            return Err(E::source_changed());
        }
        Ok(())
    }
}

#[derive(Debug)]
struct JsonlFamilyExactAbsentDependency<E: JsonlFamilyError> {
    source_path: PathBuf,
    authority_path: PathBuf,
    authority: Arc<ProviderSourceRoot<E>>,
}

impl<E: JsonlFamilyError> Clone for JsonlFamilyExactAbsentDependency<E> {
    fn clone(&self) -> Self {
        Self {
            source_path: self.source_path.clone(),
            authority_path: self.authority_path.clone(),
            authority: Arc::clone(&self.authority),
        }
    }
}

impl<E: JsonlFamilyError> JsonlFamilyExactAbsentDependency<E> {
    fn remains_absent(&self) -> JsonlResult<bool, E> {
        match self.authority.open_path(&self.authority_path) {
            Ok(_) => Ok(false),
            Err(error) if error.is_not_found() => Ok(true),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug)]
struct JsonlFamilyLeafTerminalDependencies<E: JsonlFamilyError> {
    present: Vec<JsonlFamilyExactPresentDependency<E>>,
    absent: Vec<JsonlFamilyExactAbsentDependency<E>>,
}

impl<E: JsonlFamilyError> Default for JsonlFamilyLeafTerminalDependencies<E> {
    fn default() -> Self {
        Self {
            present: Vec::new(),
            absent: Vec::new(),
        }
    }
}

impl<E: JsonlFamilyError> Clone for JsonlFamilyLeafTerminalDependencies<E> {
    fn clone(&self) -> Self {
        Self {
            present: self.present.clone(),
            absent: self.absent.clone(),
        }
    }
}

impl<E: JsonlFamilyError> JsonlFamilyLeafTerminalDependencies<E> {
    fn revalidate(&self) -> JsonlResult<bool, E> {
        for dependency in &self.present {
            dependency.revalidate()?;
        }
        for dependency in &self.absent {
            if !dependency.remains_absent()? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn is_empty(&self) -> bool {
        self.present.is_empty() && self.absent.is_empty()
    }

    fn contains_path(&self, authority_path: &Path) -> bool {
        self.present
            .iter()
            .any(|dependency| dependency.authority_path == authority_path)
            || self
                .absent
                .iter()
                .any(|dependency| dependency.authority_path == authority_path)
    }

    fn ensure_additional_capacity(&self) -> JsonlResult<(), E> {
        let count = self
            .present
            .len()
            .checked_add(self.absent.len())
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| {
                E::invalid_payload("JSONL leaf terminal dependency count overflowed".to_owned())
            })?;
        if count > JSONL_FAMILY_MAX_LEAF_TERMINAL_DEPENDENCIES {
            return Err(E::invalid_payload(format!(
                "JSONL leaf exceeds the {JSONL_FAMILY_MAX_LEAF_TERMINAL_DEPENDENCIES} terminal dependency limit"
            )));
        }
        Ok(())
    }

    fn present_content_bytes(&self) -> JsonlResult<usize, E> {
        self.present.iter().try_fold(0_usize, |total, dependency| {
            let length = usize::try_from(dependency.content_length).map_err(|_| {
                E::invalid_payload("JSONL exact dependency length exceeds usize".to_owned())
            })?;
            total.checked_add(length).ok_or_else(|| {
                E::invalid_payload("JSONL exact dependency byte count overflowed".to_owned())
            })
        })
    }
}

#[derive(Debug)]
pub struct JsonlFamilyLeaf<E: JsonlFamilyError> {
    source: SourceKey,
    source_path: PathBuf,
    authority_path: PathBuf,
    authority: Arc<ProviderSourceRoot<E>>,
    observation: JsonlFileObservation,
    logical_eof: Option<u64>,
    binding: TypedKey,
    terminal_dependencies: JsonlFamilyLeafTerminalDependencies<E>,
    identity_probe: Option<JsonlProbe>,
    identity_probe_rejected_records: u64,
    whole_record: bool,
    freeze_observation_at_scan: bool,
}

mod leaf_model;

mod inventory;
use inventory::exact_member_authority;
pub use inventory::JsonlFamilyInventory;
mod member;
pub use member::{
    JsonlFamilyInventoryMember, JsonlFamilyLeafDisposition, JsonlFamilyPendingLeaf,
    JsonlFamilyPhysicalSourceIdentity, JsonlFamilyRejectedLeaf,
};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FamilyCheckpoint {
    version: u32,
    provider_parser_revision: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    event_identity_revision: String,
    binding_digest: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exact_terminal_binding_digest: Option<[u8; 32]>,
    physical: JsonlCheckpoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    admitted_eof_sha256: Option<[u8; 32]>,
    #[serde(default, skip_serializing_if = "is_false")]
    complete_prefix_ends_with_terminal_nul_padding: bool,
    represented_physical_records: u64,
    rejected_records: u64,
    #[serde(default)]
    logical_complete_records: u64,
    #[serde(default)]
    rejected_logical_records: u64,
    indexed_documents: u64,
    provider_checkpoint: Option<TypedKey>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl FamilyCheckpoint {
    const VERSION: u32 = 5;

    fn encode_frontier_key<E: JsonlFamilyError>(&self) -> JsonlResult<TypedKey, E> {
        TypedKey::utf8(serde_json::to_string(self)?)
            .map_err(|error| E::invalid_payload(error.to_string()))
    }

    fn decode_frontier_key<E: JsonlFamilyError>(key: &TypedKey) -> JsonlResult<Self, E> {
        match key {
            // Bytes was emitted before the compact UTF-8 representation. Both
            // carry the same versioned JSON document and remain readable.
            TypedKey::Bytes(bytes) => Ok(serde_json::from_slice(bytes)?),
            TypedKey::Utf8(json) => Ok(serde_json::from_str(json)?),
            _ => Err(E::invalid_payload(
                "JSONL base checkpoint is malformed".to_owned(),
            )),
        }
    }

    fn fits_frontier_key<E: JsonlFamilyError>(&self) -> JsonlResult<bool, E> {
        let json = serde_json::to_string(self)?;
        let key = match TypedKey::utf8(json) {
            Ok(key) => key,
            Err(ProjectionContractError::FieldTooLarge { .. }) => return Ok(false),
            Err(error) => return Err(E::invalid_payload(error.to_string())),
        };
        match SourceFrontier::new(
            FAMILY_FRONTIER_KIND,
            key,
            self.physical.complete_prefix_end(),
            *self.physical.complete_prefix_sha256(),
        ) {
            Ok(_) => Ok(true),
            Err(ProjectionContractError::FieldTooLarge { .. }) => Ok(false),
            Err(error) => Err(E::invalid_payload(error.to_string())),
        }
    }

    fn exact_admitted_eof_sha256(&self) -> Option<[u8; 32]> {
        self.admitted_eof_sha256
            .or_else(|| self.physical.admitted_eof_sha256())
    }

    fn authenticates_admitted_eof(&self) -> bool {
        self.exact_admitted_eof_sha256().is_some() || self.physical.authenticates_admitted_eof()
    }

    fn exact_terminal_binding_matches<E: JsonlFamilyError>(
        &self,
        leaf: &JsonlFamilyLeaf<E>,
    ) -> bool {
        exact_terminal_binding_digest(leaf)
            .is_ok_and(|digest| self.exact_terminal_binding_digest == digest)
    }

    fn valid_for<R: JsonlFamilyRuntime>(
        &self,
        adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
        leaf: &JsonlFamilyLeaf<JsonlRuntimeError<R>>,
    ) -> bool {
        self.version == Self::VERSION
            && self.provider_parser_revision == adapter.parser_revision()
            && self.event_identity_revision == adapter.event_identity_revision()
            && continuation_binding_digest(leaf).is_ok_and(|digest| self.binding_digest == digest)
            && self.physical.is_internally_consistent()
            && self.physical.identity() == &physical_identity(adapter, leaf)
            // A provider-declared logical EOF is authoritative only when the
            // shared framer reached that exact byte after a complete record.
            // The retained physical observation may still extend beyond it.
            && self.physical.logical_eof().is_none_or(|logical_eof| {
                self.physical.complete_prefix_end() == logical_eof
            })
            && (self.admitted_eof_sha256.is_some()
                == (adapter.bind_admitted_eof() || leaf.logical_eof.is_some()))
            && self
                .provider_checkpoint
                .as_ref()
                .is_none_or(|checkpoint| checkpoint.validate_contract().is_ok())
            && self
                .represented_physical_records
                .checked_add(self.rejected_records)
                .is_some_and(|classified| classified <= self.physical.next_physical_ordinal())
            && self
                .indexed_documents
                .checked_add(self.rejected_logical_records)
                .is_some_and(|classified| classified <= self.logical_complete_records)
    }
}

fn exact_terminal_binding_digest<E: JsonlFamilyError>(
    leaf: &JsonlFamilyLeaf<E>,
) -> JsonlResult<Option<[u8; 32]>, E> {
    if leaf.logical_eof.is_none() && leaf.terminal_dependencies.is_empty() {
        return Ok(None);
    }
    binding_digest(leaf).map(Some)
}

#[derive(Debug)]
struct JsonlFamilyAbsentMember<E: JsonlFamilyError> {
    source_path: PathBuf,
    authority: Option<Arc<ProviderSourceRoot<E>>>,
    authority_path: PathBuf,
}

impl<E: JsonlFamilyError> Clone for JsonlFamilyAbsentMember<E> {
    fn clone(&self) -> Self {
        Self {
            source_path: self.source_path.clone(),
            authority: self.authority.as_ref().map(Arc::clone),
            authority_path: self.authority_path.clone(),
        }
    }
}

impl<E: JsonlFamilyError> JsonlFamilyAbsentMember<E> {
    fn from_path(opening: &JsonlFamilyInventory<E>, source_path: PathBuf) -> Option<Self> {
        if opening
            .authorities
            .iter()
            .any(|authority| source_path == authority.named_path())
        {
            return None;
        }
        let relative = opening.authorities.iter().find_map(|authority| {
            source_path
                .strip_prefix(authority.named_path())
                .ok()
                .filter(|path| !path.as_os_str().is_empty())
                .map(|path| (Arc::clone(authority), path.to_path_buf()))
        });
        Some(match relative {
            Some((authority, authority_path)) => Self {
                source_path,
                authority: Some(authority),
                authority_path,
            },
            None => Self {
                authority_path: PathBuf::new(),
                source_path,
                authority: None,
            },
        })
    }

    fn remains_absent(&self) -> JsonlResult<bool, E> {
        let opened = match &self.authority {
            Some(authority) => authority.open_path(&self.authority_path),
            None => open_provider_source_path::<E>(&self.source_path),
        };
        match opened {
            Ok(_) => Ok(false),
            Err(error) if error.is_not_found() => Ok(true),
            Err(error) => Err(error),
        }
    }
}

mod capture;
#[cfg(test)]
use capture::capture;
use capture::default_base_source_path;
pub use capture::jsonl_family_driver;
mod retirement;
use retirement::retirement_absence_dependency;

#[cfg(test)]
#[path = "route/tests.rs"]
mod tests;
