use super::*;

#[test]
fn failed_final_revalidation_keeps_the_previous_generation() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(source.clone()).unwrap();
    first
        .add_core_record(document(&source, 1, "previous generation"))
        .unwrap();
    first.certify_source(certificate(&source, 1, 1)).unwrap();
    let first_receipt = first.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    replacement.begin_source(source.clone()).unwrap();
    replacement
        .add_core_record(document(&source, 1, "uncommitted replacement"))
        .unwrap();
    replacement
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    let error = replacement.commit(|_| false).unwrap_err();
    assert!(matches!(error, IndexError::SourceInvalidated(_)));

    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(index.generation_id(), first_receipt.generation_id);
    assert_eq!(index.count_term("previous").unwrap(), 1);
    assert_eq!(index.count_term("uncommitted").unwrap(), 0);
}

#[test]
fn crash_after_candidate_commit_before_verification_keeps_old_pointer_and_restarts() {
    let temp = tempdir().unwrap();
    let source = source("candidate-crash.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "audited baseline"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();

    let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    candidate.begin_source(source.clone()).unwrap();
    candidate
        .add_core_record(document(&source, 1, "unverified candidate"))
        .unwrap();
    candidate
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    candidate.after_candidate_commit = Some(Box::new(|_| {
        panic!("simulated process death after candidate meta commit")
    }));
    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = candidate.commit(|_| true);
    }));
    assert!(crash.is_err());
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    let still_active = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(still_active.generation_id(), baseline.generation_id);
    assert_eq!(still_active.count_term("baseline").unwrap(), 1);
    assert_eq!(still_active.count_term("unverified").unwrap(), 0);

    let restarted = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    assert_eq!(
        restarted.base_manifest().unwrap().generation_id().unwrap(),
        baseline.generation_id
    );
}

#[test]
fn structural_verification_fault_never_switches_the_active_pointer() {
    let temp = tempdir().unwrap();
    let source = source("candidate-verification-fault.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "verified baseline"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();

    let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    candidate.begin_source(source.clone()).unwrap();
    candidate
        .add_core_record(document(&source, 1, "corrupt candidate"))
        .unwrap();
    candidate
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    let source_token = source_token(&source);
    candidate.before_pointer_switch = Some(Box::new(move |candidate_path| {
        let directory = DurableMmapDirectory::open(candidate_path).unwrap();
        let index = Index::open(directory).unwrap();
        let payload = index.load_metas().unwrap().payload;
        let source_key = required_field(&index.schema(), "source_key").unwrap();
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
            .unwrap();
        writer.delete_term(Term::from_field_text(source_key, &source_token));
        let mut prepared = writer.prepare_commit().unwrap();
        if let Some(payload) = payload {
            prepared.set_payload(&payload);
        }
        prepared.commit().unwrap();
        writer.wait_merging_threads().unwrap();
    }));
    let error = candidate.commit(|_| true).unwrap_err();
    assert!(
        matches!(
            error,
            IndexError::DocumentCountMismatch {
                manifest: 1,
                index: 0
            } | IndexError::CandidateDeletionDensityExceeded { .. }
                | IndexError::ChecksumMismatch
        ),
        "{error:?}"
    );
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
        baseline.generation_id
    );
    drop(GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap());
}

