#[test]
fn certificate_count_mismatch_is_rejected_before_commit() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    let error = writer
        .certify_source(certificate(&source, 1, 2))
        .unwrap_err();
    assert!(matches!(
        error,
        IndexError::SourceDocumentCountMismatch { .. }
    ));
}

#[test]
fn duplicate_event_identity_is_rejected_by_prepublication_term_audit() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    let duplicate = document(&source, 1, "first");
    writer.add_core_record(duplicate.clone()).unwrap();
    writer.add_core_record(duplicate).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    let error = writer.commit(|_| true).unwrap_err();
    assert!(matches!(error, IndexError::DuplicateEventIdentity(_)));
    assert!(load_active_generation_pointer(temp.path())
        .unwrap()
        .is_none());
}

#[test]
fn copied_event_resolves_exactly_to_its_declared_ancestor() {
    let temp = tempdir().unwrap();
    let source = source("valid-copy.jsonl");
    let original = document_for_session(&source, "root", 1, "original");
    let mut copy = document_for_session(&source, "child", 2, "copy");
    copy.set_session_relationship(
        SessionRelationshipKind::Forked,
        Some(original.session_id),
        original.session_id,
    )
    .unwrap();
    copy.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: Box::new(original.session_id),
        ancestor_event_id: Box::new(original.event_id),
        proof: EventCopyProofKind::NativeEventIdentity,
    };

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(original).unwrap();
    writer.add_core_record(copy).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();
}

#[test]
fn copied_event_with_a_missing_target_cannot_publish() {
    let temp = tempdir().unwrap();
    let source = source("missing-copy.jsonl");
    let missing = document_for_session(&source, "root", 1, "missing");
    let mut copy = document_for_session(&source, "child", 2, "copy");
    copy.set_session_relationship(
        SessionRelationshipKind::Forked,
        Some(missing.session_id),
        missing.session_id,
    )
    .unwrap();
    copy.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: Box::new(missing.session_id),
        ancestor_event_id: Box::new(missing.event_id),
        proof: EventCopyProofKind::NativeEventIdentity,
    };

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(copy).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    assert!(matches!(
        writer.commit(|_| true),
        Err(IndexError::InvalidSessionRelationshipGraph(_))
            | Err(IndexError::InvalidEventOriginGraph(_))
    ));
}

#[test]
fn deleted_session_terms_without_live_postings_do_not_block_publication() {
    let temp = tempdir().unwrap();
    let removed_source = source("removed-session.jsonl");
    let removed = document_for_session(&removed_source, "removed", 1, "removed session");

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(removed_source.clone()).unwrap();
    initial.add_core_record(removed).unwrap();
    initial
        .certify_source(certificate(&removed_source, 1, 1))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let (deletion, inventory) = deletion_evidence(&removed_source, 2);
    let mut deleting = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    deleting.delete_source(deletion, inventory).unwrap();
    deleting.commit(|_| true).unwrap();

    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().document_count(),
        0
    );
}

#[test]
fn deleting_a_live_parent_rejects_the_dangling_child_and_preserves_the_generation() {
    let temp = tempdir().unwrap();
    let parent_source = source("parent-session.jsonl");
    let child_source = source("child-session.jsonl");
    let parent = document_for_session(&parent_source, "parent", 1, "parent session");
    let mut child = document_for_session(&child_source, "child", 1, "child session");
    child
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(parent.session_id),
            parent.session_id,
        )
        .unwrap();

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    for (source, record) in [(&parent_source, parent), (&child_source, child)] {
        initial.begin_source(source.clone()).unwrap();
        initial.add_core_record(record).unwrap();
        initial.certify_source(certificate(source, 1, 1)).unwrap();
    }
    let baseline = initial.commit(|_| true).unwrap();

    let (deletion, inventory) =
        deletion_evidence_with_retained(&parent_source, 2, vec![child_source]);
    let mut deleting = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    deleting.delete_source(deletion, inventory).unwrap();
    assert!(matches!(
        deleting.commit(|_| true),
        Err(IndexError::InvalidSessionRelationshipGraph(
            "related session does not exist"
        ))
    ));
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
        baseline.generation_id
    );
}

