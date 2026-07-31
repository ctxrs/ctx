use super::*;

#[test]
fn failed_final_revalidation_keeps_the_previous_generation() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(source.clone()).unwrap();
    first
        .add_document(document(&source, 1, "previous generation"))
        .unwrap();
    first.certify_source(certificate(&source, 1, 1)).unwrap();
    let first_receipt = first.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    replacement.begin_source(source.clone()).unwrap();
    replacement
        .add_document(document(&source, 1, "uncommitted replacement"))
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
fn deletion_requires_final_inventory_revalidation() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(source.clone()).unwrap();
    first
        .add_document(document(&source, 1, "retained"))
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
    assert_eq!(accepted_receipt.manifest().removals.len(), 1);
    assert_eq!(accepted_receipt.manifest().removals[0].source(), &source);
    let current = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(current.count_term("retained").unwrap(), 0);
    assert!(current.manifest().sources.is_empty());
    assert_eq!(current.manifest().removals.len(), 1);
    assert_eq!(current.manifest().removals[0].source(), &source);
    assert!(current.manifest().removals[0]
        .deletion()
        .verifies(current.manifest().removals[0].inventory()));
}

#[test]
fn generation_removals_persist_until_the_exact_lineage_returns() {
    let temp = tempdir().unwrap();
    let removed = source("removed.jsonl");
    let retained = source("retained.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(removed.clone()).unwrap();
    first
        .add_document(document(&removed, 1, "removed body"))
        .unwrap();
    first.certify_source(certificate(&removed, 1, 1)).unwrap();
    first.begin_source(retained.clone()).unwrap();
    first
        .add_document(document(&retained, 1, "retained body"))
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
    assert_eq!(deleted.manifest().removals.len(), 1);
    let durable_removal = deleted.manifest().removals[0].clone();
    assert_eq!(durable_removal.source(), &removed);

    let mut unrelated = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    unrelated.begin_source(retained.clone()).unwrap();
    unrelated
        .add_document(document(&retained, 2, "rewritten retained body"))
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
    assert_eq!(carried.manifest().removals, vec![durable_removal]);

    let returning = source_for_provider("codex", "codex_prompt_history_jsonl", "removed.jsonl");
    assert_eq!(returning, removed);
    assert!(!returning.exact_descriptor_eq(&removed));
    let mut republishing = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    republishing.begin_source(returning.clone()).unwrap();
    republishing
        .add_document(document(&returning, 4, "returned body"))
        .unwrap();
    republishing
        .certify_source(certificate(&returning, 4, 1))
        .unwrap();
    republishing.commit(|_| true).unwrap();

    let returned = VerifiedIndex::open(temp.path()).unwrap();
    assert!(returned.manifest().removals.is_empty());
    assert!(returned.manifest().sources.iter().any(|source| source
        .observation()
        .source()
        .exact_descriptor_eq(&returning)));
}

#[test]
fn generation_removal_validation_binds_inventory_order_and_membership() {
    let first = source("first-removed.jsonl");
    let second = source("second-removed.jsonl");
    let (first_deletion, first_inventory) = deletion_evidence(&first, 1);
    let (_, wrong_inventory) = deletion_evidence(&first, 2);
    assert!(matches!(
        GenerationRemoval::new(first_deletion.clone(), wrong_inventory),
        Err(IndexError::InvalidGenerationRemoval(_))
    ));

    let first_removal = GenerationRemoval::new(first_deletion, first_inventory).unwrap();
    let (second_deletion, second_inventory) = deletion_evidence(&second, 3);
    let second_removal = GenerationRemoval::new(second_deletion, second_inventory).unwrap();
    let canonical = GenerationManifest::from_parts(
        Vec::new(),
        vec![second_removal.clone(), first_removal.clone()],
    )
    .unwrap();
    assert!(canonical
        .removals
        .windows(2)
        .all(|pair| { source_sort_key(pair[0].source()) < source_sort_key(pair[1].source()) }));
    assert_ne!(
        GenerationManifest::from_sources(Vec::new())
            .unwrap()
            .generation_id()
            .unwrap(),
        canonical.generation_id().unwrap()
    );

    let mut duplicate = canonical.clone();
    duplicate.removals.push(duplicate.removals[0].clone());
    duplicate
        .removals
        .sort_by_key(|removal| source_sort_key(removal.source()));
    assert!(matches!(
        duplicate.validate_contract(),
        Err(IndexError::NonCanonicalManifestRemovals)
    ));

    let mut out_of_order = canonical.clone();
    out_of_order.removals.reverse();
    assert!(matches!(
        out_of_order.validate_contract(),
        Err(IndexError::NonCanonicalManifestRemovals)
    ));

    assert!(matches!(
        GenerationManifest::from_parts(vec![certificate(&first, 1, 0)], vec![first_removal]),
        Err(IndexError::ManifestSourceRemovalOverlap(_))
    ));
}

#[test]
fn replacement_atomically_removes_old_source_documents() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(source.clone()).unwrap();
    first
        .add_document(document(&source, 1, "retired content"))
        .unwrap();
    first.certify_source(certificate(&source, 1, 1)).unwrap();
    first.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    replacement.begin_source(source.clone()).unwrap();
    replacement
        .add_document(document(&source, 1, "current content"))
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
    first.add_document(document(&source, 1, "base")).unwrap();
    first
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    first.commit(|_| true).unwrap();

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    let base = append.begin_source_append(source.clone()).unwrap().clone();
    append.add_document(document(&source, 2, "delta")).unwrap();
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
fn append_rejects_an_identity_already_in_the_base() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(source.clone()).unwrap();
    first.add_document(document(&source, 1, "base")).unwrap();
    first
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    first.commit(|_| true).unwrap();

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    append.begin_source_append(source.clone()).unwrap();
    let error = append
        .add_document(document(&source, 1, "duplicate"))
        .unwrap_err();
    assert!(matches!(error, IndexError::DuplicateEventIdentity(_)));
}

