use super::*;

#[derive(Default)]
struct CompoundAdapter {
    modes: Mutex<Vec<JsonlFamilyProjectionMode>>,
    projected: Arc<AtomicUsize>,
}

// Synthetic provider inventory: observe metadata through retained no-follow
// authority, not directory mtime or an unbounded collection of artifact bodies.
fn inventory_digest(authority: &ProviderSourceRoot) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    let directory = match authority.open_directory(Path::new("artifacts")) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(Sha256::digest(b"absent").into()),
        Err(error) => return Err(error),
    };
    let mut names = directory.entries(64)?;
    names.sort();
    for name in names {
        let opened = authority.open_file(&Path::new("artifacts").join(&name))?;
        let name = name.as_encoded_bytes();
        digest.update((name.len() as u64).to_be_bytes());
        digest.update(name);
        digest.update(opened.ordinary_file_token());
        opened.revalidate_leaf()?;
    }
    directory.revalidate()?;
    Ok(digest.finalize().into())
}

impl JsonlFamilyAdapter for CompoundAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        TestAdapter.provider()
    }
    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }
    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }
    fn parser_revision(&self) -> &'static str {
        "compound-test-v1"
    }
    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        let inventory = TestAdapter.discover(root)?;
        let authority = Arc::clone(inventory.accepted_leaves().next().unwrap().authority());
        let digest = inventory_digest(&authority)?;
        let leaves = inventory
            .accepted_leaves()
            .map(|leaf| {
                let authority = Arc::clone(&authority);
                leaf.clone()
                    .with_compound_input(digest, move || inventory_digest(&authority))
            })
            .collect::<Result<Vec<_>>>()?;
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn semantic_executor(
        &self,
        leaf: &JsonlFamilyLeaf,
        checkpoint: Option<&TypedKey>,
        _lookup: Option<IndexBaseEventLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Option<Box<JsonlFamilySemanticExecutorObject>>> {
        self.modes.lock().unwrap().push(mode);
        assert_eq!(
            checkpoint.is_some(),
            mode == JsonlFamilyProjectionMode::CertifiedAppend
        );
        let body = match leaf.authority().open_file(Path::new("artifacts/body.txt")) {
            Ok(file) => String::from_utf8(file.read_all_bounded(2 * 1024 * 1024)?).unwrap(),
            Err(error) if error.is_not_found() => "unavailable".to_owned(),
            Err(error) => return Err(error),
        };
        Ok(Some(Box::new(CompoundExecutor {
            source: leaf.source().clone(),
            body,
            consumed: 0,
            projected: Arc::clone(&self.projected),
        })))
    }

    fn scan_optimized_leaf(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _base: Option<&CertifiedSource>,
        _lookup: &IndexBaseEventLookup,
        _worker: &mut JsonlFamilyWorkerContext,
        _emit: &mut dyn FnMut(JsonlFamilyPublication, u64, Vec<CoreRecord>) -> Result<()>,
    ) -> Result<Option<JsonlFamilyOptimizedLeafOutcome>> {
        panic!("compound input must not bypass shared checkpoint admission")
    }
}

struct CompoundExecutor {
    source: SourceKey,
    body: String,
    consumed: u64,
    projected: Arc<AtomicUsize>,
}

impl JsonlFamilySemanticExecutor for CompoundExecutor {
    type Runtime = TestJsonlRuntime;

    fn preflight(
        &mut self,
        input: &mut JsonlFamilyExecutionIo,
    ) -> Result<JsonlFamilySemanticPreflight> {
        while input.next_record()?.is_some() {}
        Ok(JsonlFamilySemanticPreflight::Ready)
    }

    fn next_page(
        &mut self,
        input: &mut JsonlFamilyExecutionIo,
        _worker: &mut JsonlFamilyWorkerContext,
    ) -> Result<Option<JsonlFamilySemanticPage>> {
        let Some(record) = input.next_record()? else {
            return Ok(None);
        };
        let mut projected = emission_test_record(&self.source, record.physical_ordinal())?;
        projected.content.normalized_body = Some(self.body.clone());
        self.consumed += 1;
        self.projected.fetch_add(1, Ordering::SeqCst);
        Ok(Some(JsonlFamilySemanticPage::new(vec![projected])))
    }

    fn finish(self: Box<Self>) -> Result<JsonlFamilySemanticSummary> {
        Ok(JsonlFamilySemanticSummary::new(
            self.consumed,
            0,
            Some(TypedKey::U64(1)),
        ))
    }
}