#[test]
fn direct_copy_chain_resolves_to_one_noncopy_original() {
    let temp = tempdir().unwrap();
    let source = source("copy-chain.jsonl");
    let original = document_for_session(&source, "root", 1, "original");
    let mut middle = document_for_session(&source, "middle", 2, "middle copy");
    middle
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(original.session_id),
            original.session_id,
        )
        .unwrap();
    middle.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: Box::new(original.session_id),
        ancestor_event_id: Box::new(original.event_id),
        proof: EventCopyProofKind::NativeEventIdentity,
    };
    let mut leaf = document_for_session(&source, "leaf", 3, "leaf copy");
    leaf.set_session_relationship(
        SessionRelationshipKind::Forked,
        Some(middle.session_id),
        original.session_id,
    )
    .unwrap();
    leaf.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: Box::new(middle.session_id),
        ancestor_event_id: Box::new(middle.event_id),
        proof: EventCopyProofKind::NativeEventIdentity,
    };

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in [original, middle, leaf] {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();
}

#[test]
fn changed_intermediate_edge_revalidates_unchanged_descendant_copy() {
    let temp = tempdir().unwrap();
    let root_source = source("inverse-copy-root.jsonl");
    let ancestor_source = source("inverse-copy-ancestor.jsonl");
    let intermediate_source = source("inverse-copy-intermediate.jsonl");
    let copy_source = source("inverse-copy-descendant.jsonl");

    let root = document_for_session(&root_source, "root", 1, "root");
    let mut ancestor = document_for_session(&ancestor_source, "ancestor-a", 1, "ancestor A");
    ancestor
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(root.session_id),
            root.session_id,
        )
        .unwrap();
    let mut intermediate =
        document_for_session(&intermediate_source, "intermediate-b", 1, "intermediate B");
    intermediate
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(ancestor.session_id),
            root.session_id,
        )
        .unwrap();
    let mut copy = document_for_session(&copy_source, "descendant-c", 1, "copied in C");
    copy.set_session_relationship(
        SessionRelationshipKind::Forked,
        Some(intermediate.session_id),
        root.session_id,
    )
    .unwrap();
    copy.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: Box::new(ancestor.session_id),
        ancestor_event_id: Box::new(ancestor.event_id),
        proof: EventCopyProofKind::NativeEventIdentity,
    };

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    for (source, record) in [
        (&root_source, root.clone()),
        (&ancestor_source, ancestor),
        (&intermediate_source, intermediate),
        (&copy_source, copy),
    ] {
        initial.begin_source(source.clone()).unwrap();
        initial.add_core_record(record).unwrap();
        initial.certify_source(certificate(source, 1, 1)).unwrap();
    }
    let baseline = initial.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacement
        .begin_source(intermediate_source.clone())
        .unwrap();
    let mut reparented =
        document_for_session(&intermediate_source, "intermediate-b", 1, "intermediate B");
    reparented
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(root.session_id),
            root.session_id,
        )
        .unwrap();
    replacement.add_core_record(reparented).unwrap();
    replacement
        .certify_source(certificate(&intermediate_source, 2, 1))
        .unwrap();

    assert!(matches!(
        replacement.commit(|_| true),
        Err(IndexError::InvalidEventOriginGraph(
            "declared origin session is not an ancestor"
        ))
    ));
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
        baseline.generation_id
    );
}

#[test]
fn changed_edge_revalidates_copies_in_transitive_descendants() {
    let temp = tempdir().unwrap();
    let root_source = source("transitive-copy-root.jsonl");
    let ancestor_source = source("transitive-copy-ancestor.jsonl");
    let changed_source = source("transitive-copy-changed.jsonl");
    let child_source = source("transitive-copy-child.jsonl");
    let copy_source = source("transitive-copy-leaf.jsonl");

    let root = document_for_session(&root_source, "root", 1, "root");
    let mut ancestor = document_for_session(&ancestor_source, "ancestor", 1, "ancestor");
    ancestor
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(root.session_id),
            root.session_id,
        )
        .unwrap();
    let mut changed = document_for_session(&changed_source, "changed", 1, "changed");
    changed
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(ancestor.session_id),
            root.session_id,
        )
        .unwrap();
    let mut child = document_for_session(&child_source, "child", 1, "child");
    child
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(changed.session_id),
            root.session_id,
        )
        .unwrap();
    let mut copy = document_for_session(&copy_source, "copy", 1, "copy");
    copy.set_session_relationship(
        SessionRelationshipKind::Forked,
        Some(child.session_id),
        root.session_id,
    )
    .unwrap();
    copy.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: Box::new(ancestor.session_id),
        ancestor_event_id: Box::new(ancestor.event_id),
        proof: EventCopyProofKind::NativeEventIdentity,
    };

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    for (source, record) in [
        (&root_source, root.clone()),
        (&ancestor_source, ancestor),
        (&changed_source, changed),
        (&child_source, child),
        (&copy_source, copy),
    ] {
        initial.begin_source(source.clone()).unwrap();
        initial.add_core_record(record).unwrap();
        initial.certify_source(certificate(source, 1, 1)).unwrap();
    }
    initial.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacement.begin_source(changed_source.clone()).unwrap();
    let mut reparented = document_for_session(&changed_source, "changed", 1, "changed");
    reparented
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(root.session_id),
            root.session_id,
        )
        .unwrap();
    replacement.add_core_record(reparented).unwrap();
    replacement
        .certify_source(certificate(&changed_source, 2, 1))
        .unwrap();

    assert!(matches!(
        replacement.commit(|_| true),
        Err(IndexError::InvalidEventOriginGraph(
            "declared origin session is not an ancestor"
        ))
    ));
}