#[test]
fn omitted_managed_body_projection_fault_before_pointer_switch_keeps_previous_generation() {
    let temp = tempdir().unwrap();
    let source = source("candidate-checksum-fault.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "checksum baseline"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();

    let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    candidate.begin_source(source.clone()).unwrap();
    candidate
        .add_core_record(document(&source, 1, "checksum candidate"))
        .unwrap();
    candidate
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    candidate.before_pointer_switch = Some(Box::new(|candidate_path| {
        omit_managed_and_corrupt_body_projection(candidate_path);
    }));

    assert!(matches!(
        candidate.commit(|_| true),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    let still_active = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(still_active.generation_id(), baseline.generation_id);
    assert_eq!(still_active.count_term("baseline").unwrap(), 1);
    assert_eq!(still_active.count_term("candidate").unwrap(), 0);
}

#[test]
fn stored_core_aggregate_fault_before_pointer_switch_keeps_the_previous_generation() {
    const DOCUMENTS: u64 = 5;

    let temp = tempdir().unwrap();
    let source = source("candidate-core-aggregate-fault.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    for sequence in 1..=DOCUMENTS {
        initial
            .add_core_record(document(&source, sequence, "aggregate baseline"))
            .unwrap();
    }
    initial
        .certify_source(certificate(&source, 1, DOCUMENTS))
        .unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();

    let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    candidate.begin_source(source.clone()).unwrap();
    for sequence in 1..=DOCUMENTS {
        candidate
            .add_core_record(document(&source, sequence, "aggregate candidate"))
            .unwrap();
    }
    candidate
        .certify_source(certificate(&source, 2, DOCUMENTS))
        .unwrap();
    candidate.before_pointer_switch = Some(Box::new(|candidate_path| {
        let directory = DurableMmapDirectory::open(candidate_path).unwrap();
        let index = Index::open(directory).unwrap();
        let payload = index.load_metas().unwrap().payload;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        let searcher = reader.searcher();
        let address = searcher
            .search(&AllQuery, &DocSetCollector)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let mut forged = decoded_stored_core(&searcher, address);
        forged.content.normalized_body = Some("forged stored Core bytes".to_owned());
        let forged_event_id = forged.event_id;
        drop(searcher);
        drop(reader);

        let event_id = required_field(&index.schema(), "event_id").unwrap();
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
            .unwrap();
        writer.set_merge_policy(Box::<NoMergePolicy>::default());
        writer.delete_term(Term::from_field_text(
            event_id,
            &forged_event_id.to_string(),
        ));
        writer.add_document(indexed_document(forged)).unwrap();
        let mut prepared = writer.prepare_commit().unwrap();
        if let Some(payload) = payload {
            prepared.set_payload(&payload);
        }
        prepared.commit().unwrap();
        writer.wait_merging_threads().unwrap();
    }));

    assert!(matches!(
        candidate.commit(|_| true),
        Err(IndexError::CoreRecordAggregateMismatch(_))
    ));
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    let still_active = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(still_active.generation_id(), baseline.generation_id);
    assert_eq!(
        still_active.count_term("baseline").unwrap(),
        DOCUMENTS as usize
    );
    assert_eq!(still_active.count_term("forged").unwrap(), 0);
}

fn assert_valid_recommit_without_projection_is_rejected(field_name: &'static str) {
    let temp = tempdir().unwrap();
    let source = source(&format!("missing-{field_name}.jsonl"));
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "projection authority body"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let address = searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let complete = indexed_document(decoded_stored_core(&searcher, address));
    let omitted = required_field(searcher.schema(), field_name).unwrap();
    let mut forged = TantivyDocument::default();
    for (field, value) in complete.field_values() {
        if field != omitted {
            forged.add_field_value(field, value);
        }
    }
    let index = searcher.index().clone();
    drop(searcher);
    publish_unchecked_generation(
        temp.path(),
        &index,
        manifest,
        std::slice::from_ref(&source),
        vec![forged],
    );

    assert!(matches!(
        VerifiedIndex::open(temp.path()),
        Err(IndexError::InvalidStoredDocumentField(actual))
            if actual == field_name || actual == "query_projection"
    ));
}

#[test]
fn checksum_valid_recommit_without_body_search_is_rejected() {
    assert_valid_recommit_without_projection_is_rejected("body_search");
}

#[test]
fn checksum_valid_recommit_without_session_order_is_rejected() {
    assert_valid_recommit_without_projection_is_rejected("session_event_order");
}

#[test]
fn checksum_valid_recommit_without_neutral_core_order_is_rejected() {
    assert_valid_recommit_without_projection_is_rejected("semantic_event_order");
}

#[derive(Clone, Copy)]
enum QueryProjectionMutation {
    Omit(&'static str),
    Text(&'static str, &'static str),
    U64(&'static str, u64),
    I64(&'static str, i64),
}

impl QueryProjectionMutation {
    fn field_name(self) -> &'static str {
        match self {
            Self::Omit(field)
            | Self::Text(field, _)
            | Self::U64(field, _)
            | Self::I64(field, _) => field,
        }
    }
}

fn query_projection_fixture_record(source: &SourceKey, body: &str) -> CoreRecord {
    let mut record = document(source, 1, body);
    record.repository_bindings.push(RepositoryBinding {
        binding_id: "binding-1".to_owned(),
        logical_repository_id: "repo-1".to_owned(),
        checkout_id: None,
        worktree_id: None,
        aliases: Vec::new(),
        git_object_format: Some(GitObjectFormat::Sha1),
        local_root_authorization: None,
        evidence: vec![RepositoryEvidence {
            kind: RepositoryEvidenceKind::FileActivity,
            confidence: RepositoryEvidenceConfidence::Explicit,
        }],
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    });
    record.repository_file_observations = vec![RepositoryFileObservation {
        repository_binding_id: "binding-1".to_owned(),
        relative_path: "src/current.rs".to_owned(),
        kind: RepositoryFileObservationKind::Renamed,
        prior_relative_path: Some("src/prior.rs".to_owned()),
    }];
    record
        .repository_vcs_observations
        .push(RepositoryVcsObservation {
            repository_binding_id: "binding-1".to_owned(),
            kind: RepositoryVcsObservationKind::Outcome(Box::new(RepositoryOutcomeObservation {
                kind: RepositoryOutcomeKind::Commit,
                produced_object_ids: vec![GitObjectId {
                    format: GitObjectFormat::Sha1,
                    hex: "a".repeat(40),
                }],
                replacement_lineage: Vec::new(),
                pull_request: None,
                observed_at_unix_ms: 1_700_000_000_000,
                linkage: RepositoryOutcomeLinkage {
                    provider: "codex".to_owned(),
                    origin_call_id: "origin-call".to_owned(),
                    result_call_id: "result-call".to_owned(),
                    origin_event_sequence: record.event_sequence,
                    continuation_call_id_sha256: Vec::new(),
                    result_record_sha256: [7; 32],
                },
                outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
            })),
            object_id: None,
            parent_object_ids: Vec::new(),
            reference: None,
            relative_path: None,
        });
    record.validate_contract().unwrap();
    record
}

fn recommit_candidate_with_query_projection_mutation(
    candidate_path: &Path,
    mutation: QueryProjectionMutation,
) {
    let directory = DurableMmapDirectory::open(candidate_path).unwrap();
    let index = Index::open(directory).unwrap();
    let payload = index.load_metas().unwrap().payload;
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .unwrap();
    let searcher = reader.searcher();
    let address = searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let core = decoded_stored_core(&searcher, address);
    let event_id = core.event_id;
    let complete = indexed_document(core);
    let target = required_field(&index.schema(), mutation.field_name()).unwrap();
    let mut forged = TantivyDocument::default();
    for (field, value) in complete.field_values() {
        if field != target {
            forged.add_field_value(field, value);
        }
    }
    match mutation {
        QueryProjectionMutation::Omit(_) => {}
        QueryProjectionMutation::Text(_, value) => forged.add_text(target, value),
        QueryProjectionMutation::U64(_, value) => forged.add_u64(target, value),
        QueryProjectionMutation::I64(_, value) => forged.add_i64(target, value),
    }
    drop(searcher);
    drop(reader);

    let event_id_field = required_field(&index.schema(), "event_id").unwrap();
    let mut writer = index
        .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
        .unwrap();
    writer.set_merge_policy(Box::<NoMergePolicy>::default());
    writer.delete_term(Term::from_field_text(event_id_field, &event_id.to_string()));
    writer.add_document(forged).unwrap();
    let mut prepared = writer.prepare_commit().unwrap();
    if let Some(payload) = payload {
        prepared.set_payload(&payload);
    }
    prepared.commit().unwrap();
    writer.wait_merging_threads().unwrap();
}

#[test]
fn candidate_publication_rejects_every_query_authoritative_projection_mutation() {
    let cases = [
        ("event_type", QueryProjectionMutation::Omit("event_type")),
        ("role", QueryProjectionMutation::Text("role", "assistant")),
        (
            "provider",
            QueryProjectionMutation::Text("provider", "forged-provider"),
        ),
        (
            "timestamp",
            QueryProjectionMutation::I64("occurred_at_unix_ms", 42),
        ),
        ("is_primary", QueryProjectionMutation::U64("is_primary", 0)),
        (
            "workspace",
            QueryProjectionMutation::Text("workspace_filter", "/forged/workspace"),
        ),
        (
            "touched_file",
            QueryProjectionMutation::Text("touched_file_filter", "src/forged.rs"),
        ),
        (
            "produced_object",
            QueryProjectionMutation::Text(
                "repository_produced_object_id",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
        ),
        (
            "size",
            QueryProjectionMutation::U64("core_content_bytes", 1),
        ),
    ];

    for (name, mutation) in cases {
        let temp = tempdir().unwrap();
        let source = source(&format!("projection-{name}.jsonl"));
        let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        initial.begin_source(source.clone()).unwrap();
        initial
            .add_core_record(query_projection_fixture_record(&source, "prior body"))
            .unwrap();
        initial.certify_source(certificate(&source, 1, 1)).unwrap();
        let baseline = initial.commit(|_| true).unwrap();
        let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();

        let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        candidate.begin_source(source.clone()).unwrap();
        candidate
            .add_core_record(query_projection_fixture_record(&source, "candidate body"))
            .unwrap();
        candidate
            .certify_source(certificate(&source, 2, 1))
            .unwrap();
        candidate.before_pointer_switch = Some(Box::new(move |candidate_path| {
            recommit_candidate_with_query_projection_mutation(candidate_path, mutation);
        }));

        assert!(matches!(
            candidate.commit(|_| true),
            Err(IndexError::InvalidStoredDocumentField(_))
        ));
        assert_eq!(
            fs::read(temp.path().join("active-generation.json")).unwrap(),
            pointer_before,
            "{name} mutation switched the active pointer"
        );
        let retained = VerifiedIndex::open(temp.path()).unwrap();
        assert_eq!(retained.generation_id(), baseline.generation_id, "{name}");
        assert_eq!(retained.count_term("prior").unwrap(), 1, "{name}");
        assert_eq!(retained.count_term("candidate").unwrap(), 0, "{name}");
    }
}

#[test]
fn crash_immediately_after_pointer_switch_reopens_new_and_retains_previous() {
    let temp = tempdir().unwrap();
    let source = source("pointer-switch-crash.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "previous generation"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let baseline = initial.commit(|_| true).unwrap();

    let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    candidate.begin_source(source.clone()).unwrap();
    candidate
        .add_core_record(document(&source, 1, "switched generation"))
        .unwrap();
    candidate
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    candidate.after_pointer_switch = Some(Box::new(|_| {
        panic!("simulated process death after active pointer switch")
    }));
    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = candidate.commit(|_| true);
    }));
    assert!(crash.is_err());

    let switched = VerifiedIndex::open(temp.path()).unwrap();
    assert_ne!(switched.generation_id(), baseline.generation_id);
    assert_eq!(switched.count_term("switched").unwrap(), 1);
    assert_eq!(switched.count_term("previous").unwrap(), 0);
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    assert_eq!(
        pointer.previous().unwrap().generation_id(),
        baseline.generation_id
    );
    let restarted = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    assert_eq!(
        restarted.base_manifest().unwrap().generation_id().unwrap(),
        switched.generation_id()
    );
}

#[test]
fn post_pointer_cleanup_failure_preserves_success_and_runs_later_cleanup() {
    let temp = tempdir().unwrap();
    let source = source("post-pointer-cleanup.jsonl");

    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(source.clone()).unwrap();
    first
        .add_core_record(document(&source, 1, "first generation"))
        .unwrap();
    first.certify_source(certificate(&source, 1, 1)).unwrap();
    let first_receipt = first.commit(|_| true).unwrap();
    let first_generation_path = active_generation_path(temp.path());
    let first_manifest_path = manifest_path(temp.path(), &first_receipt.generation_id);

    let mut second = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    second.begin_source(source.clone()).unwrap();
    second
        .add_core_record(document(&source, 1, "second generation"))
        .unwrap();
    second.certify_source(certificate(&source, 2, 1)).unwrap();
    second.commit(|_| true).unwrap();
    assert!(first_generation_path.exists());
    assert!(first_manifest_path.exists());

    let mut third = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    third.begin_source(source.clone()).unwrap();
    third
        .add_core_record(document(&source, 1, "third generation"))
        .unwrap();
    third.certify_source(certificate(&source, 3, 1)).unwrap();

    let prior_pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    writer_support::mark_active_generation_for_rebuild(temp.path(), prior_pointer.active())
        .unwrap();
    let rebuild_marker = temp.path().join("active-generation-rebuild-required.json");
    let stale_marker_bytes = fs::read(&rebuild_marker).unwrap();
    let obstructed_marker = rebuild_marker.clone();
    third.after_pointer_switch = Some(Box::new(move |_| {
        fs::remove_file(&obstructed_marker).unwrap();
        fs::create_dir(&obstructed_marker).unwrap();
        fs::write(obstructed_marker.join("obstruction"), b"test fault").unwrap();
    }));

    let third_receipt = third.commit(|_| true).unwrap();
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
        third_receipt.generation_id
    );
    assert!(rebuild_marker.is_dir());
    assert!(!first_generation_path.exists());
    assert!(!first_manifest_path.exists());

    fs::remove_file(rebuild_marker.join("obstruction")).unwrap();
    fs::remove_dir(&rebuild_marker).unwrap();
    fs::write(&rebuild_marker, stale_marker_bytes).unwrap();
    let reopened = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    assert_eq!(
        reopened.base_manifest().unwrap().generation_id().unwrap(),
        third_receipt.generation_id
    );
    assert!(!rebuild_marker.exists());
}

#[test]
fn deletion_requires_final_inventory_revalidation() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(source.clone()).unwrap();
    first
        .add_core_record(document(&source, 1, "retained"))
        .unwrap();
    first.certify_source(certificate(&source, 1, 1)).unwrap();
    first.commit(|_| true).unwrap();

    let mut rejected = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    let (deletion, inventory) = deletion_evidence(&source, 2);
    rejected.delete_source(deletion, inventory).unwrap();
    let error = rejected
        .commit(|target| matches!(target, RevalidationTarget::Source(_)))
        .unwrap_err();
    assert!(matches!(error, IndexError::SourceInvalidated(_)));
    assert_eq!(
        VerifiedIndex::open(temp.path())
            .unwrap()
            .count_term("retained")
            .unwrap(),
        1
    );

    let mut accepted = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    let (deletion, inventory) = deletion_evidence(&source, 3);
    accepted.delete_source(deletion, inventory).unwrap();
    let accepted_receipt = accepted.commit(|_| true).unwrap();
    assert!(accepted_receipt.manifest().sources.is_empty());
    assert!(accepted_receipt.manifest().source_routes().is_empty());
    let current = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(current.count_term("retained").unwrap(), 0);
    assert!(current.manifest().sources.is_empty());
    assert!(current.manifest().source_routes().is_empty());
}

#[test]
fn generation_manifests_retain_only_current_sources() {
    let temp = tempdir().unwrap();
    let removed = source("removed.jsonl");
    let retained = source("retained.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(removed.clone()).unwrap();
    first
        .add_core_record(document(&removed, 1, "removed body"))
        .unwrap();
    first.certify_source(certificate(&removed, 1, 1)).unwrap();
    first.begin_source(retained.clone()).unwrap();
    first
        .add_core_record(document(&retained, 1, "retained body"))
        .unwrap();
    first.certify_source(certificate(&retained, 1, 1)).unwrap();
    first.commit(|_| true).unwrap();

    let (deletion, inventory) = deletion_evidence(&removed, 2);
    let mut deleting = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    deleting.delete_source(deletion, inventory).unwrap();
    let deleted_receipt = deleting.commit(|_| true).unwrap();
    let deleted = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(deleted.manifest().sources.len(), 1);
    assert_eq!(
        deleted.manifest().sources[0].observation().source(),
        &retained
    );
    assert_eq!(deleted.manifest().source_routes().len(), 1);

    let mut unrelated = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    unrelated.begin_source(retained.clone()).unwrap();
    unrelated
        .add_core_record(document(&retained, 2, "rewritten retained body"))
        .unwrap();
    unrelated
        .certify_source(certificate(&retained, 3, 1))
        .unwrap();
    let unrelated_receipt = unrelated.commit(|_| true).unwrap();
    let carried = VerifiedIndex::open(temp.path()).unwrap();
    assert_ne!(
        deleted_receipt.generation_id,
        unrelated_receipt.generation_id
    );
    assert_eq!(carried.manifest().sources.len(), 1);
    assert_eq!(carried.manifest().source_routes().len(), 1);

    let returning = source_for_provider("codex", "codex_prompt_history_jsonl", "removed.jsonl");
    assert_eq!(returning, removed);
    assert!(!returning.exact_descriptor_eq(&removed));
    let mut republishing = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    republishing.begin_source(returning.clone()).unwrap();
    republishing
        .add_core_record(document(&returning, 4, "returned body"))
        .unwrap();
    republishing
        .certify_source(certificate(&returning, 4, 1))
        .unwrap();
    republishing.commit(|_| true).unwrap();

    let returned = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(returned.manifest().source_routes().len(), 2);
    assert!(returned.manifest().sources.iter().any(|source| source
        .observation()
        .source()
        .exact_descriptor_eq(&returning)));
}

#[test]
fn generation_route_validation_binds_exact_current_membership() {
    let first = source("first-removed.jsonl");
    let route_id = SourceRouteIdentity::from_sha256("11".repeat(32)).unwrap();
    let route = SourceRouteSnapshot::present(route_id.clone(), vec![first.clone()]).unwrap();
    let canonical =
        GenerationManifest::from_parts(vec![certificate(&first, 1, 0)], vec![route.clone()])
            .unwrap();
    assert_eq!(canonical.source_routes(), std::slice::from_ref(&route));

    assert!(matches!(
        GenerationManifest::from_parts(vec![certificate(&first, 1, 0)], vec![route.clone(), route],),
        Err(IndexError::NonCanonicalSourceRoutes)
    ));

    let unretained = source("unretained.jsonl");
    assert!(matches!(
        GenerationManifest::from_parts(
            vec![certificate(&first, 1, 0)],
            vec![SourceRouteSnapshot::present(route_id, vec![unretained]).unwrap()],
        ),
        Err(IndexError::SourceRouteMemberNotRetained { .. })
    ));
}

#[test]
fn replacement_atomically_removes_old_source_documents() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(source.clone()).unwrap();
    first
        .add_core_record(document(&source, 1, "retired content"))
        .unwrap();
    first.certify_source(certificate(&source, 1, 1)).unwrap();
    first.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    replacement.begin_source(source.clone()).unwrap();
    replacement
        .add_core_record(document(&source, 1, "current content"))
        .unwrap();
    replacement
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    replacement.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(index.count_term("retired").unwrap(), 0);
    assert_eq!(index.count_term("current").unwrap(), 1);
    assert_eq!(index.manifest().sources.len(), 1);
}

#[test]
fn certified_append_indexes_only_the_delta() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(source.clone()).unwrap();
    first.add_core_record(document(&source, 1, "base")).unwrap();
    first
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    first.commit(|_| true).unwrap();

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    let base = append.begin_source_append(source.clone()).unwrap().clone();
    append
        .add_core_record(document(&source, 2, "delta"))
        .unwrap();
    let proof = CertifiedSourceAppend::certify(
        &base,
        appendable_certificate(&source, 2, 2, 20),
        10,
        [1; 32],
    )
    .unwrap();
    append.certify_source_append(proof).unwrap();
    append.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(index.document_count(), 2);
    assert_eq!(index.count_term("base").unwrap(), 1);
    assert_eq!(index.count_term("delta").unwrap(), 1);
    assert_eq!(index.manifest().sources[0].counts().indexed_documents, 2);
}

#[test]
fn append_event_term_audit_rejects_an_identity_already_in_the_base() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(source.clone()).unwrap();
    first.add_core_record(document(&source, 1, "base")).unwrap();
    first
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    let baseline = first.commit(|_| true).unwrap();

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    let base = append.begin_source_append(source.clone()).unwrap().clone();
    append
        .add_core_record(document(&source, 1, "duplicate"))
        .unwrap();
    let proof = CertifiedSourceAppend::certify(
        &base,
        appendable_certificate(&source, 2, 2, 20),
        10,
        [1; 32],
    )
    .unwrap();
    append.certify_source_append(proof).unwrap();
    let error = append.commit(|_| true).unwrap_err();
    assert!(
        matches!(error, IndexError::DuplicateEventIdentity(_)),
        "unexpected event term audit error: {error:?}"
    );
    assert_eq!(
        VerifiedIndex::active_generation_id(temp.path()).unwrap(),
        Some(baseline.generation_id)
    );
}

