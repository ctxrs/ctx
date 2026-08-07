use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use super::super::{
    jsonl_prefix_hash_bytes, reset_jsonl_prefix_hash_bytes, set_after_jsonl_prefix_hash_hook,
    JsonlReader,
};
use super::*;
use crate::provider::source_backed::{
    SourceBackedLogicalSourceFailures, SourceBackedRecordRejections, SourceBackedRouteResources,
};
use crate::repository_attribution::AttributionInput;
use ctx_history_core::{
    derive_event_id, derive_session_id, CoreRecord, EventIdentityInput, NativeItemKey,
    NativeSessionKey, SessionIdentityInput, SourceAnchor,
};
use ctx_history_index::{CommitReceipt, GenerationWriter, SourceRouteIdentity, WriterOptions};

const TEST_SOURCE_FORMAT: &str = "terminal_witness_jsonl";
const TEST_SCHEMA: &str = "terminal-witness-v1";

fn test_route_identity() -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256("00".repeat(32)).unwrap()
}

struct TestAdapter;

const TEST_RECORD: &[u8] = b"{\"message\":\"before\"}\n";

impl JsonlFamilyAdapter for TestAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "terminal-witness-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        if !root.exists() {
            return JsonlFamilyInventory::missing(self.provider(), root);
        }
        let authority = Arc::new(ProviderSourceRoot::open(root)?);
        let mut leaves = Vec::new();
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            let name = entry.file_name();
            let source = SourceKey::derive(
                self.provider().as_str(),
                TEST_SOURCE_FORMAT,
                TEST_SCHEMA,
                1,
                SourceAnchor::provider_native(
                    "terminal-witness-file",
                    TypedKey::bytes(name.as_encoded_bytes().to_vec()).map_err(contract_error)?,
                )
                .map_err(contract_error)?,
            )
            .map_err(contract_error)?;
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                path,
                Arc::clone(&authority),
                PathBuf::from(&name),
                TypedKey::bytes(name.as_encoded_bytes().to_vec()).map_err(contract_error)?,
            )?);
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Err(CaptureError::SystemInvariant(
            "terminal witness tests never project",
        ))
    }
}

fn expected_state(
    adapter: &dyn JsonlFamilyAdapter,
    root: &Path,
) -> (FamilyResident, CertifiedSourceInventory) {
    let observed = adapter.discover(root).unwrap();
    let opening_membership = adapter
        .observe_terminal_membership(root, &observed)
        .unwrap();
    let inventory = observed.certify_against(&observed).unwrap();
    let terminal_sources = observed
        .leaves()
        .iter()
        .map(|leaf| {
            let opened = leaf.open_verified().unwrap();
            let mut reader =
                JsonlReader::open(physical_identity(adapter, leaf), opened, None, None).unwrap();
            while reader
                .visit_page(&mut |_record| -> Result<()> { Ok(()) })
                .unwrap()
                .is_some()
            {}
            let checkpoint = reader.outcome().unwrap().checkpoint().clone();
            let observation =
                leaf::source_observation(leaf.source(), checkpoint.source_observation()).unwrap();
            let certificate = CertifiedSource::certify(
                observation.clone(),
                observation,
                adapter.parser_revision(),
                *checkpoint.complete_prefix_sha256(),
                ScannedSourceCounts::default(),
            )
            .unwrap();
            let terminal_proof = JsonlFamilyTerminalProof::frozen_shared_prefix(
                adapter,
                leaf,
                &certificate,
                checkpoint.complete_prefix_end(),
                *checkpoint.complete_prefix_sha256(),
            )
            .unwrap();
            (
                leaf.source().exact_descriptor_digest(),
                TerminalSourceEvidence {
                    certificate,
                    terminal_proof,
                },
            )
        })
        .collect();
    let owned_sources = observed
        .leaves()
        .iter()
        .map(|leaf| {
            (
                leaf.source().exact_descriptor_digest(),
                leaf.source().clone(),
            )
        })
        .collect();
    (
        FamilyResident {
            ownership_initialized: true,
            owned_sources,
            terminal_sources,
            terminal_rejected_sources: HashMap::new(),
            absent_sources: Vec::new(),
            opening_membership: Some(opening_membership),
            certified_inventory: Some(inventory.clone()),
            opening_inventory: Some(observed),
        },
        inventory,
    )
}

struct FrozenMultiRootTestAdapter {
    roots: Vec<PathBuf>,
}

impl JsonlFamilyAdapter for FrozenMultiRootTestAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "frozen-multi-root-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions
    }

    fn base_scope(&self) -> JsonlFamilyBaseScope {
        JsonlFamilyBaseScope::Route
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        let mut authorities = Vec::new();
        let mut leaves = Vec::new();
        for source_root in &self.roots {
            let authority = Arc::new(ProviderSourceRoot::open(source_root)?);
            for entry in fs::read_dir(source_root)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                    continue;
                }
                let name = entry.file_name();
                let source = SourceKey::derive(
                    self.provider().as_str(),
                    TEST_SOURCE_FORMAT,
                    TEST_SCHEMA,
                    1,
                    SourceAnchor::provider_native(
                        "frozen-multi-root-file",
                        TypedKey::bytes(path.as_os_str().as_encoded_bytes().to_vec())
                            .map_err(contract_error)?,
                    )
                    .map_err(contract_error)?,
                )
                .map_err(contract_error)?;
                leaves.push(JsonlFamilyLeaf::observe(
                    source,
                    path,
                    Arc::clone(&authority),
                    PathBuf::from(&name),
                    TypedKey::bytes(name.as_encoded_bytes().to_vec()).map_err(contract_error)?,
                )?);
            }
            authorities.push(authority);
        }
        authorities.reverse();
        JsonlFamilyInventory::present_multi(self.provider(), root, authorities, leaves)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Err(CaptureError::SystemInvariant(
            "frozen inventory tests never project",
        ))
    }
}

struct TerminalRootSwapTestAdapter {
    root: PathBuf,
    discoveries: AtomicUsize,
}

impl JsonlFamilyAdapter for TerminalRootSwapTestAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "terminal-root-swap-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions
    }

    fn discover(&self, selection_root: &Path) -> Result<JsonlFamilyInventory> {
        self.discoveries.fetch_add(1, Ordering::SeqCst);
        FrozenMultiRootTestAdapter {
            roots: vec![self.root.clone()],
        }
        .discover(selection_root)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Err(CaptureError::SystemInvariant(
            "terminal root swap tests never project",
        ))
    }
}

fn expected_source(resident: &FamilyResident) -> CertifiedSource {
    resident
        .terminal_sources
        .values()
        .next()
        .unwrap()
        .certificate
        .clone()
}

struct ParallelTestAdapter;

struct ParallelTestProjector;

impl JsonlFamilyProjector for ParallelTestProjector {
    fn project(
        &mut self,
        _record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        _emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        Ok(())
    }
}

impl JsonlFamilyAdapter for ParallelTestAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "parallel-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Ok(Box::new(ParallelTestProjector))
    }
}

struct PhasedTestAdapter {
    completed_first_phase: Arc<AtomicUsize>,
    second_phase_started_early: Arc<AtomicBool>,
}

struct PhasedTestProjector {
    phase: usize,
    completed_first_phase: Arc<AtomicUsize>,
    second_phase_started_early: Arc<AtomicBool>,
}