#[test]
fn cyclic_session_relationships_cannot_publish() {
    let temp = tempdir().unwrap();
    let source = source("session-cycle.jsonl");
    let root = document_for_session(&source, "root", 1, "root");
    let mut first = document_for_session(&source, "first", 2, "first");
    let mut second = document_for_session(&source, "second", 3, "second");
    first
        .set_session_relationship(
            SessionRelationshipKind::RelatedUnknown,
            Some(second.session_id),
            root.session_id,
        )
        .unwrap();
    second
        .set_session_relationship(
            SessionRelationshipKind::RelatedUnknown,
            Some(first.session_id),
            root.session_id,
        )
        .unwrap();

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in [root, first, second] {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    assert!(matches!(
        writer.commit(|_| true),
        Err(IndexError::InvalidSessionRelationshipGraph(
            "session relationship cycle"
        ))
    ));
}

#[test]
fn verified_generation_rejects_a_forged_duplicate_event_identity() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let pinned = VerifiedIndex::open(temp.path()).unwrap();
    let addresses = pinned.searcher.search(&AllQuery, &DocSetCollector).unwrap();
    let address = addresses.into_iter().next().unwrap();
    let duplicate = indexed_document(decoded_stored_core(&pinned.searcher, address));
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![certificate(&source, 2, 2)]).unwrap(),
        &[],
        vec![duplicate],
    );

    let (searcher, manifest) = open_unverified_generation(temp.path());
    assert!(matches!(
        verify_searcher(&searcher, &manifest),
        Err(IndexError::DuplicateEventIdentity(_))
    ));
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
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(first.clone()).unwrap();
    writer.add_core_record(document(&first, 1, "body")).unwrap();
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
    let document = indexed_document(decoded_stored_core(&pinned.searcher, address));
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
    assert!(matches!(
        verify_searcher(&searcher, &manifest),
        Err(IndexError::InvalidStoredDocumentField("core_record"))
    ));
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
fn verified_generation_rejects_malformed_stored_core_during_exhaustive_audit() {
    let temp = tempdir().unwrap();
    let source = source("malformed-core.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    let event = document(&source, 1, "complete body");
    writer.add_core_record(event).unwrap();
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
        if field != fields.core_record && field != fields.core_record_encoded_bytes {
            forged.add_field_value(field, value);
        }
    }
    forged.add_u64(fields.core_record_encoded_bytes, 1);
    forged.add_bytes(fields.core_record, b"{");
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![certificate(&source, 2, 1)]).unwrap(),
        std::slice::from_ref(&source),
        vec![forged],
    );

    assert!(matches!(
        VerifiedIndex::open(temp.path()),
        Err(IndexError::CoreRecord(_))
    ));
}

#[test]
fn document_identity_kinds_are_checked() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    let mut invalid = document(&source, 1, "body");
    invalid.event_id = invalid.session_id;
    let error = writer.add_core_record(invalid).unwrap_err();
    assert!(matches!(error, IndexError::CoreRecord(_)));
}

#[test]
fn document_identities_must_belong_to_the_document_source() {
    let temp = tempdir().unwrap();
    let first = source("first");
    let second = source("second");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(second.clone()).unwrap();
    let mut invalid = document(&first, 1, "body");
    invalid.source = second;
    let error = writer.add_core_record(invalid).unwrap_err();
    assert!(matches!(error, IndexError::CoreRecord(_)));
}

#[test]
fn empty_core_body_is_rejected_by_the_canonical_writer_validation() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    let mut invalid = document(&source, 1, "body");
    invalid.content.normalized_body = Some(String::new());
    let error = writer.add_core_record(invalid).unwrap_err();
    assert!(matches!(error, IndexError::CoreRecord(_)));
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
