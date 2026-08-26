use super::*;

const RETAINED_N: u64 = 32;
const RETAINED_2N: u64 = RETAINED_N * 2;
const LIVE_SEGMENT_COUNT: usize = 8;
const EVENTS_IN_TARGET_SESSION: u64 = 128;
const EVENTS_PER_PEER_SEGMENT: u64 = 32;

#[derive(Debug, PartialEq, Eq)]
struct PublicationWork {
    checksum_walks: usize,
    logical_passes: usize,
    identity_terms: usize,
    identity_documents: usize,
    projection_documents: usize,
    lineage_decodes: usize,
    lineage_spills: usize,
    complete_session_id_traversals: usize,
    hashed_artifact_bytes: u64,
    writer_constructions: usize,
    authority_lookup: PriorSessionIdentityLookupWork,
}

impl PublicationWork {
    fn capture(
        writer_constructions: usize,
        authority_lookup: PriorSessionIdentityLookupWork,
    ) -> Self {
        let (checksum_walks, logical_passes) = crate::publication::verification_activity();
        let (identity_terms, identity_documents) =
            crate::publication::candidate_identity_verification_activity();
        let (lineage_decodes, lineage_spills) =
            crate::publication::candidate_lineage_verification_activity();
        Self {
            checksum_walks,
            logical_passes,
            identity_terms,
            identity_documents,
            projection_documents: crate::publication::candidate_projection_verification_activity(),
            lineage_decodes,
            lineage_spills,
            complete_session_id_traversals: crate::publication::complete_session_id_traversals(),
            hashed_artifact_bytes: crate::publication::hashed_artifact_bytes(),
            writer_constructions,
            authority_lookup,
        }
    }
}

struct RetainedFixture {
    root: TempDir,
    source: SourceKey,
    inventory: CertifiedSourceInventory,
}

fn retained_fixture(retained_documents: u64) -> RetainedFixture {
    let root = tempdir().unwrap();
    let source = source(&format!("complexity-{retained_documents}.jsonl"));
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    let mut writer = GenerationWriter::open(root.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 1..=retained_documents {
        writer
            .add_core_record(document(&source, sequence, "retained body"))
            .unwrap();
    }
    writer
        .certify_source(appendable_certificate(
            &source,
            1,
            retained_documents,
            retained_documents * 10,
        ))
        .unwrap();
    writer.commit(|_| true).unwrap();
    RetainedFixture {
        root,
        source,
        inventory,
    }
}

fn measure_exact_noop(retained_documents: u64) -> PublicationWork {
    let fixture = retained_fixture(retained_documents);
    let mut writer = GenerationWriter::open(fixture.root.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let constructions = Arc::clone(&writer.index_writer_constructions);
    writer
        .certify_complete_inventory(fixture.inventory.clone())
        .unwrap();
    stage_exact_replay(&mut writer, &fixture.source);

    crate::publication::reset_verification_activity();
    writer
        .commit_with_complete_inventory_revalidation(
            |_| true,
            |current| current == &fixture.inventory,
        )
        .unwrap();
    PublicationWork::capture(
        constructions.load(Ordering::SeqCst),
        PriorSessionIdentityLookupWork::default(),
    )
}

fn measure_one_record_append(retained_documents: u64) -> (PublicationWork, usize) {
    let fixture = retained_fixture(retained_documents);
    let base_segment_count = {
        let (searcher, _) = open_unverified_generation(fixture.root.path());
        searcher.segment_readers().len()
    };
    let mut writer = GenerationWriter::open(fixture.root.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let constructions = Arc::clone(&writer.index_writer_constructions);
    let base = writer
        .begin_source_append(fixture.source.clone())
        .unwrap()
        .clone();
    crate::reset_prior_session_identity_lookup_work();
    writer
        .add_core_record(document(
            &fixture.source,
            retained_documents + 1,
            "one appended body",
        ))
        .unwrap();
    writer
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(
                    &fixture.source,
                    2,
                    retained_documents + 1,
                    (retained_documents + 1) * 10,
                ),
                retained_documents * 10,
                [1; 32],
            )
            .unwrap(),
        )
        .unwrap();

    crate::publication::reset_verification_activity();
    writer.commit(|_| true).unwrap();
    (
        PublicationWork::capture(
            constructions.load(Ordering::SeqCst),
            crate::prior_session_identity_lookup_work(),
        ),
        base_segment_count,
    )
}

fn multisegment_append_lookup_work() -> PriorSessionIdentityLookupWork {
    let root = tempdir().unwrap();
    let target = source("complexity-multisegment-target.jsonl");
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: INDEX_MEMORY_MIN_PER_THREAD,
    };

    for segment in 0..LIVE_SEGMENT_COUNT {
        let current = if segment == 0 {
            target.clone()
        } else {
            source(&format!("complexity-multisegment-peer-{segment}.jsonl"))
        };
        let document_count = if segment == 0 {
            EVENTS_IN_TARGET_SESSION
        } else {
            EVENTS_PER_PEER_SEGMENT
        };
        let mut writer = GenerationWriter::open(root.path(), options.clone())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.begin_source(current.clone()).unwrap();
        writer.test_disable_merges().unwrap();
        for sequence in 1..=document_count {
            writer
                .add_core_record(document(
                    &current,
                    sequence,
                    "multi-segment authority fixture",
                ))
                .unwrap();
        }
        if segment == 0 {
            writer
                .certify_source(appendable_certificate(
                    &current,
                    1,
                    document_count,
                    document_count * 10,
                ))
                .unwrap();
        } else {
            writer
                .certify_source(certificate(&current, segment as u8, document_count))
                .unwrap();
        }
        writer.commit(|_| true).unwrap();
    }

    let (searcher, _) = open_unverified_generation(root.path());
    assert_eq!(
        searcher.segment_readers().len(),
        LIVE_SEGMENT_COUNT,
        "the fixture must retain independently live production-shaped segments"
    );

    let mut writer = GenerationWriter::open(root.path(), options)
        .unwrap()
        .into_writer()
        .unwrap();
    let base = writer.begin_source_append(target.clone()).unwrap().clone();
    crate::reset_prior_session_identity_lookup_work();
    writer
        .add_core_record(document(
            &target,
            EVENTS_IN_TARGET_SESSION + 1,
            "multi-segment append",
        ))
        .unwrap();
    writer
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(
                    &target,
                    2,
                    EVENTS_IN_TARGET_SESSION + 1,
                    (EVENTS_IN_TARGET_SESSION + 1) * 10,
                ),
                EVENTS_IN_TARGET_SESSION * 10,
                [1; 32],
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap();
    crate::prior_session_identity_lookup_work()
}