impl JsonlFamilyProjector for PhasedTestProjector {
    fn project(
        &mut self,
        _record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        _emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        if self.phase == 1 && self.completed_first_phase.load(Ordering::SeqCst) != 4 {
            self.second_phase_started_early
                .store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if self.phase == 0 {
            self.completed_first_phase.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

impl JsonlFamilyAdapter for PhasedTestAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "phased-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn order_leaf_scans(&self, leaves: &mut [JsonlFamilyLeaf]) -> Result<()> {
        leaves.sort_by_key(|leaf| self.leaf_scan_phase(leaf).unwrap_or(usize::MAX));
        Ok(())
    }

    fn leaf_scan_phase(&self, leaf: &JsonlFamilyLeaf) -> Result<usize> {
        Ok(usize::from(
            leaf.source_path()
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| name.starts_with("second-")),
        ))
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Ok(Box::new(PhasedTestProjector {
            phase: self.leaf_scan_phase(leaf)?,
            completed_first_phase: Arc::clone(&self.completed_first_phase),
            second_phase_started_early: Arc::clone(&self.second_phase_started_early),
        }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SchedulerLeafState {
    partition: u64,
    phase: usize,
    ordinal: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SchedulerStateEvent {
    Begin(u64),
    Finish(u64),
    Project {
        leaf: SchedulerLeafState,
        full_probes_before: usize,
        full_probes_after: usize,
        event_time_entries_before: usize,
        event_time_entries_after: usize,
    },
}

struct SchedulerStateTestAdapter {
    repository: PathBuf,
    attributed_partitions: Vec<u64>,
    failing_leaf: Option<SchedulerLeafState>,
    parallel_frontier: Option<(u64, usize, Arc<std::sync::Barrier>)>,
    events: Arc<Mutex<Vec<SchedulerStateEvent>>>,
}

struct UnpartitionedSchedulerStateTestAdapter(SchedulerStateTestAdapter);

struct SchedulerStateTestProjector {
    leaf: SchedulerLeafState,
    repository: PathBuf,
    attribute_repository: bool,
    fail: bool,
    parallel_frontier: Option<Arc<std::sync::Barrier>>,
    events: Arc<Mutex<Vec<SchedulerStateEvent>>>,
}

fn scheduler_leaf_state(leaf: &JsonlFamilyLeaf) -> Result<SchedulerLeafState> {
    let name = leaf
        .source_path()
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .and_then(|name| name.strip_suffix(".jsonl"))
        .ok_or_else(|| {
            CaptureError::InvalidPayload("scheduler test leaf name is malformed".to_owned())
        })?;
    let fields = name.split('-').collect::<Vec<_>>();
    if fields.len() != 6 || fields[0] != "partition" || fields[2] != "phase" || fields[4] != "leaf"
    {
        return Err(CaptureError::InvalidPayload(
            "scheduler test leaf name is malformed".to_owned(),
        ));
    }
    let partition = fields[1].parse::<u64>().map_err(|_| {
        CaptureError::InvalidPayload("scheduler test partition is malformed".to_owned())
    })?;
    let phase = fields[3].parse::<usize>().map_err(|_| {
        CaptureError::InvalidPayload("scheduler test phase is malformed".to_owned())
    })?;
    let ordinal = fields[5].parse::<usize>().map_err(|_| {
        CaptureError::InvalidPayload("scheduler test ordinal is malformed".to_owned())
    })?;
    Ok(SchedulerLeafState {
        partition,
        phase,
        ordinal,
    })
}

impl JsonlFamilyProjector for SchedulerStateTestProjector {
    fn project(
        &mut self,
        _record: JsonlRecordRef<'_>,
        worker: &mut JsonlFamilyWorkerContext,
        _emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        if self.fail {
            return Err(CaptureError::InvalidPayload(
                "scheduler test requested scan failure".to_owned(),
            ));
        }
        if let Some(barrier) = &self.parallel_frontier {
            barrier.wait();
        }
        let full_probes_before = worker
            .repository_attributor()
            .full_certification_probe_count();
        let event_time_entries_before = worker.repository_attributor().event_time_cache_len();
        if self.attribute_repository {
            let annotation = worker.repository_attributor().attribute(AttributionInput {
                activity_at_unix_ms: Some(
                    1_700_000_000_000_i64
                        .saturating_add(self.leaf.phase as i64)
                        .saturating_add(self.leaf.ordinal as i64),
                ),
                declared_tool_workdir: Some(self.repository.to_string_lossy().into_owned()),
                ..AttributionInput::default()
            });
            if annotation.repository_bindings.len() != 1 {
                return Err(CaptureError::InvalidPayload(
                    "scheduler test repository attribution did not bind".to_owned(),
                ));
            }
        }
        let full_probes_after = worker
            .repository_attributor()
            .full_certification_probe_count();
        let event_time_entries_after = worker.repository_attributor().event_time_cache_len();
        self.events
            .lock()
            .map_err(|_| CaptureError::SystemInvariant("scheduler test event log was poisoned"))?
            .push(SchedulerStateEvent::Project {
                leaf: self.leaf,
                full_probes_before,
                full_probes_after,
                event_time_entries_before,
                event_time_entries_after,
            });
        Ok(())
    }
}

impl JsonlFamilyAdapter for SchedulerStateTestAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "scheduler-state-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn order_leaf_scans(&self, leaves: &mut [JsonlFamilyLeaf]) -> Result<()> {
        leaves.sort_by_key(|leaf| {
            scheduler_leaf_state(leaf).unwrap_or(SchedulerLeafState {
                partition: u64::MAX,
                phase: usize::MAX,
                ordinal: usize::MAX,
            })
        });
        Ok(())
    }

    fn leaf_scan_phase(&self, leaf: &JsonlFamilyLeaf) -> Result<usize> {
        Ok(scheduler_leaf_state(leaf)?.phase)
    }

    fn leaf_scan_partition(&self, leaf: &JsonlFamilyLeaf) -> Result<Option<u64>> {
        Ok(Some(scheduler_leaf_state(leaf)?.partition))
    }

    fn begin_leaf_scan_partition(&self, partition: u64) -> Result<()> {
        self.events
            .lock()
            .map_err(|_| CaptureError::SystemInvariant("scheduler test event log was poisoned"))?
            .push(SchedulerStateEvent::Begin(partition));
        Ok(())
    }

    fn finish_leaf_scan_partition(&self, partition: u64) -> Result<()> {
        self.events
            .lock()
            .map_err(|_| CaptureError::SystemInvariant("scheduler test event log was poisoned"))?
            .push(SchedulerStateEvent::Finish(partition));
        Ok(())
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        let leaf = scheduler_leaf_state(leaf)?;
        let parallel_frontier = self
            .parallel_frontier
            .as_ref()
            .filter(|(partition, phase, _)| *partition == leaf.partition && *phase == leaf.phase)
            .map(|(_, _, barrier)| Arc::clone(barrier));
        Ok(Box::new(SchedulerStateTestProjector {
            leaf,
            repository: self.repository.clone(),
            attribute_repository: self.attributed_partitions.contains(&leaf.partition),
            fail: self.failing_leaf == Some(leaf),
            parallel_frontier,
            events: Arc::clone(&self.events),
        }))
    }
}

impl JsonlFamilyAdapter for UnpartitionedSchedulerStateTestAdapter {
    fn provider(&self) -> CaptureProvider {
        self.0.provider()
    }

    fn source_format(&self) -> &'static str {
        self.0.source_format()
    }

    fn schema_variant(&self) -> &'static str {
        self.0.schema_variant()
    }

    fn parser_revision(&self) -> &'static str {
        self.0.parser_revision()
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        self.0.append_mode()
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        self.0.discover(root)
    }

    fn order_leaf_scans(&self, leaves: &mut [JsonlFamilyLeaf]) -> Result<()> {
        self.0.order_leaf_scans(leaves)
    }

    fn leaf_scan_phase(&self, leaf: &JsonlFamilyLeaf) -> Result<usize> {
        self.0.leaf_scan_phase(leaf)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        self.0.projector(leaf, source_file, imported_at)
    }
}

struct IdentityRevisionTestAdapter {
    parser_revision: &'static str,
    revision: &'static str,
    expected_mode: JsonlFamilyProjectionMode,
}

impl JsonlFamilyAdapter for IdentityRevisionTestAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        self.parser_revision
    }

    fn event_identity_revision(&self) -> &'static str {
        self.revision
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Ok(Box::new(ParallelTestProjector))
    }

    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<BaseEventIdentityLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        if checkpoint.is_some()
            || mode != self.expected_mode
            || base_event_lookup.is_some() != (mode != JsonlFamilyProjectionMode::Cold)
        {
            return Err(CaptureError::InvalidPayload(
                "identity revision test received inconsistent projection context".to_owned(),
            ));
        }
        self.projector(leaf, source_file, imported_at)
    }
}

struct EmissionTestAdapter {
    project_fanout: usize,
    finish_fanout: usize,
    admitted: Option<Arc<AtomicUsize>>,
    observed_before_65: Option<Arc<AtomicUsize>>,
}

struct EmissionTestProjector {
    source: SourceKey,
    project_fanout: usize,
    finish_fanout: usize,
    admitted: Option<Arc<AtomicUsize>>,
    observed_before_65: Option<Arc<AtomicUsize>>,
}

impl EmissionTestAdapter {
    fn ordinary() -> Self {
        Self {
            project_fanout: 1,
            finish_fanout: 0,
            admitted: None,
            observed_before_65: None,
        }
    }
}