#[test]
fn verified_reader_remains_pinned_to_its_generation() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(source.clone()).unwrap();
    first
        .add_document(document(&source, 1, "old pinned generation"))
        .unwrap();
    first.certify_source(certificate(&source, 1, 1)).unwrap();
    first.commit(|_| true).unwrap();
    let old_reader = VerifiedIndex::open(temp.path()).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    replacement.begin_source(source.clone()).unwrap();
    replacement
        .add_document(document(&source, 1, "new committed generation"))
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
    writer.add_document(document(&source, 1, "body")).unwrap();
    writer.certify_source(certificate).unwrap();
    let receipt = writer.commit(|_| true).unwrap();
    assert_eq!(receipt.generation_id, generation_id);
    assert!(VerifiedIndex::open(temp.path()).is_ok());
    assert!(fs::read_dir(path.parent().unwrap())
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
    writer.add_document(document(&source, 1, "body")).unwrap();
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
    writer.add_document(document(&source, 1, "body")).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let mut stale_manifest = index.manifest().clone();
    stale_manifest.lexical_schema_version = 3;
    let stale_generation_id = stale_manifest.generation_id().unwrap();
    write_manifest(temp.path(), &stale_generation_id, &stale_manifest).unwrap();
    let mut stale_metas = index.searcher.index().load_metas().unwrap();
    stale_metas.payload = Some(
        serde_json::to_string(&CommitPayload {
            version: COMMIT_PAYLOAD_VERSION,
            generation_id: stale_generation_id,
        })
        .unwrap(),
    );

    let error = load_manifest_for_metas(temp.path(), &stale_metas).unwrap_err();
    assert!(matches!(
        error,
        IndexError::GenerationContractMismatch {
            identity: IDENTITY_VERSION,
            schema: 3,
            analyzer: LEXICAL_ANALYZER_VERSION,
            core_record: ctx_history_core::CORE_RECORD_VERSION,
        }
    ));
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
    let temp = tempdir().unwrap();
    let source = source("core-fingerprint-mismatch.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_document(document(&source, 1, "body")).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let pinned = VerifiedIndex::open(temp.path()).unwrap();
    let mut mismatched_manifest = pinned.manifest().clone();
    mismatched_manifest.core_record_contract_fingerprint = "0".repeat(64);
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
                && actual == "0".repeat(64)
    ));
}