#[test]
fn verified_reader_remains_pinned_to_its_generation() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(source.clone()).unwrap();
    first
        .add_core_record(document(&source, 1, "old pinned generation"))
        .unwrap();
    first.certify_source(certificate(&source, 1, 1)).unwrap();
    first.commit(|_| true).unwrap();
    let old_reader = VerifiedIndex::open(temp.path()).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    replacement.begin_source(source.clone()).unwrap();
    replacement
        .add_core_record(document(&source, 1, "new committed generation"))
        .unwrap();
    replacement
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    replacement.commit(|_| true).unwrap();

    assert_eq!(old_reader.count_term("old").unwrap(), 1);
    assert_eq!(old_reader.count_term("new").unwrap(), 0);
    let new_reader = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(new_reader.count_term("old").unwrap(), 0);
    assert_eq!(new_reader.count_term("new").unwrap(), 1);
    assert_ne!(old_reader.generation_id(), new_reader.generation_id());
}

#[test]
fn a_partial_unreferenced_manifest_does_not_poison_retry() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let certificate = certificate(&source, 1, 1);
    let manifest = GenerationManifest::from_sources(vec![certificate.clone()]).unwrap();
    let generation_id = manifest.generation_id().unwrap();
    let path = manifest_path(temp.path(), &generation_id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"partial").unwrap();

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    writer.certify_source(certificate).unwrap();
    let receipt = writer.commit(|_| true).unwrap();
    assert_ne!(receipt.generation_id, generation_id);
    assert!(!path.exists());
    assert!(VerifiedIndex::open(temp.path()).is_ok());
    assert!(!fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(std::result::Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().contains(".corrupt-")));
}