fn emission_test_record(source: &SourceKey, ordinal: u64) -> Result<CoreRecord> {
    let session_key = NativeSessionKey::native_id(
        "session",
        TypedKey::utf8("session").map_err(contract_error)?,
    )
    .map_err(contract_error)?;
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "session",
        native_session_key: &session_key,
    })
    .map_err(contract_error)?;
    let native_item_key =
        NativeItemKey::native_id("message", TypedKey::U64(ordinal)).map_err(contract_error)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(contract_error)?;
    let mut projected = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        ordinal,
        "message",
        "primary",
        true,
        "jsonl-emission-test-v1",
        "bounded",
    )
    .map_err(contract_error)?;
    projected.provider_session_id = Some("session".to_owned());
    projected.native_event_id = Some(TypedKey::U64(ordinal));
    projected.occurred_at_unix_ms = Some(ordinal as i64);
    projected.role = Some("user".to_owned());
    Ok(projected)
}

impl JsonlFamilyProjector for EmissionTestProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let base = record
            .evidence()
            .physical_ordinal()
            .checked_mul(1_000)
            .ok_or(CaptureError::SystemInvariant(
                "emission-test ordinal overflowed",
            ))?;
        self.emit_fanout(base, self.project_fanout, emit)
    }

    fn finish_projecting(
        &mut self,
        _worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        self.emit_fanout(1_000_000, self.finish_fanout, emit)
    }
}

impl EmissionTestProjector {
    fn emit_fanout(
        &self,
        base: u64,
        count: usize,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        for index in 0..count {
            if index == 64 {
                if let (Some(admitted), Some(observed)) =
                    (self.admitted.as_ref(), self.observed_before_65.as_ref())
                {
                    observed.store(admitted.load(Ordering::SeqCst), Ordering::SeqCst);
                }
            }
            let ordinal = base
                .checked_add(index as u64)
                .ok_or(CaptureError::SystemInvariant(
                    "emission-test fanout overflowed",
                ))?;
            emit(emission_test_record(&self.source, ordinal)?)?;
        }
        Ok(())
    }
}

impl JsonlFamilyAdapter for EmissionTestAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "emission-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Ok(Box::new(EmissionTestProjector {
            source: leaf.source().clone(),
            project_fanout: self.project_fanout,
            finish_fanout: self.finish_fanout,
            admitted: self.admitted.clone(),
            observed_before_65: self.observed_before_65.clone(),
        }))
    }
}

struct CheckpointTestAdapter;

struct OptimizedLeafTestAdapter {
    scans: AtomicUsize,
    emit_wrong_source: bool,
}

impl JsonlFamilyAdapter for OptimizedLeafTestAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "optimized-leaf-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::Replacement
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Err(CaptureError::SystemInvariant(
            "optimized leaf test must not construct the generic projector",
        ))
    }

    fn scan_optimized_leaf(
        &self,
        leaf: &JsonlFamilyLeaf,
        _base: Option<&CertifiedSource>,
        _base_event_lookup: &BaseEventIdentityLookup,
        _worker: &mut JsonlFamilyWorkerContext,
        emit_page: &mut dyn FnMut(JsonlFamilyPublication, Vec<CoreRecord>) -> Result<()>,
    ) -> Result<Option<JsonlFamilyOptimizedLeafOutcome>> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        drop(leaf.open_verified()?);
        let records = if self.emit_wrong_source {
            let wrong_source = SourceKey::derive(
                self.provider().as_str(),
                TEST_SOURCE_FORMAT,
                TEST_SCHEMA,
                1,
                SourceAnchor::provider_native(
                    "wrong-optimized-source",
                    TypedKey::utf8("wrong").map_err(contract_error)?,
                )
                .map_err(contract_error)?,
            )
            .map_err(contract_error)?;
            vec![emission_test_record(&wrong_source, 0)?]
        } else {
            Vec::new()
        };
        emit_page(JsonlFamilyPublication::Replace, records)?;
        let observation = leaf::source_observation(leaf.source(), leaf.observation())?;
        let certificate = CertifiedSource::certify(
            observation.clone(),
            observation,
            self.parser_revision(),
            Sha256::digest(TEST_RECORD).into(),
            ScannedSourceCounts {
                complete_records: 1,
                retained_records: 0,
                rejected_records: 0,
                ignored_records: 1,
                indexed_documents: 0,
                certified_bytes: TEST_RECORD.len() as u64,
            },
        )
        .map_err(contract_error)?;
        let terminal_proof = JsonlFamilyTerminalProof::exact_file(self, leaf, &certificate)?;
        Ok(Some(JsonlFamilyOptimizedLeafOutcome::replacement(
            certificate,
            terminal_proof,
        )))
    }
}

struct CheckpointTestProjector {
    projected_records: u64,
    resumed: bool,
}

impl JsonlFamilyProjector for CheckpointTestProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        _emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        if self.resumed && self.projected_records != record.evidence().physical_ordinal() {
            return Err(CaptureError::InvalidPayload(
                "opaque checkpoint resumed from the wrong JSONL ordinal".to_owned(),
            ));
        }
        self.projected_records =
            self.projected_records
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "checkpoint test record count overflowed",
                ))?;
        Ok(())
    }

    fn provider_checkpoint(&self) -> Result<Option<TypedKey>> {
        Ok(Some(TypedKey::U64(self.projected_records)))
    }
}

impl JsonlFamilyAdapter for CheckpointTestAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "checkpoint-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Ok(Box::new(CheckpointTestProjector {
            projected_records: 0,
            resumed: false,
        }))
    }

    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<BaseEventIdentityLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        let Some(checkpoint) = checkpoint else {
            if mode == JsonlFamilyProjectionMode::Cold && base_event_lookup.is_some() {
                return Err(CaptureError::InvalidPayload(
                    "cold checkpoint test unexpectedly received a base lookup".to_owned(),
                ));
            }
            if mode == JsonlFamilyProjectionMode::Replacement && base_event_lookup.is_none() {
                return Err(CaptureError::InvalidPayload(
                    "replacement checkpoint test did not receive a base lookup".to_owned(),
                ));
            }
            return self.projector(leaf, source_file, imported_at);
        };
        if mode != JsonlFamilyProjectionMode::CertifiedAppend || base_event_lookup.is_none() {
            return Err(CaptureError::InvalidPayload(
                "resumed checkpoint test did not receive a base lookup".to_owned(),
            ));
        }
        let TypedKey::U64(projected_records) = checkpoint else {
            return Err(CaptureError::InvalidPayload(
                "checkpoint test state is malformed".to_owned(),
            ));
        };
        Ok(Box::new(CheckpointTestProjector {
            projected_records: *projected_records,
            resumed: true,
        }))
    }
}