#[test]
fn policy_field_change_changes_hash_and_generation_id() {
    let manifest = GenerationManifest::from_sources(Vec::new()).unwrap();
    let mut changed_policy = current_source_generation_policy();
    changed_policy.semantic.chunk_overlap_chars += 1;
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
    writer.add_document(document(&source, 1, "body")).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

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
}

#[test]
fn certificate_count_mismatch_is_rejected_before_commit() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_document(document(&source, 1, "body")).unwrap();
    let error = writer
        .certify_source(certificate(&source, 1, 2))
        .unwrap_err();
    assert!(matches!(
        error,
        IndexError::SourceDocumentCountMismatch { .. }
    ));
}

#[test]
fn duplicate_event_identity_is_rejected_before_commit() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    let duplicate = document(&source, 1, "first");
    writer.add_document(duplicate.clone()).unwrap();
    let error = writer.add_document(duplicate).unwrap_err();
    assert!(matches!(error, IndexError::DuplicateEventIdentity(_)));
}

#[test]
fn verified_generation_rejects_a_forged_duplicate_event_identity() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_document(document(&source, 1, "body")).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let pinned = VerifiedIndex::open(temp.path()).unwrap();
    let addresses = pinned.searcher.search(&AllQuery, &DocSetCollector).unwrap();
    let duplicate = pinned
        .searcher
        .doc(addresses.into_iter().next().unwrap())
        .unwrap();
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![certificate(&source, 2, 2)]).unwrap(),
        &[],
        vec![duplicate],
    );

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let reference_error =
        crate::publication::verify_searcher_reference(&searcher, &manifest).unwrap_err();
    let one_pass_error = verify_searcher(&searcher, &manifest).unwrap_err();
    assert_eq!(
        std::mem::discriminant(&reference_error),
        std::mem::discriminant(&one_pass_error)
    );
    let error = match VerifiedIndex::open(temp.path()) {
        Ok(_) => panic!("duplicate event generation unexpectedly opened"),
        Err(error) => error,
    };
    assert!(matches!(error, IndexError::DuplicateEventIdentity(_)));
}

#[test]
fn verified_generation_rejects_forged_source_ownership() {
    let temp = tempdir().unwrap();
    let first = source("first.jsonl");
    let second = source("second.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(first.clone()).unwrap();
    writer.add_document(document(&first, 1, "body")).unwrap();
    writer.certify_source(certificate(&first, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let pinned = VerifiedIndex::open(temp.path()).unwrap();
    let fields = fields_from_schema(pinned.searcher.schema()).unwrap();
    let address = pinned
        .searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let document = pinned.searcher.doc::<TantivyDocument>(address).unwrap();
    let mut forged = TantivyDocument::default();
    for (field, value) in document.field_values() {
        if field != fields.source_key {
            forged.add_field_value(field, value);
        }
    }
    forged.add_text(fields.source_key, source_token(&second));
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![certificate(&second, 2, 1)]).unwrap(),
        std::slice::from_ref(&first),
        vec![forged],
    );

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let reference_error =
        crate::publication::verify_searcher_reference(&searcher, &manifest).unwrap_err();
    let one_pass_error = verify_searcher(&searcher, &manifest).unwrap_err();
    assert_eq!(
        std::mem::discriminant(&reference_error),
        std::mem::discriminant(&one_pass_error)
    );
    let error = match VerifiedIndex::open(temp.path()) {
        Ok(_) => panic!("source ownership mismatch unexpectedly opened"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        IndexError::InvalidStoredDocumentField("core_record")
    ));
}

#[test]
fn verified_generation_rejects_malformed_stored_core_record() {
    let temp = tempdir().unwrap();
    let source = source("malformed-core.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_document(document(&source, 1, "complete body"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let pinned = VerifiedIndex::open(temp.path()).unwrap();
    let fields = fields_from_schema(pinned.searcher.schema()).unwrap();
    let address = pinned
        .searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let document = pinned.searcher.doc::<TantivyDocument>(address).unwrap();
    let mut forged = TantivyDocument::default();
    for (field, value) in document.field_values() {
        if field != fields.core_record {
            forged.add_field_value(field, value);
        }
    }
    forged.add_bytes(fields.core_record, b"{");
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![certificate(&source, 2, 1)]).unwrap(),
        std::slice::from_ref(&source),
        vec![forged],
    );

    let error = match VerifiedIndex::open(temp.path()) {
        Ok(_) => panic!("malformed stored Core generation unexpectedly opened"),
        Err(error) => error,
    };
    assert!(matches!(error, IndexError::CoreRecord(_)));
}