#[test]
fn manifest_corruption_fails_closed() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    let receipt = writer.commit(|_| true).unwrap();
    fs::write(
        manifest_path(temp.path(), &receipt.generation_id),
        b"corrupt",
    )
    .unwrap();

    let error = match VerifiedIndex::open(temp.path()) {
        Ok(_) => panic!("corrupt manifest unexpectedly opened"),
        Err(error) => error,
    };
    assert!(matches!(error, IndexError::ManifestDigestMismatch { .. }));
}

#[test]
fn stale_schema_manifest_fails_closed_at_generation_boundary() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let mut stale_manifest = index.manifest().clone();
    const STALE_LEXICAL_SCHEMA: u32 = 15;
    stale_manifest.lexical_schema_version = STALE_LEXICAL_SCHEMA;
    let stale_generation_id = stale_manifest.generation_id().unwrap();
    write_manifest(temp.path(), &stale_generation_id, &stale_manifest).unwrap();
    let mut stale_metas = index.searcher.index().load_metas().unwrap();
    stale_metas.payload = Some(
        serde_json::to_string(&CommitPayload {
            version: COMMIT_PAYLOAD_VERSION,
            generation_id: stale_generation_id,
            publication_metadata: None,
        })
        .unwrap(),
    );

    let error = load_publication_for_metas(temp.path(), &stale_metas).unwrap_err();
    assert!(matches!(
        error,
        IndexError::GenerationContractMismatch {
            identity: IDENTITY_VERSION,
            schema: STALE_LEXICAL_SCHEMA,
            analyzer: LEXICAL_ANALYZER_VERSION,
            core_record: ctx_history_core::CORE_RECORD_VERSION,
        }
    ));
}