fn capture_parallel_test_generation(
    adapter: &dyn JsonlFamilyAdapter,
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> (CommitReceipt, JsonlFamilyScannerActivity) {
    let resident = Mutex::new(FamilyResident::default());
    let mut writer = GenerationWriter::open(
        index_root,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    let mut owners = HashMap::new();
    let mut complete_inventories = Vec::new();
    let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
    let mut record_rejections = SourceBackedRecordRejections::default();
    {
        let mut sink = SourceBackedGenerationSink {
            core_record_preparer: writer.core_record_preparer(),
            writer: &mut writer,
            owners: &mut owners,
            complete_inventories: &mut complete_inventories,
            route_index: 0,
            route_identity: test_route_identity(),
            resources: SourceBackedRouteResources::production(workers),
            logical_source_failures: &mut logical_source_failures,
            record_rejections: &mut record_rejections,
            applied_removals: &mut Vec::new(),
            record_progress: None,
            current_source_progress: None,
        };
        with_family_scanner_workers(workers, || {
            capture(adapter, root, &resident, &mut sink).unwrap();
        });
    }
    let activity = jsonl_family_scanner_activity();
    let commit = writer
        .commit_with_complete_inventory_revalidation(|_| true, |_| true)
        .unwrap();
    (commit, activity)
}

fn capture_checkpoint_test_generation(
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> CommitReceipt {
    let resident = Mutex::new(FamilyResident::default());
    let mut writer = GenerationWriter::open(
        index_root,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    let mut owners = HashMap::new();
    let mut complete_inventories = Vec::new();
    let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
    let mut record_rejections = SourceBackedRecordRejections::default();
    {
        let mut sink = SourceBackedGenerationSink {
            core_record_preparer: writer.core_record_preparer(),
            writer: &mut writer,
            owners: &mut owners,
            complete_inventories: &mut complete_inventories,
            route_index: 0,
            route_identity: test_route_identity(),
            resources: SourceBackedRouteResources::production(workers),
            logical_source_failures: &mut logical_source_failures,
            record_rejections: &mut record_rejections,
            applied_removals: &mut Vec::new(),
            record_progress: None,
            current_source_progress: None,
        };
        with_family_scanner_workers(workers, || {
            capture(&CheckpointTestAdapter, root, &resident, &mut sink).unwrap();
        });
    }
    writer
        .commit_with_complete_inventory_revalidation(|_| true, |_| true)
        .unwrap()
}

fn run_scheduler_test_capture(
    adapter: &dyn JsonlFamilyAdapter,
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> SourceBackedRouteResult<JsonlFamilyScannerActivity> {
    let resident = Mutex::new(FamilyResident::default());
    let mut writer = GenerationWriter::open(
        index_root,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    let mut owners = HashMap::new();
    let mut complete_inventories = Vec::new();
    let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
    let mut record_rejections = SourceBackedRecordRejections::default();
    let result = {
        let mut sink = SourceBackedGenerationSink {
            core_record_preparer: writer.core_record_preparer(),
            writer: &mut writer,
            owners: &mut owners,
            complete_inventories: &mut complete_inventories,
            route_index: 0,
            route_identity: test_route_identity(),
            resources: SourceBackedRouteResources::production(workers),
            logical_source_failures: &mut logical_source_failures,
            record_rejections: &mut record_rejections,
            applied_removals: &mut Vec::new(),
            record_progress: None,
            current_source_progress: None,
        };
        with_family_scanner_workers(workers, || capture(adapter, root, &resident, &mut sink))
    };
    result.map(|()| jsonl_family_scanner_activity())
}

fn scheduler_test_repository(parent: &Path) -> PathBuf {
    let repository = parent.join("attributed-repository");
    fs::create_dir(&repository).unwrap();
    for arguments in [
        vec!["init", "-q"],
        vec!["config", "user.name", "ctx test"],
        vec!["config", "user.email", "ctx@example.invalid"],
    ] {
        let status = Command::new("/usr/bin/git")
            .arg("-C")
            .arg(&repository)
            .args(arguments)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success());
    }
    fs::write(repository.join("tracked.txt"), "tracked\n").unwrap();
    for arguments in [vec!["add", "tracked.txt"], vec!["commit", "-qm", "fixture"]] {
        let status = Command::new("/usr/bin/git")
            .arg("-C")
            .arg(&repository)
            .args(arguments)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success());
    }
    repository
}

fn write_scheduler_test_leaf(root: &Path, partition: u64, phase: usize, ordinal: usize) {
    fs::write(
        root.join(format!(
            "partition-{partition:02}-phase-{phase}-leaf-{ordinal}.jsonl"
        )),
        b"{\"message\":\"scheduler\"}\n",
    )
    .unwrap();
}

fn provider_checkpoints(receipt: &CommitReceipt) -> Vec<Option<TypedKey>> {
    receipt
        .manifest()
        .sources
        .iter()
        .map(|source| {
            let frontier = source.frontier().unwrap();
            let TypedKey::Bytes(bytes) = frontier.checkpoint() else {
                panic!("family checkpoint was not bytes");
            };
            serde_json::from_slice::<FamilyCheckpoint>(bytes)
                .unwrap()
                .provider_checkpoint
        })
        .collect()
}

#[test]
fn optimized_leaf_execution_keeps_publication_inside_the_shared_family() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("optimized.jsonl"), TEST_RECORD).unwrap();
    let adapter = OptimizedLeafTestAdapter {
        scans: AtomicUsize::new(0),
        emit_wrong_source: false,
    };
    let inventory = adapter.discover(&root).unwrap();
    let leaf = inventory.leaves().first().unwrap();
    let writer = GenerationWriter::open(
        temp.path().join("index"),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    let mut publications = Vec::new();
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emit = |event| {
        if let JsonlLeafOutputEvent::Page { append, records } = event {
            publications.push((append, records.len()));
        }
        Ok(())
    };
    let mut output = JsonlLeafOutput::new(&mut emit);
    let prepared = prepare_leaf(
        &adapter,
        leaf,
        None,
        &writer.base_event_identity_lookup(),
        &mut worker,
        &mut output,
    )
    .unwrap();

    assert_eq!(adapter.scans.load(Ordering::SeqCst), 1);
    assert_eq!(publications, vec![(false, 0)]);
    assert!(prepared.append.is_none());
    assert!(matches!(
        prepared.terminal_proof,
        JsonlFamilyTerminalProof::ExactFile { .. }
    ));
    assert_eq!(
        prepared.certificate.parser_revision(),
        adapter.parser_revision()
    );
}

fn optimized_test_certificate(
    adapter: &dyn JsonlFamilyAdapter,
    leaf: &JsonlFamilyLeaf,
    content_digest: [u8; 32],
) -> CertifiedSource {
    let observation = super::leaf::source_observation(leaf.source(), leaf.observation()).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        adapter.parser_revision(),
        content_digest,
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 0,
            rejected_records: 0,
            ignored_records: 1,
            indexed_documents: 0,
            certified_bytes: TEST_RECORD.len() as u64,
        },
    )
    .unwrap()
}

#[test]
fn active_source_family_contract_jsonl_optimized_proof_rejects_cross_leaf_binding() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("optimized.jsonl"), TEST_RECORD).unwrap();
    let adapter = OptimizedLeafTestAdapter {
        scans: AtomicUsize::new(0),
        emit_wrong_source: false,
    };
    let inventory = adapter.discover(&root).unwrap();
    let first = inventory.leaves().first().unwrap();
    let other_source = SourceKey::derive(
        adapter.provider().as_str(),
        TEST_SOURCE_FORMAT,
        TEST_SCHEMA,
        1,
        SourceAnchor::provider_native(
            "terminal-witness-file",
            TypedKey::utf8("other-optimized-leaf").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let other = JsonlFamilyLeaf::bind_observed(
        other_source,
        first.source_path.clone(),
        Arc::clone(&first.authority),
        first.authority_path.clone(),
        first.binding.clone(),
        first.observation.clone(),
    );
    let first_certificate =
        optimized_test_certificate(&adapter, first, Sha256::digest(TEST_RECORD).into());
    let other_certificate =
        optimized_test_certificate(&adapter, &other, Sha256::digest(TEST_RECORD).into());
    let proof = JsonlFamilyTerminalProof::exact_file(&adapter, first, &first_certificate).unwrap();
    let outcome = JsonlFamilyOptimizedLeafOutcome::replacement(other_certificate, proof);

    let error = super::leaf::validate_optimized_outcome(&adapter, &other, None, outcome)
        .err()
        .expect("proof from another optimized leaf must be rejected");
    assert!(error
        .to_string()
        .contains("bound to another leaf or certificate"));
}

#[test]
fn active_source_family_contract_jsonl_optimized_proof_rejects_mismatched_certificate() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("optimized.jsonl"), TEST_RECORD).unwrap();
    let adapter = OptimizedLeafTestAdapter {
        scans: AtomicUsize::new(0),
        emit_wrong_source: false,
    };
    let inventory = adapter.discover(&root).unwrap();
    let leaf = inventory.leaves().first().unwrap();
    let certificate =
        optimized_test_certificate(&adapter, leaf, Sha256::digest(TEST_RECORD).into());
    let mismatched = optimized_test_certificate(&adapter, leaf, [9; 32]);
    let proof = JsonlFamilyTerminalProof::exact_file(&adapter, leaf, &certificate).unwrap();
    let outcome = JsonlFamilyOptimizedLeafOutcome::replacement(mismatched, proof);

    let error = super::leaf::validate_optimized_outcome(&adapter, leaf, None, outcome)
        .err()
        .expect("proof from another certificate must be rejected");
    assert!(error
        .to_string()
        .contains("bound to another leaf or certificate"));
}

#[test]
fn optimized_leaf_execution_rejects_records_owned_by_another_source() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("optimized.jsonl"), TEST_RECORD).unwrap();
    let adapter = OptimizedLeafTestAdapter {
        scans: AtomicUsize::new(0),
        emit_wrong_source: true,
    };
    let inventory = adapter.discover(&root).unwrap();
    let leaf = inventory.leaves().first().unwrap();
    let writer = GenerationWriter::open(
        temp.path().join("index"),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emit = |_event| Ok(());
    let mut output = JsonlLeafOutput::new(&mut emit);
    let error = prepare_leaf(
        &adapter,
        leaf,
        None,
        &writer.base_event_identity_lookup(),
        &mut worker,
        &mut output,
    )
    .err()
    .expect("wrong-source optimized emission must fail");
    assert!(error
        .to_string()
        .contains("optimized JSONL leaf emitted a record for another source"));
}

