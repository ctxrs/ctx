use super::*;
use std::{io::Write as _, sync::Arc};
use tantivy::directory::TerminatingWrite as _;

#[test]
fn commit_binds_manifest_and_searchable_documents() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_document(document(&source, 1, "atomic generation"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    let receipt = writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(index.generation_id(), receipt.generation_id);
    assert_eq!(
        receipt.manifest().generation_id().unwrap(),
        receipt.generation_id
    );
    assert_eq!(receipt.manifest().sources, index.manifest().sources);
    assert_eq!(receipt.manifest().removals, index.manifest().removals);
    assert_eq!(index.manifest().indexed_documents, 1);
    assert_eq!(index.count_term("atomic").unwrap(), 1);
}

#[test]
fn replacement_reuses_missing_prior_repository_certificate_and_deletion_removes_it() {
    use ctx_history_core::{
        CoreRecordAnnotation, RepositoryAbstention, RepositoryAbstentionReason, RepositoryBinding,
        RepositoryEvidence, RepositoryEvidenceConfidence, RepositoryEvidenceKind,
        RepositoryLocalRootAuthorization,
    };

    let temp = tempdir().unwrap();
    let source = source("repository-session.jsonl");
    let initial_document = document(&source, 1, "repository event");
    let event_id = initial_document.event_id;
    let binding = RepositoryBinding {
        binding_id: "binding-1".to_owned(),
        logical_repository_id: "local:repo-1".to_owned(),
        checkout_id: Some("checkout-1".to_owned()),
        worktree_id: Some("worktree-1".to_owned()),
        aliases: Vec::new(),
        git_object_format: None,
        local_root_authorization: Some(RepositoryLocalRootAuthorization {
            local_root: "/old/repo".to_owned(),
            locator_fingerprint: [9; 32],
            observed_at_unix_ms: 1,
        }),
        evidence: vec![RepositoryEvidence {
            kind: RepositoryEvidenceKind::DeclaredToolWorkdir,
            confidence: RepositoryEvidenceConfidence::High,
        }],
        association_policy_revision: 1,
    };
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_document_with_annotation(
            initial_document,
            CoreRecordAnnotation {
                repository_bindings: vec![binding],
                ..CoreRecordAnnotation::default()
            },
        )
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    initial.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    replacement.begin_source(source.clone()).unwrap();
    replacement
        .add_document_with_annotation(
            document(&source, 1, "repository event"),
            CoreRecordAnnotation {
                repository_abstentions: vec![RepositoryAbstention {
                    evidence_kind: RepositoryEvidenceKind::DeclaredToolWorkdir,
                    reason: RepositoryAbstentionReason::CandidateMissingBeforeCertification,
                    detail: None,
                    association_policy_revision: 1,
                }],
                ..CoreRecordAnnotation::default()
            },
        )
        .unwrap();
    replacement
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    replacement.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let rebuilt = index
        .core_record_by_id(event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(rebuilt.repository_bindings.len(), 1);
    assert!(rebuilt.repository_bindings[0]
        .local_root_authorization
        .is_none());
    assert!(rebuilt
        .repository_abstentions
        .iter()
        .any(|abstention| { abstention.reason == RepositoryAbstentionReason::Unavailable }));

    let mut deleting = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    let (deletion, inventory) = deletion_evidence(&source, 3);
    deleting.delete_source(deletion, inventory).unwrap();
    deleting.commit(|_| true).unwrap();
    let deleted = VerifiedIndex::open(temp.path()).unwrap();
    assert!(deleted
        .core_record_by_id(event_id.as_uuid())
        .unwrap()
        .is_none());
}

#[test]
fn unchanged_commit_returns_the_verified_base_without_republication() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_document(document(&source, 1, "stable generation"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    let initial_receipt = initial.commit(|_| true).unwrap();

    let meta_path = temp.path().join("meta.json");
    let managed_path = temp.path().join(".managed.json");
    let manifest_path = manifest_path(temp.path(), &initial_receipt.generation_id);
    let meta_before = fs::read(&meta_path).unwrap();
    let meta_metadata_before = fs::metadata(&meta_path).unwrap();
    let managed_before = fs::read(&managed_path).unwrap();
    let managed_metadata_before = fs::metadata(&managed_path).unwrap();
    let manifest_before = fs::read(&manifest_path).unwrap();
    let manifest_metadata_before = fs::metadata(&manifest_path).unwrap();

    let mut unchanged_writer =
        GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    assert!(
        unchanged_writer.writer.is_none(),
        "opening a generation must not construct Tantivy's IndexWriter"
    );
    assert!(
        unchanged_writer.preflight_lock.is_some(),
        "the lazy writer must retain Tantivy's exclusive lock"
    );
    let index_writer_constructions =
        std::sync::Arc::clone(&unchanged_writer.index_writer_constructions);
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    unchanged_writer
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    let base = unchanged_writer
        .begin_source_append(source.clone())
        .unwrap()
        .clone();
    let base_frontier = base.frontier().unwrap();
    let replay = CertifiedSourceAppend::certify(
        &base,
        base.clone(),
        base_frontier.certified_prefix_bytes(),
        *base_frontier.certified_prefix_digest(),
    )
    .unwrap();
    unchanged_writer.certify_source_append(replay).unwrap();
    assert!(
        unchanged_writer.writer.is_none(),
        "an exact certified replay must not construct Tantivy's IndexWriter"
    );
    let mut revalidations = 0;
    let unchanged = unchanged_writer
        .commit_with_complete_inventory_revalidation(
            |target| {
                revalidations += 1;
                matches!(
                    target,
                    RevalidationTarget::Source(certificate) if certificate == &base
                )
            },
            |current| current == &inventory,
        )
        .unwrap();

    assert_eq!(
        index_writer_constructions.load(Ordering::SeqCst),
        0,
        "a healthy exact no-op must construct zero Tantivy IndexWriters"
    );
    assert_eq!(revalidations, 1);
    assert_eq!(unchanged.generation_id, initial_receipt.generation_id);
    assert_eq!(unchanged.opstamp, initial_receipt.opstamp);
    assert_eq!(unchanged.indexed_documents, 1);
    assert_eq!(unchanged.certified_sources, 1);
    assert_eq!(unchanged.certified_source_bytes, 10);
    assert_eq!(
        unchanged.manifest().generation_id().unwrap(),
        unchanged.generation_id
    );
    assert_eq!(
        unchanged.manifest().sources,
        initial_receipt.manifest().sources
    );
    assert_eq!(
        unchanged.manifest().removals,
        initial_receipt.manifest().removals
    );
    assert_eq!(fs::read(&meta_path).unwrap(), meta_before);
    assert_eq!(
        fs::metadata(&meta_path).unwrap().modified().unwrap(),
        meta_metadata_before.modified().unwrap()
    );
    assert_eq!(fs::read(&managed_path).unwrap(), managed_before);
    assert_eq!(
        fs::metadata(&managed_path).unwrap().modified().unwrap(),
        managed_metadata_before.modified().unwrap()
    );
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
    assert_eq!(
        fs::metadata(&manifest_path).unwrap().modified().unwrap(),
        manifest_metadata_before.modified().unwrap()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        assert_eq!(
            fs::metadata(&meta_path).unwrap().ino(),
            meta_metadata_before.ino()
        );
        assert_eq!(
            fs::metadata(&managed_path).unwrap().ino(),
            managed_metadata_before.ino()
        );
        assert_eq!(
            fs::metadata(&manifest_path).unwrap().ino(),
            manifest_metadata_before.ino()
        );
    }

    let verified = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(verified.generation_id(), unchanged.generation_id);
    assert_eq!(verified.document_count(), 1);
    assert_eq!(verified.count_term("stable").unwrap(), 1);
}

#[test]
fn logical_rescan_retains_nonappendable_source_without_tantivy_artifacts() {
    let temp = tempdir().unwrap();
    let source = source("logical.sqlite");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_document(document(&source, 1, "logical snapshot"))
        .unwrap();
    let certificate = certificate(&source, 1, 1);
    initial.certify_source(certificate.clone()).unwrap();
    let initial_receipt = initial.commit(|_| true).unwrap();

    let root_files_before = fs::read_dir(temp.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| {
            let path = entry.path();
            (entry.file_name(), fs::read(path).unwrap())
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let managed_metadata_before = fs::metadata(temp.path().join(".managed.json")).unwrap();

    let mut retained = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    let constructions = Arc::clone(&retained.index_writer_constructions);
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    retained
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    retained.retain_source(certificate.clone()).unwrap();
    let receipt = retained
        .commit_with_complete_inventory_revalidation(
            |target| {
                matches!(
                    target,
                    RevalidationTarget::Source(current) if current == &certificate
                )
            },
            |current| current == &inventory,
        )
        .unwrap();

    let root_files_after = fs::read_dir(temp.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| {
            let path = entry.path();
            (entry.file_name(), fs::read(path).unwrap())
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let managed_metadata_after = fs::metadata(temp.path().join(".managed.json")).unwrap();
    assert_eq!(constructions.load(Ordering::SeqCst), 0);
    assert_eq!(receipt.generation_id, initial_receipt.generation_id);
    assert_eq!(receipt.opstamp, initial_receipt.opstamp);
    assert_eq!(root_files_after, root_files_before);
    assert_eq!(
        managed_metadata_after.modified().unwrap(),
        managed_metadata_before.modified().unwrap()
    );
}

#[test]
fn logically_identical_one_pass_replacement_is_discarded_without_publication() {
    let temp = tempdir().unwrap();
    let source = source("logical-snapshot.sqlite");
    let certificate = certificate(&source, 1, 1);
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_document(document(&source, 1, "stable logical row"))
        .unwrap();
    initial.certify_source(certificate.clone()).unwrap();
    let initial_receipt = initial.commit(|_| true).unwrap();

    let meta_path = temp.path().join("meta.json");
    let meta_before = fs::read(&meta_path).unwrap();
    let meta_metadata_before = fs::metadata(&meta_path).unwrap();
    let manifests_before = fs::read_dir(temp.path().join(MANIFEST_DIRECTORY))
        .unwrap()
        .count();

    let mut staged = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    let constructions = Arc::clone(&staged.index_writer_constructions);
    staged.begin_source(source.clone()).unwrap();
    staged
        .add_document(document(&source, 1, "stable logical row"))
        .unwrap();
    staged.certify_source(certificate.clone()).unwrap();
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    staged
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    assert_eq!(
        constructions.load(Ordering::SeqCst),
        1,
        "one-pass replacement staging should construct one disposable writer"
    );

    let mut source_revalidations = 0;
    let mut inventory_revalidations = 0;
    let receipt = staged
        .commit_with_complete_inventory_revalidation(
            |target| {
                source_revalidations += 1;
                matches!(
                    target,
                    RevalidationTarget::Source(current) if current == &certificate
                )
            },
            |current| {
                inventory_revalidations += 1;
                current == &inventory
            },
        )
        .unwrap();

    assert_eq!(source_revalidations, 1);
    assert_eq!(inventory_revalidations, 1);
    assert_eq!(receipt.generation_id, initial_receipt.generation_id);
    assert_eq!(receipt.opstamp, initial_receipt.opstamp);
    assert_eq!(fs::read(&meta_path).unwrap(), meta_before);
    assert_eq!(
        fs::metadata(&meta_path).unwrap().modified().unwrap(),
        meta_metadata_before.modified().unwrap()
    );
    assert_eq!(
        fs::read_dir(temp.path().join(MANIFEST_DIRECTORY))
            .unwrap()
            .count(),
        manifests_before
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        assert_eq!(
            fs::metadata(&meta_path).unwrap().ino(),
            meta_metadata_before.ino()
        );
    }

    let verified = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(verified.generation_id(), initial_receipt.generation_id);
    assert_eq!(verified.document_count(), 1);
    assert_eq!(verified.count_term("stable").unwrap(), 1);
}

#[test]
fn exact_replay_omission_fails_with_typed_incomplete_coverage() {
    let temp = tempdir().unwrap();
    let replayed_source = source("replayed.jsonl");
    let omitted_source = source("omitted.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(replayed_source.clone()).unwrap();
    initial
        .add_document(document(&replayed_source, 1, "replayed source"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&replayed_source, 1, 1, 10))
        .unwrap();
    initial.begin_source(omitted_source.clone()).unwrap();
    initial
        .add_document(document(&omitted_source, 1, "omitted source"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&omitted_source, 1, 1, 10))
        .unwrap();
    let initial_receipt = initial.commit(|_| true).unwrap();

    let mut incomplete = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    let base = incomplete
        .begin_source_append(replayed_source.clone())
        .unwrap()
        .clone();
    let frontier = base.frontier().unwrap();
    let replay = CertifiedSourceAppend::certify(
        &base,
        base.clone(),
        frontier.certified_prefix_bytes(),
        *frontier.certified_prefix_digest(),
    )
    .unwrap();
    incomplete.certify_source_append(replay).unwrap();
    let inventory = complete_inventory(
        &replayed_source,
        1,
        vec![replayed_source.clone(), omitted_source.clone()],
    );
    incomplete
        .certify_complete_inventory(inventory.clone())
        .unwrap();

    assert!(matches!(
        incomplete.exact_replay_inventory_witness(),
        Err(IndexError::IncompleteExactReplayCoverage { .. })
    ));
    assert!(
        incomplete.writer.is_none(),
        "the regression must reach commit through the lazy preflight state"
    );
    let error = incomplete
        .commit_with_complete_inventory_revalidation(|_| true, |current| current == &inventory)
        .unwrap_err();
    assert!(matches!(
        error,
        IndexError::IncompleteExactReplayCoverage { ref source_id }
            if source_id == &omitted_source.identity().to_string()
    ));

    let verified = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(verified.generation_id(), initial_receipt.generation_id);
    assert_eq!(verified.document_count(), 2);
    assert_eq!(verified.count_term("replayed").unwrap(), 1);
    assert_eq!(verified.count_term("omitted").unwrap(), 1);
}

#[test]
fn exact_replay_witness_covers_retained_sources_and_carried_removals() {
    let temp = tempdir().unwrap();
    let retained = source("retained.jsonl");
    let removed = source("removed.jsonl");

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(retained.clone()).unwrap();
    initial
        .add_document(document(&retained, 1, "retained source"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&retained, 1, 1, 10))
        .unwrap();
    initial.begin_source(removed.clone()).unwrap();
    initial
        .add_document(document(&removed, 1, "removed source"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&removed, 1, 1, 10))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let (deletion, inventory) =
        deletion_evidence_with_retained(&removed, 2, vec![retained.clone()]);
    let current_inventory = inventory.clone();
    let mut deleting = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    deleting.delete_source(deletion, inventory).unwrap();
    let deleted = deleting.commit(|_| true).unwrap();

    let mut replay = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    let replayed_source = stage_exact_replay(&mut replay, &retained);
    replay
        .certify_complete_inventory(current_inventory.clone())
        .unwrap();
    let constructions = std::sync::Arc::clone(&replay.index_writer_constructions);
    let mut source_revalidations = 0;
    let mut inventory_revalidations = 0;
    let receipt = replay
        .commit_with_complete_inventory_revalidation(
            |target| match target {
                RevalidationTarget::Source(source) => {
                    source_revalidations += 1;
                    source == &replayed_source
                }
                RevalidationTarget::Deletion(_) => false,
            },
            |inventory| {
                inventory_revalidations += 1;
                inventory == &current_inventory
            },
        )
        .unwrap();

    assert_eq!(constructions.load(Ordering::SeqCst), 0);
    assert_eq!(source_revalidations, 1);
    assert_eq!(inventory_revalidations, 1);
    assert_eq!(receipt.generation_id, deleted.generation_id);
    assert_eq!(receipt.opstamp, deleted.opstamp);
}

#[test]
fn exact_replay_witness_covers_removal_only_and_rejects_stale_inventory() {
    let temp = tempdir().unwrap();
    let removed = source("removed.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(removed.clone()).unwrap();
    initial
        .add_document(document(&removed, 1, "removed source"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&removed, 1, 1, 10))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let (deletion, inventory) = deletion_evidence(&removed, 2);
    let current_inventory = inventory.clone();
    let mut deleting = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    deleting.delete_source(deletion, inventory).unwrap();
    let deleted = deleting.commit(|_| true).unwrap();

    let mut replay = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    replay
        .certify_complete_inventory(current_inventory.clone())
        .unwrap();
    let constructions = std::sync::Arc::clone(&replay.index_writer_constructions);
    let receipt = replay
        .commit_with_complete_inventory_revalidation(
            |_| false,
            |inventory| inventory == &current_inventory,
        )
        .unwrap();
    assert_eq!(constructions.load(Ordering::SeqCst), 0);
    assert_eq!(receipt.generation_id, deleted.generation_id);
    assert_eq!(receipt.opstamp, deleted.opstamp);

    let mut stale = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    stale
        .certify_complete_inventory(current_inventory.clone())
        .unwrap();
    let terminal_inventory = std::sync::Arc::new(std::sync::Mutex::new(current_inventory.clone()));
    let inventory_after_reappearance = complete_inventory(&removed, 3, vec![removed.clone()]);
    *terminal_inventory.lock().unwrap() = inventory_after_reappearance;
    let raced_constructions = std::sync::Arc::clone(&stale.index_writer_constructions);
    let terminal_inventory_for_commit = std::sync::Arc::clone(&terminal_inventory);
    let mut inventory_revalidations = 0;
    let error = stale
        .commit_with_complete_inventory_revalidation(
            |_| false,
            |inventory| {
                inventory_revalidations += 1;
                inventory == &*terminal_inventory_for_commit.lock().unwrap()
            },
        )
        .unwrap_err();
    assert!(matches!(
        error,
        IndexError::CompleteInventoryInvalidated { .. }
    ));
    assert_eq!(inventory_revalidations, 1);
    assert_eq!(raced_constructions.load(Ordering::SeqCst), 0);
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
        deleted.generation_id
    );
}

#[test]
fn empty_inventory_requires_terminal_witness_and_rejects_discovered_source_race() {
    let temp = tempdir().unwrap();
    let discovered = source("discovered-after-opening.jsonl");
    let initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .commit(|_| true)
        .unwrap();

    let unwitnessed = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    let unwitnessed_constructions = std::sync::Arc::clone(&unwitnessed.index_writer_constructions);
    let unwitnessed_receipt = unwitnessed.commit(|_| true).unwrap();
    assert_eq!(unwitnessed_receipt.generation_id, initial.generation_id);
    assert_eq!(
        unwitnessed_constructions.load(Ordering::SeqCst),
        1,
        "an empty base without current inventory authority must enter the ordinary writer path"
    );

    let opening_empty = complete_inventory(&discovered, 1, Vec::new());
    let mut empty = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    empty
        .certify_complete_inventory(opening_empty.clone())
        .unwrap();
    let constructions = std::sync::Arc::clone(&empty.index_writer_constructions);
    let mut revalidations = 0;
    let replay = empty
        .commit_with_complete_inventory_revalidation(
            |_| false,
            |inventory| {
                revalidations += 1;
                inventory == &opening_empty
            },
        )
        .unwrap();

    assert_eq!(constructions.load(Ordering::SeqCst), 0);
    assert_eq!(revalidations, 1);
    assert_eq!(replay.generation_id, initial.generation_id);
    assert_eq!(replay.opstamp, unwitnessed_receipt.opstamp);
    assert_eq!(replay.indexed_documents, 0);
    assert_eq!(replay.certified_sources, 0);

    let mut raced = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    raced
        .certify_complete_inventory(opening_empty.clone())
        .unwrap();
    let raced_constructions = std::sync::Arc::clone(&raced.index_writer_constructions);
    let terminal_inventory = std::sync::Arc::new(std::sync::Mutex::new(opening_empty));
    let inventory_after_discovery = complete_inventory(&discovered, 2, vec![discovered.clone()]);
    *terminal_inventory.lock().unwrap() = inventory_after_discovery;
    let terminal_inventory_for_commit = std::sync::Arc::clone(&terminal_inventory);
    let mut inventory_revalidations = 0;
    let error = raced
        .commit_with_complete_inventory_revalidation(
            |_| false,
            |inventory| {
                inventory_revalidations += 1;
                inventory == &*terminal_inventory_for_commit.lock().unwrap()
            },
        )
        .unwrap_err();
    assert!(matches!(
        error,
        IndexError::CompleteInventoryInvalidated { .. }
    ));
    assert_eq!(inventory_revalidations, 1);
    assert_eq!(raced_constructions.load(Ordering::SeqCst), 0);
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
        unwitnessed_receipt.generation_id
    );
}

#[test]
fn production_merge_policy_bounds_repeated_tiny_appends_amortized() {
    let temp = tempdir().unwrap();
    let source = source("tiny-appends.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_document(document(&source, 1, "tiny append 1"))
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
            .add_document(document(
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
        "publication should retain only the visible generation and one grace generation"
    );
}

#[test]
fn writer_open_reclaims_unreferenced_and_quarantined_manifests() {
    let temp = tempdir().unwrap();
    let source = source("manifest-retention.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_document(document(&source, 1, "visible generation"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let receipt = initial.commit(|_| true).unwrap();

    let directory = temp.path().join(MANIFEST_DIRECTORY);
    let stale_generation = "11".repeat(32);
    let stale = directory.join(format!("{stale_generation}.json"));
    let quarantine = directory.join(format!(".{stale_generation}.corrupt-test"));
    let unrelated = directory.join("operator-note.txt");
    fs::write(&stale, b"orphaned precommit manifest").unwrap();
    fs::write(&quarantine, b"quarantined collision").unwrap();
    fs::write(&unrelated, b"not managed by ctx manifest retention").unwrap();

    let writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    assert_eq!(
        writer.base_manifest().unwrap().generation_id().unwrap(),
        receipt.generation_id
    );
    assert!(!stale.exists());
    assert!(!quarantine.exists());
    assert!(unrelated.exists());
    assert!(manifest_path(temp.path(), &receipt.generation_id).exists());
}

#[test]
fn writer_exposes_the_base_manifest_captured_under_its_lock() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    assert!(first.base_manifest().is_none());
    first.begin_source(source.clone()).unwrap();
    first.add_document(document(&source, 1, "base")).unwrap();
    first.certify_source(certificate(&source, 1, 1)).unwrap();
    let receipt = first.commit(|_| true).unwrap();

    let writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    let base = writer.base_manifest().unwrap();
    assert_eq!(base.generation_id().unwrap(), receipt.generation_id);
    assert_eq!(base.sources.len(), 1);
    assert_eq!(base.sources[0].observation().source(), &source);

    let error = match GenerationWriter::open(temp.path(), WriterOptions::default()) {
        Ok(_) => panic!("competing writer unexpectedly acquired the writer lock"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        IndexError::Tantivy(tantivy::TantivyError::LockFailure(_, _))
    ));
    assert_eq!(
        writer.base_manifest().unwrap().generation_id().unwrap(),
        receipt.generation_id
    );
}

#[test]
fn orphaned_managed_files_are_reclaimed_before_exact_noop_without_index_writer() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_document(document(&source, 1, "stable generation"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    let initial_receipt = initial.commit(|_| true).unwrap();
    let pinned = VerifiedIndex::open(temp.path()).unwrap();

    let orphan_relative =
        Path::new("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.store");
    let orphan_path = temp.path().join(orphan_relative);
    let directory = DurableMmapDirectory::open(temp.path()).unwrap();
    let index = Index::open(directory).unwrap();
    let mut orphan = index.directory().open_write(orphan_relative).unwrap();
    orphan.write_all(b"abandoned candidate segment").unwrap();
    orphan.terminate().unwrap();
    drop(index);
    assert!(orphan_path.is_file());

    let mut replay = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    assert!(
        !orphan_path.exists(),
        "preflight recovery left an orphaned managed file"
    );
    let constructions = std::sync::Arc::clone(&replay.index_writer_constructions);
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    replay
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    stage_exact_replay(&mut replay, &source);
    let receipt = replay
        .commit_with_complete_inventory_revalidation(|_| true, |current| current == &inventory)
        .unwrap();

    assert_eq!(
        constructions.load(Ordering::SeqCst),
        0,
        "orphan recovery plus exact replay must not construct IndexWriter"
    );
    assert_eq!(receipt.generation_id, initial_receipt.generation_id);
    assert_eq!(receipt.opstamp, initial_receipt.opstamp);
    assert_eq!(pinned.generation_id(), initial_receipt.generation_id);
    assert_eq!(pinned.count_term("stable").unwrap(), 1);
}

#[test]
fn abandoned_publication_reclamation_does_not_construct_index_writer() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial.add_document(document(&source, 1, "base")).unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let root_abandoned = temp
        .path()
        .join(".ctx-tantivy-atomic-0123456789abcdef0123456789abcdef.tmp");
    let manifest_abandoned = temp
        .path()
        .join(MANIFEST_DIRECTORY)
        .join(".ctx-tantivy-atomic-fedcba9876543210fedcba9876543210.tmp");
    fs::write(&root_abandoned, b"abandoned root publication").unwrap();
    fs::write(&manifest_abandoned, b"abandoned manifest publication").unwrap();

    let mut replay = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    assert!(!root_abandoned.exists());
    assert!(!manifest_abandoned.exists());
    let constructions = std::sync::Arc::clone(&replay.index_writer_constructions);
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    replay
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    stage_exact_replay(&mut replay, &source);
    replay
        .commit_with_complete_inventory_revalidation(|_| true, |current| current == &inventory)
        .unwrap();
    assert_eq!(
        constructions.load(Ordering::SeqCst),
        0,
        "preflight reclamation must not construct Tantivy IndexWriter"
    );
}

#[test]
fn lazy_writer_handoff_rejects_a_generation_published_in_the_lock_gap() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial.add_document(document(&source, 1, "base")).unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let mut stale = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    stale.begin_source_append(source.clone()).unwrap();
    let competing_root = temp.path().to_path_buf();
    let competing_source = source.clone();
    stale.before_writer_handoff = Some(Box::new(move || {
        let mut competing =
            GenerationWriter::open(&competing_root, WriterOptions::default()).unwrap();
        competing.begin_source(competing_source.clone()).unwrap();
        competing
            .add_document(document(&competing_source, 1, "competing generation"))
            .unwrap();
        competing
            .certify_source(appendable_certificate(&competing_source, 2, 1, 10))
            .unwrap();
        competing.commit(|_| true).unwrap();
    }));

    let error = stale
        .add_document(document(&source, 2, "stale delta"))
        .unwrap_err();
    assert!(matches!(error, IndexError::ConcurrentGenerationChange));

    let current = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(current.count_term("competing").unwrap(), 1);
    assert_eq!(current.count_term("stale").unwrap(), 0);
}

#[test]
fn writer_rejects_a_nonempty_payloadless_index() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(source.clone()).unwrap();
    first.add_document(document(&source, 1, "body")).unwrap();
    first.certify_source(certificate(&source, 1, 1)).unwrap();
    first.commit(|_| true).unwrap();

    let directory = DurableMmapDirectory::open(temp.path()).unwrap();
    let index = Index::open(directory.clone()).unwrap();
    let mut metas = index.load_metas().unwrap();
    metas.payload = None;
    directory
        .atomic_write(Path::new("meta.json"), &serde_json::to_vec(&metas).unwrap())
        .unwrap();

    let error = match GenerationWriter::open(temp.path(), WriterOptions::default()) {
        Ok(_) => panic!("nonempty payloadless index unexpectedly opened for writing"),
        Err(error) => error,
    };
    assert!(matches!(error, IndexError::UnboundIndexState));
}

#[test]
fn stored_document_identities_use_canonical_fixed_bytes() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let expected = document(&source, 1, "body");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_document(expected.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let fields = fields_from_schema(index.searcher.schema()).unwrap();
    let address = index
        .searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let stored: TantivyDocument = index.searcher.doc(address).unwrap();
    let event_bytes = stored
        .get_first(fields.event_identity)
        .and_then(|value| value.as_bytes())
        .unwrap();
    let session_bytes = stored
        .get_first(fields.session_identity)
        .and_then(|value| value.as_bytes())
        .unwrap();

    assert_eq!(event_bytes.len(), StableEntityId::CANONICAL_LEN);
    assert_eq!(
        event_bytes,
        expected.event_id.encode_canonical().unwrap().as_slice()
    );
    assert_eq!(session_bytes.len(), StableEntityId::CANONICAL_LEN);
    assert_eq!(
        session_bytes,
        expected.session_id.encode_canonical().unwrap().as_slice()
    );
    assert_eq!(
        index
            .event_by_id(expected.event_id.as_uuid())
            .unwrap()
            .unwrap()
            .event_id,
        expected.event_id
    );
}

#[test]
fn direct_core_record_is_the_canonical_locator_free_write_path() {
    let temp = tempdir().unwrap();
    let source = source("direct-core.jsonl");
    let document = document(&source, 1, "direct Core body");
    let expected = document.to_core_record().unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(expected.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let actual = index
        .core_record_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(actual, expected);
    assert!(index
        .event_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap()
        .source
        .exact_descriptor_eq(&source));
}

#[test]
fn direct_core_record_rejects_noncurrent_policy_revisions() {
    let temp = tempdir().unwrap();
    let source = source("direct-core-policy.jsonl");
    let mut record = document(&source, 1, "direct Core body")
        .to_core_record()
        .unwrap();
    record.normalization_revision += 1;
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source).unwrap();

    assert!(matches!(
        writer.add_core_record(record),
        Err(IndexError::CoreRecordPolicyRevisionMismatch { .. })
    ));
}
