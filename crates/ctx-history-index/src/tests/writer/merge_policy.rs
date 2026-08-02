use super::*;

#[test]
fn production_merge_policy_bounds_repeated_tiny_appends_amortized() {
    let temp = tempdir().unwrap();
    let source = source("tiny-appends.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "tiny append 1"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let initial_segments = VerifiedIndex::open(temp.path())
        .unwrap()
        .searcher
        .segment_readers()
        .len();
    let append_count = LEXICAL_SEGMENT_MERGE_FAN_IN * 2 + 1;
    let mut previous_segments = initial_segments;
    let mut peak_segments = initial_segments;
    let mut saw_coalescing = false;

    for append_ordinal in 1..=append_count {
        let sequence = append_ordinal as u64 + 1;
        let mut append = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        let base = append.begin_source_append(source.clone()).unwrap().clone();
        append
            .add_core_record(document(
                &source,
                sequence,
                &format!("tiny append {sequence}"),
            ))
            .unwrap();
        let frontier = base.frontier().unwrap();
        let current = appendable_certificate(&source, sequence as u8, sequence, sequence * 10);
        append
            .certify_source_append(
                CertifiedSourceAppend::certify(
                    &base,
                    current,
                    frontier.certified_prefix_bytes(),
                    *frontier.certified_prefix_digest(),
                )
                .unwrap(),
            )
            .unwrap();
        append.commit(|_| true).unwrap();

        let current_segments = VerifiedIndex::open(temp.path())
            .unwrap()
            .searcher
            .segment_readers()
            .len();
        assert!(
            current_segments <= previous_segments + 1,
            "one tiny append exposed more than one additional active segment: \
             before={previous_segments}, after={current_segments}"
        );
        saw_coalescing |= current_segments <= previous_segments;
        peak_segments = peak_segments.max(current_segments);
        previous_segments = current_segments;
    }

    assert!(
        saw_coalescing,
        "the repeated append run crossed fan-in {LEXICAL_SEGMENT_MERGE_FAN_IN} \
         without an observable coalescing publication"
    );
    assert!(
        peak_segments < initial_segments + LEXICAL_SEGMENT_MERGE_FAN_IN,
        "same-tier tiny segments exceeded the configured fan-in bound: \
         initial={initial_segments}, peak={peak_segments}"
    );
    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(index.document_count(), append_count as u64 + 1);
    assert_eq!(
        fs::read_dir(temp.path().join(MANIFEST_DIRECTORY))
            .unwrap()
            .count(),
        2,
        "publication should retain one manifest for each visible and grace generation"
    );
}

#[test]
fn unreclaimed_delete_density_cannot_replace_the_active_generation() {
    const REPLACED_DOCUMENTS: u64 = 3;

    let temp = tempdir().unwrap();
    let replaced = source("fail-closed-replaced-source.jsonl");
    let stable = source("fail-closed-stable-source.jsonl");
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: INDEX_MEMORY_MIN_PER_THREAD,
    };
    let mut initial = GenerationWriter::open(temp.path(), options.clone()).unwrap();
    initial.begin_source(replaced.clone()).unwrap();
    for sequence in 1..=REPLACED_DOCUMENTS {
        initial
            .add_core_record(document(&replaced, sequence, "published baseline"))
            .unwrap();
    }
    initial
        .certify_source(certificate(&replaced, 1, REPLACED_DOCUMENTS))
        .unwrap();
    initial.begin_source(stable.clone()).unwrap();
    initial
        .add_core_record(document(&stable, 1, "stable baseline"))
        .unwrap();
    initial.certify_source(certificate(&stable, 1, 1)).unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), options.clone()).unwrap();
    replacement.begin_source(replaced.clone()).unwrap();
    let deleted_candidate_event = document(&replaced, 1, "candidate replacement").event_id;
    for sequence in 1..=REPLACED_DOCUMENTS {
        replacement
            .add_core_record(document(&replaced, sequence, "candidate replacement"))
            .unwrap();
    }
    replacement
        .certify_source(certificate(&replaced, 2, REPLACED_DOCUMENTS))
        .unwrap();
    replacement.before_pointer_switch = Some(Box::new(move |candidate_path| {
        let directory = DurableMmapDirectory::open(candidate_path).unwrap();
        let index = Index::open(directory).unwrap();
        let payload = index.load_metas().unwrap().payload;
        let event_id = required_field(&index.schema(), "event_id").unwrap();
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
            .unwrap();
        writer.set_merge_policy(Box::<NoMergePolicy>::default());
        writer.delete_term(Term::from_field_text(
            event_id,
            &deleted_candidate_event.to_string(),
        ));
        let mut prepared = writer.prepare_commit().unwrap();
        if let Some(payload) = payload {
            prepared.set_payload(&payload);
        }
        prepared.commit().unwrap();
        writer.wait_merging_threads().unwrap();
    }));

    let error = replacement.commit(|_| true).unwrap_err();
    match error {
        IndexError::CandidateDeletionDensityExceeded {
            deleted_documents,
            max_documents,
        } => assert!(deleted_documents * 4 > max_documents),
        other => panic!("unexpected publication failure: {other:?}"),
    }
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    let still_active = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(still_active.generation_id(), baseline.generation_id);
    assert_eq!(
        still_active.count_term("published").unwrap(),
        REPLACED_DOCUMENTS as usize
    );
    assert_eq!(still_active.count_term("candidate").unwrap(), 0);

    let restarted = GenerationWriter::open(temp.path(), options).unwrap();
    assert_eq!(
        restarted.base_manifest().unwrap().generation_id().unwrap(),
        baseline.generation_id
    );
    drop(restarted);
    assert_eq!(
        fs::read_dir(temp.path().join(INDEX_GENERATIONS_DIRECTORY))
            .unwrap()
            .count(),
        1,
        "writer restart must reclaim the rejected candidate generation"
    );
    assert_eq!(
        fs::read_dir(temp.path().join(MANIFEST_DIRECTORY))
            .unwrap()
            .count(),
        1,
        "only the active manifest may remain"
    );
}