#[test]
fn generic_projection_streams_record_and_finish_fanout_before_record_65() {
    for finish_only in [false, true] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("fanout.jsonl"), TEST_RECORD).unwrap();
        let admitted = Arc::new(AtomicUsize::new(0));
        let observed_before_65 = Arc::new(AtomicUsize::new(usize::MAX));
        let adapter = EmissionTestAdapter {
            project_fanout: if finish_only { 0 } else { 129 },
            finish_fanout: if finish_only { 129 } else { 0 },
            admitted: Some(Arc::clone(&admitted)),
            observed_before_65: Some(Arc::clone(&observed_before_65)),
        };
        let inventory = adapter.discover(&root).unwrap();
        let leaf = inventory.leaves().first().unwrap();
        let writer = GenerationWriter::open(
            temp.path().join("index"),
            WriterOptions {
                indexer_threads: 1,
                memory_bytes: 15_000_000,
            },
        )
        .unwrap()
        .into_writer()
        .unwrap();
        let mut emit = |event| {
            if matches!(event, JsonlLeafOutputEvent::Record { .. }) {
                admitted.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        };
        let mut output = JsonlLeafOutput::new(&mut emit);
        let mut worker = JsonlFamilyWorkerContext::default();
        let prepared = prepare_leaf(
            &adapter,
            leaf,
            None,
            &writer.base_event_identity_lookup(),
            &mut worker,
            &mut output,
        )
        .unwrap();

        assert_eq!(admitted.load(Ordering::SeqCst), 129);
        assert_eq!(observed_before_65.load(Ordering::SeqCst), 64);
        assert_eq!(prepared.certificate.counts().indexed_documents, 129);
    }
}

#[test]
fn borrowed_jsonl_worker_policy_honors_default_and_requested_counts() {
    assert_eq!(family_scanner_worker_count_policy(0, None), 0);
    assert_eq!(family_scanner_worker_count_policy(8, None), 8);
    assert_eq!(family_scanner_worker_count_policy(8, Some(4)), 4);
    assert_eq!(family_scanner_worker_count_policy(3, Some(4)), 3);
    assert_eq!(family_scanner_worker_count_policy(8, Some(0)), 1);
    assert_eq!(family_scanner_worker_count_policy(8, Some(usize::MAX)), 8);
}

#[test]
fn certified_append_generation_is_identical_with_one_and_eight_workers() {
    use std::io::Write;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    for index in 0..8 {
        fs::write(
            root.join(format!("{index}.jsonl")),
            format!("{{\"message\":\"cold-{index}\"}}\n"),
        )
        .unwrap();
    }
    let adapter = ParallelTestAdapter;

    let (one_cold, one_cold_activity) =
        capture_parallel_test_generation(&adapter, &root, &temp.path().join("one"), 1);
    let (eight_cold, eight_cold_activity) =
        capture_parallel_test_generation(&adapter, &root, &temp.path().join("eight"), 8);
    assert_eq!(
        one_cold_activity,
        JsonlFamilyScannerActivity {
            worker_count: 1,
            sources_started: 8,
            sources_completed: 8,
            peak_active_scanners: 1,
        }
    );
    assert_eq!(eight_cold_activity.worker_count, 8);
    assert_eq!(eight_cold_activity.sources_started, 8);
    assert_eq!(eight_cold_activity.sources_completed, 8);
    assert!(eight_cold_activity.peak_active_scanners >= 4);
    assert!(eight_cold_activity.peak_active_scanners <= 8);
    assert_eq!(one_cold.generation_id, eight_cold.generation_id);
    assert_eq!(
        one_cold.manifest().sources,
        eight_cold.manifest().sources,
        "cold certification must be independent of worker count"
    );

    for index in 0..8 {
        OpenOptions::new()
            .append(true)
            .open(root.join(format!("{index}.jsonl")))
            .unwrap()
            .write_all(format!("{{\"message\":\"append-{index}\"}}\n").as_bytes())
            .unwrap();
    }
    let (one_append, one_append_activity) =
        capture_parallel_test_generation(&adapter, &root, &temp.path().join("one"), 1);
    let (eight_append, eight_append_activity) =
        capture_parallel_test_generation(&adapter, &root, &temp.path().join("eight"), 8);
    assert_eq!(one_append_activity.sources_started, 8);
    assert_eq!(one_append_activity.sources_completed, 8);
    assert_eq!(one_append_activity.peak_active_scanners, 1);
    assert_eq!(eight_append_activity.sources_started, 8);
    assert_eq!(eight_append_activity.sources_completed, 8);
    assert!(eight_append_activity.peak_active_scanners >= 4);
    assert_eq!(one_append.generation_id, eight_append.generation_id);
    assert_eq!(
        one_append.manifest().sources,
        eight_append.manifest().sources,
        "certified append must be independent of worker count"
    );
    assert!(one_append
        .manifest()
        .sources
        .iter()
        .all(|source| source.counts().complete_records == 2));
}

#[test]
fn opaque_provider_checkpoint_and_base_lookup_resume_only_the_certified_suffix() {
    for workers in [1, 8] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let index = temp.path().join("index");
        fs::create_dir_all(&root).unwrap();
        let transcripts = (0..workers)
            .map(|index| root.join(format!("checkpoint-{index}.jsonl")))
            .collect::<Vec<_>>();
        for transcript in &transcripts {
            fs::write(transcript, b"{\"message\":\"prefix\"}\n").unwrap();
        }

        let cold = capture_checkpoint_test_generation(&root, &index, workers);
        assert!(provider_checkpoints(&cold)
            .into_iter()
            .all(|checkpoint| checkpoint == Some(TypedKey::U64(1))));

        for transcript in &transcripts {
            OpenOptions::new()
                .append(true)
                .open(transcript)
                .unwrap()
                .write_all(b"{\"message\":\"suffix\"}\n")
                .unwrap();
        }
        let appended = capture_checkpoint_test_generation(&root, &index, workers);
        assert!(provider_checkpoints(&appended)
            .into_iter()
            .all(|checkpoint| checkpoint == Some(TypedKey::U64(2))));
        assert!(appended
            .manifest()
            .sources
            .iter()
            .all(|source| source.counts().complete_records == 2));
    }
}

#[test]
fn event_identity_revision_forces_replacement_with_core_base_authority() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("identity.jsonl"), b"{\"message\":\"stable\"}\n").unwrap();

    let cold = IdentityRevisionTestAdapter {
        parser_revision: "identity-revision-test-parser-v1",
        revision: "content-occurrence-v1",
        expected_mode: JsonlFamilyProjectionMode::Cold,
    };
    let (cold_receipt, _) = capture_parallel_test_generation(&cold, &root, &index, 1);

    let upgraded = IdentityRevisionTestAdapter {
        parser_revision: "identity-revision-test-parser-v1",
        revision: "content-occurrence-v2",
        expected_mode: JsonlFamilyProjectionMode::Replacement,
    };
    let (upgraded_receipt, _) = capture_parallel_test_generation(&upgraded, &root, &index, 1);

    assert_ne!(cold_receipt.generation_id, upgraded_receipt.generation_id);
    let checkpoint = upgraded_receipt.manifest().sources[0]
        .frontier()
        .unwrap()
        .checkpoint();
    let TypedKey::Bytes(bytes) = checkpoint else {
        panic!("family checkpoint was not bytes");
    };
    assert_eq!(
        serde_json::from_slice::<FamilyCheckpoint>(bytes)
            .unwrap()
            .event_identity_revision,
        "content-occurrence-v2"
    );
}

#[test]
fn parser_revision_forces_unchanged_source_replacement() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("parser.jsonl"), b"{\"message\":\"stable\"}\n").unwrap();

    let cold = IdentityRevisionTestAdapter {
        parser_revision: "identity-revision-test-parser-v1",
        revision: "content-occurrence-v1",
        expected_mode: JsonlFamilyProjectionMode::Cold,
    };
    let (cold_receipt, _) = capture_parallel_test_generation(&cold, &root, &index, 1);

    let upgraded = IdentityRevisionTestAdapter {
        parser_revision: "identity-revision-test-parser-v2",
        revision: "content-occurrence-v1",
        expected_mode: JsonlFamilyProjectionMode::Replacement,
    };
    let (upgraded_receipt, _) = capture_parallel_test_generation(&upgraded, &root, &index, 1);

    assert_ne!(cold_receipt.generation_id, upgraded_receipt.generation_id);
    assert_eq!(
        upgraded_receipt.manifest().sources[0].parser_revision(),
        "identity-revision-test-parser-v2"
    );
}