fn assert_active_meta_incompatibility_is_rebuilt(
    source_name: &str,
    mutate_meta: impl FnOnce(&mut serde_json::Value),
    assert_incompatibility: impl FnOnce(&IndexError),
) {
    let temp = tempdir().unwrap();
    let source = source(source_name);
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "source authoritative body"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_path = temp.path().join("active-generation.json");
    let pointer_before = fs::read(&pointer_path).unwrap();
    let old_generation_path = active_generation_path(temp.path());
    let meta_path = old_generation_path.join("meta.json");
    let mut meta: serde_json::Value =
        serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
    mutate_meta(&mut meta);
    fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();

    let error = match VerifiedIndex::open(temp.path()) {
        Ok(_) => panic!("incompatible generation unexpectedly opened"),
        Err(error) => error,
    };
    assert_incompatibility(&error);

    let mut rebuild = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    assert!(
        rebuild.base_manifest().is_none(),
        "incompatible generation was exposed as reusable base state"
    );
    assert_eq!(
        fs::read(&pointer_path).unwrap(),
        pointer_before,
        "writer open changed the active pointer before replacement"
    );
    let candidate_path = temp
        .path()
        .join(INDEX_GENERATIONS_DIRECTORY)
        .join(rebuild.candidate_directory_name.as_deref().unwrap());
    let candidate = Index::open_in_dir(&candidate_path).unwrap();
    assert!(candidate.load_metas().unwrap().segments.is_empty());
    validate_schema(&candidate.schema()).unwrap();
    assert_eq!(
        candidate.settings(),
        &publication::lexical_index_settings(),
        "replacement candidate did not use current settings"
    );
    drop(candidate);

    rebuild.begin_source(source.clone()).unwrap();
    rebuild
        .add_core_record(document(&source, 1, "source authoritative body"))
        .unwrap();
    rebuild.certify_source(certificate(&source, 1, 1)).unwrap();
    let rebuilt = rebuild.commit(|_| true).unwrap();

    assert_eq!(rebuilt.generation_id, baseline.generation_id);
    assert_ne!(active_generation_path(temp.path()), old_generation_path);
    assert!(
        !old_generation_path.exists(),
        "incompatible slot survived as a retained clone or rollback base"
    );
    let verified = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(verified.count_term("authoritative").unwrap(), 1);
}

