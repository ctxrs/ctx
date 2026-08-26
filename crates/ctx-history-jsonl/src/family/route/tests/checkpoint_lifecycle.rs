use super::*;

fn logical_count_checkpoint(
    adapter: &JsonlFamilyAdapterObject,
    leaf: &JsonlFamilyLeaf,
) -> (FamilyCheckpoint, CertifiedSource) {
    let opened = leaf.open_verified().unwrap();
    let mut reader =
        JsonlReader::open(physical_identity(adapter, leaf), opened, None, None).unwrap();
    while reader
        .visit_page(&mut |_record| -> Result<()> { Ok(()) })
        .unwrap()
        .is_some()
    {}
    let physical = reader.outcome().unwrap().checkpoint().clone();
    assert_eq!(physical.next_physical_ordinal(), 2);
    assert!(physical.terminal());
    let checkpoint = FamilyCheckpoint {
        version: FamilyCheckpoint::VERSION,
        provider_parser_revision: adapter.parser_revision().to_owned(),
        event_identity_revision: adapter.event_identity_revision().to_owned(),
        binding_digest: binding_digest(leaf).unwrap(),
        physical,
        admitted_eof_sha256: None,
        complete_prefix_ends_with_terminal_nul_padding: false,
        represented_physical_records: 2,
        rejected_records: 0,
        logical_complete_records: 2,
        rejected_logical_records: 0,
        indexed_documents: 1,
        provider_checkpoint: None,
    };
    let observation =
        source_observation::<CaptureError>(leaf.source(), leaf.observation()).unwrap();
    let frontier = SourceFrontier::new(
        FAMILY_FRONTIER_KIND,
        checkpoint.encode_frontier_key::<CaptureError>().unwrap(),
        checkpoint.physical.complete_prefix_end(),
        *checkpoint.physical.complete_prefix_sha256(),
    )
    .unwrap();
    let certificate = CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        adapter.parser_revision(),
        *checkpoint.physical.complete_prefix_sha256(),
        ScannedSourceCounts {
            complete_records: 2,
            retained_records: 1,
            rejected_records: 0,
            ignored_records: 1,
            indexed_documents: 1,
            certified_bytes: checkpoint.physical.complete_prefix_end(),
        },
        Some(frontier),
    )
    .unwrap();
    (checkpoint, certificate)
}

#[test]
fn logical_counts_decode_and_retain_unchanged_committed_checkpoints() {
    const LEAVES: usize = 48;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    for leaf in 0..LEAVES {
        fs::write(
            root.join(format!("logical-{leaf:03}.jsonl")),
            b"{\"message\":\"represented\"}\n{\"message\":\"logically-ignored\"}\n",
        )
        .unwrap();
    }

    let adapter = TestAdapter;
    let inventory = adapter.discover(&root).unwrap();
    assert_eq!(inventory.accepted_len(), LEAVES);
    let mut sources = inventory
        .accepted_leaves()
        .map(|leaf| {
            let (checkpoint, certificate) = logical_count_checkpoint(&adapter, leaf);
            assert_eq!(certificate.counts().complete_records, 2);
            assert_eq!(certificate.counts().retained_records, 1);
            assert_eq!(certificate.counts().rejected_records, 0);
            assert_eq!(certificate.counts().ignored_records, 1);
            assert_eq!(
                super::super::leaf::decode_checkpoint(&adapter, leaf, &certificate).unwrap(),
                checkpoint,
            );
            certificate
        })
        .collect::<Vec<_>>();
    sources.sort_by(|left, right| {
        left.observation()
            .source()
            .cmp(right.observation().source())
    });
    let route_sources = sources
        .iter()
        .map(|source| source.observation().source().clone())
        .collect::<Vec<_>>();
    let base = TestSnapshot {
        sources,
        route_identity: Some(test_route_identity()),
        route_sources,
        records: Vec::new(),
    };
    test_generations()
        .lock()
        .unwrap()
        .insert(index.clone(), base.clone());

    let writer = capture_test_generation_without_commit(&adapter, &root, &index, 8);
    assert_eq!(
        jsonl_family_admission_activity(),
        JsonlFamilyAdmissionActivity {
            selected_leaves: LEAVES,
            bases: LEAVES,
            retained_terminal_sources: LEAVES,
            checkpoint_rejections: 0,
        }
    );
    assert_eq!(
        writer.activity(),
        TestLifecycleActivity {
            begin_source_replacements: 0,
            begin_source_appends: 0,
            retained_sources: LEAVES,
            deleted_sources: 0,
        }
    );
    assert_eq!(
        jsonl_family_scanner_activity(),
        JsonlFamilyScannerActivity::default(),
        "retained checkpoints must not enter scanner tasks",
    );
    let unchanged = IndexCaptureCommitReceipt::new(writer.commit(|_| true, |_| true).unwrap());
    assert_eq!(unchanged.generation_id, "test-generation-1");
    assert_eq!(unchanged.manifest(), &base);
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
fn family_checkpoint_writes_compact_utf8_and_reads_legacy_bytes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("checkpoint.jsonl"), b"{\"message\":\"prefix\"}\n").unwrap();

    let receipt = capture_checkpoint_test_generation(&root, &index, 1);
    let frontier = receipt.manifest().sources[0].frontier().unwrap();
    let TypedKey::Utf8(json) = frontier.checkpoint() else {
        panic!("new family checkpoint was not compact UTF-8");
    };
    let checkpoint =
        FamilyCheckpoint::decode_frontier_key::<CaptureError>(frontier.checkpoint()).unwrap();
    assert_eq!(checkpoint.version, FamilyCheckpoint::VERSION);

    let legacy = TypedKey::bytes(serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    assert_eq!(
        FamilyCheckpoint::decode_frontier_key::<CaptureError>(&legacy).unwrap(),
        checkpoint
    );
    assert!(
        serde_json::to_vec(frontier.checkpoint()).unwrap().len()
            < serde_json::to_vec(&legacy).unwrap().len()
    );
    assert_eq!(
        serde_json::from_str::<FamilyCheckpoint>(json).unwrap(),
        checkpoint
    );
}