#[test]
fn production_jsonl_scheduler_projects_multiple_sources_concurrently() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    for index in 0..8 {
        fs::write(
            root.join(format!("{index}.jsonl")),
            b"{\"message\":\"parallel\"}\n",
        )
        .unwrap();
    }
    let adapter = ParallelTestAdapter;
    let resident = Mutex::new(FamilyResident::default());
    let mut writer = GenerationWriter::open(
        temp.path().join("index"),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    let mut owners = HashMap::new();
    let mut complete_inventories = Vec::new();
    let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
    let mut record_rejections = SourceBackedRecordRejections::default();
    let mut sink = SourceBackedGenerationSink {
        core_record_preparer: writer.core_record_preparer(),
        writer: &mut writer,
        owners: &mut owners,
        complete_inventories: &mut complete_inventories,
        route_index: 0,
        route_identity: test_route_identity(),
        resources: SourceBackedRouteResources::production(4),
        logical_source_failures: &mut logical_source_failures,
        record_rejections: &mut record_rejections,
        applied_removals: &mut Vec::new(),
        record_progress: None,
        current_source_progress: None,
    };

    with_family_scanner_workers(4, || {
        capture(&adapter, &root, &resident, &mut sink).unwrap();
    });

    assert_eq!(
        jsonl_family_scanner_activity(),
        JsonlFamilyScannerActivity {
            worker_count: 4,
            sources_started: 8,
            sources_completed: 8,
            peak_active_scanners: 4,
        },
        "the production JSONL route must keep all four selected scanners active"
    );
    assert_eq!(resident.lock().unwrap().terminal_sources.len(), 8);
}

#[test]
fn dependency_phases_bar_later_jsonl_scans_without_serializing_each_phase() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    for phase in ["first", "second"] {
        for index in 0..4 {
            fs::write(
                root.join(format!("{phase}-{index}.jsonl")),
                b"{\"message\":\"phased\"}\n",
            )
            .unwrap();
        }
    }
    let completed_first_phase = Arc::new(AtomicUsize::new(0));
    let second_phase_started_early = Arc::new(AtomicBool::new(false));
    let adapter = PhasedTestAdapter {
        completed_first_phase: Arc::clone(&completed_first_phase),
        second_phase_started_early: Arc::clone(&second_phase_started_early),
    };

    let (_, activity) =
        capture_parallel_test_generation(&adapter, &root, &temp.path().join("index"), 4);

    assert_eq!(completed_first_phase.load(Ordering::SeqCst), 4);
    assert!(!second_phase_started_early.load(Ordering::SeqCst));
    assert_eq!(activity.sources_started, 8);
    assert_eq!(activity.sources_completed, 8);
    assert_eq!(activity.peak_active_scanners, 4);
}

#[test]
fn partitioned_component_balances_hooks_and_parallelizes_parent_first_frontiers() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    write_scheduler_test_leaf(&root, 7, 0, 0);
    write_scheduler_test_leaf(&root, 7, 1, 0);
    write_scheduler_test_leaf(&root, 7, 1, 1);
    let events = Arc::new(Mutex::new(Vec::new()));
    let adapter = SchedulerStateTestAdapter {
        repository: scheduler_test_repository(temp.path()),
        attributed_partitions: vec![7],
        failing_leaf: None,
        parallel_frontier: Some((7, 1, Arc::new(std::sync::Barrier::new(2)))),
        events: Arc::clone(&events),
    };

    let activity =
        run_scheduler_test_capture(&adapter, &root, &temp.path().join("index"), 4).unwrap();
    let events = events.lock().unwrap().clone();
    let hooks = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_)
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        hooks,
        [
            SchedulerStateEvent::Begin(7),
            SchedulerStateEvent::Finish(7)
        ]
    );
    let project_order = events
        .iter()
        .filter_map(|event| match event {
            SchedulerStateEvent::Project { leaf, .. } => Some(*leaf),
            SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_) => None,
        })
        .collect::<Vec<_>>();
    let parent = SchedulerLeafState {
        partition: 7,
        phase: 0,
        ordinal: 0,
    };
    assert_eq!(project_order.first(), Some(&parent));

    let mut projects = events
        .iter()
        .filter_map(|event| match event {
            SchedulerStateEvent::Project {
                leaf,
                full_probes_before,
                full_probes_after,
                event_time_entries_before,
                event_time_entries_after,
            } => Some((
                *leaf,
                *full_probes_before,
                *full_probes_after,
                *event_time_entries_before,
                *event_time_entries_after,
            )),
            SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_) => None,
        })
        .collect::<Vec<_>>();
    projects.sort_by_key(|project| project.0);
    assert_eq!(projects.len(), 3);
    assert_eq!((projects[0].1, projects[0].2), (0, 1));
    assert_eq!(
        projects
            .iter()
            .map(|(_, before, after, _, _)| after.saturating_sub(*before))
            .sum::<usize>(),
        2,
        "the parent lane should reuse its repository certificate while the parallel sibling lane probes once"
    );
    assert!(projects
        .iter()
        .all(|(_, _, _, event_entries_before, event_entries_after)| (
            *event_entries_before,
            *event_entries_after
        ) == (0, 1)));
    assert_eq!(activity.worker_count, 3);
    assert_eq!(activity.sources_started, 3);
    assert_eq!(activity.sources_completed, 3);
    assert_eq!(activity.peak_active_scanners, 2);
}

#[test]
fn partitioned_generation_is_identical_with_one_and_three_workers() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    write_scheduler_test_leaf(&root, 7, 0, 0);
    write_scheduler_test_leaf(&root, 7, 1, 0);
    write_scheduler_test_leaf(&root, 7, 1, 1);

    let repository = scheduler_test_repository(temp.path());
    let one = SchedulerStateTestAdapter {
        repository: repository.clone(),
        attributed_partitions: vec![7],
        failing_leaf: None,
        parallel_frontier: None,
        events: Arc::new(Mutex::new(Vec::new())),
    };
    let three = SchedulerStateTestAdapter {
        repository,
        attributed_partitions: vec![7],
        failing_leaf: None,
        parallel_frontier: Some((7, 1, Arc::new(std::sync::Barrier::new(2)))),
        events: Arc::new(Mutex::new(Vec::new())),
    };

    let (one_receipt, one_activity) =
        capture_parallel_test_generation(&one, &root, &temp.path().join("one"), 1);
    let (three_receipt, three_activity) =
        capture_parallel_test_generation(&three, &root, &temp.path().join("three"), 3);

    assert_eq!(one_activity.peak_active_scanners, 1);
    assert!(three_activity.peak_active_scanners >= 2);
    assert_eq!(one_receipt.generation_id, three_receipt.generation_id);
    assert_eq!(
        one_receipt.manifest().sources,
        three_receipt.manifest().sources
    );
}

#[test]
fn partition_scan_failure_finishes_every_begun_component() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    write_scheduler_test_leaf(&root, 2, 0, 0);
    write_scheduler_test_leaf(&root, 3, 0, 0);
    let events = Arc::new(Mutex::new(Vec::new()));
    let adapter = SchedulerStateTestAdapter {
        repository: scheduler_test_repository(temp.path()),
        attributed_partitions: Vec::new(),
        failing_leaf: Some(SchedulerLeafState {
            partition: 3,
            phase: 0,
            ordinal: 0,
        }),
        parallel_frontier: None,
        events: Arc::clone(&events),
    };

    let error =
        run_scheduler_test_capture(&adapter, &root, &temp.path().join("index"), 2).unwrap_err();
    assert!(error
        .detail
        .contains("scheduler test requested scan failure"));
    let hooks = events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| {
            matches!(
                event,
                SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_)
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        hooks,
        [
            SchedulerStateEvent::Begin(2),
            SchedulerStateEvent::Begin(3),
            SchedulerStateEvent::Finish(3),
            SchedulerStateEvent::Finish(2),
        ],
        "every begun component must finish exactly once even when its wave fails"
    );
}