#[test]
fn exact_noop_work_is_zero_for_n_and_2n_retained_documents() {
    let n = measure_exact_noop(RETAINED_N);
    let two_n = measure_exact_noop(RETAINED_2N);

    assert_eq!(n, two_n, "W_noop(2N) must equal W_noop(N)");
    assert_eq!(
        n,
        PublicationWork {
            checksum_walks: 0,
            logical_passes: 0,
            identity_terms: 0,
            identity_documents: 0,
            projection_documents: 0,
            lineage_decodes: 0,
            lineage_spills: 0,
            complete_session_id_traversals: 0,
            hashed_artifact_bytes: 0,
            writer_constructions: 0,
            authority_lookup: PriorSessionIdentityLookupWork::default(),
        },
        "an exact replay must do no publication or verification work"
    );
}

#[test]
fn one_record_append_verification_work_tracks_live_segment_topology() {
    let (n, n_base_segments) = measure_one_record_append(RETAINED_N);
    let (two_n, two_n_base_segments) = measure_one_record_append(RETAINED_2N);

    assert_eq!(
        n.logical_passes, 0,
        "an append must not run a full logical pass"
    );
    assert_eq!(two_n.logical_passes, 0, "W_full_pass(2N) must remain zero");
    assert_eq!(n.checksum_walks, 1);
    assert_eq!(two_n.checksum_walks, n.checksum_walks);
    assert_eq!(n.identity_terms, 1);
    assert_eq!(n.identity_documents, 1);
    assert_eq!(n.projection_documents, 0);
    assert_eq!(n.lineage_decodes, 0);
    assert_eq!(n.lineage_spills, 0);
    assert_eq!(n.complete_session_id_traversals, 0);
    assert_eq!(n.writer_constructions, 1);
    for (lookup, base_segments) in [
        (n.authority_lookup, n_base_segments),
        (two_n.authority_lookup, two_n_base_segments),
    ] {
        assert_eq!(
            lookup.segment_range_probes, base_segments,
            "the append must probe each live base segment exactly once"
        );
        assert!(lookup.segment_range_probes <= MAX_SESSION_WITNESS_SEGMENT_PROBES);
        assert_eq!(
            (
                lookup.dictionary_terms,
                lookup.postings,
                lookup.core_decodes
            ),
            (1, 1, 1),
            "retained corpus size must not increase sparse witness work"
        );
    }
    assert_eq!(two_n.identity_terms, n.identity_terms);
    assert_eq!(two_n.identity_documents, n.identity_documents);
    assert_eq!(two_n.projection_documents, n.projection_documents);
    assert_eq!(two_n.lineage_decodes, n.lineage_decodes);
    assert_eq!(two_n.lineage_spills, n.lineage_spills);
    assert_eq!(
        two_n.complete_session_id_traversals,
        n.complete_session_id_traversals
    );
    assert_eq!(two_n.writer_constructions, n.writer_constructions);
}

