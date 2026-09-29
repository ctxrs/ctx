use super::*;

#[test]
fn replacing_a_previously_deleted_source_restores_its_documents() {
    let root = tempdir().unwrap();
    let source = source("differential-returning.jsonl");
    let base = records(&source);
    seed(root.path(), &source, &base);
    let mut writer = open(root.path());
    let (deletion, inventory) = deletion_evidence(&source, 2);
    writer.delete_source(deletion, inventory).unwrap();
    stage(&mut writer, &source, 3, &base);
    assert_eq!(writer.replacement_work.base_documents, 0);
    assert_eq!(writer.replacement_work.document_adds, 9);
    writer.commit(|_| true).unwrap();
    assert_records(root.path(), &base);
}

#[test]
fn capacity_fallback_happens_before_source_mutations_and_keeps_complete_results() {
    let source = source("differential-capacity.jsonl");
    let base = records(&source);
    for allowance in [0, 128 * 1024 + 9 * 64] {
        let root = tempdir().unwrap();
        seed(root.path(), &source, &base);
        let mut writer = open(root.path());
        writer.replacement_memory_bytes = allowance;
        stage(&mut writer, &source, 2, &base);
        assert_eq!(writer.replacement_memory_used, 0);
        assert_eq!(writer.replacement_work.source_deletions, 1);
        assert_eq!(writer.replacement_work.document_adds, 9);
        assert_eq!(writer.replacement_work.retained_documents, 0);
        writer.commit(|_| true).unwrap();
        assert_records(root.path(), &base);
    }
}

#[test]
fn allocation_is_shared_between_open_sources_and_released_on_certification() {
    let root = tempdir().unwrap();
    let first = source("differential-first.jsonl");
    let second = source("differential-second.jsonl");
    let first_records = records(&first);
    let second_records = records(&second);
    let mut initial = open(root.path());
    stage(&mut initial, &first, 1, &first_records);
    stage(&mut initial, &second, 1, &second_records);
    initial.commit(|_| true).unwrap();

    let mut writer = open(root.path());
    writer.begin_source(first.clone()).unwrap();
    let first_charge = writer.replacement_memory_used;
    assert!(first_charge > 0);
    writer.replacement_memory_bytes = first_charge;
    writer.begin_source(second.clone()).unwrap();
    assert_eq!(writer.replacement_memory_used, first_charge);
    assert_eq!(
        writer.replacement_work.base_documents, 9,
        "only the first source fits"
    );
    assert_eq!(writer.replacement_work.source_deletions, 1);
    for (first, second) in first_records.iter().zip(&second_records) {
        writer.add_core_record(first.clone()).unwrap();
        writer.add_core_record(second.clone()).unwrap();
    }
    writer.certify_source(certificate(&first, 2, 9)).unwrap();
    assert_eq!(writer.replacement_memory_used, 0);
    writer.certify_source(certificate(&second, 2, 9)).unwrap();
    assert_eq!(writer.replacement_work.retained_documents, 9);
    assert_eq!(writer.replacement_work.document_adds, 9);
    writer.commit(|_| true).unwrap();
    assert_records(root.path(), &[first_records, second_records].concat());
}

#[test]
fn exact_descriptor_mismatch_uses_full_replacement() {
    let original = source("differential-descriptor.jsonl");
    let changed = source_for_provider("codex", "different-format", "differential-descriptor.jsonl");
    assert_eq!(original, changed);
    assert!(!original.exact_descriptor_eq(&changed));
    let base = records(&original);
    let current = records(&changed);
    assert_eq!(base[0].session_id.digest(), current[0].session_id.digest());
    assert_ne!(
        base[0].session_id.encode_canonical().unwrap(),
        current[0].session_id.encode_canonical().unwrap()
    );

    for differential in [true, false] {
        let root = tempdir().unwrap();
        seed(root.path(), &original, &base);
        let mut writer = open(root.path());
        if !differential {
            writer.replacement_memory_bytes = 0;
        }
        writer.begin_source(changed.clone()).unwrap();
        assert_eq!(writer.replacement_work.source_deletions, 1);
        assert_eq!(writer.replacement_work.base_documents, 0);
        assert_eq!(writer.replacement_memory_used, 0);
        // Full replacement does not bypass the existing canonical session
        // identity guard: the descriptor scope differs despite equal digests.
        assert!(matches!(
            writer.add_core_record(current[0].clone()),
            Err(IndexError::CompactIdentityCollision {
                kind: "session", existing_digest, new_digest, ..
            }) if existing_digest == new_digest
        ));
        assert_eq!(writer.replacement_work.document_adds, 0);
        assert_eq!(writer.replacement_work.retained_documents, 0);
        drop(writer);
        assert_records(root.path(), &base);
    }
}