#[test]
fn partition_lifecycle_ids_are_separate_from_frontier_worker_lanes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    write_scheduler_test_leaf(&root, 2, 0, 0);
    write_scheduler_test_leaf(&root, 3, 0, 0);
    let events = Arc::new(Mutex::new(Vec::new()));
    let adapter = SchedulerStateTestAdapter {
        repository: scheduler_test_repository(temp.path()),
        attributed_partitions: vec![2, 3],
        failing_leaf: None,
        parallel_frontier: None,
        events: Arc::clone(&events),
    };

    run_scheduler_test_capture(&adapter, &root, &temp.path().join("index"), 2).unwrap();
    let events = events.lock().unwrap().clone();
    let hooks = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_)
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        hooks,
        [
            SchedulerStateEvent::Begin(2),
            SchedulerStateEvent::Begin(3),
            SchedulerStateEvent::Finish(3),
            SchedulerStateEvent::Finish(2),
        ],
        "dense lifecycle IDs must continue to drive deterministic component hooks"
    );

    let mut projects = events
        .iter()
        .filter_map(|event| match event {
            SchedulerStateEvent::Project {
                leaf,
                full_probes_before,
                full_probes_after,
                ..
            } => Some((*leaf, *full_probes_before, *full_probes_after)),
            SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_) => None,
        })
        .collect::<Vec<_>>();
    projects.sort_by_key(|project| project.0);
    assert_eq!(projects.len(), 2);
    assert_eq!((projects[0].1, projects[0].2), (0, 1));
    assert_eq!((projects[1].1, projects[1].2), (0, 1));
}

#[test]
fn partition_waves_admit_largest_components_first() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    for partition in 0..17 {
        write_scheduler_test_leaf(&root, partition, 0, 0);
    }
    fs::write(
        root.join("partition-16-phase-0-leaf-0.jsonl"),
        b"{\"message\":\"large scheduler component\"}\n".repeat(128),
    )
    .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let adapter = SchedulerStateTestAdapter {
        repository: scheduler_test_repository(temp.path()),
        attributed_partitions: Vec::new(),
        failing_leaf: None,
        parallel_frontier: None,
        events: Arc::clone(&events),
    };

    run_scheduler_test_capture(&adapter, &root, &temp.path().join("index"), 4).unwrap();
    let events = events.lock().unwrap();
    let first_hook = events.iter().find(|event| {
        matches!(
            event,
            SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_)
        )
    });
    assert_eq!(first_hook, Some(&SchedulerStateEvent::Begin(16)));
}

#[test]
fn partition_logical_cache_lanes_are_fixed_and_clear_source_semantic_state() {
    for workers in [1, 4, 16] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir_all(&root).unwrap();
        for partition in 0..32 {
            write_scheduler_test_leaf(&root, partition, 0, 0);
        }
        let events = Arc::new(Mutex::new(Vec::new()));
        let adapter = SchedulerStateTestAdapter {
            repository: scheduler_test_repository(temp.path()),
            attributed_partitions: (0..32).collect(),
            failing_leaf: None,
            parallel_frontier: None,
            events: Arc::clone(&events),
        };

        let activity =
            run_scheduler_test_capture(&adapter, &root, &temp.path().join("index"), workers)
                .unwrap();
        let events = events.lock().unwrap().clone();
        let hooks = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_)
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(hooks.len(), 64);
        let mut begun_partitions = Vec::new();
        for wave in hooks.chunks(32) {
            let begun = wave[..16]
                .iter()
                .map(|event| match event {
                    SchedulerStateEvent::Begin(partition) => *partition,
                    _ => panic!("partition wave did not begin before finishing"),
                })
                .collect::<Vec<_>>();
            let finished = wave[16..]
                .iter()
                .map(|event| match event {
                    SchedulerStateEvent::Finish(partition) => *partition,
                    _ => panic!("partition wave did not finish in its closing half"),
                })
                .collect::<Vec<_>>();
            assert_eq!(finished, begun.iter().rev().copied().collect::<Vec<_>>());
            begun_partitions.extend(begun);
        }
        begun_partitions.sort_unstable();
        assert_eq!(begun_partitions, (0_u64..32).collect::<Vec<_>>());

        let mut projects = events
            .iter()
            .filter_map(|event| match event {
                SchedulerStateEvent::Project {
                    leaf,
                    full_probes_before,
                    full_probes_after,
                    event_time_entries_before,
                    event_time_entries_after,
                } => Some((
                    *leaf,
                    *full_probes_before,
                    *full_probes_after,
                    *event_time_entries_before,
                    *event_time_entries_after,
                )),
                SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_) => None,
            })
            .collect::<Vec<_>>();
        projects.sort_by_key(|project| project.0);
        assert_eq!(projects.len(), 32);
        let full_probes = projects
            .iter()
            .map(|(_, before, after, _, _)| after.saturating_sub(*before))
            .sum::<usize>();
        assert_eq!(
            full_probes, 16,
            "same-repository components must reuse fixed logical cache lanes independently of physical workers"
        );
        for (leaf, _, _, event_entries_before, event_entries_after) in &projects {
            assert_eq!(
                *event_entries_before, 0,
                "component {} leaked source-semantic event-time state on its shared cache lane",
                leaf.partition
            );
            assert_eq!(*event_entries_after, 1);
        }
        assert_eq!(activity.worker_count, workers);
        assert_eq!(activity.sources_started, 32);
        assert_eq!(activity.sources_completed, 32);
    }
}

#[test]
fn unpartitioned_defaults_keep_persistent_phase_worker_contexts() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    for phase in 0..=1 {
        for ordinal in 0..=1 {
            write_scheduler_test_leaf(&root, 0, phase, ordinal);
        }
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let adapter = UnpartitionedSchedulerStateTestAdapter(SchedulerStateTestAdapter {
        repository: scheduler_test_repository(temp.path()),
        attributed_partitions: vec![0],
        failing_leaf: None,
        parallel_frontier: None,
        events: Arc::clone(&events),
    });

    let activity =
        run_scheduler_test_capture(&adapter, &root, &temp.path().join("index"), 2).unwrap();
    let mut projects = events
        .lock()
        .unwrap()
        .iter()
        .map(|event| match event {
            SchedulerStateEvent::Project {
                leaf,
                full_probes_before,
                full_probes_after,
                event_time_entries_before,
                event_time_entries_after,
            } => (
                *leaf,
                *full_probes_before,
                *full_probes_after,
                *event_time_entries_before,
                *event_time_entries_after,
            ),
            SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_) => {
                panic!("unpartitioned defaults must not call partition hooks")
            }
        })
        .collect::<Vec<_>>();
    projects.sort_by_key(|project| project.0);
    assert_eq!(projects.len(), 4);
    for (leaf, probes_before, probes_after, event_entries_before, event_entries_after) in projects {
        assert_eq!(probes_before, usize::from(leaf.phase == 1));
        assert_eq!(probes_after, 1);
        assert_eq!(event_entries_before, 0);
        assert_eq!(event_entries_after, 1);
    }
    assert_eq!(activity.worker_count, 2);
    assert_eq!(activity.sources_started, 4);
    assert_eq!(activity.sources_completed, 4);
    assert_eq!(activity.peak_active_scanners, 2);
}

#[test]
fn serial_and_parallel_jsonl_emission_preserve_resource_unavailable() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    for workers in [1, 4] {
        let root = temp.path().join(format!("sessions-{workers}"));
        fs::create_dir_all(&root).unwrap();
        for index in 0..workers {
            fs::write(
                root.join(format!("{index}.jsonl")),
                b"{\"message\":\"bounded\"}\n",
            )
            .unwrap();
        }
        let resident = Mutex::new(FamilyResident::default());
        let mut writer = GenerationWriter::open(
            temp.path().join(format!("index-{workers}")),
            WriterOptions {
                indexer_threads: 1,
                memory_bytes: 15_000_000,
            },
        )
        .unwrap()
        .into_writer()
        .unwrap();
        let mut owners = HashMap::new();
        let mut complete_inventories = Vec::new();
        let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
        let mut record_rejections = SourceBackedRecordRejections::default();
        let mut sink = SourceBackedGenerationSink {
            core_record_preparer: writer.core_record_preparer(),
            writer: &mut writer,
            owners: &mut owners,
            complete_inventories: &mut complete_inventories,
            route_index: 0,
            route_identity: test_route_identity(),
            resources: SourceBackedRouteResources::for_test(workers, 1, u64::MAX),
            logical_source_failures: &mut logical_source_failures,
            record_rejections: &mut record_rejections,
            applied_removals: &mut Vec::new(),
            record_progress: None,
            current_source_progress: None,
        };

        let error = with_family_scanner_workers(workers, || {
            capture(
                &EmissionTestAdapter::ordinary(),
                &root,
                &resident,
                &mut sink,
            )
            .unwrap_err()
        });
        assert_eq!(error.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
    }
}