#[test]
fn document_identity_kinds_are_checked() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    let mut invalid = document(&source, 1, "body");
    invalid.event_id = invalid.session_id;
    let error = writer.add_document(invalid).unwrap_err();
    assert!(matches!(error, IndexError::CoreRecord(_)));
}

#[test]
fn document_identities_must_belong_to_the_document_source() {
    let temp = tempdir().unwrap();
    let first = source("first");
    let second = source("second");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(second.clone()).unwrap();
    let mut invalid = document(&first, 1, "body");
    invalid.locator = document(&second, 2, "other").locator;
    invalid.source = second;
    let error = writer.add_document(invalid).unwrap_err();
    assert!(matches!(error, IndexError::CoreRecord(_)));
}

#[test]
fn empty_body_is_rejected_without_an_index_side_length_limit() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    let error = writer.add_document(document(&source, 1, "")).unwrap_err();
    assert!(matches!(
        error,
        IndexError::EmptyDocumentField { field: "body" }
    ));
}

#[test]
fn invalid_memory_budget_has_no_filesystem_side_effect() {
    let parent = tempdir().unwrap();
    let root = parent.path().join("not-created");
    let error = match GenerationWriter::open(
        &root,
        WriterOptions {
            indexer_threads: 2,
            memory_bytes: 1,
        },
    ) {
        Ok(_) => panic!("invalid memory budget unexpectedly opened an index"),
        Err(error) => error,
    };
    assert!(matches!(error, IndexError::IndexMemoryTooSmall { .. }));
    assert!(!root.exists());
}

#[test]
fn one_pass_verifier_matches_reference_with_bounded_parallel_segment_state() {
    const SOURCE_COUNT: usize = 6;
    const DOCUMENTS_PER_SOURCE: u64 = 24;

    let (temp, sources) = multisegment_fixture(SOURCE_COUNT, DOCUMENTS_PER_SOURCE);
    let (searcher, manifest) = open_unverified_generation(temp.path());
    assert_eq!(sources.len(), SOURCE_COUNT);
    assert_eq!(searcher.segment_readers().len(), SOURCE_COUNT);

    let reference =
        crate::publication::verify_searcher_reference_with_metrics(&searcher, &manifest).unwrap();
    let one_pass =
        crate::publication::verify_searcher_with_metrics(&searcher, &manifest, 2, true).unwrap();
    let expected_documents = SOURCE_COUNT * DOCUMENTS_PER_SOURCE as usize;

    assert_eq!(reference.query_passes, SOURCE_COUNT + 1);
    assert_eq!(
        reference.segment_query_visits,
        (SOURCE_COUNT + 1) * SOURCE_COUNT
    );
    assert_eq!(reference.document_decodes, expected_documents);
    assert_eq!(one_pass.worker_budget, 2);
    assert_eq!(one_pass.segment_tasks, SOURCE_COUNT);
    assert_eq!(one_pass.document_decodes, expected_documents);
    assert_eq!(one_pass.source_terms, SOURCE_COUNT);
    assert_eq!(one_pass.max_active_workers, 2);
    assert!(one_pass.segment_tasks < reference.segment_query_visits);

    assert_eq!(one_pass.max_buffered_segments, one_pass.worker_budget);
    assert!(
        one_pass.max_buffered_event_identities
            <= one_pass.worker_budget * DOCUMENTS_PER_SOURCE as usize
    );
    assert!(
        one_pass.max_buffered_session_identities <= one_pass.worker_budget,
        "one fixture session identity per segment should be buffered"
    );
    assert!(
        one_pass.max_buffered_event_identities < expected_documents,
        "temporary segment maps must be bounded below full-generation cardinality"
    );
}