fn fixture(root: &Path) {
    fs::create_dir_all(root.join("artifacts")).unwrap();
    fs::write(root.join("events.jsonl"), TEST_RECORD).unwrap();
    fs::write(root.join("artifacts/body.txt"), "before").unwrap();
}

fn capture_compound(
    adapter: &CompoundAdapter,
    root: &Path,
    index: &Path,
    workers: usize,
) -> IndexCaptureCommitReceipt {
    capture_parallel_test_generation_with_terminal_revalidation(adapter, root, index, workers)
        .unwrap()
        .0
}

#[test]
fn compound_input_cold_noop_append_and_artifact_only_replacement() {
    for workers in [1, 4] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let index = temp.path().join("index");
        fixture(&root);
        // A second leaf exercises parallel staging and admission as well.
        fs::write(root.join("sibling.jsonl"), TEST_RECORD).unwrap();
        let adapter = CompoundAdapter::default();
        let cold = capture_compound(&adapter, &root, &index, workers);
        assert_eq!(adapter.projected.load(Ordering::SeqCst), 2);
        let noop = capture_compound(&CompoundAdapter::default(), &root, &index, workers);
        assert_eq!(noop.manifest(), cold.manifest());
        assert_eq!(
            jsonl_family_admission_activity().retained_terminal_sources,
            2
        );
        assert_eq!(
            jsonl_family_scanner_activity(),
            JsonlFamilyScannerActivity::default()
        );

        OpenOptions::new()
            .append(true)
            .open(root.join("events.jsonl"))
            .unwrap()
            .write_all(TEST_RECORD)
            .unwrap();
        let adapter = CompoundAdapter::default();
        let append = capture_compound(&adapter, &root, &index, workers);
        assert_eq!(
            *adapter.modes.lock().unwrap(),
            [JsonlFamilyProjectionMode::CertifiedAppend]
        );
        assert_eq!(adapter.projected.load(Ordering::SeqCst), 1);
        assert_eq!(append.manifest().records.len(), 3);

        // Fresh adapter/resident: only persisted checkpoint binding can detect
        // this artifact-only change while transcript observations remain equal.
        fs::write(root.join("artifacts/body.txt"), "after").unwrap();
        let adapter = CompoundAdapter::default();
        let replaced = capture_compound(&adapter, &root, &index, workers);
        assert_eq!(
            *adapter.modes.lock().unwrap(),
            [JsonlFamilyProjectionMode::Replacement; 2]
        );
        assert_eq!(adapter.projected.load(Ordering::SeqCst), 3);
        assert_eq!(replaced.manifest().records.len(), 3);
        assert!(replaced
            .manifest()
            .records
            .iter()
            .all(|record| record.content.meaningful_text() == "after"));
        for (old, new) in append
            .manifest()
            .sources
            .iter()
            .zip(&replaced.manifest().sources)
        {
            assert_eq!(old.observation(), new.observation());
            assert_ne!(old.frontier(), new.frontier());
        }
        let old_ids: BTreeSet<_> = append
            .manifest()
            .records
            .iter()
            .map(|r| r.event_id.as_uuid())
            .collect();
        let new_ids: BTreeSet<_> = replaced
            .manifest()
            .records
            .iter()
            .map(|r| r.event_id.as_uuid())
            .collect();
        assert_eq!(old_ids, new_ids);
    }
}

#[test]
fn compound_input_growth_removal_and_repair_force_replacement_even_with_append() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fixture(&root);
    capture_compound(&CompoundAdapter::default(), &root, &index, 1);
    for mutation in ["growth", "removal", "repair"] {
        match mutation {
            "growth" => {
                for n in 0..12 {
                    fs::write(root.join(format!("artifacts/{n}.txt")), vec![b'x'; 100_000])
                        .unwrap();
                }
            }
            "removal" => fs::remove_file(root.join("artifacts/body.txt")).unwrap(),
            "repair" => fs::write(root.join("artifacts/body.txt"), "repaired").unwrap(),
            _ => unreachable!(),
        }
        OpenOptions::new()
            .append(true)
            .open(root.join("events.jsonl"))
            .unwrap()
            .write_all(TEST_RECORD)
            .unwrap();
        let adapter = CompoundAdapter::default();
        let receipt = capture_compound(&adapter, &root, &index, 1);
        assert_eq!(
            *adapter.modes.lock().unwrap(),
            [JsonlFamilyProjectionMode::Replacement]
        );
        assert_eq!(
            adapter.projected.load(Ordering::SeqCst),
            receipt.manifest().records.len()
        );
        let expected = match mutation {
            "removal" => "unavailable",
            "repair" => "repaired",
            _ => "before",
        };
        assert!(receipt
            .manifest()
            .records
            .iter()
            .all(|record| record.content.meaningful_text() == expected));
        let checkpoint = receipt.manifest().sources[0]
            .frontier()
            .unwrap()
            .checkpoint();
        assert!(serde_json::to_vec(checkpoint).unwrap().len() < 4096);
    }
}