#[test]
fn production_merge_policy_bounds_repeated_substantial_replacements() {
    const REPLACED_DOCUMENTS: u64 = 96;
    const STABLE_DOCUMENTS: u64 = 32;
    const REPLACEMENTS: u8 = 6;

    let temp = tempdir().unwrap();
    let replaced = source("large-replaced-source.sqlite");
    let stable = source("stable-source.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(replaced.clone()).unwrap();
    for sequence in 1..=REPLACED_DOCUMENTS {
        initial
            .add_core_record(document(&replaced, sequence, "large replacement v1"))
            .unwrap();
    }
    initial
        .certify_source(appendable_certificate(
            &replaced,
            1,
            REPLACED_DOCUMENTS,
            REPLACED_DOCUMENTS * 10,
        ))
        .unwrap();
    initial.begin_source(stable.clone()).unwrap();
    for sequence in 1..=STABLE_DOCUMENTS {
        initial
            .add_core_record(document(&stable, sequence, "stable content"))
            .unwrap();
    }
    initial
        .certify_source(appendable_certificate(
            &stable,
            1,
            STABLE_DOCUMENTS,
            STABLE_DOCUMENTS * 10,
        ))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let segment_stats = || {
        let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
        let segments = index.searcher.segment_readers();
        for segment in segments {
            assert!(
                u64::from(segment.num_deleted_docs()) * 4 <= u64::from(segment.max_doc()),
                "published segment exceeded the 25% deletion bound: {segment:?}"
            );
        }
        (
            segments.len(),
            segments
                .iter()
                .map(|segment| u64::from(segment.max_doc()))
                .sum::<u64>(),
            segments
                .iter()
                .map(|segment| u64::from(segment.num_docs()))
                .sum::<u64>(),
            segments
                .iter()
                .map(|segment| u64::from(segment.num_deleted_docs()))
                .sum::<u64>(),
        )
    };
    let mut peak_segments = 0;
    let mut latest_generation = String::new();
    for revision in 2..=REPLACEMENTS + 1 {
        let mut replacement =
            GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        replacement.begin_source(replaced.clone()).unwrap();
        for sequence in 1..=REPLACED_DOCUMENTS {
            replacement
                .add_core_record(document(
                    &replaced,
                    sequence,
                    &format!("large replacement v{revision}"),
                ))
                .unwrap();
        }
        replacement
            .certify_source(appendable_certificate(
                &replaced,
                revision,
                REPLACED_DOCUMENTS,
                REPLACED_DOCUMENTS * 10,
            ))
            .unwrap();
        latest_generation = replacement.commit(|_| true).unwrap().generation_id;

        let (active_segments, max_documents, live_documents, deleted_documents) = segment_stats();
        assert_eq!(live_documents, REPLACED_DOCUMENTS + STABLE_DOCUMENTS);
        assert_eq!(live_documents, max_documents);
        assert_eq!(deleted_documents, 0);
        peak_segments = peak_segments.max(active_segments);
        assert!(
            active_segments <= 2,
            "replacement reclamation exposed {active_segments} active segments"
        );
        assert_eq!(
            fs::read_dir(temp.path().join(MANIFEST_DIRECTORY))
                .unwrap()
                .count(),
            2,
            "only current and grace manifests may remain"
        );
        assert!(
            fs::read_dir(temp.path().join(INDEX_GENERATIONS_DIRECTORY))
                .unwrap()
                .count()
                <= 2,
            "only current and grace generation directories may remain"
        );
    }
    assert!(peak_segments > 0);

    let status_before_noop = segment_stats();
    let manifests_before_noop = fs::read_dir(temp.path().join(MANIFEST_DIRECTORY))
        .unwrap()
        .count();
    let generations_before_noop = fs::read_dir(temp.path().join(INDEX_GENERATIONS_DIRECTORY))
        .unwrap()
        .count();
    let inventory = complete_inventory(
        &replaced,
        REPLACEMENTS + 2,
        vec![replaced.clone(), stable.clone()],
    );
    let mut replay = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    let replay_constructions = Arc::clone(&replay.index_writer_constructions);
    let replaced_base = stage_exact_replay(&mut replay, &replaced);
    let stable_base = stage_exact_replay(&mut replay, &stable);
    replay
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    let replay_receipt = replay
        .commit_with_complete_inventory_revalidation(
            |target| match target {
                RevalidationTarget::Source(source) => {
                    source == &replaced_base || source == &stable_base
                }
                RevalidationTarget::Deletion(_) => false,
            },
            |current| current == &inventory,
        )
        .unwrap();
    assert_eq!(replay_receipt.generation_id, latest_generation);
    assert_eq!(replay_constructions.load(Ordering::SeqCst), 0);
    assert_eq!(segment_stats(), status_before_noop);
    assert_eq!(
        fs::read_dir(temp.path().join(MANIFEST_DIRECTORY))
            .unwrap()
            .count(),
        manifests_before_noop
    );
    assert_eq!(
        fs::read_dir(temp.path().join(INDEX_GENERATIONS_DIRECTORY))
            .unwrap()
            .count(),
        generations_before_noop
    );
}
