use std::{fs, path::Path, sync::Arc};

use super::super::JsonlReader;
use super::*;
use ctx_history_core::{CoreRecord, SourceAnchor};
use ctx_history_index::{CommitReceipt, GenerationWriter, WriterOptions};

const TEST_SOURCE_FORMAT: &str = "terminal_witness_jsonl";
const TEST_SCHEMA: &str = "terminal-witness-v1";

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
    adapter: &TestAdapter,
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
                "terminal-witness-parser-v1",
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
        },
        inventory,
    )
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

struct CheckpointTestAdapter;

struct CheckpointTestProjector {
    projected_records: u64,
    resumed: bool,
}

impl JsonlFamilyProjector for CheckpointTestProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
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
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        let Some(checkpoint) = checkpoint else {
            if base_event_lookup.is_some() {
                return Err(CaptureError::InvalidPayload(
                    "cold checkpoint test unexpectedly received a base lookup".to_owned(),
                ));
            }
            return self.projector(leaf, source_file, imported_at);
        };
        if base_event_lookup.is_none() {
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
    adapter: &ParallelTestAdapter,
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
    .unwrap();
    let mut owners = HashMap::new();
    let mut complete_inventories = Vec::new();
    {
        let mut report_progress = |_| Ok(());
        let mut sink = SourceBackedGenerationSink {
            writer: &mut writer,
            owners: &mut owners,
            complete_inventories: &mut complete_inventories,
            route_index: 0,
            leaf_worker_budget: workers,
            automatic_missing_observed_at_unix_ms: None,
            report_current_source_progress: &mut report_progress,
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
    .unwrap();
    let mut owners = HashMap::new();
    let mut complete_inventories = Vec::new();
    {
        let mut report_progress = |_| Ok(());
        let mut sink = SourceBackedGenerationSink {
            writer: &mut writer,
            owners: &mut owners,
            complete_inventories: &mut complete_inventories,
            route_index: 0,
            leaf_worker_budget: workers,
            automatic_missing_observed_at_unix_ms: None,
            report_current_source_progress: &mut report_progress,
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
    .unwrap();
    let mut owners = HashMap::new();
    let mut complete_inventories = Vec::new();
    let mut report_progress = |_| Ok(());
    let mut sink = SourceBackedGenerationSink {
        writer: &mut writer,
        owners: &mut owners,
        complete_inventories: &mut complete_inventories,
        route_index: 0,
        leaf_worker_budget: 4,
        automatic_missing_observed_at_unix_ms: None,
        report_current_source_progress: &mut report_progress,
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