#[test]
fn zstd_settings_generation_is_rebuilt_without_clone_or_pointer_churn() {
    assert_active_meta_incompatibility_is_rebuilt(
        "zstd-settings.jsonl",
        |meta| {
            meta["index_settings"] = serde_json::to_value(tantivy::IndexSettings {
                docstore_compression: tantivy::store::Compressor::Zstd(
                    tantivy::store::ZstdCompressor {
                        compression_level: Some(1),
                    },
                ),
                docstore_compress_dedicated_thread: true,
                docstore_blocksize: 64 * 1024,
            })
            .unwrap();
        },
        |error| {
            assert!(matches!(
                error,
                IndexError::IndexSettingsMismatch(LEXICAL_SCHEMA_VERSION)
            ));
        },
    );
}

#[test]
fn incompatible_schema_generation_is_rebuilt_without_interpretation() {
    assert_active_meta_incompatibility_is_rebuilt(
        "schema-mismatch-rebuild.jsonl",
        |meta| {
            meta["schema"] =
                serde_json::to_value(tantivy::schema::Schema::builder().build()).unwrap();
        },
        |error| {
            assert!(matches!(
                error,
                IndexError::SchemaMismatch(LEXICAL_SCHEMA_VERSION)
            ));
        },
    );
}

#[test]
fn schema_without_encoded_core_size_is_rebuilt_without_fallback() {
    assert_eq!(LEXICAL_SCHEMA_VERSION, 16);
    assert_active_meta_incompatibility_is_rebuilt(
        "encoded-size-schema-rebuild.jsonl",
        |meta| {
            let schema = meta["schema"].as_array_mut().unwrap();
            let current_fields = schema.len();
            schema.retain(|field| field["name"] != "core_record_encoded_bytes");
            assert_eq!(schema.len() + 1, current_fields);
        },
        |error| {
            assert!(matches!(
                error,
                IndexError::SchemaMismatch(LEXICAL_SCHEMA_VERSION)
            ));
        },
    );
}