#[test]
fn one_pass_verifier_matches_reference_for_identity_digest_corruption() {
    let temp = tempdir().unwrap();
    let source = source("digest-corruption.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_document(document(&source, 1, "body")).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let pinned = VerifiedIndex::open(temp.path()).unwrap();
    let fields = fields_from_schema(pinned.searcher.schema()).unwrap();
    let address = pinned
        .searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let document = pinned.searcher.doc::<TantivyDocument>(address).unwrap();
    let mut forged = TantivyDocument::default();
    for (field, value) in document.field_values() {
        if field != fields.event_identity_digest {
            forged.add_field_value(field, value);
        }
    }
    forged.add_text(fields.event_identity_digest, "00");
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![certificate(&source, 2, 1)]).unwrap(),
        std::slice::from_ref(&source),
        vec![forged],
    );

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let reference =
        crate::publication::verify_searcher_reference(&searcher, &manifest).unwrap_err();
    let one_pass = verify_searcher(&searcher, &manifest).unwrap_err();
    assert_eq!(
        std::mem::discriminant(&reference),
        std::mem::discriminant(&one_pass)
    );
    assert!(matches!(
        one_pass,
        IndexError::InvalidStoredDocumentField("event_identity")
    ));
}

#[test]
fn one_pass_verifier_matches_reference_for_source_count_corruption() {
    let temp = tempdir().unwrap();
    let first = source("count-first.jsonl");
    let second = source("count-second.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(first.clone()).unwrap();
    writer.add_document(document(&first, 1, "first")).unwrap();
    writer.certify_source(certificate(&first, 1, 1)).unwrap();
    writer.begin_source(second.clone()).unwrap();
    writer.add_document(document(&second, 1, "second")).unwrap();
    writer.add_document(document(&second, 2, "second")).unwrap();
    writer.certify_source(certificate(&second, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();

    let pinned = VerifiedIndex::open(temp.path()).unwrap();
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![
            certificate(&first, 2, 2),
            certificate(&second, 2, 1),
        ])
        .unwrap(),
        &[],
        Vec::new(),
    );

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let reference =
        crate::publication::verify_searcher_reference(&searcher, &manifest).unwrap_err();
    let one_pass = verify_searcher(&searcher, &manifest).unwrap_err();
    assert_eq!(
        std::mem::discriminant(&reference),
        std::mem::discriminant(&one_pass)
    );
    assert!(matches!(one_pass, IndexError::SourceCountMismatch { .. }));
}

#[test]
fn one_pass_verifier_matches_reference_for_total_count_corruption() {
    let temp = tempdir().unwrap();
    let source = source("total-count.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_document(document(&source, 1, "body")).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let pinned = VerifiedIndex::open(temp.path()).unwrap();
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![certificate(&source, 2, 2)]).unwrap(),
        &[],
        Vec::new(),
    );

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let reference =
        crate::publication::verify_searcher_reference(&searcher, &manifest).unwrap_err();
    let one_pass = verify_searcher(&searcher, &manifest).unwrap_err();
    assert_eq!(
        std::mem::discriminant(&reference),
        std::mem::discriminant(&one_pass)
    );
    assert!(matches!(
        one_pass,
        IndexError::DocumentCountMismatch {
            manifest: 2,
            index: 1
        }
    ));
}
