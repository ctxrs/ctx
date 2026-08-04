use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use super::super::JsonlReader;
use super::*;
use crate::provider::source_backed::{
    SourceBackedLogicalSourceFailures, SourceBackedRecordRejections, SourceBackedRouteResources,
};
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
                source_observation(leaf.source(), checkpoint.source_observation()).unwrap();
            let certificate = CertifiedSource::certify(
                observation.clone(),
                observation,
                adapter.parser_revision(),
                *checkpoint.complete_prefix_sha256(),
                ScannedSourceCounts::default(),
            )
            .unwrap();
            (
                leaf.source().exact_descriptor_digest(),
                TerminalSourceEvidence {
                    certificate,
                    checkpoint: Some(checkpoint),
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
        if self.discoveries.fetch_add(1, Ordering::SeqCst) == 1 {
            let moved = self.root.with_file_name("moved-sessions");
            fs::rename(&self.root, moved).unwrap();
            fs::create_dir(&self.root).unwrap();
            fs::write(self.root.join("first.jsonl"), TEST_RECORD).unwrap();
        }
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

    fn revalidate_leaf(
        &self,
        leaf: &JsonlFamilyLeaf,
        certificate: &CertifiedSource,
        _checkpoint: Option<&JsonlCheckpoint>,
    ) -> Result<bool> {
        Ok(leaf
            .source()
            .exact_descriptor_eq(certificate.observation().source()))
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

struct IdentityRevisionTestAdapter {
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
        "identity-revision-test-parser-v1"
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

struct EmissionTestAdapter;

struct EmissionTestProjector {
    source: SourceKey,
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
        let ordinal = record.evidence().physical_ordinal();
        emit(emission_test_record(&self.source, ordinal)?)
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
        let observation = source_observation(leaf.source(), leaf.observation())?;
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
        Ok(Some(JsonlFamilyOptimizedLeafOutcome::replacement(
            certificate,
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
    .unwrap();
    let mut publications = Vec::new();
    let mut worker = JsonlFamilyWorkerContext::default();
    let prepared = prepare_leaf(
        &adapter,
        leaf,
        None,
        &writer.base_event_identity_lookup(),
        &mut worker,
        &mut |append, records| {
            publications.push((append, records.len()));
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(adapter.scans.load(Ordering::SeqCst), 1);
    assert_eq!(publications, vec![(false, 0)]);
    assert!(prepared.append.is_none());
    assert!(prepared.checkpoint.is_none());
    assert_eq!(
        prepared.certificate.parser_revision(),
        adapter.parser_revision()
    );
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
    .unwrap();
    let mut worker = JsonlFamilyWorkerContext::default();
    let error = prepare_leaf(
        &adapter,
        leaf,
        None,
        &writer.base_event_identity_lookup(),
        &mut worker,
        &mut |_append, _records| Ok(()),
    )
    .err()
    .expect("wrong-source optimized emission must fail");
    assert!(error
        .to_string()
        .contains("optimized JSONL leaf emitted a record for another source"));
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
    use std::{fs::OpenOptions, io::Write};

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
    use std::{fs::OpenOptions, io::Write};

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
        revision: "content-occurrence-v1",
        expected_mode: JsonlFamilyProjectionMode::Cold,
    };
    let (cold_receipt, _) = capture_parallel_test_generation(&cold, &root, &index, 1);

    let upgraded = IdentityRevisionTestAdapter {
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
fn serial_and_parallel_jsonl_emission_preserve_resource_unavailable() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    for index in 0..4 {
        fs::write(
            root.join(format!("{index}.jsonl")),
            b"{\"message\":\"bounded\"}\n",
        )
        .unwrap();
    }

    for workers in [1, 4] {
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
            capture(&EmissionTestAdapter, &root, &resident, &mut sink).unwrap_err()
        });
        assert_eq!(error.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
    }
}

#[test]
fn active_source_family_contract_jsonl_terminal_inventory_rediscovers_live_tree() {
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
    assert!(!revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap());
}

#[test]
fn active_source_family_contract_jsonl_terminal_inventory_accepts_proven_append() {
    use std::{fs::OpenOptions, io::Write};

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
    let (resident, inventory) = expected_state(&adapter, &root);
    let deletion = CertifiedSourceDeletion::from_inventory(deleted_source, &inventory).unwrap();
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Deletion(&deletion),
    ));

    fs::write(&deleted_path, b"{\"message\":\"reappeared\"}\n").unwrap();
    assert!(!revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap());
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
        !revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,).unwrap()
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
fn active_source_family_contract_jsonl_frozen_rejects_root_swap_during_rediscovery() {
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

    assert!(
        !revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,).unwrap()
    );
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