#[test]
fn jsonl_terminal_drift_and_io_failures_keep_distinct_route_kinds() {
    assert_eq!(
        normalized_jsonl_error_kind(&CaptureError::SourceChangedDuringCapture),
        Some(SourceBackedRouteErrorKind::SourceChanged)
    );
    assert_eq!(
        normalized_jsonl_error_kind(&CaptureError::Io(std::io::Error::from_raw_os_error(5))),
        Some(SourceBackedRouteErrorKind::ResourceUnavailable)
    );
    assert_eq!(
        normalized_jsonl_error_kind(&CaptureError::Io(std::io::Error::from_raw_os_error(24))),
        Some(SourceBackedRouteErrorKind::ResourceUnavailable)
    );
    assert_eq!(
        route_scan(
            &TestAdapter,
            CaptureError::Io(std::io::Error::from(std::io::ErrorKind::NotFound)),
        )
        .kind,
        SourceBackedRouteErrorKind::SourceChanged
    );
}

#[test]
fn active_source_family_contract_jsonl_terminal_inventory_observes_live_tree() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let first = root.join("first.jsonl");
    fs::write(&first, b"{\"message\":\"before\"}\n").unwrap();
    let adapter = TestAdapter;

    let (resident, inventory) = expected_state(&adapter, &root);
    let source = expected_source(&resident);
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Source(&source),
    ));
    fs::write(&first, b"{\"message\":\"changed between callbacks\"}\n").unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap_or(false)
    );

    let (resident, inventory) = expected_state(&adapter, &root);
    let source = expected_source(&resident);
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Source(&source),
    ));
    fs::write(root.join("new.jsonl"), b"{\"message\":\"late leaf\"}\n").unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap_or(false)
    );
}

#[test]
fn active_source_family_contract_jsonl_terminal_inventory_accepts_proven_append() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let first = root.join("first.jsonl");
    fs::write(&first, TEST_RECORD).unwrap();
    let adapter = TestAdapter;
    let (resident, inventory) = expected_state(&adapter, &root);
    let source = expected_source(&resident);
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Source(&source),
    ));

    OpenOptions::new()
        .append(true)
        .open(&first)
        .unwrap()
        .write_all(b"{\"message\":\"next refresh\"}\n")
        .unwrap();
    assert!(revalidate_complete_inventory(&adapter, &root, &resident, &inventory,).unwrap());
}

#[test]
fn active_source_family_contract_jsonl_terminal_inventory_rejects_reappearance() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("retained.jsonl"), b"{\"message\":\"kept\"}\n").unwrap();
    let deleted_path = root.join("deleted.jsonl");
    fs::write(&deleted_path, b"{\"message\":\"old\"}\n").unwrap();
    let adapter = TestAdapter;
    let before = adapter.discover(&root).unwrap();
    let deleted_source = before
        .leaves()
        .iter()
        .find(|leaf| leaf.source_path() == deleted_path)
        .unwrap()
        .source()
        .clone();

    fs::remove_file(&deleted_path).unwrap();
    let (mut resident, inventory) = expected_state(&adapter, &root);
    let opening = resident.opening_inventory.as_ref().unwrap().clone();
    resident
        .absent_sources
        .push(JsonlFamilyAbsentMember::from_path(&opening, deleted_path.clone()).unwrap());
    let deletion = CertifiedSourceDeletion::from_inventory(deleted_source, &inventory).unwrap();
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Deletion(&deletion),
    ));

    fs::write(&deleted_path, b"{\"message\":\"reappeared\"}\n").unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap_or(false)
    );
}

#[test]
fn active_source_family_contract_jsonl_frozen_multi_root_defers_new_leaves() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first_root = temp.path().join("sessions");
    let second_root = temp.path().join("archived_sessions");
    fs::create_dir_all(&first_root).unwrap();
    fs::create_dir_all(&second_root).unwrap();
    let retained = first_root.join("first.jsonl");
    fs::write(&retained, TEST_RECORD).unwrap();
    fs::write(second_root.join("archived.jsonl"), TEST_RECORD).unwrap();
    let adapter = FrozenMultiRootTestAdapter {
        roots: vec![first_root.clone(), second_root.clone()],
    };
    let selection_root = temp.path().join("codex-selection");

    let (resident, inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);
    fs::write(second_root.join("late.jsonl"), TEST_RECORD).unwrap();
    assert!(
        revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,).unwrap()
    );

    let (resident, inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);
    fs::remove_file(retained).unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,)
            .unwrap_or(false)
    );
}

#[test]
fn active_source_family_contract_jsonl_frozen_root_replacement_fails_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("first.jsonl"), TEST_RECORD).unwrap();
    let adapter = FrozenMultiRootTestAdapter {
        roots: vec![root.clone()],
    };
    let selection_root = temp.path().join("codex-selection");
    let (resident, inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);

    let moved = temp.path().join("moved-sessions");
    fs::rename(&root, &moved).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("first.jsonl"), TEST_RECORD).unwrap();
    assert!(
        revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,).is_err()
    );
}

#[test]
fn active_source_family_contract_jsonl_terminal_noop_is_metadata_only_without_recataloging() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("first.jsonl"), TEST_RECORD).unwrap();
    let adapter = TerminalRootSwapTestAdapter {
        root,
        discoveries: AtomicUsize::new(0),
    };
    let selection_root = temp.path().join("codex-selection");
    let (resident, inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);

    reset_jsonl_prefix_hash_bytes();
    assert!(
        revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory).unwrap()
    );
    assert_eq!(adapter.discoveries.load(Ordering::SeqCst), 1);
    assert_eq!(jsonl_prefix_hash_bytes(), 0);
}

#[test]
fn active_source_family_contract_jsonl_frozen_rejects_root_swap_without_recataloging() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("first.jsonl"), TEST_RECORD).unwrap();
    let adapter = TerminalRootSwapTestAdapter {
        root: root.clone(),
        discoveries: AtomicUsize::new(0),
    };
    let selection_root = temp.path().join("codex-selection");
    let (resident, inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);

    fs::OpenOptions::new()
        .append(true)
        .open(root.join("first.jsonl"))
        .unwrap()
        .write_all(b"{\"message\":\"appended\"}\n")
        .unwrap();
    let moved = temp.path().join("moved-sessions");
    let swap_root = root.clone();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let worker_barrier = Arc::clone(&barrier);
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        fs::rename(&swap_root, moved).unwrap();
        fs::create_dir(&swap_root).unwrap();
        fs::write(swap_root.join("first.jsonl"), TEST_RECORD).unwrap();
        worker_barrier.wait();
    });
    set_after_jsonl_prefix_hash_hook(move || {
        barrier.wait();
        barrier.wait();
    });

    assert!(
        revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,).is_err()
    );
    worker.join().unwrap();
    assert_eq!(adapter.discoveries.load(Ordering::SeqCst), 1);
}

#[test]
fn active_source_family_contract_jsonl_frozen_inventory_rejects_deleted_source_reappearance() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("retained.jsonl"), TEST_RECORD).unwrap();
    let deleted_path = root.join("deleted.jsonl");
    fs::write(&deleted_path, TEST_RECORD).unwrap();
    let adapter = FrozenMultiRootTestAdapter {
        roots: vec![root.clone()],
    };
    let selection_root = temp.path().join("codex-selection");
    let before = adapter.discover(&selection_root).unwrap();
    let deleted_source = before
        .leaves()
        .iter()
        .find(|leaf| leaf.source_path() == deleted_path)
        .unwrap()
        .source()
        .clone();

    fs::remove_file(&deleted_path).unwrap();
    let (mut resident, inventory) = expected_state(&adapter, &selection_root);
    let opening = resident.opening_inventory.as_ref().unwrap().clone();
    resident
        .absent_sources
        .push(JsonlFamilyAbsentMember::from_path(&opening, deleted_path.clone()).unwrap());
    resident.owned_sources.insert(
        deleted_source.exact_descriptor_digest(),
        deleted_source.clone(),
    );
    let deletion = CertifiedSourceDeletion::from_inventory(deleted_source, &inventory).unwrap();
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Deletion(&deletion),
    ));

    fs::write(&deleted_path, TEST_RECORD).unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,).unwrap()
    );
}