#[test]
fn same_session_append_charges_each_live_segment_and_not_each_session_event() {
    let work = multisegment_append_lookup_work();

    assert_eq!(work.segment_range_probes, LIVE_SEGMENT_COUNT);
    assert_eq!(work.dictionary_terms, 1);
    assert_eq!(work.postings, 1);
    assert_eq!(work.core_decodes, 1);
    assert!(
        work.segment_range_probes <= MAX_SESSION_WITNESS_SEGMENT_PROBES,
        "segment range probes must remain inside their fixed budget"
    );
    assert!(
        work.dictionary_terms + work.postings <= MAX_SESSION_WITNESS_VISITS,
        "carrier visits must remain inside their fixed budget"
    );
}

#[test]
#[ignore = "high-cardinality authority dictionary contract; tier-nightly"]
fn high_cardinality_same_session_append_reads_only_the_live_witness() {
    let (work, base_segment_count) = measure_one_record_append(4_096);
    assert_eq!(
        work.authority_lookup.segment_range_probes,
        base_segment_count
    );
    assert_eq!(work.authority_lookup.dictionary_terms, 1);
    assert_eq!(work.authority_lookup.postings, 1);
    assert_eq!(work.authority_lookup.core_decodes, 1);
}

#[test]
fn repeated_replacement_tombstones_hit_the_witness_cap_and_fail_closed() {
    let root = tempdir().unwrap();
    let replaced = source("witness-cap-replacements.jsonl");
    let replaced_route = SourceRouteIdentity::from_sha256("51".repeat(32)).unwrap();
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: INDEX_MEMORY_MIN_PER_THREAD,
    };
    let mut initial = GenerationWriter::open(root.path(), options.clone())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(replaced.clone()).unwrap();
    initial
        .writer_mut()
        .unwrap()
        .set_merge_policy(Box::<NoMergePolicy>::default());
    initial
        .add_core_record(document(&replaced, 1, "replacement 1"))
        .unwrap();
    initial
        .certify_source(certificate(&replaced, 1, 1))
        .unwrap();
    let first_peer = source("witness-cap-peer-1.jsonl");
    initial.begin_source(first_peer.clone()).unwrap();
    for sequence in 1..=3 {
        initial
            .add_core_record(document(&first_peer, sequence, "retained peer 1"))
            .unwrap();
    }
    initial
        .certify_source(certificate(&first_peer, 1, 3))
        .unwrap();
    let mut replaced_route_sources = vec![replaced.clone(), first_peer];
    initial
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            replaced_route.clone(),
            replaced_route_sources.clone(),
        )
        .unwrap()])
        .unwrap();
    initial.commit(|_| true).unwrap();

    let mut failure = None;
    let mut failure_work = None;
    for revision in 2_u8..=40 {
        let (searcher, _) = open_unverified_generation(root.path());
        let exercise_route_rollback =
            searcher.segment_readers().len() * 2 > MAX_SESSION_WITNESS_VISITS;
        let mut replacement = GenerationWriter::open(root.path(), options.clone())
            .unwrap()
            .into_writer()
            .unwrap();
        let successful_route = SourceRouteIdentity::from_sha256("62".repeat(32)).unwrap();
        let successful_source = source("witness-cap-successful-route.jsonl");
        if exercise_route_rollback {
            replacement
                .set_source_route_plan(
                    BTreeSet::from([replaced_route.clone(), successful_route.clone()]),
                    BTreeSet::new(),
                )
                .unwrap();
            replacement
                .begin_source_route_stage(successful_route.clone())
                .unwrap();
            replacement.begin_source(successful_source.clone()).unwrap();
            replacement
                .add_core_record(document(&successful_source, 1, "successful prior route"))
                .unwrap();
            replacement
                .certify_source(certificate(&successful_source, revision, 1))
                .unwrap();
            replacement
                .finish_source_route_stage(&successful_route)
                .unwrap();
            replacement
                .begin_source_route_stage(replaced_route.clone())
                .unwrap();
        }
        replacement.begin_source(replaced.clone()).unwrap();
        replacement
            .writer_mut()
            .unwrap()
            .set_merge_policy(Box::<NoMergePolicy>::default());
        crate::reset_prior_session_identity_lookup_work();
        match replacement.add_core_record(document(
            &replaced,
            1,
            &format!("replacement {revision}"),
        )) {
            Ok(()) => {
                replacement
                    .certify_source(certificate(&replaced, revision, 1))
                    .unwrap();
                let peer = source(&format!("witness-cap-peer-{revision}.jsonl"));
                replacement.begin_source(peer.clone()).unwrap();
                for sequence in 1..=3 {
                    replacement
                        .add_core_record(document(
                            &peer,
                            sequence,
                            &format!("retained peer {revision}"),
                        ))
                        .unwrap();
                }
                replacement
                    .certify_source(certificate(&peer, revision, 3))
                    .unwrap();
                replaced_route_sources.push(peer);
                replacement
                    .set_present_source_routes(vec![SourceRouteSnapshot::present(
                        replaced_route.clone(),
                        replaced_route_sources.clone(),
                    )
                    .unwrap()])
                    .unwrap();
                replacement.commit(|_| true).unwrap();
            }
            Err(error) => {
                failure_work = Some(crate::prior_session_identity_lookup_work());
                failure = Some(error);
                assert!(exercise_route_rollback);
                replacement
                    .rollback_source_route_stage(&replaced_route)
                    .unwrap();
                assert!(replacement
                    .carry_failed_source_route_from_base(&replaced_route)
                    .unwrap());
                replacement
                    .set_present_source_routes(vec![SourceRouteSnapshot::present(
                        successful_route,
                        vec![successful_source],
                    )
                    .unwrap()])
                    .unwrap();
                assert!(matches!(
                    replacement.commit(|_| true),
                    Err(IndexError::ActiveGenerationNeedsRebuild { .. })
                ));
                let rebuild = GenerationWriter::open(root.path(), options.clone())
                    .unwrap()
                    .into_writer()
                    .unwrap();
                assert!(rebuild.base_manifest().is_none());
                break;
            }
        }
    }

    assert!(
        matches!(
            failure,
            Some(IndexError::ActiveGenerationNeedsRebuild { .. })
        ),
        "unexpected cap outcome: {failure:?}"
    );
    let work = failure_work.unwrap();
    assert_eq!(
        work.dictionary_terms + work.postings,
        MAX_SESSION_WITNESS_VISITS + 1,
        "the lookup must stop immediately after crossing its fixed carrier visit cap"
    );
    assert!(work.segment_range_probes <= MAX_SESSION_WITNESS_SEGMENT_PROBES);
    assert!(
        work.core_decodes <= 1,
        "only the live witness may be decoded"
    );
    assert!(root
        .path()
        .join("active-generation-rebuild-required.json")
        .is_file());
}