#[test]
fn compound_input_terminal_races_reject_cold_noop_and_append() {
    for phase in ["cold", "noop", "append"] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let index = temp.path().join("index");
        fixture(&root);
        if phase != "cold" {
            capture_compound(&CompoundAdapter::default(), &root, &index, 1);
        }
        if phase == "append" {
            OpenOptions::new()
                .append(true)
                .open(root.join("events.jsonl"))
                .unwrap()
                .write_all(TEST_RECORD)
                .unwrap();
        }
        let base = test_generations().lock().unwrap().get(&index).cloned();
        let artifact = root.join("artifacts/body.txt");
        set_before_jsonl_terminal_physical_revalidation_hook(root.clone(), move || {
            fs::write(artifact, "raced").unwrap();
        });
        let error = capture_parallel_test_generation_with_terminal_revalidation(
            &CompoundAdapter::default(),
            &root,
            &index,
            1,
        )
        .unwrap_err();
        assert!(error.is_source_changed(), "{phase}: {error:?}");
        assert_eq!(
            test_generations().lock().unwrap().get(&index).cloned(),
            base
        );
    }
}

#[test]
fn compound_input_artifact_member_event_enters_normal_route_replacement() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fixture(&root);
    capture_compound(&CompoundAdapter::default(), &root, &index, 1);
    let artifact = root.join("artifacts/body.txt");
    fs::write(&artifact, "daemon repair").unwrap();
    let adapter = CompoundAdapter::default();
    let (writer, resident, ()) = capture_test_generation!(
        &adapter,
        &root,
        &index,
        1,
        |resident, sink: &mut SourceBackedGenerationSink<'_>| {
            // The directory watch catalog passes an ordinary artifact path as
            // an exact member. Compound providers keep partial-member admission
            // disabled, so the normal route falls back to complete discovery.
            sink.resources = sink
                .resources
                .clone()
                .with_member_workset(Some(BTreeSet::from([artifact])));
            capture(&adapter, &root, resident, sink).unwrap();
        }
    );
    assert_eq!(writer.activity().begin_source_replacements, 1);
    assert_eq!(writer.activity().begin_source_appends, 0);
    assert_eq!(
        *adapter.modes.lock().unwrap(),
        [JsonlFamilyProjectionMode::Replacement]
    );
    assert!(revalidate_test_sources(&root, &resident).unwrap());
    let inventory = resident
        .lock()
        .unwrap()
        .certified_inventory
        .clone()
        .unwrap();
    assert!(revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap());
    let receipt = writer.commit(|_| true, |_| true).unwrap();
    let receipt = IndexCaptureCommitReceipt::new(receipt);
    assert_eq!(
        receipt.manifest().records[0].content.meaningful_text(),
        "daemon repair"
    );
}

#[test]
fn compound_input_admission_rejects_stale_duplicate_and_failed_observers() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    fixture(temp.path());
    let inventory = TestAdapter.discover(temp.path()).unwrap();
    let leaf = inventory.accepted_leaves().next().unwrap();
    assert!(leaf
        .clone()
        .with_compound_input([1; 32], || Ok([2; 32]))
        .unwrap_err()
        .is_source_changed());
    let bound = leaf
        .clone()
        .with_compound_input([1; 32], || Ok([1; 32]))
        .unwrap();
    assert!(bound.with_compound_input([1; 32], || Ok([1; 32])).is_err());
    assert!(matches!(
        leaf.clone().with_compound_input([1; 32], || {
            Err(CaptureError::SystemInvariant("observer failed"))
        }),
        Err(CaptureError::SystemInvariant("observer failed"))
    ));
}

#[cfg(unix)]
#[test]
fn compound_input_observer_does_not_follow_artifact_symlinks() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fixture(&root);
    let adapter = CompoundAdapter::default();
    let opening = adapter.discover(&root).unwrap();
    let leaf = opening.accepted_leaves().next().unwrap();
    fs::remove_file(root.join("artifacts/body.txt")).unwrap();
    std::os::unix::fs::symlink(temp.path().join("outside"), root.join("artifacts/body.txt"))
        .unwrap();
    assert!(leaf.terminal_dependencies.revalidate().is_err());
    assert!(adapter.discover(&root).is_err());
}