#[test]
fn oversized_optional_provider_checkpoint_is_omitted() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("checkpoint.jsonl"), b"{\"message\":\"prefix\"}\n").unwrap();

    let adapter = CheckpointTestAdapter {
        fixed_checkpoint_bytes: Some(60 * 1024),
        ..CheckpointTestAdapter::default()
    };
    let receipt = capture_parallel_test_generation(&adapter, &root, &index, 1).0;
    assert_eq!(provider_checkpoints(&receipt), [None]);
}

#[test]
fn nonterminal_checkpoint_noops_then_resumes_only_its_uncertified_tail() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let transcript = root.join("incomplete.jsonl");
    fs::write(
        &transcript,
        b"{\"message\":\"prefix\"}\n{\"message\":\"tail\"",
    )
    .unwrap();
    let writer = match TestLifecycle::open(&temp.path().join("index"), ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(writer) => writer,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => unreachable!(),
    };
    let lookup = writer.base_event_identity_lookup();
    let adapter = CheckpointTestAdapter::default();
    let mut worker = JsonlFamilyWorkerContext::default();

    let cold = {
        let inventory = adapter.discover(&root).unwrap();
        let leaf = inventory.accepted_leaves().next().unwrap();
        let mut emit = |_event| Ok(());
        let mut output = JsonlLeafOutput::new(&mut emit);
        prepare_leaf(
            &adapter,
            leaf,
            None,
            &lookup,
            &mut worker,
            &mut output,
            true,
        )
        .unwrap()
    };
    let cold_inventory = adapter.discover(&root).unwrap();
    let cold_checkpoint = super::super::leaf::decode_checkpoint(
        &adapter,
        cold_inventory.accepted_leaves().next().unwrap(),
        &cold.certificate,
    )
    .unwrap();
    assert!(!cold_checkpoint.physical.terminal());
    assert_eq!(cold_checkpoint.physical.next_physical_ordinal(), 1);
    assert_eq!(cold_checkpoint.provider_checkpoint, Some(TypedKey::U64(1)));
    assert_eq!(
        adapter.projection_modes.lock().unwrap().as_slice(),
        [JsonlFamilyProjectionMode::Cold]
    );

    let unchanged = {
        let inventory = adapter.discover(&root).unwrap();
        let leaf = inventory.accepted_leaves().next().unwrap();
        let mut events = Vec::new();
        let mut emit = |event| {
            events.push(event);
            Ok(())
        };
        let prepared = {
            let mut output = JsonlLeafOutput::new(&mut emit);
            prepare_leaf(
                &adapter,
                leaf,
                Some(&cold.certificate),
                &lookup,
                &mut worker,
                &mut output,
                true,
            )
            .unwrap()
        };
        assert!(events.is_empty());
        prepared
    };
    assert_eq!(unchanged.certificate, cold.certificate);
    assert!(unchanged.append.is_some());
    assert_eq!(
        adapter.projection_modes.lock().unwrap().as_slice(),
        [JsonlFamilyProjectionMode::Cold],
        "an exactly unchanged incomplete tail must not reconstruct a projector"
    );

    OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap()
        .write_all(b"}\n")
        .unwrap();
    let completed = {
        let inventory = adapter.discover(&root).unwrap();
        let leaf = inventory.accepted_leaves().next().unwrap();
        let mut emit = |_event| Ok(());
        let mut output = JsonlLeafOutput::new(&mut emit);
        prepare_leaf(
            &adapter,
            leaf,
            Some(&cold.certificate),
            &lookup,
            &mut worker,
            &mut output,
            true,
        )
        .unwrap()
    };
    assert!(completed.append.is_some());
    let completed_inventory = adapter.discover(&root).unwrap();
    let completed_checkpoint = super::super::leaf::decode_checkpoint(
        &adapter,
        completed_inventory.accepted_leaves().next().unwrap(),
        &completed.certificate,
    )
    .unwrap();
    assert!(completed_checkpoint.physical.terminal());
    assert_eq!(completed_checkpoint.physical.next_physical_ordinal(), 2);
    assert_eq!(
        completed_checkpoint.provider_checkpoint,
        Some(TypedKey::U64(2))
    );
    assert_eq!(
        adapter.projection_modes.lock().unwrap().as_slice(),
        [
            JsonlFamilyProjectionMode::Cold,
            JsonlFamilyProjectionMode::CertifiedAppend,
        ],
        "tail completion must resume the shared checkpoint instead of replacing the source"
    );
}