#[test]
fn current_manifest_roundtrips_with_exact_policy_hash() {
    let source = source("manifest-roundtrip.jsonl");
    let manifest = GenerationManifest::from_sources(vec![certificate(&source, 7, 3)]).unwrap();
    let canonical = serde_json::to_vec(&manifest).unwrap();
    let roundtrip: GenerationManifest = serde_json::from_slice(&canonical).unwrap();

    assert_eq!(serde_json::to_vec(&roundtrip).unwrap(), canonical);
    assert_eq!(
        roundtrip.policy_schema_hash,
        current_source_generation_policy_hash().unwrap()
    );
    assert_eq!(
        roundtrip.core_record_version,
        ctx_history_core::CORE_RECORD_VERSION
    );
    assert_eq!(
        roundtrip.core_record_contract_fingerprint,
        ctx_history_core::core_record_contract_fingerprint()
    );
    assert_eq!(
        roundtrip.generation_id().unwrap(),
        manifest.generation_id().unwrap()
    );
}

#[test]
fn verified_open_rejects_mismatched_core_contract_fingerprint() {
    const PRIOR_REPOSITORY_CONTRACT_FINGERPRINT: &str =
        "e4a46c8bac8fce97b984f4cf11b92ab926f69993e20176873cbfc03739f5b6cc";
    let temp = tempdir().unwrap();
    let source = source("core-fingerprint-mismatch.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let pinned = VerifiedIndex::open(temp.path()).unwrap();
    let mut mismatched_manifest = pinned.manifest().clone();
    mismatched_manifest.core_record_contract_fingerprint =
        PRIOR_REPOSITORY_CONTRACT_FINGERPRINT.to_owned();
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(temp.path(), &index, mismatched_manifest, &[], Vec::new());

    let error = match VerifiedIndex::open(temp.path()) {
        Ok(_) => panic!("mismatched Core fingerprint generation unexpectedly opened"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        IndexError::CoreRecordContractMismatch { expected, actual }
            if expected == ctx_history_core::core_record_contract_fingerprint()
                && actual == PRIOR_REPOSITORY_CONTRACT_FINGERPRINT
    ));
}

#[test]
fn policy_field_change_changes_hash_and_generation_id() {
    let manifest = GenerationManifest::from_sources(Vec::new()).unwrap();
    let mut changed_policy = current_source_generation_policy();
    changed_policy
        .lexical
        .core_repository_association_policy_revision += 1;
    let changed_policy_hash = changed_policy.canonical_sha256().unwrap();
    let mut changed_manifest = manifest.clone();
    changed_manifest.policy_schema_hash = changed_policy_hash.clone();

    assert_ne!(manifest.policy_schema_hash, changed_policy_hash);
    assert_ne!(
        manifest.generation_id().unwrap(),
        changed_manifest.generation_id().unwrap()
    );
}

#[test]
fn verified_open_rejects_mismatched_active_policy() {
    let temp = tempdir().unwrap();
    let source = source("policy-mismatch.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    let baseline = writer.commit(|_| true).unwrap();

    let pinned = VerifiedIndex::open(temp.path()).unwrap();
    let mut mismatched_policy = current_source_generation_policy();
    mismatched_policy.lexical.event_projector_revision += 1;
    let mismatched_policy_hash = mismatched_policy.canonical_sha256().unwrap();
    let mut mismatched_manifest = pinned.manifest().clone();
    mismatched_manifest.policy_schema_hash = mismatched_policy_hash.clone();
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(temp.path(), &index, mismatched_manifest, &[], Vec::new());

    let error = match VerifiedIndex::open(temp.path()) {
        Ok(_) => panic!("mismatched policy generation unexpectedly opened"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        IndexError::GenerationPolicyMismatch {
            expected,
            actual,
        } if expected == current_source_generation_policy_hash().unwrap()
            && actual == mismatched_policy_hash
    ));

    let pointer_path = temp.path().join("active-generation.json");
    let pointer_before = fs::read(&pointer_path).unwrap();
    let incompatible_path = active_generation_path(temp.path());
    drop(pinned);
    let mut rebuild = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    assert!(rebuild.base_manifest().is_none());
    assert_eq!(fs::read(&pointer_path).unwrap(), pointer_before);
    rebuild.begin_source(source.clone()).unwrap();
    rebuild
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    rebuild.certify_source(certificate(&source, 1, 1)).unwrap();
    let rebuilt = rebuild.commit(|_| true).unwrap();

    assert_eq!(rebuilt.generation_id, baseline.generation_id);
    assert!(!incompatible_path.exists());
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
        baseline.generation_id
    );
}

include!("recovery/identity_validation.rs");

include!("recovery/verifier.rs");