#[test]
fn session_witness_segment_probe_cap_admits_65_segment_generation() {
    const VALID_SEGMENT_COUNT: usize = 65;
    let root = tempdir().unwrap();
    let target = source("witness-segment-cap-target.jsonl");
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: INDEX_MEMORY_MIN_PER_THREAD,
    };
    let mut initial = GenerationWriter::open(root.path(), options.clone())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.test_disable_merges().unwrap();
    for segment in 0..VALID_SEGMENT_COUNT {
        let current = if segment == 0 {
            target.clone()
        } else {
            source(&format!("witness-segment-cap-peer-{segment}.jsonl"))
        };
        initial.begin_source(current.clone()).unwrap();
        initial
            .add_core_record(document(&current, 1, "segment cap fixture"))
            .unwrap();
        if segment == 0 {
            initial
                .certify_source(appendable_certificate(&current, 1, 1, 10))
                .unwrap();
        } else {
            initial.certify_source(certificate(&current, 1, 1)).unwrap();
        }
        initial.writer_mut().unwrap().commit().unwrap();
    }
    initial.commit(|_| true).unwrap();
    let (searcher, _) = open_unverified_generation(root.path());
    assert_eq!(searcher.segment_readers().len(), VALID_SEGMENT_COUNT);

    let mut accepted = GenerationWriter::open(root.path(), options.clone())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = accepted
        .begin_source_append(target.clone())
        .unwrap()
        .clone();
    accepted.test_disable_merges().unwrap();
    crate::reset_prior_session_identity_lookup_work();
    accepted
        .add_core_record(document(&target, 2, "accepted at segment cap"))
        .unwrap();
    assert_eq!(
        crate::prior_session_identity_lookup_work().segment_range_probes,
        VALID_SEGMENT_COUNT
    );
    accepted
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(&target, 2, 2, 20),
                10,
                [1; 32],
            )
            .unwrap(),
        )
        .unwrap();
    accepted.commit(|_| true).unwrap();
    let (searcher, _) = open_unverified_generation(root.path());
    assert_eq!(searcher.segment_readers().len(), VALID_SEGMENT_COUNT + 1);
    assert!(!root
        .path()
        .join("active-generation-rebuild-required.json")
        .exists());
}