#[test]
fn whole_source_retain_has_no_differential_work_or_allocation() {
    let root = tempdir().unwrap();
    let source = source("differential-retain.jsonl");
    let base = records(&source);
    seed(root.path(), &source, &base);
    let mut writer = open(root.path());
    writer.retain_source(certificate(&source, 1, 9)).unwrap();
    assert_eq!(writer.replacement_memory_used, 0);
    assert_eq!(writer.replacement_work, ReplacementWork::default());
    assert!(writer.writer.is_none());
}

#[test]
fn repeated_existing_record_is_rejected_instead_of_coalesced() {
    let root = tempdir().unwrap();
    let source = source("differential-duplicate.jsonl");
    let base = records(&source);
    seed(root.path(), &source, &base);
    let mut writer = open(root.path());
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(base[0].clone()).unwrap();
    assert!(matches!(
        writer.add_core_record(base[0].clone()),
        Err(IndexError::DuplicateEventIdentity(_))
    ));
    assert_eq!(writer.pending[&source_token(&source)].staged_documents, 1);
    assert_eq!(writer.replacement_work.retained_documents, 1);
    assert_eq!(writer.replacement_work.event_deletions, 0);
    assert_records(root.path(), &base);
}

#[test]
fn duplicate_new_records_still_fail_candidate_publication() {
    let root = tempdir().unwrap();
    let source = source("differential-new-duplicate.jsonl");
    let base = records(&source);
    seed(root.path(), &source, &base);
    let mut writer = open(root.path());
    let new = document_for_session(&source, "new-session", 20, "appended");
    let mut current = base.clone();
    current.extend([new.clone(), new]);
    stage(&mut writer, &source, 2, &current);
    assert!(matches!(
        writer.commit(|_| true),
        Err(IndexError::DuplicateEventIdentity(_))
    ));
    assert_records(root.path(), &base);
}

#[test]
fn missing_or_wrong_certificate_cannot_delete_unseen_records() {
    let root = tempdir().unwrap();
    let source = source("differential-incomplete.jsonl");
    let base = records(&source);
    seed(root.path(), &source, &base);
    let mut writer = open(root.path());
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(base[0].clone()).unwrap();
    assert!(matches!(
        writer.certify_source(certificate(&source, 2, 9)),
        Err(IndexError::SourceDocumentCountMismatch { .. })
    ));
    assert_eq!(writer.replacement_work.event_deletions, 0);
    assert!(writer.replacement_memory_used > 0);
    assert!(matches!(
        writer.commit(|_| true),
        Err(IndexError::SourceNotCertified(_))
    ));
    assert_records(root.path(), &base);
}

#[test]
fn complete_empty_replacement_deletes_all_and_seals_the_source() {
    let source = source("differential-empty.jsonl");
    let base = records(&source);
    let (root, work) = paired(&source, &base, &[]);
    assert_eq!(work.event_deletions, 9);
    assert_eq!(work.document_adds, 0);
    assert_records(root.path(), &[]);

    let root = tempdir().unwrap();
    seed(root.path(), &source, &base);
    let mut writer = open(root.path());
    stage(&mut writer, &source, 2, &base);
    writer.certify_source(certificate(&source, 2, 9)).unwrap();
    assert!(matches!(
        writer.add_core_record(base[0].clone()),
        Err(IndexError::DocumentSourceNotActive)
    ));
    assert_eq!(writer.replacement_memory_used, 0);
    writer.commit(|_| true).unwrap();
    assert_records(root.path(), &base);
}

#[test]
fn minimum_writer_budget_disables_reconciliation_without_rejecting_refresh() {
    let root = tempdir().unwrap();
    let source = source("differential-minimum.jsonl");
    let base = records(&source);
    seed(root.path(), &source, &base);
    let mut writer = GenerationWriter::open(
        root.path(),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: INDEX_MEMORY_MIN_PER_THREAD,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    assert_eq!(writer.replacement_memory_bytes, 0);
    stage(&mut writer, &source, 2, &base);
    assert_eq!(writer.replacement_work.document_adds, 9);
    writer.commit(|_| true).unwrap();
    assert_records(root.path(), &base);
}