#[test]
fn cold_writer_publication_avoids_an_exhaustive_logical_pass() {
    let root = tempdir().unwrap();
    let source = source("complexity-cold.jsonl");
    crate::publication::reset_candidate_clone_metrics();
    let mut writer = GenerationWriter::open(root.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 1..=RETAINED_N {
        writer
            .add_core_record(document(&source, sequence, "cold body"))
            .unwrap();
    }
    writer
        .certify_source(certificate(&source, 1, RETAINED_N))
        .unwrap();

    crate::publication::reset_verification_activity();
    writer.commit(|_| true).unwrap();
    let (_, logical_passes) = crate::publication::verification_activity();
    assert_eq!(
        logical_passes, 0,
        "writer-produced proof must replace exhaustive cold logical replay"
    );
    assert_eq!(
        crate::publication::candidate_clone_metrics(),
        crate::publication::CandidateCloneMetrics::default(),
        "cold creation must stay off the incremental candidate clone path"
    );
}

#[test]
fn session_registry_budget_counts_unique_changes_not_noops_or_same_session_appends() {
    use crate::writer_options::CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES;

    assert!(
        WriterOptions::default().memory_bytes / CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES
            >= 9_000,
        "the default writer budget must admit the known 9K-session corpus"
    );
    assert!(
        std::mem::size_of::<(Uuid, PreparedSessionIdentityFacts)>() + std::mem::size_of::<Uuid>()
            < CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES,
        "the conservative charge must cover registry payload plus route undo UUID"
    );

    let fixture = retained_fixture(1);
    let initial_generation = VerifiedIndex::open(fixture.root.path())
        .unwrap()
        .generation_id()
        .to_owned();

    let mut noop = GenerationWriter::open(fixture.root.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    noop.changed_session_registry_memory_bytes = 0;
    noop.certify_complete_inventory(fixture.inventory.clone())
        .unwrap();
    stage_exact_replay(&mut noop, &fixture.source);
    let noop_receipt = noop
        .commit_with_complete_inventory_revalidation(
            |_| true,
            |current| current == &fixture.inventory,
        )
        .unwrap();
    assert_eq!(noop_receipt.generation_id, initial_generation);

    let mut append = GenerationWriter::open(fixture.root.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    append.changed_session_registry_memory_bytes = CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES;
    let base = append
        .begin_source_append(fixture.source.clone())
        .unwrap()
        .clone();
    append
        .add_core_record(document(&fixture.source, 2, "first same-session append"))
        .unwrap();
    append
        .add_core_record(document(&fixture.source, 3, "second same-session append"))
        .unwrap();
    assert_eq!(append.changed_sessions.len(), 1);

    let error = append
        .add_core_record(document_for_session(
            &fixture.source,
            "second-session",
            4,
            "over budget",
        ))
        .unwrap_err();
    assert!(matches!(
        error,
        IndexError::ChangedSessionRegistryMemoryLimitExceeded {
            attempted_entries: 2,
            required_bytes: 2048,
            maximum_bytes: 1024,
            maximum_entries: 1,
        }
    ));
    assert_eq!(append.changed_sessions.len(), 1);

    let frontier = base.frontier().unwrap();
    append
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(&fixture.source, 2, 3, 30),
                frontier.certified_prefix_bytes(),
                *frontier.certified_prefix_digest(),
            )
            .unwrap(),
        )
        .unwrap();
    append.commit(|_| true).unwrap();

    let published = VerifiedIndex::open(fixture.root.path()).unwrap();
    assert_eq!(published.manifest().indexed_documents, 3);
    assert_eq!(published.count_term("same").unwrap(), 2);
    assert_eq!(published.count_term("budget").unwrap(), 0);
}
